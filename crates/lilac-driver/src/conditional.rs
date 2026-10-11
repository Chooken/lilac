//! Top-level conditionals (SPEC §6 "Conditional items"): an `if` chain or
//! `match` over items whose condition is constant. Only the selected arm's
//! items exist, spliced in place of the conditional before the program is
//! resolved.
//!
//! Selection runs in rounds. Each round resolves the program as it stands
//! (undecided conditionals define nothing), turns each outermost
//! conditional into a selector (an `if`/`match` value yielding the selected
//! arm's index), checks only the selectors and what they reach, and folds
//! them with the constant evaluator. A selector that fails to check is
//! retried in a later round while other conditionals still get decided,
//! since it may name an item one of them declares. Arms may hold further
//! conditionals, decided in the next round. Diagnostics of a round are
//! scratch: only a stuck selector's own errors (those inside its condition)
//! are reported, and the final resolve reports everything else.

use std::collections::HashMap;

use lilac_ast::{
    ArmTest, Conditional, Expr, Item, ItemKind, MatchArm, MatchArmBody, Program, Stmt,
};
use lilac_error::{codes, Diagnostic, Diags, Level};
use lilac_hir::{MemberReceiver, MemberSite, SemanticSite, TagSite};
use lilac_module::Project;
use lilac_span::{FileId, SourceMap, Span};
use lilac_target::Target;

/// What happens to one conditional this round.
enum Decision {
    /// Splice in the items of this arm (`arms.len()`: the `else`, if any).
    Arm(usize),
    /// Drop it: its condition was reported.
    Remove,
}

/// One outermost conditional: its file, the span that identifies it, and
/// the conditional itself.
struct Found<'p> {
    file: FileId,
    span: Span,
    conditional: &'p Conditional,
}

/// Decides every top-level conditional of `programs`, splicing in the
/// selected arms, and refreshes `project`'s `using`s to the live ones.
/// Returns what the editor needs from the checked conditions, which the
/// final check never sees.
pub(crate) fn select(
    project: &mut Project,
    programs: &mut [(FileId, Program)],
    diags: &mut Diags,
    sources: &SourceMap,
    target: Target,
) -> Sites {
    let mut any = false;
    let mut sites = Sites::default();
    loop {
        let decisions = round(project, programs, diags, sources, target, &mut sites);
        if decisions.is_empty() {
            break;
        }
        any = true;
        for (_, program) in programs.iter_mut() {
            splice(&mut program.items, &decisions);
        }
    }
    if any {
        refresh_usings(project, programs);
    }
    sites
}

/// The editor sites of the checked conditions.
#[derive(Default)]
pub(crate) struct Sites {
    /// Semantic-token sites.
    pub semantics: Vec<SemanticSite>,
    /// Tags, for completion.
    pub tags: Vec<TagSite>,
    /// Namespace members, for completion. A round's types and definitions
    /// are its own, so sites of a value or type receiver are dropped; a
    /// namespace is the same in every round.
    pub members: Vec<MemberSite>,
}

/// One round: the decision for each outermost conditional decided now
/// (empty when there are none left). The conditions' editor sites go to
/// `sites`.
fn round(
    project: &mut Project,
    programs: &[(FileId, Program)],
    diags: &mut Diags,
    sources: &SourceMap,
    target: Target,
    sites: &mut Sites,
) -> HashMap<Span, Decision> {
    let mut found = Vec::new();
    for (file, program) in programs {
        outermost(*file, &program.items, &mut found);
    }
    if found.is_empty() {
        return HashMap::new();
    }
    refresh_usings(project, programs);
    // Scratch starts with the errors so far (the parser's), so poison from a
    // construct the parser reported finds its error.
    let mut scratch = Diags::new();
    for diagnostic in diags.diagnostics() {
        if diagnostic.level == Level::Error {
            scratch.emit(diagnostic.clone());
        }
    }
    let resolved = lilac_resolve::resolve(project, programs, &mut scratch);
    let selectors: Vec<(FileId, Expr)> = found
        .iter()
        .map(|found| (found.file, selector(found.conditional, found.span)))
        .collect();
    let mut types = lilac_ty::Types::new();
    let options = lilac_typeck::Options {
        native_bits: target.bit_native_width(),
        require_entry: false,
    };
    let checked =
        lilac_typeck::check_selectors(&resolved, &mut types, &mut scratch, options, &selectors);
    sites.semantics.extend_from_slice(&checked.hir.semantics);
    sites.tags.extend_from_slice(&checked.hir.tags);
    sites.members.extend(
        checked
            .hir
            .members
            .iter()
            .filter(|site| matches!(site.receiver, MemberReceiver::Namespace(_))),
    );
    let values = lilac_lower::evaluate_selectors(
        &checked.hir,
        &resolved,
        &types,
        sources,
        &target.to_triple(),
        u64::from(target.bit_native_width() / 8),
        &checked.values,
    );
    // While some selector checks, the failed ones may be waiting on an item
    // it declares: they are retried next round.
    let progress = checked.values.iter().any(Option::is_some);
    let mut decisions = HashMap::new();
    for ((found, checked), value) in found.iter().zip(&checked.values).zip(values) {
        let tests = found.conditional.test_spans();
        let decision = match (checked, value) {
            (Some(_), Some(index)) => Decision::Arm(usize::try_from(index).unwrap_or(usize::MAX)),
            (Some(_), None) => {
                diags.emit(not_constant(&tests));
                Decision::Remove
            }
            (None, _) if progress => continue,
            (None, _) => {
                report_stuck(&scratch, &tests, diags);
                Decision::Remove
            }
        };
        decisions.insert(found.span, decision);
    }
    decisions
}

/// Collects the conditionals of `items` not inside another one (looking
/// through `private` blocks).
fn outermost<'p>(file: FileId, items: &'p [Item], out: &mut Vec<Found<'p>>) {
    for item in items {
        match &item.kind {
            ItemKind::Conditional(conditional) => out.push(Found {
                file,
                span: item.span,
                conditional,
            }),
            ItemKind::Private { items } => outermost(file, items, out),
            _ => {}
        }
    }
}

/// The selector of `conditional`: an `if` or `match` value that is the
/// index of the arm whose test holds, else `arms.len()`.
fn selector(conditional: &Conditional, span: Span) -> Expr {
    let index = |at: usize, span: Span| Expr::Number(at.to_string(), span);
    let fallback = index(conditional.arms.len(), span);
    match &conditional.scrutinee {
        None => conditional
            .arms
            .iter()
            .enumerate()
            .rev()
            .fold(fallback, |els, (at, arm)| {
                let ArmTest::If(cond) = &arm.test else {
                    return els;
                };
                Expr::IfValue {
                    cond: Box::new(cond.clone()),
                    then: Box::new(index(at, cond.span())),
                    els: Box::new(els),
                    span,
                }
            }),
        Some(scrutinee) => Expr::MatchValue {
            scrutinee: Box::new(scrutinee.clone()),
            arms: conditional
                .arms
                .iter()
                .enumerate()
                .filter_map(|(at, arm)| {
                    let ArmTest::Pattern(pattern, pattern_span) = &arm.test else {
                        return None;
                    };
                    Some(MatchArm {
                        pattern: pattern.clone(),
                        capture: None,
                        body: MatchArmBody::Inline(Box::new(Stmt::ExprStmt(index(
                            at,
                            *pattern_span,
                        )))),
                        span: arm.span,
                    })
                })
                .collect(),
            els: Box::new(fallback),
            span,
        },
    }
}

/// `E039` at the condition of a conditional that checks but is not
/// constant.
fn not_constant(tests: &[Span]) -> Diagnostic {
    let mut diagnostic = Diagnostic::error(codes::E039, "a top-level condition must be constant");
    for (at, span) in tests.iter().enumerate() {
        diagnostic = diagnostic.label(*span, at == 0, "not a constant expression");
    }
    diagnostic.help("conditions over items may only use constants, `=>` functions and `@builtin`")
}

/// Reports why a selector that never checked failed: its errors inside the
/// condition, or `E039` when they all lie elsewhere (in code it calls).
fn report_stuck(scratch: &Diags, tests: &[Span], diags: &mut Diags) {
    let inside = |span: Span| {
        tests
            .iter()
            .any(|test| test.file == span.file && test.lo.0 <= span.lo.0 && span.hi.0 <= test.hi.0)
    };
    let mut reported = false;
    for diagnostic in scratch.diagnostics() {
        let primary = diagnostic.labels.iter().find(|label| label.primary);
        if diagnostic.level == Level::Error && primary.is_some_and(|label| inside(label.span)) {
            diags.emit(diagnostic.clone());
            reported = true;
        }
    }
    if !reported {
        diags.emit(
            not_constant(tests)
                .note("checking it failed in code it uses, reported where that code is used"),
        );
    }
}

/// Replaces each decided conditional in `items` (and in `private` blocks)
/// by the items of its selected arm.
fn splice(items: &mut Vec<Item>, decisions: &HashMap<Span, Decision>) {
    let old = std::mem::take(items);
    for mut item in old {
        match &mut item.kind {
            ItemKind::Conditional(conditional) => match decisions.get(&item.span) {
                Some(Decision::Arm(at)) => {
                    let selected = if *at < conditional.arms.len() {
                        Some(std::mem::take(&mut conditional.arms[*at].items))
                    } else {
                        conditional.els.take()
                    };
                    items.extend(selected.into_iter().flatten());
                }
                Some(Decision::Remove) => {}
                None => items.push(item),
            },
            ItemKind::Private { items: inner } => {
                splice(inner, decisions);
                items.push(item);
            }
            _ => items.push(item),
        }
    }
}

/// Sets each file's `using`s to those of its live items.
fn refresh_usings(project: &mut Project, programs: &[(FileId, Program)]) {
    let programs: HashMap<FileId, &Program> = programs
        .iter()
        .map(|(file, program)| (*file, program))
        .collect();
    let namespaces = &project.namespaces;
    for unit in &mut project.files {
        if let Some(program) = programs.get(&unit.file) {
            unit.usings = lilac_module::usings_of(program, &namespaces[unit.ns].path);
        }
    }
}
