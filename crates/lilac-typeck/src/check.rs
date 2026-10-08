//! Body checking: statements and expressions with expected types.

use std::collections::{HashMap, HashSet};

use lilac_ast::{BinOp, Expr, Item, ItemKind, Program, Stmt, Type as AstType};
use lilac_error::{Diagnostic, Diags, ErrorGuaranteed};
use lilac_hir::{
    HirArm, HirBlock, HirCallee, HirConst, HirExpr, HirExprKind, HirField, HirFunc, HirParam,
    HirPattern, HirProgram, HirStmt, NameSite, SemanticKind, SemanticSite, TempId,
};
use lilac_resolve::{flat_member_pairs, DefId, ResItem, ResKind, ResolvedProject, Visibility};
use lilac_span::{FileId, Span};
use lilac_ty::{FuncSig, TypeId, TypeRef, Types};

use super::{
    lookup_intrinsic, lookup_one, qualify_name, resolve_type, FloatWidth, Intrinsic, Sigs,
};

mod generics;
use generics::FuncInstance;

/// A checked `=>` body cached for call-site expansion: parameter names,
/// control-flow prelude, and value HIR.
type InlineBody = (Vec<String>, Vec<HirStmt>, HirExpr);

/// One checked call: overload selected, arguments checked.
struct CheckedCall {
    /// Return-slot types.
    outs: Vec<TypeRef>,
    /// Resolved callee.
    callee: HirCallee,
    /// Checked arguments in order.
    args: Vec<HirExpr>,
}

/// The checked shape of one function body, shared by concrete functions and
/// monomorphized generic instances.
enum CheckedBody {
    /// No body (extern/declaration): an empty block, no slot sweep.
    Extern,
    /// A checked `{ ... }` block.
    Block(HirBlock),
    /// A checked `=>` body: parameter names, control-flow prelude, and value
    /// (inlined at calls).
    Inline {
        /// Parameter names in order (for call-site substitution).
        param_names: Vec<String>,
        /// Statements the body's control flow produced (emitted at each call).
        prelude: Vec<HirStmt>,
        /// Checked body value.
        value: HirExpr,
    },
    /// Rejected (bad shape or deferred control flow): nothing is cached.
    Rejected,
}

/// A call either stays a call, vanishes into an inlined `=>` body, or is a
/// recorded `@compile_error` (folding decides its liveness).
enum CallOutcome {
    /// Ordinary call (emitted as HIR).
    Call(CheckedCall),
    /// Substituted `=>` body with its type.
    Inline(HirExpr, TypeRef),
    /// Recorded `@compile_error` (never E022, even in value position).
    CompilerError(HirExpr),
}

/// A multi-`let` call: out-slot types, value HIR, and call span.
struct MultiCall {
    outs: Vec<TypeRef>,
    value_hir: HirExpr,
    call_span: Span,
}

/// A method-call receiver: checked type plus its HIR.
struct Receiver {
    ty: TypeRef,
    hir: HirExpr,
}

/// A capture condition call: out-slot types, value HIR, call span, and
/// callee definition for slot names.
struct CapturedCall {
    outs: Vec<TypeRef>,
    value_hir: HirExpr,
    call_span: Span,
    callee_def: Option<DefId>,
}

/// A callee known to have an `=>` body, with everything needed to check it.
struct InlineCallee<'a> {
    def: DefId,
    ast: &'a Item,
    resolved: ResItem,
    decl: &'a lilac_ast::FuncDecl,
    parent: Option<(String, DefId)>,
}

/// Checks every non-generic body and builds typed HIR in the same walk.
/// Generic items belong to M4; `E052` for generic hooks already fired during
/// signature collection. Only error-free programs produce usable HIR: the
/// driver discards it whenever diagnostics contain errors.
pub fn check_bodies(
    program: &Program,
    project: &ResolvedProject,
    sigs: &Sigs,
    types: &mut Types,
    diags: &mut Diags,
    native_bits: u32,
) -> HirProgram {
    let mut checker = Checker {
        program,
        project,
        file: FileId(u32::MAX),
        sigs,
        types,
        diags,
        native_bits,
        checked: HashSet::new(),
        fields: HashMap::new(),
        hir: Vec::new(),
        consts: Vec::new(),
        drops: HashMap::new(),
        inline_bodies: HashMap::new(),
        in_progress: HashSet::new(),
        next_temp: 0,
        instances: HashMap::new(),
        failed: HashSet::new(),
        active: Vec::new(),
        current_params: Vec::new(),
        current_args: Vec::new(),
        instance_inline: HashMap::new(),
        object_instances: HashMap::new(),
        failed_objects: HashSet::new(),
        active_objects: Vec::new(),
        layouts: HashMap::new(),
        reported_failures: HashSet::new(),
        enclosing_object: None,
        uses: Vec::new(),
        semantics: Vec::new(),
    };
    checker.collect_fields(&program.items);
    checker.check_items(&program.items);
    let generic_objects: HashSet<String> = checker
        .project
        .all_items()
        .filter(|(_, item)| {
            matches!(
                &item.kind,
                ResKind::Object { generic_params, .. } if !generic_params.is_empty()
            )
        })
        .map(|(name, item)| qualify_name(checker.project, item.id, name))
        .collect();
    let mut layouts = std::mem::take(&mut checker.layouts);
    if !checker.diags.has_errors() {
        collect_layouts(&mut checker, &program.items, &generic_objects, &mut layouts);
    }
    HirProgram {
        funcs: checker.hir,
        consts: checker.consts,
        layouts,
        drops: checker.drops,
        uses: checker.uses,
        semantics: checker.semantics,
    }
}

/// The field token's span inside a member-access span (the AST keeps only the
/// whole-access span; the field name is the trailing identifier).
fn field_span(span: Span, field: &str) -> Span {
    let len = u32::try_from(field.len()).unwrap_or(u32::MAX);
    Span::new(span.hi.0.saturating_sub(len), span.hi.0, span.file)
}

/// The leading name token's span inside a slot/statement span. `let`/`param`
/// spans begin at the name, so the name occupies the first `name.len()` bytes.
fn name_span(span: Span, name: &str) -> Span {
    let len = u32::try_from(name.len()).unwrap_or(u32::MAX);
    Span::new(span.lo.0, span.lo.0.saturating_add(len), span.file)
}

/// Collects object/enum layouts, descending into `private { … }` blocks so a
/// private object/enum still gets a layout and a private field still occupies
/// its slot (privacy gates access, not layout).
fn collect_layouts(
    checker: &mut Checker,
    items: &[Item],
    generic_objects: &HashSet<String>,
    layouts: &mut HashMap<String, Vec<(String, Option<TypeRef>)>>,
) {
    for item in items {
        if let ItemKind::Private { items } = &item.kind {
            collect_layouts(checker, items, generic_objects, layouts);
            continue;
        }
        let owner = checker.qualify_local(item.span.file, &item.name);
        if generic_objects.contains(&owner) {
            // Generic object layouts are concrete per instance (M4 Task 5);
            // resolving `T` here would leak a template into HIR.
            continue;
        }
        checker.file = item.span.file;
        if let ItemKind::Object { members, .. } = &item.kind {
            let mut fields = Vec::new();
            collect_layout_fields(checker, members, &mut fields);
            layouts.insert(owner, fields);
        } else if let ItemKind::Enum { variants, .. } = &item.kind {
            let mut entries = Vec::new();
            for variant in variants {
                let payload = variant.payload.as_ref().map(|ty| {
                    resolve_type(
                        ty,
                        checker.types,
                        checker.project,
                        checker.file,
                        checker.diags,
                        &[],
                        &mut checker.semantics,
                    )
                });
                entries.push((variant.name.clone(), payload));
            }
            layouts.insert(owner, entries);
        }
    }
}

/// Appends an object's field slots in declaration order, flattening `private`
/// member blocks.
fn collect_layout_fields(
    checker: &mut Checker,
    members: &[Item],
    fields: &mut Vec<(String, Option<TypeRef>)>,
) {
    for member in members {
        if let ItemKind::Private { items } = &member.kind {
            collect_layout_fields(checker, items, fields);
            continue;
        }
        if let ItemKind::Field { ty } = &member.kind {
            let field_ty = resolve_type(
                ty,
                checker.types,
                checker.project,
                checker.file,
                checker.diags,
                &[],
                &mut checker.semantics,
            );
            fields.push((member.name.clone(), Some(field_ty)));
        }
    }
}

struct Checker<'a> {
    program: &'a Program,
    project: &'a ResolvedProject,
    /// File of the item currently being checked (for per-file name scopes).
    file: FileId,
    sigs: &'a Sigs,
    types: &'a mut Types,
    diags: &'a mut Diags,
    /// Native pointer width in bits (`lilac_target::Target::bit_native_width`),
    /// so `@bit_native` literals range-check against the real target width.
    native_bits: u32,
    checked: HashSet<DefId>,
    fields: HashMap<(String, String), AstType>,
    hir: Vec<HirFunc>,
    /// Checked module constants (inlined wherever used).
    consts: Vec<HirConst>,
    /// Drop hooks by type display name (mangled `@on_drop` callees).
    drops: HashMap<String, String>,
    /// Checked `=>` bodies by function: parameter names, control-flow prelude,
    /// and value HIR. Inline functions never reach `hir` (every call
    /// substitutes them).
    inline_bodies: HashMap<DefId, InlineBody>,
    /// Functions currently being checked (rejects recursive `=>` calls).
    in_progress: HashSet<DefId>,
    /// Fresh value temporaries for desugared control flow.
    next_temp: u32,
    /// Monomorphized function instances, keyed by `(DefId, args)`.
    instances: HashMap<(DefId, Vec<TypeRef>), FuncInstance>,
    /// Instantiation keys that failed; a later reference stays silent.
    failed: HashSet<(DefId, Vec<TypeRef>)>,
    /// Instantiation keys currently being specialized (the depth stack).
    active: Vec<(DefId, Vec<TypeRef>)>,
    /// Generic parameter names of the body currently being checked.
    current_params: Vec<String>,
    /// Concrete arguments substituting `current_params` in the current body.
    current_args: Vec<TypeRef>,
    /// Specialized `=>` bodies by instance key (generic inline functions):
    /// parameter names, control-flow prelude, and value HIR.
    instance_inline: HashMap<(DefId, Vec<TypeRef>), InlineBody>,
    /// Concrete object instances, keyed by `(base DefId, args)`.
    object_instances: HashMap<(DefId, Vec<TypeRef>), TypeRef>,
    /// Object instantiation keys that failed; a later reference stays silent.
    failed_objects: HashSet<(DefId, Vec<TypeRef>)>,
    /// Object instantiation keys currently being laid out (the depth stack).
    active_objects: Vec<(DefId, Vec<TypeRef>)>,
    /// Concrete per-instance object layouts, keyed by mangled instance name.
    layouts: HashMap<String, Vec<(String, Option<TypeRef>)>>,
    /// `E042` reports already emitted, keyed by
    /// `(generic DefId, args, call-site file, call-site byte)`.
    reported_failures: HashSet<(DefId, Vec<TypeRef>, u32, u32)>,
    /// The object whose member body is being checked, if any: an object-private
    /// member is visible only while this matches the member's owning object.
    enclosing_object: Option<DefId>,
    /// Resolved item uses recorded for the editor (hover / go-to-definition).
    uses: Vec<NameSite>,
    /// Non-item semantic-token sites recorded for the editor (locals,
    /// parameters, body/signature type names, builtins).
    semantics: Vec<SemanticSite>,
}

struct FuncCtx {
    locals: HashMap<String, TypeRef>,
    slots: Vec<(String, TypeRef, Span)>,
    assigned: HashSet<String>,
    /// `loop` nesting depth (`break` outside any loop is `E022`).
    loop_depth: u32,
    /// HIR statement buffers; `blocks[0]` is the function body. Nested
    /// blocks (match arms, desugared branches) push their own buffer so
    /// expression preludes land in the block that runs them.
    blocks: Vec<Vec<HirStmt>>,
}

impl FuncCtx {
    fn new() -> Self {
        Self {
            locals: HashMap::new(),
            slots: Vec::new(),
            assigned: HashSet::new(),
            loop_depth: 0,
            blocks: vec![Vec::new()],
        }
    }
}

fn is_poison(ty: TypeRef) -> bool {
    ty.id == TypeId::ERROR
}

/// Well-formed HIR for error paths (never lowered: the driver discards HIR
/// whenever diagnostics contain errors).
fn poison_expr(span: Span) -> HirExpr {
    HirExpr {
        ty: TypeRef {
            id: TypeId::ERROR,
            is_ref: false,
        },
        span,
        kind: HirExprKind::Poison,
    }
}

/// Whether a collected signature has concrete types on every slot. A signature
/// carrying a poison (`TypeId::ERROR`) parameter or return cannot be checked;
/// the tolerant path poisons that definition's body instead.
fn signature_usable(sig: &FuncSig) -> bool {
    sig.params
        .iter()
        .chain(sig.returns.iter())
        .all(|slot| slot.id != TypeId::ERROR)
}

/// HIR slots for a definition whose signature was not collected: every slot is
/// poison. This HIR is retained for the editor only and never lowered (errors
/// exist whenever a signature is missing).
fn poison_slots(slots: &[lilac_ast::Param]) -> Vec<HirParam> {
    slots
        .iter()
        .map(|slot| HirParam {
            name: slot.name.clone(),
            ty: TypeRef {
                id: TypeId::ERROR,
                is_ref: false,
            },
            span: slot.span,
        })
        .collect()
}

/// Retypes a checked expression, preserving its shape. The identity and
/// `ref unknown` casts emit no instruction, so only the static type changes.
fn retype(hir: HirExpr, ty: TypeRef, span: Span) -> HirExpr {
    HirExpr {
        ty,
        span,
        kind: hir.kind,
    }
}

/// Pushes one statement into the innermost open block.
fn push_stmt(fctx: &mut FuncCtx, stmt: HirStmt, span: Span) {
    match fctx.blocks.last_mut() {
        Some(top) => top.push(stmt),
        None => lilac_error::bug!(span, "statement with no enclosing block"),
    }
}

/// Emits `@format_args_push(ref __f, piece)` as a void effect statement.
fn push_fstring_piece(
    fctx: &mut FuncCtx,
    fname: &str,
    format_args: TypeRef,
    piece: HirExpr,
    span: Span,
) {
    push_stmt(
        fctx,
        HirStmt::ExprStmt(HirExpr {
            ty: TypeRef {
                id: TypeId::ERROR,
                is_ref: false,
            },
            span,
            kind: HirExprKind::Call {
                callee: HirCallee::Intrinsic {
                    name: "@format_args_push".into(),
                },
                args: vec![
                    HirExpr {
                        ty: TypeRef {
                            id: format_args.id,
                            is_ref: true,
                        },
                        span,
                        kind: HirExprKind::RefOf {
                            value: Box::new(HirExpr {
                                ty: format_args,
                                span,
                                kind: HirExprKind::Ident {
                                    name: fname.to_owned(),
                                    def: DefId::ERROR,
                                },
                            }),
                        },
                    },
                    piece,
                ],
            },
        }),
        span,
    );
}

/// Well-formed empty `if` for error paths (never lowered).
fn empty_if(span: Span) -> HirStmt {
    HirStmt::If {
        cond: poison_expr(span),
        then: HirBlock {
            stmts: Vec::new(),
            span,
        },
        els: HirBlock {
            stmts: Vec::new(),
            span,
        },
        out: None,
        span,
    }
}

/// Builds `temp = value` for desugared branch-joins.
fn assign_temp(temp: TempId, result_ty: TypeId, value: HirExpr) -> HirStmt {
    let span = value.span;
    HirStmt::Assign {
        target: HirExpr {
            ty: TypeRef {
                id: result_ty,
                is_ref: false,
            },
            span,
            kind: HirExprKind::Temp(temp),
        },
        value,
        span,
    }
}

/// Generates a fresh hidden local, distinct from every visible one.
fn fresh_local(fctx: &mut FuncCtx, base: &str) -> String {
    let mut name = format!("__{base}");
    let mut counter = 0;
    while fctx.locals.contains_key(&name) {
        counter += 1;
        name = format!("__{base}{counter}");
    }
    name
}

/// An AST item plus its resolution and enclosing object (if any).
type ItemLookup<'a> = Option<(&'a Item, ResItem, Option<(String, DefId)>)>;

/// Finds the enclosing object's name for a member definition, if any.
fn find_parent_name(project: &ResolvedProject, def: DefId) -> Option<String> {
    for (item_name, item) in project.all_items() {
        if let ResKind::Object { members, hooks, .. } = &item.kind {
            let in_members = members.values().any(|member| member.id == def);
            let in_hooks = hooks.values().flatten().any(|hook| hook.id == def);
            if in_members || in_hooks {
                return Some(qualify_name(project, item.id, item_name));
            }
        }
    }
    None
}

/// Finds the AST item plus its resolution for a definition, tracking the
/// enclosing object (if any) for implicit-`instance` binding.
fn find_item<'a>(program: &'a Program, project: &ResolvedProject, def: DefId) -> ItemLookup<'a> {
    fn rec<'a>(
        ast_items: &'a [Item],
        scope: &dyn Fn(&Item) -> Option<ResItem>,
        def: DefId,
        parent: Option<(String, DefId)>,
    ) -> ItemLookup<'a> {
        for ast in ast_items {
            if let ItemKind::Private { items } = &ast.kind {
                // `private` wrappers are not definitions: descend with the same
                // scope, which resolves the wrapped items by name.
                if let Some(found) = rec(items, scope, def, parent.clone()) {
                    return Some(found);
                }
                continue;
            }
            let Some(resolved) = scope(ast) else {
                continue;
            };
            if resolved.id == def {
                return Some((ast, resolved, parent));
            }
            if let (
                ItemKind::Object { members, .. },
                ResKind::Object {
                    members: resolved_members,
                    hooks,
                    ..
                },
            ) = (&ast.kind, &resolved.kind)
            {
                let grandparent = Some((ast.name.clone(), resolved.id));
                // Hooks pair by (name, declaration order): a per-name counter
                // advances as `rec` visits the flattened members.
                let seen = std::cell::RefCell::new(HashMap::<String, usize>::new());
                let member_scope = |member: &Item| -> Option<ResItem> {
                    if let Some(overloads) = hooks.get(&member.name) {
                        let mut seen = seen.borrow_mut();
                        let index = seen.entry(member.name.clone()).or_insert(0);
                        let resolved = overloads.get(*index).cloned();
                        if resolved.is_some() {
                            *index += 1;
                        }
                        return resolved;
                    }
                    resolved_members.get(&member.name).cloned()
                };
                if let Some(found) = rec(members, &member_scope, def, grandparent) {
                    return Some(found);
                }
            }
        }
        None
    }
    // Pair each AST item with the definition from *its own* namespace. The same
    // local name can be declared in several namespaces, so match on
    // `(name, namespace)`, never name alone; otherwise a def whose namespace
    // appears later in program order would be paired with an earlier
    // namespace's AST item and compiled from the wrong body/members.
    let ns_of = |file: FileId| {
        project
            .files
            .iter()
            .find(|scope| scope.file == file)
            .map(|scope| scope.ns)
    };
    let top_scope = |ast: &Item| {
        let ns = ns_of(ast.span.file);
        project
            .all_items()
            .find(|(name, item)| *name == ast.name && Some(item.ns) == ns)
            .map(|(_, item)| item.clone())
    };
    rec(&program.items, &top_scope, def, None)
}

/// Whether HIR contains a branch-join temp: control flow inside a value that
/// must stay straight-line. Module constants still fold in M3, so they stay
/// `E030` here; `=>` bodies carry their control flow as a prelude instead.
fn has_temp(expr: &HirExpr) -> bool {
    match &expr.kind {
        HirExprKind::Temp(_) => true,
        HirExprKind::Call { args, .. } => args.iter().any(has_temp),
        HirExprKind::EqEq { lhs, rhs } => has_temp(lhs) || has_temp(rhs),
        HirExprKind::Member { obj, .. } => has_temp(obj),
        HirExprKind::RefOf { value } => has_temp(value),
        HirExprKind::StructLit { fields, .. } => fields.iter().any(|f| has_temp(&f.value)),
        HirExprKind::Variant { payload, .. } => payload.as_ref().is_some_and(|p| has_temp(p)),
        HirExprKind::Ident { .. }
        | HirExprKind::Tag { .. }
        | HirExprKind::Number { .. }
        | HirExprKind::Str { .. }
        | HirExprKind::Char { .. }
        | HirExprKind::CompileError { .. }
        | HirExprKind::Poison => false,
    }
}

/// Clones an HIR tree with `=>` parameters replaced by their call-site
/// arguments (textual substitution is the documented `=>` semantic: an
/// argument used twice evaluates twice).
fn subst_ident(expr: &HirExpr, map: &HashMap<String, HirExpr>) -> HirExpr {
    let kind = match &expr.kind {
        HirExprKind::Ident { name, .. } => {
            if let Some(replacement) = map.get(name) {
                return replacement.clone();
            }
            expr.kind.clone()
        }
        HirExprKind::Call { callee, args } => HirExprKind::Call {
            callee: callee.clone(),
            args: args.iter().map(|arg| subst_ident(arg, map)).collect(),
        },
        HirExprKind::EqEq { lhs, rhs } => HirExprKind::EqEq {
            lhs: Box::new(subst_ident(lhs, map)),
            rhs: Box::new(subst_ident(rhs, map)),
        },
        HirExprKind::Member { obj, field } => HirExprKind::Member {
            obj: Box::new(subst_ident(obj, map)),
            field: field.clone(),
        },
        HirExprKind::RefOf { value } => HirExprKind::RefOf {
            value: Box::new(subst_ident(value, map)),
        },
        HirExprKind::StructLit { fields } => HirExprKind::StructLit {
            fields: fields
                .iter()
                .map(|field| HirField {
                    name: field.name.clone(),
                    value: subst_ident(&field.value, map),
                    span: field.span,
                })
                .collect(),
        },
        HirExprKind::Variant {
            def,
            variant,
            payload,
        } => HirExprKind::Variant {
            def: *def,
            variant: variant.clone(),
            payload: payload
                .as_ref()
                .map(|value| Box::new(subst_ident(value, map))),
        },
        HirExprKind::Tag { .. }
        | HirExprKind::Number { .. }
        | HirExprKind::Str { .. }
        | HirExprKind::Char { .. }
        | HirExprKind::Temp(_)
        | HirExprKind::CompileError { .. }
        | HirExprKind::Poison => expr.kind.clone(),
    };
    HirExpr {
        ty: expr.ty,
        span: expr.span,
        kind,
    }
}

/// Substitutes `=>` parameters in a checked block (see [`subst_stmts`]).
fn subst_block(block: &HirBlock, map: &HashMap<String, HirExpr>) -> HirBlock {
    HirBlock {
        stmts: subst_stmts(&block.stmts, map),
        span: block.span,
    }
}

/// Substitutes `=>` parameters in a statement sequence, recursing into every
/// embedded expression and nested block (the prelude counterpart of
/// [`subst_ident`]).
fn subst_stmts(stmts: &[HirStmt], map: &HashMap<String, HirExpr>) -> Vec<HirStmt> {
    stmts.iter().map(|stmt| subst_stmt(stmt, map)).collect()
}

fn subst_stmt(stmt: &HirStmt, map: &HashMap<String, HirExpr>) -> HirStmt {
    match stmt {
        HirStmt::Let {
            name,
            ty,
            value,
            span,
        } => HirStmt::Let {
            name: name.clone(),
            ty: *ty,
            value: subst_ident(value, map),
            span: *span,
        },
        HirStmt::MultiLet {
            bindings,
            value,
            span,
        } => HirStmt::MultiLet {
            bindings: bindings.clone(),
            value: subst_ident(value, map),
            span: *span,
        },
        HirStmt::Assign {
            target,
            value,
            span,
        } => HirStmt::Assign {
            target: subst_ident(target, map),
            value: subst_ident(value, map),
            span: *span,
        },
        HirStmt::ExprStmt(expr) => HirStmt::ExprStmt(subst_ident(expr, map)),
        HirStmt::Return { span } => HirStmt::Return { span: *span },
        HirStmt::Break { span } => HirStmt::Break { span: *span },
        HirStmt::If {
            cond,
            then,
            els,
            out,
            span,
        } => HirStmt::If {
            cond: subst_ident(cond, map),
            then: subst_block(then, map),
            els: subst_block(els, map),
            out: *out,
            span: *span,
        },
        HirStmt::Loop { body, span } => HirStmt::Loop {
            body: subst_block(body, map),
            span: *span,
        },
        HirStmt::Match {
            scrutinee,
            arms,
            els,
            out,
            span,
        } => HirStmt::Match {
            scrutinee: subst_ident(scrutinee, map),
            arms: arms
                .iter()
                .map(|arm| HirArm {
                    pattern: arm.pattern.clone(),
                    capture: arm.capture.clone(),
                    body: subst_block(&arm.body, map),
                })
                .collect(),
            els: subst_block(els, map),
            out: *out,
            span: *span,
        },
    }
}

/// Resolves a temp through a renaming, defaulting to itself when absent.
fn renamed_temp(temp: TempId, map: &HashMap<TempId, TempId>) -> TempId {
    map.get(&temp).copied().unwrap_or(temp)
}

/// Alpha-renames every branch-join temp in a value expression (see
/// [`rename_temps_in_stmts`]).
fn rename_temps_in_expr(expr: &HirExpr, map: &HashMap<TempId, TempId>) -> HirExpr {
    let kind = match &expr.kind {
        HirExprKind::Temp(temp) => HirExprKind::Temp(renamed_temp(*temp, map)),
        HirExprKind::Call { callee, args } => HirExprKind::Call {
            callee: callee.clone(),
            args: args
                .iter()
                .map(|arg| rename_temps_in_expr(arg, map))
                .collect(),
        },
        HirExprKind::EqEq { lhs, rhs } => HirExprKind::EqEq {
            lhs: Box::new(rename_temps_in_expr(lhs, map)),
            rhs: Box::new(rename_temps_in_expr(rhs, map)),
        },
        HirExprKind::Member { obj, field } => HirExprKind::Member {
            obj: Box::new(rename_temps_in_expr(obj, map)),
            field: field.clone(),
        },
        HirExprKind::RefOf { value } => HirExprKind::RefOf {
            value: Box::new(rename_temps_in_expr(value, map)),
        },
        HirExprKind::StructLit { fields } => HirExprKind::StructLit {
            fields: fields
                .iter()
                .map(|field| HirField {
                    name: field.name.clone(),
                    value: rename_temps_in_expr(&field.value, map),
                    span: field.span,
                })
                .collect(),
        },
        HirExprKind::Variant {
            def,
            variant,
            payload,
        } => HirExprKind::Variant {
            def: *def,
            variant: variant.clone(),
            payload: payload
                .as_ref()
                .map(|value| Box::new(rename_temps_in_expr(value, map))),
        },
        HirExprKind::Ident { .. }
        | HirExprKind::Tag { .. }
        | HirExprKind::Number { .. }
        | HirExprKind::Str { .. }
        | HirExprKind::Char { .. }
        | HirExprKind::CompileError { .. }
        | HirExprKind::Poison => expr.kind.clone(),
    };
    HirExpr {
        ty: expr.ty,
        span: expr.span,
        kind,
    }
}

/// Alpha-renames every branch-join temp in a statement sequence, recursing
/// into nested blocks and covering the `out` slot of `if`/`match` joins.
fn rename_temps_in_stmts(stmts: &[HirStmt], map: &HashMap<TempId, TempId>) -> Vec<HirStmt> {
    stmts
        .iter()
        .map(|stmt| rename_temps_in_stmt(stmt, map))
        .collect()
}

fn rename_temps_in_block(block: &HirBlock, map: &HashMap<TempId, TempId>) -> HirBlock {
    HirBlock {
        stmts: rename_temps_in_stmts(&block.stmts, map),
        span: block.span,
    }
}

fn rename_temps_in_stmt(stmt: &HirStmt, map: &HashMap<TempId, TempId>) -> HirStmt {
    match stmt {
        HirStmt::Let {
            name,
            ty,
            value,
            span,
        } => HirStmt::Let {
            name: name.clone(),
            ty: *ty,
            value: rename_temps_in_expr(value, map),
            span: *span,
        },
        HirStmt::MultiLet {
            bindings,
            value,
            span,
        } => HirStmt::MultiLet {
            bindings: bindings.clone(),
            value: rename_temps_in_expr(value, map),
            span: *span,
        },
        HirStmt::Assign {
            target,
            value,
            span,
        } => HirStmt::Assign {
            target: rename_temps_in_expr(target, map),
            value: rename_temps_in_expr(value, map),
            span: *span,
        },
        HirStmt::ExprStmt(expr) => HirStmt::ExprStmt(rename_temps_in_expr(expr, map)),
        HirStmt::Return { span } => HirStmt::Return { span: *span },
        HirStmt::Break { span } => HirStmt::Break { span: *span },
        HirStmt::If {
            cond,
            then,
            els,
            out,
            span,
        } => HirStmt::If {
            cond: rename_temps_in_expr(cond, map),
            then: rename_temps_in_block(then, map),
            els: rename_temps_in_block(els, map),
            out: out.map(|temp| renamed_temp(temp, map)),
            span: *span,
        },
        HirStmt::Loop { body, span } => HirStmt::Loop {
            body: rename_temps_in_block(body, map),
            span: *span,
        },
        HirStmt::Match {
            scrutinee,
            arms,
            els,
            out,
            span,
        } => HirStmt::Match {
            scrutinee: rename_temps_in_expr(scrutinee, map),
            arms: arms
                .iter()
                .map(|arm| HirArm {
                    pattern: arm.pattern.clone(),
                    capture: arm.capture.clone(),
                    body: rename_temps_in_block(&arm.body, map),
                })
                .collect(),
            els: rename_temps_in_block(els, map),
            out: out.map(|temp| renamed_temp(temp, map)),
            span: *span,
        },
    }
}

/// Every distinct branch-join temp in a prelude plus its value (the coverage
/// of [`rename_temps_in_stmts`] / [`rename_temps_in_expr`]).
fn collect_temps(prelude: &[HirStmt], value: &HirExpr) -> HashSet<TempId> {
    let mut temps = HashSet::new();
    for stmt in prelude {
        collect_stmt_temps(stmt, &mut temps);
    }
    collect_expr_temps(value, &mut temps);
    temps
}

fn collect_expr_temps(expr: &HirExpr, temps: &mut HashSet<TempId>) {
    match &expr.kind {
        HirExprKind::Temp(temp) => {
            temps.insert(*temp);
        }
        HirExprKind::Call { args, .. } => {
            for arg in args {
                collect_expr_temps(arg, temps);
            }
        }
        HirExprKind::EqEq { lhs, rhs } => {
            collect_expr_temps(lhs, temps);
            collect_expr_temps(rhs, temps);
        }
        HirExprKind::Member { obj, .. } => collect_expr_temps(obj, temps),
        HirExprKind::RefOf { value } => collect_expr_temps(value, temps),
        HirExprKind::StructLit { fields } => {
            for field in fields {
                collect_expr_temps(&field.value, temps);
            }
        }
        HirExprKind::Variant { payload, .. } => {
            if let Some(payload) = payload {
                collect_expr_temps(payload, temps);
            }
        }
        HirExprKind::Ident { .. }
        | HirExprKind::Tag { .. }
        | HirExprKind::Number { .. }
        | HirExprKind::Str { .. }
        | HirExprKind::Char { .. }
        | HirExprKind::CompileError { .. }
        | HirExprKind::Poison => {}
    }
}

fn collect_stmt_temps(stmt: &HirStmt, temps: &mut HashSet<TempId>) {
    match stmt {
        HirStmt::Let { value, .. } | HirStmt::MultiLet { value, .. } => {
            collect_expr_temps(value, temps);
        }
        HirStmt::Assign { target, value, .. } => {
            collect_expr_temps(target, temps);
            collect_expr_temps(value, temps);
        }
        HirStmt::ExprStmt(expr) => collect_expr_temps(expr, temps),
        HirStmt::Return { .. } | HirStmt::Break { .. } => {}
        HirStmt::If {
            cond,
            then,
            els,
            out,
            ..
        } => {
            collect_expr_temps(cond, temps);
            collect_block_temps(then, temps);
            collect_block_temps(els, temps);
            if let Some(temp) = out {
                temps.insert(*temp);
            }
        }
        HirStmt::Loop { body, .. } => collect_block_temps(body, temps),
        HirStmt::Match {
            scrutinee,
            arms,
            els,
            out,
            ..
        } => {
            collect_expr_temps(scrutinee, temps);
            for arm in arms {
                collect_block_temps(&arm.body, temps);
            }
            collect_block_temps(els, temps);
            if let Some(temp) = out {
                temps.insert(*temp);
            }
        }
    }
}

fn collect_block_temps(block: &HirBlock, temps: &mut HashSet<TempId>) {
    for stmt in &block.stmts {
        collect_stmt_temps(stmt, temps);
    }
}

fn is_bit(types: &Types, id: TypeId) -> bool {
    matches!(
        types.kind(id),
        lilac_ty::TypeKind::Bit8
            | lilac_ty::TypeKind::Bit16
            | lilac_ty::TypeKind::Bit32
            | lilac_ty::TypeKind::Bit64
            | lilac_ty::TypeKind::BitNative
    )
}

fn is_float(types: &Types, id: TypeId) -> bool {
    matches!(
        types.kind(id),
        lilac_ty::TypeKind::FBit16
            | lilac_ty::TypeKind::FBit32
            | lilac_ty::TypeKind::FBit64
            | lilac_ty::TypeKind::FBit128
    )
}

/// The `@bitW` container a float reinterpret cast pairs with. `@fbit128` has
/// no bit container, so the lookup never yields such a cast (internal error).
fn bit_container(width: FloatWidth, span: Span) -> &'static str {
    match width.int_builtin() {
        Some(name) => name,
        None => lilac_error::bug!(span, "float bitcast width without a bit container"),
    }
}

/// Width in bits of a float container (callers guarantee `is_float` first).
fn float_width(types: &Types, id: TypeId) -> u32 {
    match types.kind(id) {
        lilac_ty::TypeKind::FBit16 => 16,
        lilac_ty::TypeKind::FBit32 => 32,
        lilac_ty::TypeKind::FBit64 => 64,
        lilac_ty::TypeKind::FBit128 => 128,
        _ => lilac_error::bug!(Span::dummy(), "float width of non-float type"),
    }
}

/// Builds a checked number-literal HIR value at base type `id`.
fn number_expr(id: TypeId, bytes: &str, span: Span) -> (TypeRef, HirExpr) {
    let ty = TypeRef { id, is_ref: false };
    (
        ty,
        HirExpr {
            ty,
            span,
            kind: HirExprKind::Number {
                bytes: bytes.to_owned(),
            },
        },
    )
}

/// Whether a number literal's source text names a float (`3.14`, `1e3`). A
/// `0x`/`0b` form never is (`0xE` is an integer), so the prefix is checked
/// first; otherwise a `.` or an exponent `e`/`E` makes it a float.
fn is_float_literal(bytes: &str) -> bool {
    let prefixed = bytes.starts_with("0x")
        || bytes.starts_with("0X")
        || bytes.starts_with("0b")
        || bytes.starts_with("0B");
    !prefixed && bytes.bytes().any(|byte| matches!(byte, b'.' | b'e' | b'E'))
}

/// The unsigned magnitude of an integer literal, or `None` for empty digits.
/// Decimal, hex, and binary forms are accepted (with `_` separators). A
/// magnitude wider than `u128` (far past 64 bits) reports `u128::MAX`, which
/// trips every width check.
fn integer_magnitude(bytes: &str) -> Option<u128> {
    let digits = bytes.replace('_', "");
    let (radix, body) = if let Some(hex) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        (16, hex)
    } else if let Some(bin) = digits
        .strip_prefix("0b")
        .or_else(|| digits.strip_prefix("0B"))
    {
        (2, bin)
    } else {
        (10, digits.as_str())
    };
    if body.is_empty() {
        return None;
    }
    Some(u128::from_str_radix(body, radix).unwrap_or(u128::MAX))
}

/// Number of significant bits in an integer literal's magnitude, or `None`
/// for empty digits. A magnitude wider than `u128` (far past 64 bits) reports
/// `128`, which trips every width check; counting via `u128` keeps a value
/// past `u64` faithful instead of a wrapping parse.
fn integer_bits(bytes: &str) -> Option<u32> {
    let value = integer_magnitude(bytes)?;
    Some(if value == 0 {
        0
    } else {
        u128::BITS - value.leading_zeros()
    })
}

/// The smallest `@bitW` (8/16/32/64) whose unsigned range holds `bits`, or
/// `None` when the magnitude needs more than 64 bits.
fn smallest_bit_width(bits: u32) -> Option<u32> {
    match bits {
        0..=8 => Some(8),
        9..=16 => Some(16),
        17..=32 => Some(32),
        33..=64 => Some(64),
        _ => None,
    }
}

/// The smallest `@bitW` (8/16/32/64) whose **signed** range holds `-magnitude`
/// (i.e. `magnitude <= 2^(W-1)`), or `None` when it needs more than 64 signed
/// bits. This is the mirror of [`smallest_bit_width`].
fn signed_smallest_bit_width(magnitude: u128) -> Option<u32> {
    if magnitude <= 1u128 << 7 {
        Some(8)
    } else if magnitude <= 1u128 << 15 {
        Some(16)
    } else if magnitude <= 1u128 << 31 {
        Some(32)
    } else if magnitude <= 1u128 << 63 {
        Some(64)
    } else {
        None
    }
}

/// Largest finite value representable by an `@fbitN` (16/32/64) container.
fn max_finite_float(width: u32) -> f64 {
    match width {
        16 => 65504.0,
        32 => f64::from(f32::MAX),
        _ => f64::MAX,
    }
}

/// Well-formed error value for a failed literal fold (never lowered).
fn poison_folded(span: Span) -> (TypeRef, HirExpr) {
    (
        TypeRef {
            id: TypeId::ERROR,
            is_ref: false,
        },
        poison_expr(span),
    )
}

/// Whether a binop is literal-foldable arithmetic (`+ - * / %`).
fn is_arithmetic(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
    )
}

/// Whether `expr` is a syntactically all-literal arithmetic subtree (a number,
/// or a `+ - * / %` of two such). Pure: emits no diagnostics; used to pick the
/// width anchor in `check_binop_sides`.
fn is_foldable_literal(expr: &Expr) -> bool {
    match expr {
        Expr::Number(..) => true,
        Expr::BinOp { lhs, op, rhs, .. } if is_arithmetic(*op) => {
            is_foldable_literal(lhs) && is_foldable_literal(rhs)
        }
        _ => false,
    }
}

/// A folded literal value: an integer as sign plus magnitude, or a float.
#[derive(Clone, Copy)]
enum LiteralValue {
    Int { negative: bool, magnitude: u128 },
    Float(f64),
}

/// The result of folding an expression to a literal value.
enum FoldedLiteral {
    /// Not an all-literal arithmetic subtree: the operator keeps its hook.
    NotLiteral,
    /// A fully folded literal value.
    Value(LiteralValue),
    /// A fold error; the diagnostic was already emitted.
    Error,
}

/// The signed `i128` value of an integer literal's sign plus magnitude.
/// Callers guarantee the magnitude fits `u64`, so it fits `i128`.
fn signed_value(negative: bool, magnitude: u128) -> i128 {
    let value = i128::try_from(magnitude).unwrap_or(i128::MAX);
    if negative {
        -value
    } else {
        value
    }
}

/// Parses a float literal's source bytes (underscores stripped); a malformed
/// form reports `NaN`. The lexer only emits valid forms.
fn parse_float_literal(bytes: &str) -> f64 {
    let cleaned: String = bytes.chars().filter(|c| *c != '_').collect();
    cleaned.parse::<f64>().unwrap_or(f64::NAN)
}

fn deferred(diags: &mut Diags, span: Span, what: &str, milestone: &str) -> ErrorGuaranteed {
    diags.emit(
        Diagnostic::error(
            "E030",
            format!("{what} is not implemented yet ({milestone})"),
        )
        .label(span, true, "lands in a later milestone".to_owned()),
    )
}

impl Checker<'_> {
    /// Width in bits of a bit container (callers guarantee `is_bit` first).
    /// `@bit_native` takes the target's native pointer width, so a literal for
    /// it is range-checked against the real target (single pointer-width rule).
    fn bit_width(&self, id: TypeId) -> u32 {
        match self.types.kind(id) {
            lilac_ty::TypeKind::Bit8 => 8,
            lilac_ty::TypeKind::Bit16 => 16,
            lilac_ty::TypeKind::Bit32 => 32,
            lilac_ty::TypeKind::Bit64 => 64,
            lilac_ty::TypeKind::BitNative => self.native_bits,
            _ => lilac_error::bug!(Span::dummy(), "bit width of non-bit type"),
        }
    }

    /// The namespace-qualified name of an item declared in `file` (root adds no
    /// prefix), matching the type interner's `types.display`/`types.mangle`.
    fn qualify_local(&self, file: FileId, local: &str) -> String {
        let path = self
            .project
            .files
            .iter()
            .find(|scope| scope.file == file)
            .map_or("", |scope| self.project.namespace_path(scope.ns));
        if path.is_empty() {
            local.to_owned()
        } else {
            format!("{path}.{local}")
        }
    }

    fn collect_fields(&mut self, items: &[Item]) {
        for item in items {
            if let ItemKind::Private { items } = &item.kind {
                self.collect_fields(items);
                continue;
            }
            if let ItemKind::Object { members, .. } = &item.kind {
                let owner = self.qualify_local(item.span.file, &item.name);
                self.collect_member_fields(&owner, members);
            }
        }
    }

    /// Records an object's member fields, flattening `private` blocks at any
    /// nesting depth into the same owner, and recurses into any nested object
    /// declaration found among the members.
    fn collect_member_fields(&mut self, owner: &str, members: &[Item]) {
        for member in members {
            if let ItemKind::Private { items } = &member.kind {
                self.collect_member_fields(owner, items);
                continue;
            }
            if let ItemKind::Field { ty } = &member.kind {
                self.fields
                    .insert((owner.to_owned(), member.name.clone()), ty.clone());
            }
            self.collect_fields(std::slice::from_ref(member));
        }
    }

    fn mismatch(
        &mut self,
        expected: TypeRef,
        found: TypeRef,
        expected_span: Span,
        found_span: Span,
    ) -> ErrorGuaranteed {
        let exp = self.types.display_ref(expected);
        let got = self.types.display_ref(found);
        self.diags.emit(
            Diagnostic::error(
                "E010",
                format!("mismatched types: expected {exp}, found {got}"),
            )
            .label(expected_span, false, format!("expected `{exp}` here"))
            .label(found_span, true, format!("found `{got}` here")),
        )
    }

    /// Wraps a value in its `@conversion` call when the expected type was
    /// reached by conversion (exact matches and poison pass through).
    /// Must run wherever `expect_type` succeeds on a value that flows into
    /// HIR; otherwise lowering passes wrong-width values to hooks.
    fn convert_if_needed(&self, got: TypeRef, expected: TypeId, value_hir: HirExpr) -> HirExpr {
        if is_poison(got) || expected == TypeId::ERROR || got.id == expected {
            return value_hir;
        }
        let Some(binding) = self
            .sigs
            .conversions
            .iter()
            .find(|conversion| conversion.from == got.id && conversion.to == expected)
        else {
            return value_hir;
        };
        let (func, to) = (binding.func, binding.to);
        if self.func_sig(func).is_none() {
            return value_hir;
        }
        let qual = match find_parent_name(self.project, func) {
            Some(parent) => format!("{parent}.@conversion"),
            None => "@conversion".to_owned(),
        };
        let name = self.hook_name(&qual, func);
        HirExpr {
            ty: TypeRef {
                id: to,
                is_ref: false,
            },
            span: value_hir.span,
            kind: HirExprKind::Call {
                callee: HirCallee::Hook { name, def: func },
                args: vec![value_hir],
            },
        }
    }

    /// Unifies an actual value against an expected type: exact first (with
    /// silent auto-deref), then one directed `@conversion` step for non-`ref`
    /// mismatches. `ref` shape mismatches stay `E010`; failed searches are
    /// `E043`. Never chains.
    fn expect_type(
        &mut self,
        actual: TypeRef,
        expected: TypeId,
        expected_span: Span,
        actual_span: Span,
    ) -> Result<(), ErrorGuaranteed> {
        if is_poison(actual) || expected == TypeId::ERROR {
            return Ok(());
        }
        if actual.id == expected && !actual.is_ref {
            return Ok(());
        }
        if actual.is_ref {
            if actual.id == expected {
                return Ok(());
            }
            return Err(self.mismatch(
                TypeRef {
                    id: expected,
                    is_ref: false,
                },
                actual,
                expected_span,
                actual_span,
            ));
        }
        match super::ops::convert(
            self.types,
            self.diags,
            self.sigs,
            actual,
            expected,
            expected_span,
            actual_span,
        ) {
            Ok(_) => Ok(()),
            Err(guaranteed) => Err(guaranteed),
        }
    }

    /// Generates a fresh branch-join temporary.
    fn fresh_temp(&mut self) -> TempId {
        let temp = TempId(self.next_temp);
        self.next_temp += 1;
        temp
    }

    /// Checks a `{ ... }` block into its own buffer (nested expression
    /// preludes land inside it, not in the enclosing block).
    fn check_block(&mut self, fctx: &mut FuncCtx, block: &lilac_ast::Block) -> HirBlock {
        fctx.blocks.push(Vec::new());
        for stmt in &block.stmts {
            self.check_stmt(fctx, stmt);
        }
        let Some(stmts) = fctx.blocks.pop() else {
            lilac_error::bug!(block.span, "block stack underflow");
        };
        HirBlock {
            stmts,
            span: block.span,
        }
    }

    /// Checks an expression whose value is placed inside a constructed
    /// block (branch bodies, join assignments): returns its prelude with it.
    fn check_nested(
        &mut self,
        fctx: &mut FuncCtx,
        expr: &Expr,
        expected: Option<TypeId>,
    ) -> (TypeRef, HirExpr, Vec<HirStmt>) {
        fctx.blocks.push(Vec::new());
        let (ty, hir) = self.check_expr(fctx, expr, expected);
        let Some(pre) = fctx.blocks.pop() else {
            lilac_error::bug!(expr.span(), "block stack underflow");
        };
        (ty, hir, pre)
    }

    /// Checks a test position (`if`/`loop` conditions, `match` scrutinees,
    /// `and`/`or` operands): bare numbers default to `@bit8`, any other
    /// `@bitN` tests non-zero. References never test (deref explicitly).
    fn check_test(&mut self, fctx: &mut FuncCtx, expr: &Expr) -> (TypeRef, HirExpr) {
        let (ty, hir) = if matches!(expr, Expr::Number(..)) {
            let bit8 = self.builtin_ty("@bit8");
            self.check_expr(fctx, expr, Some(bit8))
        } else {
            self.check_expr(fctx, expr, None)
        };
        if !is_poison(ty) && ty.is_ref {
            self.diags
                .emit(Diagnostic::error("E010", "cannot test a reference").label(
                    expr.span(),
                    true,
                    "deref explicitly".to_owned(),
                ));
        }
        (ty, hir)
    }

    /// Checks every item in `items`, descending into `private { … }` blocks so
    /// a namespace-private body is checked and later lowered like a public one.
    fn check_items(&mut self, items: &[Item]) {
        for item in items {
            if let ItemKind::Private { items } = &item.kind {
                self.check_items(items);
                continue;
            }
            let file = item.span.file;
            let project = self.project;
            if let Ok(Some(resolved)) = lookup_one(project, file, &item.name, item.span, self.diags)
            {
                let resolved = resolved.clone();
                self.file = file;
                self.check_item(item, &resolved, None);
            }
        }
    }

    fn check_item(&mut self, item: &Item, resolved: &ResItem, parent: Option<(String, DefId)>) {
        self.file = item.span.file;
        match &item.kind {
            ItemKind::Func(decl) => self.check_func(item, resolved, decl, parent),
            ItemKind::Object { members, .. } => {
                if let ResKind::Object { generic_params, .. } = &resolved.kind {
                    if !generic_params.is_empty() {
                        // A generic object body is a template: members (methods,
                        // in-body hooks) carry `Param` slots and are specialized
                        // per instantiation (M4 Tasks 4-5). Checking or emitting
                        // them as concrete items would leak `Param` into HIR and
                        // trip the verifier (`E070`), so skip the whole body.
                        return;
                    }
                    for (member, member_resolved) in flat_member_pairs(members, &resolved.kind) {
                        let parent = Some((item.name.clone(), resolved.id));
                        self.check_item(member, &member_resolved, parent);
                    }
                    if let Some(drop) = self.object_hook(resolved.id, "@on_drop") {
                        if let Some(sig) = self.func_sig(drop.id) {
                            let parent_id = self.types.intern_object(resolved.id);
                            let shaped = sig.params.len() == 1
                                && sig.params[0].is_ref
                                && sig.params[0].id == parent_id
                                && sig.returns.is_empty();
                            if shaped {
                                let owner = self.qualify_local(item.span.file, &item.name);
                                let name = self.hook_name(&format!("{owner}.@on_drop"), drop.id);
                                self.drops.insert(owner, name);
                            }
                        }
                    }
                }
            }
            ItemKind::Const { ty, value, .. } => {
                if !self.checked.insert(resolved.id) {
                    return;
                }
                let declared = self.resolve_ty(ty);
                let mut fctx = FuncCtx::new();
                let (got, hir) = self.check_expr(&mut fctx, value, Some(declared.id));
                let _ = self.expect_type(got, declared.id, ty.span, value.span());
                let hir = self.convert_if_needed(got, declared.id, hir);
                if has_temp(&hir) {
                    let _ = deferred(self.diags, value.span(), "control flow in constants", "M3");
                }
                self.consts.push(HirConst {
                    name: item.name.clone(),
                    def: resolved.id,
                    value: hir,
                });
            }
            ItemKind::Enum { .. }
            | ItemKind::Using { .. }
            | ItemKind::Private { .. }
            | ItemKind::Field { .. }
            | ItemKind::Error => {}
        }
    }

    fn func_sig(&self, def: DefId) -> Option<FuncSig> {
        if let Some(sig) = self.sigs.funcs.get(&def) {
            return Some(sig.clone());
        }
        for overloads in self.sigs.hooks.values() {
            for (id, sig) in overloads {
                if *id == def {
                    return Some(sig.clone());
                }
            }
        }
        None
    }

    /// The resolved `name` hook owned by object `owner`, if any. Hooks live in
    /// the unit-global index, so ownership is matched by `HookParent::def`.
    fn object_hook(&self, owner: DefId, name: &str) -> Option<ResItem> {
        self.project
            .hooks
            .get(name)?
            .iter()
            .find_map(|hook| match &hook.kind {
                ResKind::Hook {
                    parent: Some(parent),
                    ..
                } if parent.def == owner => Some(hook.clone()),
                _ => None,
            })
    }

    /// Every resolved `name` hook owned by object `owner`, in declaration
    /// order (member hooks overload by signature).
    fn object_hooks(&self, owner: DefId, name: &str) -> Vec<ResItem> {
        self.project
            .hooks
            .get(name)
            .map(|hooks| {
                hooks
                    .iter()
                    .filter(|hook| match &hook.kind {
                        ResKind::Hook {
                            parent: Some(parent),
                            ..
                        } => parent.def == owner,
                        _ => false,
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Resolves a syntactic type in the current body's environment.
    ///
    /// Generic parameter names of the enclosing declaration resolve to `Param`
    /// and are then substituted with the current instance's concrete arguments
    /// (ruling R3). Concrete bodies have an empty environment, so this behaves
    /// exactly like `resolve_type(..., &[])`.
    fn resolve_ty(&mut self, ty: &AstType) -> TypeRef {
        let params = self.current_params.clone();
        let resolved = self.resolve_in_scope(ty, &params);
        let resolved = if self.current_args.is_empty() {
            resolved
        } else {
            let args = self.current_args.clone();
            self.types.substitute(resolved, &args)
        };
        self.instantiate_ty(resolved, ty.span)
    }

    /// Resolves a syntactic type against the current project/file scope.
    fn resolve_in_scope(&mut self, ty: &AstType, params: &[String]) -> TypeRef {
        resolve_type(
            ty,
            self.types,
            self.project,
            self.file,
            self.diags,
            params,
            &mut self.semantics,
        )
    }

    /// The concrete type bound to a generic parameter name in the current
    /// specialization, if any (`T` -> its substituted argument).
    fn generic_param_value(&self, name: &str) -> Option<TypeRef> {
        let index = self.current_params.iter().position(|param| param == name)?;
        self.current_args.get(index).copied()
    }

    /// Builds a function context (implicit `instance` for member functions,
    /// then declared parameters and return slots) plus the HIR signatures.
    fn func_ctx(
        &mut self,
        decl: &lilac_ast::FuncDecl,
        sig: &FuncSig,
        parent: Option<(String, DefId)>,
    ) -> (FuncCtx, Vec<HirParam>, Vec<HirParam>) {
        let mut fctx = FuncCtx::new();
        // An anonymous `self` parameter denotes the receiver; it is exposed as
        // `instance` (the lowering's member receiver name) and aliased as
        // `self` for source bodies.
        let has_self = decl
            .params
            .first()
            .is_some_and(|param| param.name.is_empty() && param.ty.name == "self");
        if let Some((parent_name, parent_def)) = parent {
            let parent_id = self.types.intern_object(parent_def);
            let qualified = qualify_name(self.project, parent_def, &parent_name);
            self.types.set_object_name(parent_def, &qualified);
            let receiver = if has_self {
                sig.params.first().copied().unwrap_or(TypeRef {
                    id: parent_id,
                    is_ref: true,
                })
            } else {
                TypeRef {
                    id: parent_id,
                    is_ref: true,
                }
            };
            fctx.locals.insert("instance".to_owned(), receiver);
            if has_self {
                fctx.locals.insert("self".to_owned(), receiver);
            }
        }
        for (param, ty) in decl.params.iter().zip(sig.params.iter()) {
            if !param.name.is_empty() {
                self.record_semantic(name_span(param.span, &param.name), SemanticKind::Param);
                fctx.locals.insert(param.name.clone(), *ty);
            }
        }
        for (slot, ty) in decl.returns.iter().zip(sig.returns.iter()) {
            if !slot.name.is_empty() {
                fctx.locals.insert(slot.name.clone(), *ty);
                fctx.slots.push((slot.name.clone(), *ty, slot.span));
            }
        }
        let mut params = Self::hir_slots(&decl.params, &sig.params);
        if has_self {
            if let Some(first) = params.first_mut() {
                "instance".clone_into(&mut first.name);
            }
        }
        let returns = Self::hir_slots(&decl.returns, &sig.returns);
        (fctx, params, returns)
    }

    /// Pairs declared slots with their resolved types for HIR signatures.
    fn hir_slots(slots: &[lilac_ast::Param], tys: &[TypeRef]) -> Vec<HirParam> {
        slots
            .iter()
            .zip(tys.iter())
            .map(|(slot, ty)| HirParam {
                name: slot.name.clone(),
                ty: *ty,
                span: slot.span,
            })
            .collect()
    }

    /// Checks one function body against `sig` and returns its checked shape
    /// plus the HIR parameter/return slots.
    ///
    /// Shared by concrete functions and monomorphized generic instances
    /// (ruling R2), so the `=>` control-flow deferral and the return-slot
    /// assignment/E027 sweep behave identically on both paths.
    fn check_func_body(
        &mut self,
        decl: &lilac_ast::FuncDecl,
        sig: &FuncSig,
        parent: Option<(String, DefId)>,
    ) -> (CheckedBody, Vec<HirParam>, Vec<HirParam>) {
        let errors_before = self.diags.checkpoint();
        let enclosing_def = parent.as_ref().map(|(_, def)| *def);
        let saved_enclosing = std::mem::replace(&mut self.enclosing_object, enclosing_def);
        let (mut fctx, params, returns) = self.func_ctx(decl, sig, parent);
        let outcome = match &decl.body {
            None => CheckedBody::Extern,
            Some(lilac_ast::FuncBody::Block(block)) => {
                CheckedBody::Block(self.check_block(&mut fctx, block))
            }
            Some(lilac_ast::FuncBody::Inline(expr)) => {
                if sig.returns.len() == 1 {
                    let (got, value) = self.check_expr(&mut fctx, expr, Some(sig.returns[0].id));
                    let span = expr.span();
                    let _ = self.expect_type(got, sig.returns[0].id, span, span);
                    let value = self.convert_if_needed(got, sig.returns[0].id, value);
                    let prelude = std::mem::take(&mut fctx.blocks[0]);
                    if let Some((name, _, _)) = fctx.slots.first() {
                        fctx.assigned.insert(name.clone());
                    }
                    let param_names: Vec<String> =
                        decl.params.iter().map(|param| param.name.clone()).collect();
                    CheckedBody::Inline {
                        param_names,
                        prelude,
                        value,
                    }
                } else {
                    self.diags.emit(
                        Diagnostic::error(
                            "E022",
                            "inline `=>` bodies need exactly one return type",
                        )
                        .label(expr.span(), true, "here".to_owned()),
                    );
                    CheckedBody::Rejected
                }
            }
        };
        self.enclosing_object = saved_enclosing;
        if matches!(outcome, CheckedBody::Block(_) | CheckedBody::Inline { .. })
            && self.diags.checkpoint() == errors_before
        {
            for (name, _, span) in &fctx.slots {
                if !fctx.assigned.contains(name) {
                    self.diags.emit(
                        Diagnostic::error(
                            "E027",
                            format!("return slot `{name}` is never assigned"),
                        )
                        .label(
                            *span,
                            true,
                            "assign it before `return`".to_owned(),
                        ),
                    );
                }
            }
        }
        (outcome, params, returns)
    }

    fn check_func(
        &mut self,
        item: &Item,
        resolved: &ResItem,
        decl: &lilac_ast::FuncDecl,
        parent: Option<(String, DefId)>,
    ) {
        if !self.checked.insert(resolved.id) {
            return;
        }
        if matches!(
            &resolved.kind,
            ResKind::Func { generic: true, .. } | ResKind::Hook { generic: true, .. }
        ) {
            return;
        }
        self.in_progress.insert(resolved.id);
        // Emitted (LIR) names: member functions qualify with their object,
        // hooks mangle with their signature (overloads share the source name).
        let hir_name = match &resolved.kind {
            ResKind::Hook { .. } => {
                let qual = match &parent {
                    Some((parent_name, parent_def)) => {
                        // Object hooks keep their qualified owner so two
                        // namespaces' same-named hooks cannot collide (lower
                        // leaves `@`-bearing names bare).
                        let owner = qualify_name(self.project, *parent_def, parent_name);
                        format!("{owner}.{}", item.name)
                    }
                    None => item.name.clone(),
                };
                self.hook_name(&qual, resolved.id)
            }
            _ => match &parent {
                // Qualify the owner with its namespace, like hook owners and
                // generic instances: otherwise a namespace named after its own
                // object (`foo` containing object `foo`) makes lowering's
                // namespace prefix check mis-strip and emit `foo.bar` instead
                // of `foo.foo.bar` (spec §7.1).
                Some((parent_name, parent_def)) => {
                    let owner = qualify_name(self.project, *parent_def, parent_name);
                    format!("{owner}.{}", item.name)
                }
                None => item.name.clone(),
            },
        };
        // Tolerant mode reaches body checking with errors present. A definition
        // whose signature could not be collected (its item or one of its types
        // was an error) is poisoned rather than `bug!`-panicking. Errors exist,
        // so this HIR is never lowered or verified.
        let Some(sig) = self.func_sig(resolved.id).filter(signature_usable) else {
            let body = if decl.is_extern {
                HirBlock {
                    stmts: Vec::new(),
                    span: item.span,
                }
            } else {
                HirBlock {
                    stmts: vec![HirStmt::ExprStmt(poison_expr(item.span))],
                    span: item.span,
                }
            };
            self.hir.push(HirFunc {
                def: resolved.id,
                name: hir_name,
                params: poison_slots(&decl.params),
                returns: poison_slots(&decl.returns),
                is_extern: decl.is_extern,
                body,
                span: item.span,
            });
            self.in_progress.remove(&resolved.id);
            return;
        };
        let (outcome, params, returns) = self.check_func_body(decl, &sig, parent);
        match outcome {
            CheckedBody::Extern => self.hir.push(HirFunc {
                def: resolved.id,
                name: hir_name,
                params,
                returns,
                is_extern: true,
                body: HirBlock {
                    stmts: Vec::new(),
                    span: item.span,
                },
                span: item.span,
            }),
            CheckedBody::Block(body) => self.hir.push(HirFunc {
                def: resolved.id,
                name: hir_name,
                params,
                returns,
                is_extern: false,
                body,
                span: item.span,
            }),
            CheckedBody::Inline {
                param_names,
                prelude,
                value,
            } => {
                self.inline_bodies
                    .insert(resolved.id, (param_names, prelude, value));
            }
            CheckedBody::Rejected => {}
        }
        self.in_progress.remove(&resolved.id);
    }

    fn place_type(
        &mut self,
        fctx: &mut FuncCtx,
        target: &Expr,
    ) -> Result<(TypeRef, HirExpr), ErrorGuaranteed> {
        match target {
            Expr::Ident(name, span) => {
                if let Some(ty) = fctx.locals.get(name) {
                    let hir_name = if name == "self" && fctx.locals.contains_key("self") {
                        "instance".to_owned()
                    } else {
                        name.clone()
                    };
                    let place = HirExpr {
                        ty: *ty,
                        span: *span,
                        kind: HirExprKind::Ident {
                            name: hir_name,
                            def: DefId::ERROR,
                        },
                    };
                    Ok((*ty, place))
                } else {
                    Err(self.diags.emit(
                        Diagnostic::error("E021", format!("unresolved name `{name}`")).label(
                            *span,
                            true,
                            "not a local or item".to_owned(),
                        ),
                    ))
                }
            }
            Expr::Member { obj, field, span } => {
                let (obj_ty, obj_hir) = self.check_expr(fctx, obj, None);
                if is_poison(obj_ty) {
                    return Ok((
                        TypeRef {
                            id: TypeId::ERROR,
                            is_ref: false,
                        },
                        poison_expr(*span),
                    ));
                }
                match self.field_type(obj_ty.id, field, *span, None, false) {
                    Ok(ty) => {
                        if let Some(def) = self.member_def(obj_ty.id, field) {
                            self.record_use(field_span(*span, field), def);
                        }
                        Ok((
                            ty,
                            HirExpr {
                                ty,
                                span: *span,
                                kind: HirExprKind::Member {
                                    obj: Box::new(obj_hir),
                                    field: field.clone(),
                                },
                            },
                        ))
                    }
                    Err(guaranteed) => Err(guaranteed),
                }
            }
            _ => Err(self.diags.emit(
                Diagnostic::error("E010", "cannot assign to this expression".to_owned()).label(
                    target.span(),
                    true,
                    "only variables and fields are assignable".to_owned(),
                ),
            )),
        }
    }

    /// Resolves a field access. `expected` is the use-site expected type and
    /// `annotate` controls whether a missing field on a generic-derived receiver
    /// gains the inferred-requirement note (read sites only; writes leave it
    /// unconstrained).
    fn field_type(
        &mut self,
        obj: TypeId,
        field: &str,
        span: Span,
        expected: Option<TypeId>,
        annotate: bool,
    ) -> Result<TypeRef, ErrorGuaranteed> {
        let owner = match self.types.kind(obj).clone() {
            lilac_ty::TypeKind::Object(def) => Some(def),
            lilac_ty::TypeKind::Instance { base, .. } => Some(base),
            _ => None,
        };
        if let Some(owner) = owner {
            if let Some(ResKind::Object { members, .. }) = self.find_by_def(owner) {
                if let Some(member) = members.get(field) {
                    if self.private_member_denied(member, owner, span) {
                        return Err(ErrorGuaranteed::new());
                    }
                }
            }
        }
        // A generic instance resolves its base object's field template under
        // the object's parameter scope, then substitutes the instance's args.
        if let lilac_ty::TypeKind::Instance { base, args } = self.types.kind(obj).clone() {
            let name = self.object_name_of(base);
            if let Some(ty) = self.fields.get(&(name.clone(), field.to_owned())).cloned() {
                let params = self.object_params(base);
                let resolved = resolve_type(
                    &ty,
                    self.types,
                    self.project,
                    self.file,
                    self.diags,
                    &params,
                    &mut self.semantics,
                );
                return Ok(self.types.substitute(resolved, &args));
            }
            let mut diagnostic = Diagnostic::error(
                "E021",
                format!("no field `{field}` on `{name}`"),
            )
            .label(span, true, "no such member".to_owned());
            diagnostic = self.annotate_field_miss(diagnostic, obj, field, expected, annotate);
            return Err(self.diags.emit(diagnostic));
        }
        let name = self.types.display(obj);
        if let Some(ty) = self.fields.get(&(name.clone(), field.to_owned())).cloned() {
            Ok(self.resolve_ty(&ty))
        } else {
            let mut diagnostic = Diagnostic::error(
                "E021",
                format!("no field `{field}` on `{name}`"),
            )
            .label(span, true, "no such member".to_owned());
            diagnostic = self.annotate_field_miss(diagnostic, obj, field, expected, annotate);
            Err(self.diags.emit(diagnostic))
        }
    }

    /// Adds the inferred-requirement field note to a missing-field diagnostic
    /// when the receiver is generic-derived and this is an annotated read.
    fn annotate_field_miss(
        &self,
        diagnostic: Diagnostic,
        obj: TypeId,
        field: &str,
        expected: Option<TypeId>,
        annotate: bool,
    ) -> Diagnostic {
        if annotate
            && self.is_generic_derived(TypeRef {
                id: obj,
                is_ref: false,
            })
        {
            diagnostic.note(self.required_field_note(field, expected))
        } else {
            diagnostic
        }
    }

    fn find_by_def(&self, def: DefId) -> Option<ResKind> {
        for (_, item) in self.project.all_items() {
            if item.id == def {
                return Some(item.kind.clone());
            }
            if let ResKind::Object { members, .. } = &item.kind {
                for member in members.values() {
                    if member.id == def {
                        return Some(member.kind.clone());
                    }
                }
            }
        }
        None
    }

    /// Resolves the `DefId` of `field` on a receiver type, for editor uses.
    fn member_def(&self, obj: TypeId, field: &str) -> Option<DefId> {
        let owner = match self.types.kind(obj) {
            lilac_ty::TypeKind::Object(def) => Some(*def),
            lilac_ty::TypeKind::Instance { base, .. } => Some(*base),
            _ => None,
        }?;
        if let Some(ResKind::Object { members, .. }) = self.find_by_def(owner) {
            members.get(field).map(|m| m.id)
        } else {
            None
        }
    }

    /// Emits `E026` when `member` is an object-private member accessed from
    /// outside its owning object, labelled at the access and the declaration.
    /// Returns `true` when the access must be rejected.
    fn private_member_denied(&mut self, member: &ResItem, owner: DefId, access: Span) -> bool {
        if member.vis != Visibility::Private || self.enclosing_object == Some(owner) {
            return false;
        }
        self.diags.emit(
            Diagnostic::error("E026", format!("`{}` is a private member", member.name))
                .label(access, true, "accessed from outside its object")
                .label(member.span, false, "declared private here"),
        );
        true
    }

    fn check_stmt(&mut self, fctx: &mut FuncCtx, stmt: &Stmt) {
        match stmt {
            Stmt::Let {
                name,
                ty,
                value,
                span,
            } => {
                if !name.is_empty() {
                    self.record_semantic(name_span(*span, name), SemanticKind::Local);
                }
                let declared = self.resolve_ty(ty);
                let expected = if is_poison(declared) {
                    None
                } else {
                    Some(declared.id)
                };
                let (got, hir) = self.check_expr(fctx, value, expected);
                if !is_poison(declared) && !is_poison(got) {
                    let _ = self.expect_type(got, declared.id, ty.span, value.span());
                }
                let hir = self.convert_if_needed(got, declared.id, hir);
                fctx.locals.insert(name.clone(), declared);
                push_stmt(
                    fctx,
                    HirStmt::Let {
                        name: name.clone(),
                        ty: declared,
                        value: hir,
                        span: *span,
                    },
                    *span,
                );
            }
            Stmt::MultiLet {
                bindings,
                value,
                span,
            } => {
                for binding in bindings {
                    if !binding.name.is_empty() {
                        self.record_semantic(
                            name_span(binding.span, &binding.name),
                            SemanticKind::Local,
                        );
                    }
                }
                self.check_multilet(fctx, bindings, value, *span);
            }
            Stmt::Assign {
                target,
                value,
                span,
            } => self.check_assign(fctx, target, value, *span),
            Stmt::ExprStmt(expr) => self.check_expr_stmt(fctx, expr),
            Stmt::Return { span } => push_stmt(fctx, HirStmt::Return { span: *span }, *span),
            Stmt::Break { span } => {
                if fctx.loop_depth == 0 {
                    self.diags
                        .emit(Diagnostic::error("E022", "break outside `loop`").label(
                            *span,
                            true,
                            "break needs an enclosing loop".to_owned(),
                        ));
                }
                push_stmt(fctx, HirStmt::Break { span: *span }, *span);
            }
            Stmt::If {
                cond,
                capture,
                then,
                els,
                span,
            } => self.check_if(fctx, cond, capture.as_ref(), then, els.as_ref(), *span),
            Stmt::Loop {
                cond,
                body,
                els,
                span,
            } => self.check_loop(fctx, cond.as_ref(), body, els.as_ref(), *span),
            Stmt::Match {
                scrutinee,
                arms,
                els,
                span,
            } => self.check_match(fctx, scrutinee, arms, els.as_ref(), *span),
            Stmt::Error { .. } => {}
        }
    }

    /// Checks `target = value` (marks return slots assigned).
    fn check_assign(&mut self, fctx: &mut FuncCtx, target: &Expr, value: &Expr, span: Span) {
        let Ok((target_ty, target_hir)) = self.place_type(fctx, target) else {
            let _ = self.check_expr(fctx, value, None);
            push_stmt(
                fctx,
                HirStmt::Assign {
                    target: poison_expr(target.span()),
                    value: poison_expr(value.span()),
                    span,
                },
                span,
            );
            return;
        };
        if is_poison(target_ty) {
            let (_, value_hir) = self.check_expr(fctx, value, None);
            push_stmt(
                fctx,
                HirStmt::Assign {
                    target: target_hir,
                    value: value_hir,
                    span,
                },
                span,
            );
            return;
        }
        let base = target_ty.id;
        let (got, value_hir) = self.check_expr(fctx, value, Some(base));
        if !is_poison(got) {
            let _ = self.expect_type(got, base, target.span(), value.span());
        }
        let value_hir = self.convert_if_needed(got, base, value_hir);
        if let Expr::Ident(name, _) = target {
            if fctx.slots.iter().any(|(slot, _, _)| slot == name) {
                fctx.assigned.insert(name.clone());
            }
        }
        push_stmt(
            fctx,
            HirStmt::Assign {
                target: target_hir,
                value: value_hir,
                span,
            },
            span,
        );
    }

    /// Checks an effect statement (a call must return nothing).
    fn check_expr_stmt(&mut self, fctx: &mut FuncCtx, expr: &Expr) {
        if let Expr::Call {
            callee,
            generic_args,
            args,
            span: call_span,
        } = expr
        {
            match self.check_call(fctx, callee, generic_args, args, *call_span, None) {
                Ok(CallOutcome::Call(call)) => {
                    if !call.outs.is_empty() {
                        self.diags.emit(
                            Diagnostic::error(
                                "E022",
                                format!(
                                    "call returns {} value{}, but the statement drops them",
                                    call.outs.len(),
                                    if call.outs.len() == 1 { "" } else { "s" },
                                ),
                            )
                            .label(
                                *call_span,
                                true,
                                "assign or drop explicitly".to_owned(),
                            ),
                        );
                    }
                    push_stmt(
                        fctx,
                        HirStmt::ExprStmt(HirExpr {
                            ty: TypeRef {
                                id: TypeId::ERROR,
                                is_ref: false,
                            },
                            span: *call_span,
                            kind: HirExprKind::Call {
                                callee: call.callee,
                                args: call.args,
                            },
                        }),
                        *call_span,
                    );
                    return;
                }
                Ok(CallOutcome::Inline(..)) => {
                    self.diags.emit(
                        Diagnostic::error(
                            "E022",
                            "call returns 1 value, but the statement drops it",
                        )
                        .label(
                            *call_span,
                            true,
                            "assign or drop explicitly".to_owned(),
                        ),
                    );
                }
                Ok(CallOutcome::CompilerError(hir)) => {
                    push_stmt(fctx, HirStmt::ExprStmt(hir), *call_span);
                    return;
                }
                Err(_) => {}
            }
            push_stmt(
                fctx,
                HirStmt::ExprStmt(poison_expr(expr.span())),
                expr.span(),
            );
            return;
        }
        let (_, hir) = self.check_expr(fctx, expr, None);
        push_stmt(fctx, HirStmt::ExprStmt(hir), expr.span());
    }

    /// Checks `ref place` (double references stay `E010`).
    fn check_ref_of(&mut self, fctx: &mut FuncCtx, value: &Expr, span: Span) -> (TypeRef, HirExpr) {
        match self.place_type(fctx, value) {
            Ok((place, _)) if place.is_ref => {
                self.diags.emit(
                    Diagnostic::error("E010", "cannot take `ref` of a reference").label(
                        span,
                        true,
                        "double references are invalid".to_owned(),
                    ),
                );
                (
                    TypeRef {
                        id: TypeId::ERROR,
                        is_ref: false,
                    },
                    poison_expr(span),
                )
            }
            Ok((place, place_hir)) => {
                let ty = TypeRef {
                    id: place.id,
                    is_ref: true,
                };
                (
                    ty,
                    HirExpr {
                        ty,
                        span,
                        kind: HirExprKind::RefOf {
                            value: Box::new(place_hir),
                        },
                    },
                )
            }
            Err(_) => (
                TypeRef {
                    id: TypeId::ERROR,
                    is_ref: false,
                },
                poison_expr(span),
            ),
        }
    }

    /// Checks `operand as T` (spec §5/§6). `T` is threaded as the operand's
    /// expected type: a literal therefore pins/grows to `T` with no conversion,
    /// a redundant cast is identity, and any other typed operand takes exactly
    /// one `@conversion` (else `E043`). The builtin `ref unknown` <-> any
    /// `ref T` cast is a retype handled before the expected-type path, so a
    /// pointer cast is never mistaken for a `ref` mismatch.
    fn check_cast(
        &mut self,
        fctx: &mut FuncCtx,
        operand: &Expr,
        target_ty: &AstType,
    ) -> (TypeRef, HirExpr) {
        let target = self.resolve_ty(target_ty);
        let span = operand.span();
        let poison = (
            TypeRef {
                id: TypeId::ERROR,
                is_ref: false,
            },
            poison_expr(span),
        );
        if is_poison(target) {
            let _ = self.check_expr(fctx, operand, None);
            return poison;
        }
        if target.is_ref {
            // A ref target is checked without an expected type so the §6
            // exception sees the operand's actual `ref unknown`/`ref T`.
            let (got, hir) = self.check_expr(fctx, operand, None);
            if is_poison(got) {
                return poison;
            }
            let unknown = self.builtin_ty("unknown");
            let pointer_cast =
                got.is_ref && (got.id == unknown || target.id == unknown || got.id == target.id);
            if pointer_cast {
                return (target, retype(hir, target, span));
            }
            self.mismatch(target, got, target_ty.span, span);
            return poison;
        }
        // A non-ref target threads `T` as the operand's expected type; a
        // literal pins/grows here, and a typed operand is checked against `T`.
        let (got, hir) = self.check_expr(fctx, operand, Some(target.id));
        if is_poison(got) {
            return poison;
        }
        if got.id == target.id && !got.is_ref {
            return (target, retype(hir, target, span));
        }
        if got.is_ref {
            // A ref operand to a non-ref target is a ref mismatch, not the §6
            // exception: `E010`, consistent with `expect_type`.
            self.mismatch(target, got, target_ty.span, span);
            return poison;
        }
        if super::ops::convert(
            self.types,
            self.diags,
            self.sigs,
            got,
            target.id,
            target_ty.span,
            span,
        )
        .is_err()
        {
            return poison;
        }
        let converted = self.convert_if_needed(got, target.id, hir);
        (target, retype(converted, target, span))
    }

    /// Checks `obj.field` reads (method calls go through `check_call`).
    fn check_member_read(
        &mut self,
        fctx: &mut FuncCtx,
        obj: &Expr,
        field: &str,
        span: Span,
        expected: Option<TypeId>,
    ) -> (TypeRef, HirExpr) {
        let (obj_ty, obj_hir) = self.check_expr(fctx, obj, None);
        if is_poison(obj_ty) {
            return (
                TypeRef {
                    id: TypeId::ERROR,
                    is_ref: false,
                },
                poison_expr(span),
            );
        }
        match self.field_type(obj_ty.id, field, span, expected, true) {
            Ok(ty) => {
                if let Some(def) = self.member_def(obj_ty.id, field) {
                    self.record_use(field_span(span, field), def);
                }
                (
                    ty,
                    HirExpr {
                        ty,
                        span,
                        kind: HirExprKind::Member {
                            obj: Box::new(obj_hir),
                            field: field.to_owned(),
                        },
                    },
                )
            }
            Err(_) => (
                TypeRef {
                    id: TypeId::ERROR,
                    is_ref: false,
                },
                poison_expr(span),
            ),
        }
    }

    /// Checks `a: T, b: U = f()`: the value must be a call returning exactly
    /// one value per binding (an inlined `=>` body counts as one value).
    fn check_multilet(
        &mut self,
        fctx: &mut FuncCtx,
        bindings: &[lilac_ast::LetBinding],
        value: &Expr,
        span: Span,
    ) {
        let mut hir_bindings = Vec::with_capacity(bindings.len());
        for binding in bindings {
            let ty = self.resolve_ty(&binding.ty);
            hir_bindings.push(HirParam {
                name: binding.name.clone(),
                ty,
                span: binding.span,
            });
        }
        let push_poison = |fctx: &mut FuncCtx| {
            push_stmt(
                fctx,
                HirStmt::MultiLet {
                    bindings: Vec::new(),
                    value: poison_expr(span),
                    span,
                },
                span,
            );
        };
        let Expr::Call {
            callee,
            generic_args,
            args,
            span: call_span,
        } = value
        else {
            self.diags.emit(
                Diagnostic::error(
                    "E022",
                    format!(
                        "multi-`let` needs a function call with {} returns",
                        bindings.len(),
                    ),
                )
                .label(span, true, "not a call".to_owned()),
            );
            push_poison(fctx);
            return;
        };
        let Some(call) = self.check_multilet_call(fctx, callee, generic_args, args, *call_span)
        else {
            push_poison(fctx);
            return;
        };
        if call.outs.len() != bindings.len() {
            self.diags.emit(
                Diagnostic::error(
                    "E022",
                    format!(
                        "wrong number of values: expected {}, call returns {}",
                        bindings.len(),
                        call.outs.len(),
                    ),
                )
                .label(call.call_span, true, "arity mismatch here".to_owned()),
            );
            push_poison(fctx);
            return;
        }
        for ((binding, expected), got) in bindings
            .iter()
            .zip(hir_bindings.iter())
            .zip(call.outs.iter())
        {
            if !is_poison(expected.ty) && !is_poison(*got) {
                let _ = self.expect_type(*got, expected.ty.id, binding.ty.span, value.span());
            }
        }
        let needs_wrap = call
            .outs
            .iter()
            .zip(hir_bindings.iter())
            .any(|(got, slot)| {
                !is_poison(*got)
                    && !is_poison(slot.ty)
                    && got.id != slot.ty.id
                    && self
                        .sigs
                        .conversions
                        .iter()
                        .any(|c| c.from == got.id && c.to == slot.ty.id)
            });
        if needs_wrap {
            self.check_multilet_converted(fctx, bindings, &hir_bindings, &call, span);
            return;
        }
        for (binding, expected) in bindings.iter().zip(hir_bindings.iter()) {
            fctx.locals.insert(binding.name.clone(), expected.ty);
        }
        push_stmt(
            fctx,
            HirStmt::MultiLet {
                bindings: hir_bindings,
                value: call.value_hir,
                span,
            },
            span,
        );
    }

    /// Checks a multi-`let` call: returns its out-slot types, value HIR,
    /// and call span (`None` after diagnosing).
    fn check_multilet_call(
        &mut self,
        fctx: &mut FuncCtx,
        callee: &Expr,
        generic_args: &[AstType],
        args: &[Expr],
        call_span: Span,
    ) -> Option<MultiCall> {
        match self.check_call(fctx, callee, generic_args, args, call_span, None) {
            Ok(CallOutcome::Call(call)) => {
                let outs = call.outs.clone();
                let value_hir = HirExpr {
                    ty: outs.first().copied().unwrap_or(TypeRef {
                        id: TypeId::ERROR,
                        is_ref: false,
                    }),
                    span: call_span,
                    kind: HirExprKind::Call {
                        callee: call.callee,
                        args: call.args,
                    },
                };
                Some(MultiCall {
                    outs,
                    value_hir,
                    call_span,
                })
            }
            Ok(CallOutcome::Inline(hir, ty)) => Some(MultiCall {
                outs: vec![ty],
                value_hir: hir,
                call_span,
            }),
            Ok(CallOutcome::CompilerError(_)) => Some(MultiCall {
                outs: Vec::new(),
                value_hir: poison_expr(call_span),
                call_span,
            }),
            Err(_) => None,
        }
    }

    /// Desugars converting multi-`let`s into a hidden exact multi-`let`
    /// plus one `let` per binding (each converting like a plain `let`).
    fn check_multilet_converted(
        &mut self,
        fctx: &mut FuncCtx,
        bindings: &[lilac_ast::LetBinding],
        hir_bindings: &[HirParam],
        call: &MultiCall,
        span: Span,
    ) {
        let outs = &call.outs;
        let value_hir = call.value_hir.clone();
        let call_span = call.call_span;
        let mut hidden_names = Vec::with_capacity(outs.len());
        let mut hidden = Vec::with_capacity(outs.len());
        for (index, out_ty) in outs.iter().enumerate() {
            let name = fresh_local(fctx, &format!("m{index}"));
            fctx.locals.insert(name.clone(), *out_ty);
            hidden_names.push(name.clone());
            hidden.push(HirParam {
                name,
                ty: *out_ty,
                span: call_span,
            });
        }
        push_stmt(
            fctx,
            HirStmt::MultiLet {
                bindings: hidden,
                value: value_hir,
                span,
            },
            span,
        );
        for ((binding, slot), (got, hidden)) in bindings
            .iter()
            .zip(hir_bindings.iter())
            .zip(outs.iter().zip(hidden_names.iter()))
        {
            let ident = HirExpr {
                ty: *got,
                span: binding.span,
                kind: HirExprKind::Ident {
                    name: hidden.clone(),
                    def: DefId::ERROR,
                },
            };
            if !is_poison(slot.ty) && !is_poison(*got) {
                let _ = self.expect_type(*got, slot.ty.id, binding.ty.span, binding.span);
            }
            let value = self.convert_if_needed(*got, slot.ty.id, ident);
            fctx.locals.insert(binding.name.clone(), slot.ty);
            push_stmt(
                fctx,
                HirStmt::Let {
                    name: binding.name.clone(),
                    ty: slot.ty,
                    value,
                    span: binding.span,
                },
                binding.span,
            );
        }
        for name in &hidden_names {
            fctx.locals.remove(name);
        }
    }

    fn check_expr(
        &mut self,
        fctx: &mut FuncCtx,
        expr: &Expr,
        expected: Option<TypeId>,
    ) -> (TypeRef, HirExpr) {
        match expr {
            Expr::Ident(name, span) => {
                let (ty, def) = self.ident_type(fctx, name, *span).unwrap_or((
                    TypeRef {
                        id: TypeId::ERROR,
                        is_ref: false,
                    },
                    DefId::ERROR,
                ));
                if fctx.locals.contains_key(name) {
                    self.record_semantic(*span, SemanticKind::Local);
                }
                self.record_use(*span, def);
                let hir_name = if name == "self" && fctx.locals.contains_key("self") {
                    "instance".to_owned()
                } else {
                    name.clone()
                };
                let hir = HirExpr {
                    ty,
                    span: *span,
                    kind: HirExprKind::Ident {
                        name: hir_name,
                        def,
                    },
                };
                (ty, hir)
            }
            Expr::Tag(name, span) => {
                let ty = self.tag_type(name, *span, expected).unwrap_or(TypeRef {
                    id: TypeId::ERROR,
                    is_ref: false,
                });
                if let lilac_ty::TypeKind::Object(def) = self.types.kind(ty.id) {
                    self.record_use(*span, *def);
                }
                let hir = HirExpr {
                    ty,
                    span: *span,
                    kind: HirExprKind::Tag { name: name.clone() },
                };
                (ty, hir)
            }
            Expr::Number(_, _) | Expr::Str(_, _) | Expr::Char(_, _) => {
                self.check_literal(expr, expected)
            }
            Expr::RefOf { value, span } => self.check_ref_of(fctx, value, *span),
            Expr::Cast(operand, target) => self.check_cast(fctx, operand, target),
            Expr::Member { obj, field, span } => {
                self.check_member_read(fctx, obj, field, *span, expected)
            }
            Expr::Call {
                callee,
                generic_args,
                args,
                span,
            } => self.check_call_expr(fctx, callee, generic_args, args, *span, expected),
            Expr::BinOp { lhs, op, rhs, span } => {
                self.check_binop_expr(fctx, lhs, *op, rhs, *span, expected)
            }
            Expr::IfValue {
                cond,
                then,
                els,
                span,
            } => self.check_if_value(fctx, cond, then, els, *span, expected),
            Expr::MatchValue {
                scrutinee,
                arms,
                els,
                span,
            } => self.check_match_value(fctx, scrutinee, arms, els, *span, expected),
            Expr::StructLit {
                ty_name,
                fields,
                span,
            } => self.check_struct_lit(fctx, ty_name, fields, *span, expected),
            Expr::FString { parts, span } => self.check_fstring(fctx, parts, *span),
            Expr::Error { span } => (
                TypeRef {
                    id: TypeId::ERROR,
                    is_ref: false,
                },
                poison_expr(*span),
            ),
        }
    }

    /// Desugars `$"..."` into `@format_args` construction: a hidden local `__f`
    /// built with `@format_args_new`, then one `@format_args_push` per literal
    /// chunk and hole (holes converted to `@byte_buffer`). The value is `__f`.
    fn check_fstring(
        &mut self,
        fctx: &mut FuncCtx,
        parts: &[lilac_ast::FStrPart],
        span: Span,
    ) -> (TypeRef, HirExpr) {
        let format_args = TypeRef {
            id: self.builtin_ty("@format_args"),
            is_ref: false,
        };
        let byte_buffer = TypeRef {
            id: self.builtin_ty("@byte_buffer"),
            is_ref: false,
        };
        let poison = (
            TypeRef {
                id: TypeId::ERROR,
                is_ref: false,
            },
            poison_expr(span),
        );
        if parts.len() > 16 {
            self.diags.emit(
                Diagnostic::error("E061", "too many format pieces (max 16)".to_owned()).label(
                    span,
                    true,
                    "too many pieces here".to_owned(),
                ),
            );
            return poison;
        }
        let fname = fresh_local(fctx, "f");
        fctx.locals.insert(fname.clone(), format_args);
        push_stmt(
            fctx,
            HirStmt::Let {
                name: fname.clone(),
                ty: format_args,
                value: HirExpr {
                    ty: format_args,
                    span,
                    kind: HirExprKind::Call {
                        callee: HirCallee::Intrinsic {
                            name: "@format_args_new".into(),
                        },
                        args: Vec::new(),
                    },
                },
                span,
            },
            span,
        );
        for part in parts {
            let piece = self.check_fstring_piece(fctx, part, byte_buffer, span);
            push_fstring_piece(fctx, &fname, format_args, piece, span);
        }
        (
            format_args,
            HirExpr {
                ty: format_args,
                span,
                kind: HirExprKind::Ident {
                    name: fname,
                    def: DefId::ERROR,
                },
            },
        )
    }

    /// Checks one f-string piece: literal chunks are `@byte_buffer` strings,
    /// holes convert to `@byte_buffer` (a missing conversion is `E043`).
    fn check_fstring_piece(
        &mut self,
        fctx: &mut FuncCtx,
        part: &lilac_ast::FStrPart,
        byte_buffer: TypeRef,
        span: Span,
    ) -> HirExpr {
        match part {
            lilac_ast::FStrPart::Lit(chunk) => HirExpr {
                ty: byte_buffer,
                span,
                kind: HirExprKind::Str {
                    value: chunk.clone(),
                },
            },
            lilac_ast::FStrPart::Hole(expr) => {
                let (got, hir) = self.check_expr(fctx, expr, None);
                let _ = self.expect_type(got, byte_buffer.id, expr.span(), expr.span());
                self.convert_if_needed(got, byte_buffer.id, hir)
            }
        }
    }

    /// Checks `Type { field = value, ... }`: every field against its declared
    /// type, no missing or unknown fields. Struct literals initialize
    /// fields directly and never call `@init` (factories serve `Type(params)`).
    fn check_struct_lit(
        &mut self,
        fctx: &mut FuncCtx,
        ty_name: &str,
        fields: &[lilac_ast::StructField],
        span: Span,
        expected: Option<TypeId>,
    ) -> (TypeRef, HirExpr) {
        let poison = (
            TypeRef {
                id: TypeId::ERROR,
                is_ref: false,
            },
            poison_expr(span),
        );
        let item = match lookup_one(self.project, self.file, ty_name, span, self.diags) {
            Ok(Some(item)) => item.clone(),
            Ok(None) => {
                self.diags.emit(
                    Diagnostic::error("E023", format!("unknown type `{ty_name}`")).label(
                        span,
                        true,
                        "no such type in scope".to_owned(),
                    ),
                );
                return poison;
            }
            Err(()) => return poison,
        };
        let ResKind::Object {
            members,
            generic_params,
            ..
        } = &item.kind
        else {
            self.diags.emit(
                Diagnostic::error("E010", format!("`{ty_name}` is not an object")).label(
                    span,
                    true,
                    "only objects construct with `{ ... }`".to_owned(),
                ),
            );
            return poison;
        };
        let obj_ty = if generic_params.is_empty() {
            let name = qualify_name(self.project, item.id, ty_name);
            let id = self.types.intern_object(item.id);
            self.types.set_object_name(item.id, &name);
            TypeRef { id, is_ref: false }
        } else {
            // A generic object literal takes its arguments from the expected
            // type (`x: GenObject[@bit32] = GenObject { ... }`). Type arguments
            // are always explicit, so an unannotated literal cannot be built.
            let args = expected.and_then(|id| match self.types.kind(id) {
                lilac_ty::TypeKind::Instance { base, args } if *base == item.id => {
                    Some(args.clone())
                }
                _ => None,
            });
            let Some(args) = args else {
                self.diags.emit(
                    Diagnostic::error(
                        "E023",
                        format!("cannot infer generic arguments for `{ty_name}`"),
                    )
                    .label(
                        span,
                        true,
                        "annotate the binding instead".to_owned(),
                    ),
                );
                return poison;
            };
            match self.instantiate_object(item.id, &args, span) {
                Ok(ty) => ty,
                Err(_) => return poison,
            }
        };
        let obj_id = obj_ty.id;
        let (hir_fields, seen, mut failed) =
            self.check_struct_fields(fctx, obj_id, ty_name, members, fields);
        let mut missing: Vec<&str> = members
            .iter()
            .filter(|(_, member)| matches!(member.kind, ResKind::Field))
            .map(|(name, _)| name.as_str())
            .filter(|name| !seen.contains(*name))
            .collect();
        missing.sort_unstable();
        if let Some(first) = missing.first() {
            self.diags.emit(
                Diagnostic::error(
                    "E022",
                    format!("struct literal missing field `{first}` of `{ty_name}`"),
                )
                .label(span, true, "add the missing field".to_owned()),
            );
            failed = true;
        }
        if failed {
            return poison;
        }
        (
            obj_ty,
            HirExpr {
                ty: obj_ty,
                span,
                kind: HirExprKind::StructLit { fields: hir_fields },
            },
        )
    }

    /// Checks each struct-literal field against its declared type; returns
    /// the HIR fields, the seen names, and whether anything failed.
    fn check_struct_fields(
        &mut self,
        fctx: &mut FuncCtx,
        obj_id: TypeId,
        ty_name: &str,
        members: &HashMap<String, ResItem>,
        fields: &[lilac_ast::StructField],
    ) -> (Vec<HirField>, HashSet<String>, bool) {
        let mut seen = HashSet::new();
        let mut hir_fields = Vec::with_capacity(fields.len());
        let mut failed = false;
        for field in fields {
            if !seen.insert(field.name.clone()) {
                self.diags.emit(
                    Diagnostic::error(
                        "E020",
                        format!("duplicate field `{}` in struct literal", field.name),
                    )
                    .label(field.span, true, "redefined here".to_owned()),
                );
                failed = true;
                continue;
            }
            let Ok(expected) = self.field_type(obj_id, &field.name, field.span, None, false) else {
                failed = true;
                continue;
            };
            if !matches!(
                members.get(&field.name).map(|m| &m.kind),
                Some(ResKind::Field)
            ) {
                self.diags.emit(
                    Diagnostic::error(
                        "E022",
                        format!("`{}` is not a field of `{ty_name}`", field.name),
                    )
                    .label(
                        field.span,
                        true,
                        "only fields initialize here".to_owned(),
                    ),
                );
                failed = true;
                continue;
            }
            let (got, value_hir) = self.check_expr(fctx, &field.value, Some(expected.id));
            if !is_poison(got)
                && self
                    .expect_type(got, expected.id, field.span, field.value.span())
                    .is_err()
            {
                failed = true;
            }
            let value_hir = self.convert_if_needed(got, expected.id, value_hir);
            hir_fields.push(HirField {
                name: field.name.clone(),
                value: value_hir,
                span: field.span,
            });
        }
        (hir_fields, seen, failed)
    }

    fn check_literal(&mut self, expr: &Expr, expected: Option<TypeId>) -> (TypeRef, HirExpr) {
        match expr {
            Expr::Number(bytes, span) => self.check_number_literal(bytes, *span, expected),
            Expr::Str(text, span) => {
                let ty = TypeRef {
                    id: self.builtin_ty("@byte_buffer"),
                    is_ref: false,
                };
                (
                    ty,
                    HirExpr {
                        ty,
                        span: *span,
                        kind: HirExprKind::Str {
                            value: text.clone(),
                        },
                    },
                )
            }
            Expr::Char(scalar, span) => {
                let ty = TypeRef {
                    id: self.builtin_ty("@bit32"),
                    is_ref: false,
                };
                (
                    ty,
                    HirExpr {
                        ty,
                        span: *span,
                        kind: HirExprKind::Char { value: *scalar },
                    },
                )
            }
            _ => {
                lilac_error::bug!(expr.span(), "check_literal on non-literal");
            }
        }
    }

    fn check_number_literal(
        &mut self,
        bytes: &str,
        span: Span,
        expected: Option<TypeId>,
    ) -> (TypeRef, HirExpr) {
        if is_float_literal(bytes) {
            return self.check_float_literal(bytes, span, expected);
        }
        self.check_int_literal(bytes, span, expected)
    }

    /// The single `@conversion(from: <bit|fbit>)` source whose target is
    /// `expected`, if exactly one exists. A numeric literal assigned to a
    /// user object (e.g. `x: Int16 = 5`, `x: Float32 = 3.14`) grows to the
    /// conversion's source width so the existing exact-width conversion applies
    /// (never shrinks). Multiple candidates are ambiguous and yield `None`, so
    /// the literal falls back to its smallest-fit type and the normal search.
    fn object_conversion_source(&self, expected: TypeId, want_float: bool) -> Option<TypeId> {
        let mut found: Option<TypeId> = None;
        for conversion in &self.sigs.conversions {
            if conversion.to != expected {
                continue;
            }
            let matches = if want_float {
                is_float(self.types, conversion.from)
            } else {
                is_bit(self.types, conversion.from)
            };
            if !matches {
                continue;
            }
            if found.is_some() {
                return None;
            }
            found = Some(conversion.from);
        }
        found
    }

    /// Types an integer literal (`0x`/`0b`/decimal): its natural type is the
    /// smallest `@bit8/16/32/64` holding its unsigned magnitude, grown (never
    /// shrunk) to an expected bit type, or to a user object's single
    /// `@conversion(from: @bitN)` source width; a value past that width, or
    /// past 64 bits, is `E060`. A non-bit expected type with no such conversion
    /// does not constrain it, and the normal conversion path applies.
    fn check_int_literal(
        &mut self,
        bytes: &str,
        span: Span,
        expected: Option<TypeId>,
    ) -> (TypeRef, HirExpr) {
        let poison = (
            TypeRef {
                id: TypeId::ERROR,
                is_ref: false,
            },
            poison_expr(span),
        );
        let Some(bits) = integer_bits(bytes) else {
            self.diags
                .emit(Diagnostic::error("E060", "invalid integer literal").label(
                    span,
                    true,
                    "not a valid integer literal".to_owned(),
                ));
            return poison;
        };
        let Some(ty) = self.resolve_int_literal_ty(bits, span, expected) else {
            return poison;
        };
        number_expr(ty, bytes, span)
    }

    /// The bit type an integer of `bits` significant magnitude bits takes under
    /// `expected`: the expected bit type or a user object's single
    /// `@conversion(from: @bitN)` source (grown, never shrunk; `E060` when too
    /// narrow), else the smallest-fit `@bitW` (`E060` past 64 bits). `None`
    /// after emitting `E060`.
    fn resolve_int_literal_ty(
        &mut self,
        bits: u32,
        span: Span,
        expected: Option<TypeId>,
    ) -> Option<TypeId> {
        let constrained = expected
            .filter(|id| is_bit(self.types, *id))
            .or_else(|| expected.and_then(|id| self.object_conversion_source(id, false)));
        if let Some(t) = constrained {
            let width = self.bit_width(t);
            if bits > width {
                self.diags.emit(
                    Diagnostic::error(
                        "E060",
                        format!(
                            "value needs {bits} bits but {} holds {width}",
                            self.types.display(t)
                        ),
                    )
                    .label(span, true, "literal overflows".to_owned()),
                );
                return None;
            }
            return Some(t);
        }
        match smallest_bit_width(bits) {
            Some(8) => Some(self.builtin_ty("@bit8")),
            Some(16) => Some(self.builtin_ty("@bit16")),
            Some(32) => Some(self.builtin_ty("@bit32")),
            Some(_) => Some(self.builtin_ty("@bit64")),
            None => {
                self.diags.emit(
                    Diagnostic::error("E060", "integer literal does not fit in 64 bits").label(
                        span,
                        true,
                        "literal too large".to_owned(),
                    ),
                );
                None
            }
        }
    }

    /// Folds a `@negate` applied to a single integer literal into a
    /// two's-complement constant (design §5): the value is the literal's
    /// magnitude negated in the smallest **signed** width that holds it, grown
    /// (never shrunk) to an expected bit type or a user object's single
    /// `@conversion(from: @bitN)` source. An expected narrower width, or a
    /// magnitude past the signed 64-bit range, is `E060`. Returns `None` when
    /// the argument is not an integer literal (a float literal or a non-literal
    /// keeps the `@negate` hook).
    fn fold_negative_literal(
        &mut self,
        args: &[Expr],
        span: Span,
        expected: Option<TypeId>,
    ) -> Option<(TypeRef, HirExpr)> {
        if args.len() != 1 {
            return None;
        }
        let Expr::Number(bytes, lit_span) = &args[0] else {
            return None;
        };
        if is_float_literal(bytes) {
            return None;
        }
        let poison = (
            TypeRef {
                id: TypeId::ERROR,
                is_ref: false,
            },
            poison_expr(span),
        );
        let Some(magnitude) = integer_magnitude(bytes) else {
            self.diags
                .emit(Diagnostic::error("E060", "invalid integer literal").label(
                    *lit_span,
                    true,
                    "not a valid integer literal".to_owned(),
                ));
            return Some(poison);
        };
        let Some(fit) = signed_smallest_bit_width(magnitude) else {
            self.diags.emit(
                Diagnostic::error("E060", "negative integer literal does not fit in 64 bits")
                    .label(*lit_span, true, "literal too large".to_owned()),
            );
            return Some(poison);
        };
        let Some((ty, value_bytes)) =
            self.signed_int_literal_value(magnitude, fit, *lit_span, expected)
        else {
            return Some(poison);
        };
        Some(number_expr(ty, &value_bytes, span))
    }

    /// The bit type and two's-complement bit pattern of a negative integer with
    /// signed smallest-fit `fit` and unsigned `magnitude`, grown (never shrunk)
    /// to the expected bit type or a user object's single
    /// `@conversion(from: @bitN)` source. An expected narrower width is `E060`
    /// (labeled at `label_span`). `None` after emitting `E060`.
    fn signed_int_literal_value(
        &mut self,
        magnitude: u128,
        fit: u32,
        label_span: Span,
        expected: Option<TypeId>,
    ) -> Option<(TypeId, String)> {
        let constrained = expected
            .filter(|id| is_bit(self.types, *id))
            .or_else(|| expected.and_then(|id| self.object_conversion_source(id, false)));
        let (ty, width) = if let Some(t) = constrained {
            let width = self.bit_width(t);
            if fit > width {
                self.diags.emit(
                    Diagnostic::error(
                        "E060",
                        format!(
                            "value needs {fit} bits but {} holds {width}",
                            self.types.display(t)
                        ),
                    )
                    .label(label_span, true, "literal overflows".to_owned()),
                );
                return None;
            }
            (t, width)
        } else {
            let ty = match fit {
                8 => self.builtin_ty("@bit8"),
                16 => self.builtin_ty("@bit16"),
                32 => self.builtin_ty("@bit32"),
                _ => self.builtin_ty("@bit64"),
            };
            (ty, fit)
        };
        let mask = (1u128 << width) - 1;
        let value = ((1u128 << width) - magnitude) & mask;
        Some((ty, format!("0x{value:X}")))
    }

    /// Folds an all-literal `+ - * / %` subtree bottom-up (design §7): an
    /// operand subtree folds first, then a parent whose operands are both
    /// literal values. Returns `None` when either operand is non-literal, so
    /// the operator keeps its hook resolution and its sides are checked
    /// normally.
    fn fold_literal_binop(
        &mut self,
        lhs: &Expr,
        op: BinOp,
        rhs: &Expr,
        span: Span,
        expected: Option<TypeId>,
    ) -> Option<(TypeRef, HirExpr)> {
        if !is_arithmetic(op) {
            return None;
        }
        let left = self.fold_literal_value(lhs);
        let right = self.fold_literal_value(rhs);
        if matches!(left, FoldedLiteral::Error) || matches!(right, FoldedLiteral::Error) {
            return Some(poison_folded(span));
        }
        let (FoldedLiteral::Value(left), FoldedLiteral::Value(right)) = (left, right) else {
            return None;
        };
        match self.combine_literal_values(left, op, right, span) {
            Some(value) => Some(self.type_folded_value(value, span, expected)),
            None => Some(poison_folded(span)),
        }
    }

    /// Folds one expression to a literal value: a number literal, or a `+ - *
    /// / %` `BinOp` whose operands both fold. `NotLiteral` when an operand is
    /// not a literal (the operator keeps its hook); `Error` after emitting a
    /// fold diagnostic (invalid/too-wide literal, mixed kinds, divide by zero,
    /// or 64-bit overflow).
    fn fold_literal_value(&mut self, expr: &Expr) -> FoldedLiteral {
        match expr {
            Expr::Number(bytes, span) => {
                if is_float_literal(bytes) {
                    FoldedLiteral::Value(LiteralValue::Float(parse_float_literal(bytes)))
                } else if let Some(magnitude) = integer_magnitude(bytes) {
                    if magnitude > u128::from(u64::MAX) {
                        self.diags.emit(
                            Diagnostic::error("E060", "integer literal does not fit in 64 bits")
                                .label(*span, true, "literal too large".to_owned()),
                        );
                        FoldedLiteral::Error
                    } else {
                        FoldedLiteral::Value(LiteralValue::Int {
                            negative: false,
                            magnitude,
                        })
                    }
                } else {
                    self.diags
                        .emit(Diagnostic::error("E060", "invalid integer literal").label(
                            *span,
                            true,
                            "not a valid integer literal".to_owned(),
                        ));
                    FoldedLiteral::Error
                }
            }
            Expr::BinOp { lhs, op, rhs, span } if is_arithmetic(*op) => {
                let left = self.fold_literal_value(lhs);
                let right = self.fold_literal_value(rhs);
                if matches!(left, FoldedLiteral::Error) || matches!(right, FoldedLiteral::Error) {
                    return FoldedLiteral::Error;
                }
                match (left, right) {
                    (FoldedLiteral::Value(left), FoldedLiteral::Value(right)) => {
                        match self.combine_literal_values(left, *op, right, *span) {
                            Some(value) => FoldedLiteral::Value(value),
                            None => FoldedLiteral::Error,
                        }
                    }
                    _ => FoldedLiteral::NotLiteral,
                }
            }
            _ => FoldedLiteral::NotLiteral,
        }
    }

    /// Combines two folded literal values under an arithmetic operator at
    /// 64-bit (design §7). A mixed int/float pair is `E010`; divide/modulo by
    /// zero, a non-finite float result, and an integer result outside the
    /// signed/unsigned 64-bit range are `E060`. Returns `None` after emitting.
    fn combine_literal_values(
        &mut self,
        left: LiteralValue,
        op: BinOp,
        right: LiteralValue,
        span: Span,
    ) -> Option<LiteralValue> {
        match (left, right) {
            (LiteralValue::Float(left), LiteralValue::Float(right)) => {
                let value = match op {
                    BinOp::Add => left + right,
                    BinOp::Sub => left - right,
                    BinOp::Mul => left * right,
                    BinOp::Div => left / right,
                    BinOp::Mod => left % right,
                    _ => lilac_error::bug!(span, "non-arithmetic operator in float literal fold"),
                };
                if value.is_finite() {
                    Some(LiteralValue::Float(value))
                } else {
                    self.diags.emit(
                        Diagnostic::error("E060", "float expression does not fit in @fbit64")
                            .label(span, true, "expression out of range".to_owned()),
                    );
                    None
                }
            }
            (
                LiteralValue::Int {
                    negative: left_negative,
                    magnitude: left_magnitude,
                },
                LiteralValue::Int {
                    negative: right_negative,
                    magnitude: right_magnitude,
                },
            ) => {
                let left = signed_value(left_negative, left_magnitude);
                let right = signed_value(right_negative, right_magnitude);
                let result = match op {
                    BinOp::Add => left.checked_add(right),
                    BinOp::Sub => left.checked_sub(right),
                    BinOp::Mul => left.checked_mul(right),
                    BinOp::Div | BinOp::Mod if right == 0 => {
                        self.diags.emit(
                            Diagnostic::error(
                                "E060",
                                "division by zero in integer literal expression",
                            )
                            .label(
                                span,
                                true,
                                "divide by zero".to_owned(),
                            ),
                        );
                        return None;
                    }
                    BinOp::Div => left.checked_div(right),
                    BinOp::Mod => left.checked_rem(right),
                    _ => {
                        lilac_error::bug!(span, "non-arithmetic operator in integer literal fold")
                    }
                };
                let Some(result) = result else {
                    self.diags.emit(
                        Diagnostic::error("E060", "integer expression does not fit in 64 bits")
                            .label(span, true, "expression too large".to_owned()),
                    );
                    return None;
                };
                // Each operand fits 64 bits, and `checked_*` rejects an
                // `i128` overflow, so `result` is in `±2^128`. Enforce the
                // 64-bit range here so an intermediate overflow is `E060` even
                // when a later operation would bring the value back in range.
                let magnitude = result.unsigned_abs();
                let negative = result < 0;
                if (!negative && magnitude > u128::from(u64::MAX))
                    || (negative && magnitude > 1u128 << 63)
                {
                    self.diags.emit(
                        Diagnostic::error("E060", "integer expression does not fit in 64 bits")
                            .label(span, true, "expression too large".to_owned()),
                    );
                    return None;
                }
                Some(LiteralValue::Int {
                    negative,
                    magnitude,
                })
            }
            _ => {
                self.diags.emit(
                    Diagnostic::error(
                        "E010",
                        "mismatched types: cannot mix an integer and a float literal",
                    )
                    .label(span, true, "mixed int/float operands".to_owned()),
                );
                None
            }
        }
    }

    /// Types a folded literal value by the literal rules: an integer grows to
    /// the consumed bit type (or a user object's single `@conversion(from:
    /// @bitN)` source), `E060` on overflow; a negative result uses the signed
    /// two's-complement rule (design §5); a float defaults to `@fbit64`, pins
    /// to an expected `@fbitN`, and is `E060` past the max finite.
    fn type_folded_value(
        &mut self,
        value: LiteralValue,
        span: Span,
        expected: Option<TypeId>,
    ) -> (TypeRef, HirExpr) {
        let poison = poison_folded(span);
        match value {
            LiteralValue::Int {
                negative: true,
                magnitude,
            } => {
                let Some(fit) = signed_smallest_bit_width(magnitude) else {
                    self.diags.emit(
                        Diagnostic::error("E060", "integer expression does not fit in 64 bits")
                            .label(span, true, "expression too large".to_owned()),
                    );
                    return poison;
                };
                let Some((ty, bytes)) =
                    self.signed_int_literal_value(magnitude, fit, span, expected)
                else {
                    return poison;
                };
                number_expr(ty, &bytes, span)
            }
            LiteralValue::Int {
                negative: false,
                magnitude,
            } => {
                let bits = if magnitude == 0 {
                    0
                } else {
                    u128::BITS - magnitude.leading_zeros()
                };
                let Some(ty) = self.resolve_int_literal_ty(bits, span, expected) else {
                    return poison;
                };
                number_expr(ty, &format!("0x{magnitude:X}"), span)
            }
            LiteralValue::Float(value) => {
                if let Some(t) = expected {
                    if is_bit(self.types, t) {
                        self.diags.emit(
                            Diagnostic::error(
                                "E010",
                                "mismatched types: expected an integer, found a float",
                            )
                            .label(
                                span,
                                true,
                                "float expression here".to_owned(),
                            ),
                        );
                        return poison;
                    }
                    if is_float(self.types, t) && float_width(self.types, t) == 128 {
                        self.diags.emit(
                            Diagnostic::error("E010", "`@fbit128` literals are not supported yet")
                                .label(
                                    span,
                                    true,
                                    "only 16/32/64-bit float literals can be typed".to_owned(),
                                ),
                        );
                        return poison;
                    }
                }
                let target = match expected {
                    Some(t) if is_float(self.types, t) => t,
                    Some(t) => match self.object_conversion_source(t, true) {
                        Some(src) => src,
                        None => self.builtin_ty("@fbit64"),
                    },
                    None => self.builtin_ty("@fbit64"),
                };
                let width = float_width(self.types, target);
                if !value.is_finite() || value.abs() > max_finite_float(width) {
                    self.diags.emit(
                        Diagnostic::error(
                            "E060",
                            format!(
                                "float expression does not fit in {}",
                                self.types.display(target)
                            ),
                        )
                        .label(
                            span,
                            true,
                            "expression out of range".to_owned(),
                        ),
                    );
                    return poison;
                }
                number_expr(target, &format!("{value:?}"), span)
            }
        }
    }

    /// Types a float literal (`.` or exponent form): it defaults to `@fbit64`,
    /// an expected `@fbit16/32/64` (or a user object's single
    /// `@conversion(from: @fbitN)` source) pins it (the value rounds to
    /// nearest-even at codegen); a value past the target's max finite is
    /// `E060`, a bit expected type is `E010` (no implicit float→int), and an
    /// expected `@fbit128` is out of scope (`E010`).
    fn check_float_literal(
        &mut self,
        bytes: &str,
        span: Span,
        expected: Option<TypeId>,
    ) -> (TypeRef, HirExpr) {
        let poison = (
            TypeRef {
                id: TypeId::ERROR,
                is_ref: false,
            },
            poison_expr(span),
        );
        if let Some(t) = expected {
            if is_bit(self.types, t) {
                self.diags.emit(
                    Diagnostic::error(
                        "E010",
                        "mismatched types: expected an integer, found a float",
                    )
                    .label(span, true, "float literal here".to_owned()),
                );
                return poison;
            }
            if is_float(self.types, t) && float_width(self.types, t) == 128 {
                self.diags.emit(
                    Diagnostic::error("E010", "`@fbit128` literals are not supported yet").label(
                        span,
                        true,
                        "only 16/32/64-bit float literals can be typed".to_owned(),
                    ),
                );
                return poison;
            }
        }
        let target = match expected {
            Some(t) if is_float(self.types, t) => t,
            Some(t) => match self.object_conversion_source(t, true) {
                Some(src) => src,
                None => self.builtin_ty("@fbit64"),
            },
            None => self.builtin_ty("@fbit64"),
        };
        let width = float_width(self.types, target);
        let cleaned: String = bytes.chars().filter(|c| *c != '_').collect();
        let value = cleaned.parse::<f64>().unwrap_or(f64::NAN);
        if !value.is_finite() || value.abs() > max_finite_float(width) {
            self.diags.emit(
                Diagnostic::error(
                    "E060",
                    format!(
                        "float literal does not fit in {}",
                        self.types.display(target)
                    ),
                )
                .label(span, true, "literal out of range".to_owned()),
            );
            return poison;
        }
        number_expr(target, &cleaned, span)
    }

    fn check_call_expr(
        &mut self,
        fctx: &mut FuncCtx,
        callee: &Expr,
        generic_args: &[AstType],
        args: &[Expr],
        span: Span,
        expected: Option<TypeId>,
    ) -> (TypeRef, HirExpr) {
        let poison = (
            TypeRef {
                id: TypeId::ERROR,
                is_ref: false,
            },
            poison_expr(span),
        );
        match self.check_call(fctx, callee, generic_args, args, span, expected) {
            Ok(CallOutcome::Call(call)) => {
                if call.outs.len() != 1 {
                    self.diags.emit(
                        Diagnostic::error(
                            "E022",
                            "call in value position must return exactly one value",
                        )
                        .label(
                            span,
                            true,
                            "one value expected here".to_owned(),
                        ),
                    );
                    return poison;
                }
                let ty = call.outs[0];
                (
                    ty,
                    HirExpr {
                        ty,
                        span,
                        kind: HirExprKind::Call {
                            callee: call.callee,
                            args: call.args,
                        },
                    },
                )
            }
            Ok(CallOutcome::Inline(hir, ty)) => (ty, hir),
            Ok(CallOutcome::CompilerError(hir)) => {
                let poison = TypeRef {
                    id: TypeId::ERROR,
                    is_ref: false,
                };
                (poison, hir)
            }
            Err(_) => poison,
        }
    }

    fn check_binop_expr(
        &mut self,
        fctx: &mut FuncCtx,
        lhs: &Expr,
        op: BinOp,
        rhs: &Expr,
        span: Span,
        expected: Option<TypeId>,
    ) -> (TypeRef, HirExpr) {
        let poison = (
            TypeRef {
                id: TypeId::ERROR,
                is_ref: false,
            },
            poison_expr(span),
        );
        if matches!(op, BinOp::And | BinOp::Or) {
            return self.check_logic_op(fctx, lhs, op, rhs, span);
        }
        if op != BinOp::EqEq {
            let Some(hook) = super::ops::binop_hook_name(op) else {
                lilac_error::bug!(span, "equality handled above");
            };
            if let Some(result) = self.fold_literal_binop(lhs, op, rhs, span, expected) {
                return result;
            }
            let ((left, left_hir), (right, right_hir)) = self.check_binop_sides(fctx, lhs, rhs);
            if is_poison(left) || is_poison(right) {
                return poison;
            }
            match super::ops::resolve_binop(self.sigs, hook, left.id, right.id) {
                Ok((def, sig)) => {
                    let ty = TypeRef {
                        id: sig.returns[0].id,
                        is_ref: false,
                    };
                    let name = self.hook_name(hook, def);
                    return (
                        ty,
                        HirExpr {
                            ty,
                            span,
                            kind: HirExprKind::Call {
                                callee: HirCallee::Hook { name, def },
                                args: vec![left_hir, right_hir],
                            },
                        },
                    );
                }
                Err(miss) => {
                    self.diags.emit(
                        Diagnostic::error(
                            "E043",
                            format!(
                                "no {} for ({}, {})",
                                miss.hook,
                                self.types.display(miss.lhs),
                                self.types.display(miss.rhs),
                            ),
                        )
                        .label(
                            span,
                            true,
                            "no exact operator overload".to_owned(),
                        ),
                    );
                    return poison;
                }
            }
        }
        let ((left, left_hir), (right, right_hir)) = self.check_binop_sides(fctx, lhs, rhs);
        self.check_equality(left, left_hir, right, right_hir, span)
    }

    /// Checks `==` (identical base types, no conversions) and builds its HIR.
    fn check_equality(
        &mut self,
        left: TypeRef,
        left_hir: HirExpr,
        right: TypeRef,
        right_hir: HirExpr,
        span: Span,
    ) -> (TypeRef, HirExpr) {
        let poison = (
            TypeRef {
                id: TypeId::ERROR,
                is_ref: false,
            },
            poison_expr(span),
        );
        if is_poison(left) || is_poison(right) {
            return poison;
        }
        if left.id == right.id {
            let ty = TypeRef {
                id: self.builtin_ty("@bit8"),
                is_ref: false,
            };
            return (
                ty,
                HirExpr {
                    ty,
                    span,
                    kind: HirExprKind::EqEq {
                        lhs: Box::new(left_hir),
                        rhs: Box::new(right_hir),
                    },
                },
            );
        }
        self.diags.emit(
            Diagnostic::error(
                "E010",
                format!(
                    "mismatched types: `==` needs identical types, found {} and {}",
                    self.types.display(left.id),
                    self.types.display(right.id),
                ),
            )
            .label(
                left_hir.span,
                false,
                format!("left side `{}` here", self.types.display(left.id)),
            )
            .label(
                right_hir.span,
                true,
                format!("right side `{}` here", self.types.display(right.id)),
            ),
        );
        poison
    }

    /// Checks both binop sides exactly once: non-literals first to anchor the
    /// width, literals take the anchor. Two literal operands are folded by
    /// [`Self::fold_literal_binop`] before reaching here; this path handles the
    /// remaining shapes (one or both operands non-literal).
    fn check_binop_sides(
        &mut self,
        fctx: &mut FuncCtx,
        lhs: &Expr,
        rhs: &Expr,
    ) -> ((TypeRef, HirExpr), (TypeRef, HirExpr)) {
        let left_lit = is_foldable_literal(lhs);
        let right_lit = is_foldable_literal(rhs);
        if left_lit && right_lit {
            let left = self.check_expr(fctx, lhs, None);
            let right = self.check_expr(fctx, rhs, None);
            return (left, right);
        }
        let left = if left_lit {
            None
        } else {
            Some(self.check_expr(fctx, lhs, None))
        };
        let right = if right_lit {
            None
        } else {
            Some(self.check_expr(fctx, rhs, None))
        };
        let anchor = left
            .iter()
            .chain(right.iter())
            .map(|(ty, _)| ty.id)
            .find(|id| is_bit(self.types, *id));
        let left = match left {
            Some(found) => found,
            None => self.check_expr(fctx, lhs, anchor),
        };
        let right = match right {
            Some(found) => found,
            None => self.check_expr(fctx, rhs, anchor),
        };
        (left, right)
    }

    #[allow(clippy::too_many_lines)]
    fn check_call(
        &mut self,
        fctx: &mut FuncCtx,
        callee: &Expr,
        generic_args: &[AstType],
        args: &[Expr],
        span: Span,
        expected: Option<TypeId>,
    ) -> Result<CallOutcome, ErrorGuaranteed> {
        match callee {
            Expr::Ident(name, name_span) => {
                if name == "@negate" && generic_args.is_empty() {
                    if let Some((ty, hir)) = self.fold_negative_literal(args, span, expected) {
                        return Ok(CallOutcome::Inline(hir, ty));
                    }
                }
                if let Some(local_ty) = fctx.locals.get(name).copied() {
                    if !generic_args.is_empty() {
                        return Err(self.emit_generic_arity(span, name, 0, generic_args.len()));
                    }
                    if let lilac_ty::TypeKind::Func { params, returns } =
                        self.types.kind(local_ty.id).clone()
                    {
                        let sig = FuncSig { params, returns };
                        let hir_args = self.check_args(fctx, &sig, args, span)?;
                        return Ok(CallOutcome::Call(CheckedCall {
                            outs: sig.returns.clone(),
                            callee: HirCallee::Local { name: name.clone() },
                            args: hir_args,
                        }));
                    }
                    return Err(self.diags.emit(
                        Diagnostic::error("E022", format!("`{name}` is not callable")).label(
                            *name_span,
                            true,
                            "only functions can be called".to_owned(),
                        ),
                    ));
                }
                match lookup_one(self.project, self.file, name, *name_span, self.diags) {
                    Ok(Some(item)) => {
                        if matches!(item.kind, ResKind::Func { .. } | ResKind::Hook { .. }) {
                            self.record_use(*name_span, item.id);
                        }
                        return self.check_named_call(
                            fctx,
                            name,
                            &item.clone(),
                            generic_args,
                            args,
                            span,
                        );
                    }
                    Err(()) => return Err(ErrorGuaranteed::new()),
                    Ok(None) => {}
                }
                if let Some(intrinsic) = lookup_intrinsic(name) {
                    self.record_semantic(*name_span, SemanticKind::BuiltinFunc);
                    if !generic_args.is_empty() {
                        return Err(self.emit_generic_arity(span, name, 0, generic_args.len()));
                    }
                    return self.check_intrinsic(fctx, name, intrinsic, args, span, expected);
                }
                Err(self.diags.emit(
                    Diagnostic::error("E021", format!("unresolved name `{name}`")).label(
                        *name_span,
                        true,
                        "not a local, item, or intrinsic".to_owned(),
                    ),
                ))
            }
            Expr::Member {
                obj,
                field,
                span: member_span,
            } => {
                if let Expr::Ident(name, _) = obj.as_ref() {
                    // `T.method()`: a bare type parameter receiver within a
                    // specialization is a static (associated) call (ruling R3),
                    // and shadows an outer type of the same name.
                    if let Some(result) = self.check_bare_static_call(
                        fctx,
                        name,
                        field,
                        generic_args,
                        args,
                        span,
                        expected,
                    ) {
                        return result;
                    }
                }
                if !generic_args.is_empty() {
                    return Err(self.emit_generic_arity(span, field, 0, generic_args.len()));
                }
                let (recv_ty, recv_hir) = self.check_expr(fctx, obj, None);
                if is_poison(recv_ty) {
                    return Err(ErrorGuaranteed::new());
                }
                let recv = Receiver {
                    ty: recv_ty,
                    hir: recv_hir,
                };
                self.check_member_call(fctx, field, recv, args, span, *member_span, expected)
            }
            Expr::Tag(name, tag_span) => {
                self.check_tag_call(fctx, name, args, span, expected, *tag_span)
            }
            _ => {
                let _ = self.check_expr(fctx, callee, None);
                Err(self.diags.emit(
                    Diagnostic::error("E022", "this expression is not callable").label(
                        span,
                        true,
                        "only functions can be called".to_owned(),
                    ),
                ))
            }
        }
    }

    fn check_named_call(
        &mut self,
        fctx: &mut FuncCtx,
        name: &str,
        item: &ResItem,
        generic_args: &[AstType],
        args: &[Expr],
        span: Span,
    ) -> Result<CallOutcome, ErrorGuaranteed> {
        match &item.kind {
            ResKind::Func {
                generic,
                generic_params,
                ..
            } => {
                if *generic {
                    return self.check_generic_func_call(
                        fctx,
                        item.id,
                        generic_params,
                        generic_args,
                        args,
                        span,
                    );
                }
                if !generic_args.is_empty() {
                    return Err(self.emit_generic_arity(span, name, 0, generic_args.len()));
                }
                if let Some((ast_item, resolved, parent)) =
                    find_item(self.program, self.project, item.id)
                {
                    if let ItemKind::Func(decl) = &ast_item.kind {
                        if matches!(decl.body, Some(lilac_ast::FuncBody::Inline(_))) {
                            let callee = InlineCallee {
                                def: item.id,
                                ast: ast_item,
                                resolved,
                                decl,
                                parent,
                            };
                            return self.check_inline_call(fctx, callee, args, span);
                        }
                    }
                }
                let Some(sig) = self.func_sig(item.id) else {
                    lilac_error::bug!(span, "missing signature for called function");
                };
                let hir_args = self.check_args(fctx, &sig, args, span)?;
                return Ok(CallOutcome::Call(CheckedCall {
                    outs: sig.returns.clone(),
                    callee: HirCallee::Item(item.id),
                    args: hir_args,
                }));
            }
            ResKind::Hook { generic, .. } => {
                if *generic {
                    return Err(ErrorGuaranteed::new());
                }
                if !generic_args.is_empty() {
                    return Err(self.emit_generic_arity(span, name, 0, generic_args.len()));
                }
            }
            ResKind::Object { .. } | ResKind::Enum { .. } => {
                if !generic_args.is_empty() {
                    return Err(self.emit_generic_arity(span, name, 0, generic_args.len()));
                }
                return self.check_factory_call(fctx, name, item, args, span);
            }
            ResKind::Const | ResKind::Field => {
                return Err(self.diags.emit(
                    Diagnostic::error("E022", "this item is not callable").label(
                        span,
                        true,
                        "only functions can be called".to_owned(),
                    ),
                ));
            }
        }
        let overloads = self.sigs.hooks.get(name).cloned().unwrap_or_default();
        let call = self.check_hook_call(fctx, name, &overloads, args, span)?;
        Ok(CallOutcome::Call(call))
    }

    /// Calls `Type(args)`: routes to the `@init` factory hook, which takes
    /// plain parameters and returns exactly the constructed type (SPEC §8).
    fn check_factory_call(
        &mut self,
        fctx: &mut FuncCtx,
        name: &str,
        item: &ResItem,
        args: &[Expr],
        span: Span,
    ) -> Result<CallOutcome, ErrorGuaranteed> {
        if !matches!(&item.kind, ResKind::Object { .. }) {
            return Err(self.diags.emit(
                Diagnostic::error("E022", format!("type `{name}` has no `@init` constructor"))
                    .label(span, true, "constructors need `@init`".to_owned()),
            ));
        }
        let hooks = self.object_hooks(item.id, "@init");
        if hooks.is_empty() {
            return Err(self.diags.emit(
                Diagnostic::error("E022", format!("type `{name}` has no `@init` constructor"))
                    .label(span, true, "define `@init` or use `= { ... }`".to_owned()),
            ));
        }
        let qualified = qualify_name(self.project, item.id, name);
        let obj_ty = {
            let id = self.types.intern_object(item.id);
            self.types.set_object_name(item.id, &qualified);
            id
        };
        let mut overloads: Vec<(DefId, FuncSig)> = Vec::with_capacity(hooks.len());
        for hook in &hooks {
            // A hook whose signature was rejected earlier is skipped rather
            // than panicking; an all-skipped `overloads` then reports `E022`
            // through `select_hook_overload`.
            if let Some(sig) = self.func_sig(hook.id) {
                overloads.push((hook.id, sig));
            }
        }
        let qual = format!("{qualified}.@init");
        let Some((def, sig)) = self.select_hook_overload(fctx, &overloads, args) else {
            return Err(self.diags.emit(
                Diagnostic::error(
                    "E022",
                    format!("no overload of `{qual}` takes {} arguments", args.len()),
                )
                .label(span, true, "arity mismatch here".to_owned()),
            ));
        };
        if let Some(hook) = hooks.iter().find(|hook| hook.id == def) {
            if self.private_member_denied(hook, item.id, span) {
                return Err(ErrorGuaranteed::new());
            }
        }
        let shape_span = hooks
            .iter()
            .find(|hook| hook.id == def)
            .map_or(span, |hook| hook.span);
        if sig.returns.len() != 1 || sig.returns[0].id != obj_ty {
            return Err(self.diags.emit(
                Diagnostic::error("E022", "`@init` must return exactly its object type").label(
                    shape_span,
                    true,
                    "bad `@init` shape".to_owned(),
                ),
            ));
        }
        let hir_args = self.check_hook_args(fctx, &sig, args, span)?;
        let hook_name = self.hook_name(&qual, def);
        Ok(CallOutcome::Call(CheckedCall {
            outs: sig.returns.clone(),
            callee: HirCallee::Hook {
                name: hook_name,
                def,
            },
            args: hir_args,
        }))
    }

    /// Selects the overload whose arity and (probed) parameter types match
    /// `args`, mirroring the exact-match rule of [`Self::check_hook_call`].
    /// Returns `None` when no overload takes `args.len()` parameters.
    fn select_hook_overload(
        &mut self,
        fctx: &mut FuncCtx,
        overloads: &[(DefId, FuncSig)],
        args: &[Expr],
    ) -> Option<(DefId, FuncSig)> {
        let arity_ok: Vec<(DefId, FuncSig)> = overloads
            .iter()
            .filter(|(_, sig)| sig.params.len() == args.len())
            .cloned()
            .collect();
        let (first_def, first) = arity_ok.first().cloned()?;
        if arity_ok.len() == 1 {
            return Some((first_def, first));
        }
        let mut probed: Vec<Option<TypeRef>> = Vec::with_capacity(args.len());
        for arg in args {
            if matches!(arg, Expr::Number(..)) {
                probed.push(None);
                continue;
            }
            let (got, _, _) = self.check_nested(fctx, arg, None);
            probed.push(if is_poison(got) { None } else { Some(got) });
        }
        Some(
            arity_ok
                .iter()
                .find(|(_, sig)| {
                    sig.params
                        .iter()
                        .zip(probed.iter())
                        .all(|(param, got)| match got {
                            None => true,
                            Some(ty) if param.is_ref => ty.is_ref && ty.id == param.id,
                            Some(ty) => ty.id == param.id,
                        })
                })
                .cloned()
                .unwrap_or((first_def, first)),
        )
    }

    /// Selects a hook overload (exact first, arity-filtered) and checks the
    /// arguments against it.
    fn check_hook_call(
        &mut self,
        fctx: &mut FuncCtx,
        name: &str,
        overloads: &[(DefId, FuncSig)],
        args: &[Expr],
        span: Span,
    ) -> Result<CheckedCall, ErrorGuaranteed> {
        let Some((def, sig)) = self.select_hook_overload(fctx, overloads, args) else {
            return Err(self.diags.emit(
                Diagnostic::error(
                    "E022",
                    format!("no overload of `{name}` takes {} arguments", args.len()),
                )
                .label(span, true, "arity mismatch here".to_owned()),
            ));
        };
        let hir_args = self.check_hook_args(fctx, &sig, args, span)?;
        let name = self.hook_name(name, def);
        Ok(CheckedCall {
            outs: sig.returns.clone(),
            callee: HirCallee::Hook { name, def },
            args: hir_args,
        })
    }

    /// Mangled LIR name for a hook overload: the qualified hook name plus
    /// its signature (`@add:bit32:bit32`, `Parent.@init:ref_Object`).
    /// Overloads that collide here were already rejected as `E020` dupes.
    fn hook_name(&self, qual: &str, def: DefId) -> String {
        let Some(sig) = self.func_sig(def) else {
            // A missing signature means the hook was rejected earlier; fall
            // back to the bare qualified name rather than panicking.
            return qual.to_owned();
        };
        let mut name = qual.to_owned();
        for param in sig.params.iter().chain(sig.returns.iter()) {
            name.push(':');
            name.push_str(
                &self
                    .types
                    .display_ref(*param)
                    .replace('@', "")
                    .replace(' ', "_"),
            );
        }
        name
    }

    /// Expands a stored `=>` body at a call site. The body's own branch-join
    /// temps are renamed to fresh ids *before* the checked arguments are
    /// substituted: a substituted argument may itself be a `Temp` whose
    /// defining prelude the caller already emitted under its original id, so
    /// renaming after substitution would desynchronize the definition and
    /// lowering would see a `use of unassigned join temp`. Renaming is
    /// id-sorted for reproducible generated HIR/LIR.
    fn expand_inline_body(
        &mut self,
        prelude: &[HirStmt],
        body: &HirExpr,
        param_names: Vec<String>,
        hir_args: Vec<HirExpr>,
    ) -> (Vec<HirStmt>, HirExpr) {
        let temps: HashSet<TempId> = collect_temps(prelude, body);
        let mut ordered: Vec<TempId> = temps.into_iter().collect();
        ordered.sort_unstable_by_key(|temp| temp.0);
        let ren: HashMap<TempId, TempId> = ordered
            .into_iter()
            .map(|temp| (temp, self.fresh_temp()))
            .collect();
        let prelude = rename_temps_in_stmts(prelude, &ren);
        let body = rename_temps_in_expr(body, &ren);
        let map: HashMap<String, HirExpr> = param_names.into_iter().zip(hir_args).collect();
        let prelude = subst_stmts(&prelude, &map);
        let body = subst_ident(&body, &map);
        (prelude, body)
    }

    /// Expands a call to an `=>` function by substituting the checked
    /// arguments for its parameters. A control-flow-bearing argument is
    /// evaluated once: its prelude is emitted once and its `Temp` substituted.
    /// A prelude-free argument (pure or impure) is textually substituted into
    /// each use, so an argument used twice is evaluated twice. Recursive `=>`
    /// calls cannot inline.
    fn check_inline_call(
        &mut self,
        fctx: &mut FuncCtx,
        callee: InlineCallee<'_>,
        args: &[Expr],
        span: Span,
    ) -> Result<CallOutcome, ErrorGuaranteed> {
        let def = callee.def;
        if self.in_progress.contains(&def) {
            return Err(self.diags.emit(
                Diagnostic::error("E022", "cannot inline recursive `=>` call").label(
                    span,
                    true,
                    "recursive inline calls never terminate".to_owned(),
                ),
            ));
        }
        if !self.checked.contains(&def) {
            self.in_progress.insert(def);
            // A concrete `=>` body may be lazily checked while a generic body is
            // being specialized; clear the substitution environment so the
            // concrete body resolves against its own (empty) scope.
            let saved_params = std::mem::take(&mut self.current_params);
            let saved_args = std::mem::take(&mut self.current_args);
            // A lazily-checked inline callee may live in another file than its
            // caller; its body resolves its own types in the declaring file.
            let saved_file = self.file;
            self.file = callee.ast.span.file;
            self.check_func(callee.ast, &callee.resolved, callee.decl, callee.parent);
            self.file = saved_file;
            self.current_params = saved_params;
            self.current_args = saved_args;
            self.in_progress.remove(&def);
        }
        let Some((param_names, prelude, body_hir)) = self.inline_bodies.get(&def).cloned() else {
            return Err(ErrorGuaranteed::new());
        };
        let Some(sig) = self.func_sig(def) else {
            lilac_error::bug!(span, "missing signature for inlined function");
        };
        if sig.returns.len() != 1 {
            return Err(ErrorGuaranteed::new());
        }
        let hir_args = self.check_args(fctx, &sig, args, span)?;
        let (prelude, value) = self.expand_inline_body(&prelude, &body_hir, param_names, hir_args);
        if let Some(top) = fctx.blocks.last_mut() {
            top.extend(prelude);
        }
        Ok(CallOutcome::Inline(value, sig.returns[0]))
    }

    /// Calls `recv.field(args)` on a value: injects `ref` receiver as `self`.
    /// (Static `Type.field(args)` calls never reach here.)
    #[allow(clippy::too_many_arguments)]
    fn check_member_call(
        &mut self,
        fctx: &mut FuncCtx,
        field: &str,
        recv: Receiver,
        args: &[Expr],
        span: Span,
        member_span: Span,
        expected: Option<TypeId>,
    ) -> Result<CallOutcome, ErrorGuaranteed> {
        if let lilac_ty::TypeKind::Instance {
            base,
            args: inst_args,
        } = self.types.kind(recv.ty.id).clone()
        {
            return self.check_instance_member_call(
                fctx,
                field,
                recv,
                base,
                &inst_args,
                args,
                span,
                member_span,
                expected,
            );
        }
        let owner = self.types.display(recv.ty.id);
        let lilac_ty::TypeKind::Object(def) = self.types.kind(recv.ty.id) else {
            let mut diagnostic =
                Diagnostic::error("E010", format!("cannot call method `{field}` on `{owner}`"))
                    .label(span, true, "methods need an object".to_owned());
            if self.is_generic_derived(recv.ty) {
                let note = self.required_signature_note(fctx, field, Some(recv.ty), args, expected);
                diagnostic = diagnostic.note(note);
            }
            return Err(self.diags.emit(diagnostic));
        };
        let found = self.find_by_def(*def);
        let Some(ResKind::Object { members, .. }) = found else {
            return Err(self.diags.emit(
                Diagnostic::error("E010", format!("cannot call method `{field}` on `{owner}`"))
                    .label(span, true, "methods need an object".to_owned()),
            ));
        };
        let Some(member) = members.get(field).cloned() else {
            let mut diagnostic = Diagnostic::error(
                "E021",
                format!("no member `{field}` on `{owner}`"),
            )
            .label(member_span, true, "no such member".to_owned());
            if self.is_generic_derived(recv.ty) {
                let note = self.required_signature_note(fctx, field, Some(recv.ty), args, expected);
                diagnostic = diagnostic.note(note);
            }
            return Err(self.diags.emit(diagnostic));
        };
        if self.private_member_denied(&member, *def, member_span) {
            return Err(ErrorGuaranteed::new());
        }
        self.record_use(field_span(member_span, field), member.id);
        let (sig, callee) = self.resolve_method(field, &member, recv.ty, member_span)?;
        self.check_method_args(fctx, &sig, callee, recv, args, span)
    }

    /// Calls `value.method(...)` where `value` is a concrete generic instance:
    /// specializes the method per the instance's arguments and requires its
    /// first parameter to be `ref <instance>` (value-based duck typing).
    #[allow(clippy::too_many_arguments)]
    fn check_instance_member_call(
        &mut self,
        fctx: &mut FuncCtx,
        field: &str,
        recv: Receiver,
        base: DefId,
        inst_args: &[TypeRef],
        args: &[Expr],
        span: Span,
        member_span: Span,
        expected: Option<TypeId>,
    ) -> Result<CallOutcome, ErrorGuaranteed> {
        let owner = self.types.display(recv.ty.id);
        let Some(ResKind::Object { members, .. }) = self.find_by_def(base) else {
            return Err(self.diags.emit(
                Diagnostic::error("E010", format!("cannot call method `{field}` on `{owner}`"))
                    .label(span, true, "methods need an object".to_owned()),
            ));
        };
        let Some(member) = members.get(field).cloned() else {
            let mut diagnostic = Diagnostic::error(
                "E021",
                format!("no member `{field}` on `{owner}`"),
            )
            .label(member_span, true, "no such member".to_owned());
            if self.is_generic_derived(recv.ty) {
                let note = self.required_signature_note(fctx, field, Some(recv.ty), args, expected);
                diagnostic = diagnostic.note(note);
            }
            return Err(self.diags.emit(diagnostic));
        };
        if self.private_member_denied(&member, base, member_span) {
            return Err(ErrorGuaranteed::new());
        }
        self.record_use(field_span(member_span, field), member.id);
        let ResKind::Func { generic_params, .. } = &member.kind else {
            return Err(self.diags.emit(
                Diagnostic::error("E022", format!("`{field}` is not callable")).label(
                    member_span,
                    true,
                    "only functions can be called".to_owned(),
                ),
            ));
        };
        if !generic_params.is_empty() {
            // A method-own generic parameter cannot be supplied through a
            // value receiver (`value.method[@T]()`); report arity instead of
            // leaking a `Param` into the specialized signature.
            return Err(self.emit_generic_arity(member_span, field, generic_params.len(), 0));
        }
        let sig = self.instantiate_func(member.id, inst_args, member_span)?;
        let Some(first) = sig.params.first() else {
            return Err(self.diags.emit(
                Diagnostic::error("E022", format!("method `{field}` takes no `self`")).label(
                    member_span,
                    true,
                    "first parameter must be `ref Self`".to_owned(),
                ),
            ));
        };
        if !(first.is_ref && first.id == recv.ty.id) {
            return Err(self.diags.emit(
                Diagnostic::error("E022", format!("method `{field}` takes no `self`")).label(
                    member_span,
                    true,
                    "first parameter must be `ref Self`".to_owned(),
                ),
            ));
        }
        let name = self.instance_name(member.id, inst_args);
        let callee = HirCallee::Hook {
            name,
            def: member.id,
        };
        self.check_method_args(fctx, &sig, callee, recv, args, span)
    }

    /// Resolves a member to a callable method: the first parameter must be
    /// `ref Self` (missing members and non-functions stay errors here).
    fn resolve_method(
        &mut self,
        field: &str,
        member: &ResItem,
        recv_ty: TypeRef,
        member_span: Span,
    ) -> Result<(FuncSig, HirCallee), ErrorGuaranteed> {
        let (sig, callee) = match &member.kind {
            ResKind::Func { .. } => {
                let Some(sig) = self.func_sig(member.id) else {
                    lilac_error::bug!(member.span, "missing signature for method");
                };
                (sig, HirCallee::Item(member.id))
            }
            ResKind::Hook { generic, .. } => {
                if *generic {
                    return Err(ErrorGuaranteed::new());
                }
                let Some(sig) = self.func_sig(member.id) else {
                    lilac_error::bug!(member.span, "missing signature for member hook");
                };
                let qual = match find_parent_name(self.project, member.id) {
                    Some(parent) => format!("{parent}.{field}"),
                    None => field.to_owned(),
                };
                let name = self.hook_name(&qual, member.id);
                (
                    sig,
                    HirCallee::Hook {
                        name,
                        def: member.id,
                    },
                )
            }
            _ => {
                return Err(self.diags.emit(
                    Diagnostic::error("E022", format!("`{field}` is not callable")).label(
                        member_span,
                        true,
                        "only functions can be called".to_owned(),
                    ),
                ));
            }
        };
        let Some(first) = sig.params.first() else {
            return Err(self.diags.emit(
                Diagnostic::error("E022", format!("method `{field}` takes no `self`")).label(
                    member_span,
                    true,
                    "first parameter must be `ref Self`".to_owned(),
                ),
            ));
        };
        if !(first.is_ref && first.id == recv_ty.id) {
            return Err(self.diags.emit(
                Diagnostic::error("E022", format!("method `{field}` takes no `self`")).label(
                    member_span,
                    true,
                    "first parameter must be `ref Self`".to_owned(),
                ),
            ));
        }
        Ok((sig, callee))
    }

    /// Checks method arguments with the `ref` receiver injected as `self`.
    fn check_method_args(
        &mut self,
        fctx: &mut FuncCtx,
        sig: &FuncSig,
        callee: HirCallee,
        recv: Receiver,
        args: &[Expr],
        span: Span,
    ) -> Result<CallOutcome, ErrorGuaranteed> {
        if sig.params.len() != args.len() + 1 {
            return Err(self.diags.emit(
                Diagnostic::error(
                    "E022",
                    format!(
                        "wrong number of arguments: expected {}, found {}",
                        sig.params.len() - 1,
                        args.len(),
                    ),
                )
                .label(span, true, "arity mismatch here".to_owned()),
            ));
        }
        if !matches!(
            recv.hir.kind,
            HirExprKind::Ident { .. } | HirExprKind::Member { .. } | HirExprKind::Temp(_)
        ) {
            return Err(self.diags.emit(
                Diagnostic::error("E010", "cannot take `ref` of this expression").label(
                    span,
                    true,
                    "method receivers must be places".to_owned(),
                ),
            ));
        }
        // Ref receivers already hold the address: pass them directly instead
        // of wrapping another `ref` (which would double-indirect).
        let mut hir_args = Vec::with_capacity(args.len() + 1);
        if recv.ty.is_ref {
            hir_args.push(recv.hir);
        } else {
            let self_ty = TypeRef {
                id: recv.ty.id,
                is_ref: true,
            };
            hir_args.push(HirExpr {
                ty: self_ty,
                span: recv.hir.span,
                kind: HirExprKind::RefOf {
                    value: Box::new(recv.hir),
                },
            });
        }
        let mut failed = false;
        for (arg, param) in args.iter().zip(sig.params.iter().skip(1)) {
            if let Ok(hir) = self.check_arg(fctx, arg, *param, span) {
                hir_args.push(hir);
            } else {
                failed = true;
                hir_args.push(poison_expr(arg.span()));
            }
        }
        if failed {
            return Err(ErrorGuaranteed::new());
        }
        Ok(CallOutcome::Call(CheckedCall {
            outs: sig.returns.clone(),
            callee,
            args: hir_args,
        }))
    }

    /// Calls `Type.field(args)`: no `self` injection, arguments as written.
    #[allow(clippy::too_many_arguments)]
    fn check_static_call(
        &mut self,
        fctx: &mut FuncCtx,
        type_name: &str,
        type_item: &ResItem,
        field: &str,
        generic_args: &[AstType],
        args: &[Expr],
        span: Span,
        expected: Option<TypeId>,
    ) -> Result<CallOutcome, ErrorGuaranteed> {
        let ResKind::Object {
            members,
            generic_params,
            ..
        } = &type_item.kind
        else {
            return Err(self.diags.emit(
                Diagnostic::error("E021", format!("no member `{field}` on `{type_name}`")).label(
                    span,
                    true,
                    "only objects have static members".to_owned(),
                ),
            ));
        };
        // A bare generic-object receiver (`GenObj.method()`) never has its
        // members monomorphized, so the template signature must not reach HIR.
        // Require explicit arguments: `GenObj[@bit32]` (via the instance path).
        if !generic_params.is_empty() && generic_args.is_empty() {
            return Err(self.emit_generic_arity(span, type_name, generic_params.len(), 0));
        }
        let Some(member) = members.get(field).cloned() else {
            let mut diagnostic = Diagnostic::error(
                "E021",
                format!("no member `{field}` on `{type_name}`"),
            )
            .label(span, true, "no such member".to_owned());
            let owner_id = self.types.intern_object(type_item.id);
            let qualified = qualify_name(self.project, type_item.id, type_name);
            self.types.set_object_name(type_item.id, &qualified);
            let owner_ty = TypeRef {
                id: owner_id,
                is_ref: false,
            };
            if self.is_generic_derived(owner_ty) {
                let note = self.required_signature_note(fctx, field, None, args, expected);
                diagnostic = diagnostic.note(note);
            }
            return Err(self.diags.emit(diagnostic));
        };
        if self.private_member_denied(&member, type_item.id, span) {
            return Err(ErrorGuaranteed::new());
        }
        match &member.kind {
            ResKind::Func {
                generic,
                generic_params,
                ..
            } => {
                if *generic {
                    return self.check_generic_func_call(
                        fctx,
                        member.id,
                        generic_params,
                        generic_args,
                        args,
                        span,
                    );
                }
                if !generic_args.is_empty() {
                    return Err(self.emit_generic_arity(span, field, 0, generic_args.len()));
                }
                let Some(sig) = self.func_sig(member.id) else {
                    lilac_error::bug!(member.span, "missing signature for static member");
                };
                let hir_args = self.check_args(fctx, &sig, args, span)?;
                Ok(CallOutcome::Call(CheckedCall {
                    outs: sig.returns.clone(),
                    callee: HirCallee::Item(member.id),
                    args: hir_args,
                }))
            }
            ResKind::Hook { generic, .. } => {
                if *generic {
                    return Err(ErrorGuaranteed::new());
                }
                if !generic_args.is_empty() {
                    return Err(self.emit_generic_arity(span, field, 0, generic_args.len()));
                }
                let Some(sig) = self.func_sig(member.id) else {
                    lilac_error::bug!(member.span, "missing signature for static hook");
                };
                let name = self.hook_name(&format!("{type_name}.{field}"), member.id);
                let hir_args = self.check_args(fctx, &sig, args, span)?;
                Ok(CallOutcome::Call(CheckedCall {
                    outs: sig.returns.clone(),
                    callee: HirCallee::Hook {
                        name,
                        def: member.id,
                    },
                    args: hir_args,
                }))
            }
            _ => Err(self.diags.emit(
                Diagnostic::error("E022", format!("`{field}` is not callable")).label(
                    span,
                    true,
                    "only functions can be called".to_owned(),
                ),
            )),
        }
    }

    /// Handles `Name.member(...)` where `Name` is a bare type parameter
    /// (`T.method()`, ruling R3) or a declared object/enum name. Returns `None`
    /// when `name` is neither, so the caller falls through to a member call.
    #[allow(clippy::too_many_arguments)]
    fn check_bare_static_call(
        &mut self,
        fctx: &mut FuncCtx,
        name: &str,
        field: &str,
        generic_args: &[AstType],
        args: &[Expr],
        span: Span,
        expected: Option<TypeId>,
    ) -> Option<Result<CallOutcome, ErrorGuaranteed>> {
        if let Some(result) =
            self.check_generic_param_static(fctx, name, field, generic_args, args, span, expected)
        {
            return Some(result);
        }
        match lookup_one(self.project, self.file, name, span, self.diags) {
            Ok(Some(type_item)) => {
                if matches!(
                    &type_item.kind,
                    ResKind::Object { .. } | ResKind::Enum { .. }
                ) {
                    let type_item = type_item.clone();
                    return Some(self.check_static_call(
                        fctx,
                        name,
                        &type_item,
                        field,
                        generic_args,
                        args,
                        span,
                        expected,
                    ));
                }
            }
            Err(()) => return Some(Err(ErrorGuaranteed::new())),
            Ok(None) => {}
        }
        None
    }

    /// Resolves a bare type-parameter receiver to its concrete type and routes
    /// the call as a static (associated) call (`T.method()`, ruling R3).
    /// Returns `None` when `name` is not a current generic parameter.
    #[allow(clippy::too_many_arguments)]
    fn check_generic_param_static(
        &mut self,
        fctx: &mut FuncCtx,
        name: &str,
        field: &str,
        generic_args: &[AstType],
        args: &[Expr],
        span: Span,
        expected: Option<TypeId>,
    ) -> Option<Result<CallOutcome, ErrorGuaranteed>> {
        let concrete = self.generic_param_value(name)?;
        Some(self.check_static_call_ty(fctx, concrete, field, generic_args, args, span, expected))
    }

    /// Routes `T.method(...)` for a concrete receiver type: a plain object
    /// uses the existing static path; a generic instance specializes the
    /// associated function (static duck typing, ruling R3 / design §3.3).
    #[allow(clippy::too_many_arguments)]
    fn check_static_call_ty(
        &mut self,
        fctx: &mut FuncCtx,
        ty: TypeRef,
        field: &str,
        generic_args: &[AstType],
        args: &[Expr],
        span: Span,
        expected: Option<TypeId>,
    ) -> Result<CallOutcome, ErrorGuaranteed> {
        match self.types.kind(ty.id).clone() {
            lilac_ty::TypeKind::Object(def) => {
                let name = self.object_name_of(def);
                let Some(item) = self.res_item(def) else {
                    return Err(ErrorGuaranteed::new());
                };
                self.check_static_call(
                    fctx,
                    &name,
                    &item,
                    field,
                    generic_args,
                    args,
                    span,
                    expected,
                )
            }
            lilac_ty::TypeKind::Instance {
                base,
                args: inst_args,
            } => {
                let name = self.object_name_of(base);
                self.check_instance_static_call(
                    fctx,
                    &name,
                    base,
                    &inst_args,
                    field,
                    generic_args,
                    args,
                    span,
                    expected,
                )
            }
            _ => {
                let owner = self.types.display(ty.id);
                let mut diagnostic = Diagnostic::error(
                    "E010",
                    format!("cannot call static member `{field}` on `{owner}`"),
                )
                .label(span, true, "static calls need an object type".to_owned());
                if self.is_generic_derived(ty) {
                    let note = self.required_signature_note(fctx, field, None, args, expected);
                    diagnostic = diagnostic.note(note);
                }
                Err(self.diags.emit(diagnostic))
            }
        }
    }

    /// Calls `Instance.field(args)` where `Instance` is `base[inst_args]`: the
    /// associated function is monomorphized per `(field DefId, args)`.
    #[allow(clippy::too_many_arguments)]
    fn check_instance_static_call(
        &mut self,
        fctx: &mut FuncCtx,
        type_name: &str,
        base: DefId,
        inst_args: &[TypeRef],
        field: &str,
        generic_args: &[AstType],
        args: &[Expr],
        span: Span,
        expected: Option<TypeId>,
    ) -> Result<CallOutcome, ErrorGuaranteed> {
        let Some(ResKind::Object { members, .. }) = self.find_by_def(base) else {
            return Err(self.diags.emit(
                Diagnostic::error(
                    "E010",
                    format!("cannot call static member `{field}` on `{type_name}`"),
                )
                .label(span, true, "only objects have static members".to_owned()),
            ));
        };
        let Some(member) = members.get(field).cloned() else {
            let mut diagnostic = Diagnostic::error(
                "E021",
                format!("no member `{field}` on `{type_name}`"),
            )
            .label(span, true, "no such member".to_owned());
            let owner_id = self.types.intern_instance(base, inst_args.to_vec());
            let owner_ty = TypeRef {
                id: owner_id,
                is_ref: false,
            };
            if self.is_generic_derived(owner_ty) {
                let note = self.required_signature_note(fctx, field, None, args, expected);
                diagnostic = diagnostic.note(note);
            }
            return Err(self.diags.emit(diagnostic));
        };
        if self.private_member_denied(&member, base, span) {
            return Err(ErrorGuaranteed::new());
        }
        let ResKind::Func {
            generic,
            generic_params,
            ..
        } = &member.kind
        else {
            return Err(self.diags.emit(
                Diagnostic::error("E022", format!("`{field}` is not callable")).label(
                    span,
                    true,
                    "only functions can be called".to_owned(),
                ),
            ));
        };
        // A member's own generic arguments follow the object's in the shared
        // substitution vector (`sigs::register` appends them), so indices line
        // up with `substitute` (Task 3 convention).
        let mut all_args = inst_args.to_vec();
        if *generic {
            if generic_args.len() != generic_params.len() {
                return Err(self.emit_generic_arity(
                    span,
                    field,
                    generic_params.len(),
                    generic_args.len(),
                ));
            }
            all_args.extend(self.resolve_generic_args(generic_args));
        } else if !generic_args.is_empty() {
            return Err(self.emit_generic_arity(span, field, 0, generic_args.len()));
        }
        let sig = self.instantiate_func(member.id, &all_args, span)?;
        let name = self.instance_name(member.id, &all_args);
        let callee = HirCallee::Hook {
            name,
            def: member.id,
        };
        let hir_args = self.check_args(fctx, &sig, args, span)?;
        Ok(CallOutcome::Call(CheckedCall {
            outs: sig.returns.clone(),
            callee,
            args: hir_args,
        }))
    }

    fn check_arg(
        &mut self,
        fctx: &mut FuncCtx,
        arg: &Expr,
        param: TypeRef,
        span: Span,
    ) -> Result<HirExpr, ErrorGuaranteed> {
        let (got, hir) = self.check_expr(fctx, arg, Some(param.id));
        if is_poison(got) {
            return Err(ErrorGuaranteed::new());
        }
        if param.is_ref && !got.is_ref {
            return Err(self.mismatch(param, got, span, arg.span()));
        }
        self.expect_type(got, param.id, span, arg.span())?;
        Ok(self.convert_if_needed(got, param.id, hir))
    }

    /// Checks one hook-call argument without inserting a `@conversion`:
    /// a non-`ref` argument whose type is not the parameter type is `E010`.
    /// Numeric literals still take the parameter type (`check_expr`).
    fn check_hook_arg(
        &mut self,
        fctx: &mut FuncCtx,
        arg: &Expr,
        param: TypeRef,
        span: Span,
    ) -> Result<HirExpr, ErrorGuaranteed> {
        let (got, hir) = self.check_expr(fctx, arg, Some(param.id));
        if is_poison(got) {
            return Err(ErrorGuaranteed::new());
        }
        if param.is_ref && !got.is_ref {
            return Err(self.mismatch(param, got, span, arg.span()));
        }
        if got.id != param.id {
            return Err(self.mismatch(param, got, span, arg.span()));
        }
        Ok(hir)
    }

    /// [`check_args`] for a hook call: matches each argument exactly, never
    /// converting. Ordinary calls keep the converting `check_args` path.
    fn check_hook_args(
        &mut self,
        fctx: &mut FuncCtx,
        sig: &FuncSig,
        args: &[Expr],
        span: Span,
    ) -> Result<Vec<HirExpr>, ErrorGuaranteed> {
        if args.len() != sig.params.len() {
            return Err(self.diags.emit(
                Diagnostic::error(
                    "E022",
                    format!(
                        "wrong number of arguments: expected {}, found {}",
                        sig.params.len(),
                        args.len(),
                    ),
                )
                .label(span, true, "arity mismatch here".to_owned()),
            ));
        }
        let mut hir_args = Vec::with_capacity(args.len());
        let mut failed = false;
        for (arg, param) in args.iter().zip(sig.params.iter()) {
            if let Ok(hir) = self.check_hook_arg(fctx, arg, *param, span) {
                hir_args.push(hir);
            } else {
                failed = true;
                hir_args.push(poison_expr(arg.span()));
            }
        }
        if failed {
            return Err(ErrorGuaranteed::new());
        }
        Ok(hir_args)
    }

    fn check_args(
        &mut self,
        fctx: &mut FuncCtx,
        sig: &FuncSig,
        args: &[Expr],
        span: Span,
    ) -> Result<Vec<HirExpr>, ErrorGuaranteed> {
        if args.len() != sig.params.len() {
            return Err(self.diags.emit(
                Diagnostic::error(
                    "E022",
                    format!(
                        "wrong number of arguments: expected {}, found {}",
                        sig.params.len(),
                        args.len(),
                    ),
                )
                .label(span, true, "arity mismatch here".to_owned()),
            ));
        }
        let mut hir_args = Vec::with_capacity(args.len());
        let mut failed = false;
        for (arg, param) in args.iter().zip(sig.params.iter()) {
            if let Ok(hir) = self.check_arg(fctx, arg, *param, span) {
                hir_args.push(hir);
            } else {
                failed = true;
                hir_args.push(poison_expr(arg.span()));
            }
        }
        if failed {
            return Err(ErrorGuaranteed::new());
        }
        Ok(hir_args)
    }

    fn builtin_ty(&mut self, name: &str) -> TypeId {
        match self.types.builtin(name) {
            Some(id) => id,
            None => lilac_error::bug!(Span::dummy(), "missing builtin `{name}`"),
        }
    }

    fn expect_arity(
        &mut self,
        span: Span,
        what: &str,
        args: &[Expr],
        want: usize,
    ) -> Result<(), ErrorGuaranteed> {
        if args.len() != want {
            return Err(self.diags.emit(
                Diagnostic::error(
                    "E022",
                    format!(
                        "wrong number of arguments to `{what}`: expected {want}, found {}",
                        args.len()
                    ),
                )
                .label(span, true, "arity mismatch here".to_owned()),
            ));
        }
        Ok(())
    }

    fn check_intrinsic(
        &mut self,
        fctx: &mut FuncCtx,
        name: &str,
        intrinsic: Intrinsic,
        args: &[Expr],
        span: Span,
        expected: Option<TypeId>,
    ) -> Result<CallOutcome, ErrorGuaranteed> {
        let (outs, hir_args) = match intrinsic {
            Intrinsic::ArithInt(_)
            | Intrinsic::ArithFloat(_)
            | Intrinsic::CmpInt(_)
            | Intrinsic::CmpFloat(_)
            | Intrinsic::NegateInt(_)
            | Intrinsic::NegateFloat(_) => self.numeric_out(fctx, intrinsic, name, args, span)?,
            Intrinsic::Truncate | Intrinsic::ExtendZero => {
                self.converting_out(fctx, intrinsic, args, span, expected)?
            }
            Intrinsic::FbitToBits(_) | Intrinsic::BitsToFbit(_) => {
                self.bitcast_out(fctx, intrinsic, name, args, span)?
            }
            Intrinsic::LoadU8 | Intrinsic::StoreU8 | Intrinsic::Memcpy => {
                self.mem_intrinsic(fctx, intrinsic, args, span)?
            }
            Intrinsic::ByteBufferPtr
            | Intrinsic::ByteBufferLen
            | Intrinsic::ByteBufferFrom
            | Intrinsic::FormatArgsNew
            | Intrinsic::FormatArgsPush
            | Intrinsic::FormatArgsCount
            | Intrinsic::FormatArgsPiece => self.buffer_intrinsic(fctx, intrinsic, args, span)?,
            Intrinsic::SizeOf => {
                self.expect_arity(span, "@size_of", args, 1)?;
                let (_, hir) = self.check_expr(fctx, &args[0], None);
                (
                    vec![TypeRef {
                        id: self.builtin_ty("@bit_native"),
                        is_ref: false,
                    }],
                    vec![hir],
                )
            }
            Intrinsic::CompileError => {
                self.expect_arity(span, name, args, 1)?;
                let Expr::Str(msg, _) = &args[0] else {
                    return Err(self.diags.emit(
                        Diagnostic::error(
                            "E010",
                            "`@compile_error` needs a string literal message",
                        )
                        .label(
                            args[0].span(),
                            true,
                            "message goes here".to_owned(),
                        ),
                    ));
                };
                let hir = HirExpr {
                    ty: TypeRef {
                        id: TypeId::ERROR,
                        is_ref: false,
                    },
                    span,
                    kind: HirExprKind::CompileError {
                        msg: msg.clone(),
                        msg_span: args[0].span(),
                        call_span: span,
                    },
                };
                return Ok(CallOutcome::CompilerError(hir));
            }
        };
        Ok(CallOutcome::Call(CheckedCall {
            outs,
            callee: HirCallee::Intrinsic {
                name: name.to_owned(),
            },
            args: hir_args,
        }))
    }

    /// Checks a width-suffixed numeric intrinsic against its exact signature:
    /// every argument is checked against its one parameter type (literals adopt
    /// that width) and the result is the exact result type. A mismatch is
    /// `E010`; there is no width unification or conversion.
    fn numeric_out(
        &mut self,
        fctx: &mut FuncCtx,
        intrinsic: Intrinsic,
        name: &str,
        args: &[Expr],
        span: Span,
    ) -> Result<(Vec<TypeRef>, Vec<HirExpr>), ErrorGuaranteed> {
        let (param_name, result_name, arity) = match intrinsic {
            Intrinsic::ArithInt(width) => (width.builtin(), width.builtin(), 2),
            Intrinsic::NegateInt(width) => (width.builtin(), width.builtin(), 1),
            Intrinsic::CmpInt(width) => (width.builtin(), "@bit8", 2),
            Intrinsic::ArithFloat(width) => (width.builtin(), width.builtin(), 2),
            Intrinsic::NegateFloat(width) => (width.builtin(), width.builtin(), 1),
            Intrinsic::CmpFloat(width) => (width.builtin(), "@bit8", 2),
            _ => lilac_error::bug!(span, "numeric_out on a non-numeric intrinsic"),
        };
        let param = self.builtin_ty(param_name);
        let result = self.builtin_ty(result_name);
        self.expect_arity(span, name, args, arity)?;
        let mut hir_args = Vec::with_capacity(args.len());
        let mut failed = false;
        for arg in args {
            let (got, hir) = self.check_expr(fctx, arg, Some(param));
            if !is_poison(got) && got.id != param {
                self.diags.emit(
                    Diagnostic::error(
                        "E010",
                        format!(
                            "mismatched types: expected {}, found {}",
                            self.types.display(param),
                            self.types.display(got.id),
                        ),
                    )
                    .label(
                        arg.span(),
                        true,
                        "wrong operand type here".to_owned(),
                    ),
                );
                failed = true;
            }
            hir_args.push(hir);
        }
        if failed {
            return Err(ErrorGuaranteed::new());
        }
        Ok((
            vec![TypeRef {
                id: result,
                is_ref: false,
            }],
            hir_args,
        ))
    }

    fn converting_out(
        &mut self,
        fctx: &mut FuncCtx,
        intrinsic: Intrinsic,
        args: &[Expr],
        span: Span,
        expected: Option<TypeId>,
    ) -> Result<(Vec<TypeRef>, Vec<HirExpr>), ErrorGuaranteed> {
        let value = |id: TypeId| TypeRef { id, is_ref: false };
        let name = match intrinsic {
            Intrinsic::Truncate => "@truncate",
            _ => "@extend_zero",
        };
        self.expect_arity(span, name, args, 1)?;
        let (got, hir) = self.check_expr(fctx, &args[0], None);
        if !is_poison(got) && !is_bit(self.types, got.id) {
            self.diags.emit(
                Diagnostic::error(
                    "E010",
                    format!(
                        "mismatched types: expected a bit container, found {}",
                        self.types.display(got.id),
                    ),
                )
                .label(args[0].span(), true, "not a bit container".to_owned()),
            );
            return Err(ErrorGuaranteed::new());
        }
        match expected {
            Some(t) if is_bit(self.types, t) => Ok((vec![value(t)], vec![hir])),
            _ => {
                Err(self.diags.emit(
                    Diagnostic::error("E024", format!("`{name}` needs an expected bit type"))
                        .label(span, true, "annotate the binding".to_owned()),
                ))
            }
        }
    }

    /// Checks a float reinterpret cast against its exact signature: the single
    /// operand must be exactly the source type, and the result is exactly the
    /// destination type (no conversion, no widening). A mismatch is `E010`.
    fn bitcast_out(
        &mut self,
        fctx: &mut FuncCtx,
        intrinsic: Intrinsic,
        name: &str,
        args: &[Expr],
        span: Span,
    ) -> Result<(Vec<TypeRef>, Vec<HirExpr>), ErrorGuaranteed> {
        let (src_name, dst_name) = match intrinsic {
            Intrinsic::FbitToBits(width) => (width.builtin(), bit_container(width, span)),
            Intrinsic::BitsToFbit(width) => (bit_container(width, span), width.builtin()),
            _ => lilac_error::bug!(span, "bitcast_out on a non-cast intrinsic"),
        };
        let src = self.builtin_ty(src_name);
        let dst = self.builtin_ty(dst_name);
        self.expect_arity(span, name, args, 1)?;
        let (got, hir) = self.check_expr(fctx, &args[0], Some(src));
        if !is_poison(got) && got.id != src {
            self.diags.emit(
                Diagnostic::error(
                    "E010",
                    format!(
                        "mismatched types: expected {}, found {}",
                        self.types.display(src),
                        self.types.display(got.id),
                    ),
                )
                .label(args[0].span(), true, "wrong operand type here".to_owned()),
            );
            return Err(ErrorGuaranteed::new());
        }
        Ok((
            vec![TypeRef {
                id: dst,
                is_ref: false,
            }],
            vec![hir],
        ))
    }

    fn mem_intrinsic(
        &mut self,
        fctx: &mut FuncCtx,
        intrinsic: Intrinsic,
        args: &[Expr],
        span: Span,
    ) -> Result<(Vec<TypeRef>, Vec<HirExpr>), ErrorGuaranteed> {
        let value = |id: TypeId| TypeRef { id, is_ref: false };
        match intrinsic {
            Intrinsic::LoadU8 => {
                self.expect_arity(span, "@load_u8", args, 2)?;
                let h0 = self.check_ref_bit8_arg(fctx, &args[0], span)?;
                let h1 = self.check_fixed_arg(fctx, &args[1], "@bit_native")?;
                Ok((vec![value(self.builtin_ty("@bit8"))], vec![h0, h1]))
            }
            Intrinsic::StoreU8 => {
                self.expect_arity(span, "@store_u8", args, 3)?;
                let h0 = self.check_ref_bit8_arg(fctx, &args[0], span)?;
                let h1 = self.check_fixed_arg(fctx, &args[1], "@bit_native")?;
                let h2 = self.check_fixed_arg(fctx, &args[2], "@bit8")?;
                Ok((Vec::new(), vec![h0, h1, h2]))
            }
            _ => {
                self.expect_arity(span, "@memcpy", args, 3)?;
                let h0 = self.check_ref_bit8_arg(fctx, &args[0], span)?;
                let h1 = self.check_ref_bit8_arg(fctx, &args[1], span)?;
                let h2 = self.check_fixed_arg(fctx, &args[2], "@bit_native")?;
                Ok((Vec::new(), vec![h0, h1, h2]))
            }
        }
    }

    fn buffer_intrinsic(
        &mut self,
        fctx: &mut FuncCtx,
        intrinsic: Intrinsic,
        args: &[Expr],
        span: Span,
    ) -> Result<(Vec<TypeRef>, Vec<HirExpr>), ErrorGuaranteed> {
        let value = |id: TypeId| TypeRef { id, is_ref: false };
        match intrinsic {
            Intrinsic::ByteBufferPtr => {
                self.expect_arity(span, "@byte_buffer_ptr", args, 1)?;
                let h0 = self.check_fixed_arg(fctx, &args[0], "@byte_buffer")?;
                Ok((
                    vec![TypeRef {
                        id: self.builtin_ty("@bit8"),
                        is_ref: true,
                    }],
                    vec![h0],
                ))
            }
            Intrinsic::ByteBufferLen => {
                self.expect_arity(span, "@byte_buffer_len", args, 1)?;
                let h0 = self.check_fixed_arg(fctx, &args[0], "@byte_buffer")?;
                Ok((vec![value(self.builtin_ty("@bit_native"))], vec![h0]))
            }
            Intrinsic::ByteBufferFrom => {
                self.expect_arity(span, "@byte_buffer_from", args, 2)?;
                let h0 = self.check_ref_bit8_arg(fctx, &args[0], span)?;
                let h1 = self.check_fixed_arg(fctx, &args[1], "@bit_native")?;
                Ok((vec![value(self.builtin_ty("@byte_buffer"))], vec![h0, h1]))
            }
            Intrinsic::FormatArgsNew => {
                self.expect_arity(span, "@format_args_new", args, 0)?;
                Ok((vec![value(self.builtin_ty("@format_args"))], Vec::new()))
            }
            Intrinsic::FormatArgsPush => {
                self.expect_arity(span, "@format_args_push", args, 2)?;
                let (got, h0) = self.check_expr(fctx, &args[0], None);
                if !is_poison(got) && (!got.is_ref || got.id != self.builtin_ty("@format_args")) {
                    self.diags.emit(
                        Diagnostic::error(
                            "E010",
                            "mismatched types: expected ref @format_args".to_owned(),
                        )
                        .label(span, false, "expected `ref @format_args` here".to_owned())
                        .label(
                            args[0].span(),
                            true,
                            "needs an explicit `ref`".to_owned(),
                        ),
                    );
                    return Err(ErrorGuaranteed::new());
                }
                let h1 = self.check_fixed_arg(fctx, &args[1], "@byte_buffer")?;
                Ok((Vec::new(), vec![h0, h1]))
            }
            Intrinsic::FormatArgsCount => {
                self.expect_arity(span, "@format_args_count", args, 1)?;
                let h0 = self.check_fixed_arg(fctx, &args[0], "@format_args")?;
                Ok((vec![value(self.builtin_ty("@bit_native"))], vec![h0]))
            }
            _ => {
                self.expect_arity(span, "@format_args_piece", args, 2)?;
                let h0 = self.check_fixed_arg(fctx, &args[0], "@format_args")?;
                let h1 = self.check_fixed_arg(fctx, &args[1], "@bit_native")?;
                Ok((vec![value(self.builtin_ty("@byte_buffer"))], vec![h0, h1]))
            }
        }
    }

    fn check_ref_bit8_arg(
        &mut self,
        fctx: &mut FuncCtx,
        arg: &Expr,
        span: Span,
    ) -> Result<HirExpr, ErrorGuaranteed> {
        let (got, hir) = self.check_expr(fctx, arg, None);
        if is_poison(got) {
            return Err(ErrorGuaranteed::new());
        }
        if got.is_ref && got.id == self.builtin_ty("@bit8") {
            return Ok(hir);
        }
        Err(self.diags.emit(
            Diagnostic::error("E010", "mismatched types: expected ref @bit8".to_owned())
                .label(span, false, "expected `ref @bit8` here".to_owned())
                .label(arg.span(), true, "needs an explicit `ref`".to_owned()),
        ))
    }

    fn check_fixed_arg(
        &mut self,
        fctx: &mut FuncCtx,
        arg: &Expr,
        want: &str,
    ) -> Result<HirExpr, ErrorGuaranteed> {
        let want_id = self.builtin_ty(want);
        let (got, hir) = self.check_expr(fctx, arg, Some(want_id));
        if is_poison(got) {
            return Err(ErrorGuaranteed::new());
        }
        self.expect_type(got, want_id, arg.span(), arg.span())?;
        Ok(self.convert_if_needed(got, want_id, hir))
    }

    fn record_use(&mut self, span: Span, def: DefId) {
        if def != DefId::ERROR {
            self.uses.push(NameSite { span, def });
        }
    }

    /// Records a non-item semantic-token site (locals, parameters, type names,
    /// builtins). Dummy spans are skipped; the driver dedups and sorts.
    fn record_semantic(&mut self, span: Span, kind: SemanticKind) {
        if !span.is_dummy() {
            self.semantics.push(SemanticSite { span, kind });
        }
    }

    fn ident_type(
        &mut self,
        fctx: &mut FuncCtx,
        name: &str,
        span: Span,
    ) -> Result<(TypeRef, DefId), ErrorGuaranteed> {
        if let Some(ty) = fctx.locals.get(name) {
            return Ok((*ty, DefId::ERROR));
        }
        let item = match lookup_one(self.project, self.file, name, span, self.diags) {
            Ok(Some(item)) => item.clone(),
            Ok(None) => {
                return Err(self.diags.emit(
                    Diagnostic::error("E021", format!("unresolved name `{name}`")).label(
                        span,
                        true,
                        "not a local or item".to_owned(),
                    ),
                ));
            }
            Err(()) => return Err(ErrorGuaranteed::new()),
        };
        let kind = item.kind.clone();
        match kind {
            ResKind::Func {
                generic,
                generic_params,
                ..
            } => {
                if generic {
                    Err(self.emit_generic_arity(span, name, generic_params.len(), 0))
                } else {
                    let Some(sig) = self.func_sig(item.id) else {
                        lilac_error::bug!(span, "missing signature for function value");
                    };
                    let id = self
                        .types
                        .intern_func(sig.params.clone(), sig.returns.clone());
                    Ok((TypeRef { id, is_ref: false }, item.id))
                }
            }
            ResKind::Hook { .. } => Err(self.diags.emit(
                Diagnostic::error("E021", format!("cannot use function `{name}` as a value"))
                    .label(span, true, "hooks are not values".to_owned()),
            )),
            ResKind::Const => {
                let ty = self.const_type(&item, name, span)?;
                Ok((ty, item.id))
            }
            ResKind::Object { .. } | ResKind::Enum { .. } => {
                Err(self.diags.emit(
                    Diagnostic::error("E021", format!("cannot use type `{name}` as a value"))
                        .label(span, true, "name a variable instead".to_owned()),
                ))
            }
            ResKind::Field => Err(self.diags.emit(
                Diagnostic::error("E021", format!("unresolved name `{name}`")).label(
                    span,
                    true,
                    "fields need a receiver".to_owned(),
                ),
            )),
        }
    }

    fn const_type(
        &mut self,
        item: &ResItem,
        name: &str,
        span: Span,
    ) -> Result<TypeRef, ErrorGuaranteed> {
        // Match the const's AST item by identity/namespace (never by name alone:
        // two namespaces may declare the same const name with different types),
        // and resolve its declared type in the file that declares it.
        let (ty, file) = match find_item(self.program, self.project, item.id) {
            Some((ast, _, _)) => match &ast.kind {
                ItemKind::Const { ty, .. } => (ty.clone(), ast.span.file),
                _ => {
                    return Err(self.diags.emit(
                        Diagnostic::error("E021", format!("unresolved name `{name}`")).label(
                            span,
                            true,
                            "not a local or item".to_owned(),
                        ),
                    ));
                }
            },
            None => {
                return Err(self.diags.emit(
                    Diagnostic::error("E021", format!("unresolved name `{name}`")).label(
                        span,
                        true,
                        "not a local or item".to_owned(),
                    ),
                ));
            }
        };
        let saved = self.file;
        self.file = file;
        let resolved = self.resolve_ty(&ty);
        self.file = saved;
        Ok(resolved)
    }

    fn tag_type(
        &mut self,
        tag: &str,
        span: Span,
        expected: Option<TypeId>,
    ) -> Result<TypeRef, ErrorGuaranteed> {
        let Some(want) = expected else {
            return Err(self.diags.emit(
                Diagnostic::error("E024", "tag literal needs an expected enum type").label(
                    span,
                    true,
                    "annotate the binding".to_owned(),
                ),
            ));
        };
        if want == TypeId::ERROR {
            return Ok(TypeRef {
                id: TypeId::ERROR,
                is_ref: false,
            });
        }
        let payload = self.check_tag_pattern(want, tag, span)?;
        if payload.is_some() {
            return Err(self.diags.emit(
                Diagnostic::error(
                    "E022",
                    format!("variant `.{tag}` needs a payload; write `.{tag}(...)`"),
                )
                .label(span, true, "payload missing here".to_owned()),
            ));
        }
        Ok(TypeRef {
            id: want,
            is_ref: false,
        })
    }

    /// Validates `.Tag` against an enum type: returns the variant's
    /// payload type (`None` for payload-less variants).
    fn check_tag_pattern(
        &mut self,
        scrut_ty: TypeId,
        variant: &str,
        span: Span,
    ) -> Result<Option<TypeRef>, ErrorGuaranteed> {
        let lilac_ty::TypeKind::Object(def) = self.types.kind(scrut_ty) else {
            return Err(self.diags.emit(
                Diagnostic::error("E010", format!("tag `.{variant}` needs an enum type")).label(
                    span,
                    true,
                    "expected a tag here".to_owned(),
                ),
            ));
        };
        let payload_ty =
            match self.find_by_def(*def) {
                Some(ResKind::Enum { variants }) => {
                    let Some(found) = variants.iter().find(|v| v.name == variant) else {
                        return Err(self.diags.emit(
                            Diagnostic::error("E010", format!("no variant `.{variant}` here"))
                                .label(span, true, "unknown tag for this enum".to_owned()),
                        ));
                    };
                    found.payload.clone()
                }
                _ => {
                    return Err(self.diags.emit(
                        Diagnostic::error("E010", format!("tag `.{variant}` needs an enum type"))
                            .label(span, true, "expected a tag here".to_owned()),
                    ));
                }
            };
        match payload_ty {
            None => Ok(None),
            Some(ty) => Ok(Some(self.resolve_ty(&ty))),
        }
    }
    /// Checks a branch condition (`if`/`loop`): any `@bitN` tests non-zero.
    fn check_cond(
        &mut self,
        fctx: &mut FuncCtx,
        cond: &Expr,
        what: &str,
        span: Span,
    ) -> (TypeRef, HirExpr) {
        let (cond_ty, cond_hir) = self.check_test(fctx, cond);
        if !is_poison(cond_ty) && !is_bit(self.types, cond_ty.id) {
            self.diags.emit(
                Diagnostic::error(
                    "E010",
                    format!(
                        "`{what}` condition must be a bit container, found {}",
                        self.types.display(cond_ty.id),
                    ),
                )
                .label(span, true, "not a bit container".to_owned()),
            );
        }
        (cond_ty, cond_hir)
    }

    /// Checks `if cond { } else { }`, with optional `-> capture` desugar.
    fn check_if(
        &mut self,
        fctx: &mut FuncCtx,
        cond: &Expr,
        capture: Option<&lilac_ast::Param>,
        then: &lilac_ast::Block,
        els: Option<&lilac_ast::Block>,
        span: Span,
    ) {
        if let Some(cap) = capture {
            self.check_if_capture(fctx, cond, cap, then, els, span);
            return;
        }
        let (cond_ty, cond_hir) = self.check_test(fctx, cond);
        if !is_poison(cond_ty) && !is_bit(self.types, cond_ty.id) {
            self.diags.emit(
                Diagnostic::error(
                    "E010",
                    format!(
                        "`if` condition must be a bit container, found {}",
                        self.types.display(cond_ty.id),
                    ),
                )
                .label(cond.span(), true, "not a bit container".to_owned()),
            );
        }
        let then_hir = self.check_block(fctx, then);
        let els_hir = els.map(|block| self.check_block(fctx, block));
        let els_hir = els_hir.unwrap_or(HirBlock {
            stmts: Vec::new(),
            span,
        });
        push_stmt(
            fctx,
            HirStmt::If {
                cond: cond_hir,
                then: then_hir,
                els: els_hir,
                out: None,
                span,
            },
            span,
        );
    }

    /// Desugars `if CALL -> name: T { }` into a hidden multi-`let`, the
    /// capture binding, and a plain `if` over the first out-slot.
    fn check_if_capture(
        &mut self,
        fctx: &mut FuncCtx,
        cond: &Expr,
        cap: &lilac_ast::Param,
        then: &lilac_ast::Block,
        els: Option<&lilac_ast::Block>,
        span: Span,
    ) {
        let Some(call) = self.capture_call(fctx, cond, span) else {
            return;
        };
        let outs = &call.outs;
        if !is_poison(outs[0]) && !is_bit(self.types, outs[0].id) {
            self.diags.emit(
                Diagnostic::error(
                    "E010",
                    format!(
                        "`if` capture tests `{}`, needs a bit container",
                        self.types.display(outs[0].id),
                    ),
                )
                .label(call.call_span, true, "not a bit container".to_owned()),
            );
        }
        let Some((cond_hir, hidden_names, saved)) = self.capture_bind(fctx, cap, call, span) else {
            return;
        };
        let then_hir = self.check_block(fctx, then);
        match saved {
            Some(previous) => {
                fctx.locals.insert(cap.name.clone(), previous);
            }
            None => {
                fctx.locals.remove(&cap.name);
            }
        }
        for name in &hidden_names {
            fctx.locals.remove(name);
        }
        let els_hir = els.map(|block| self.check_block(fctx, block));
        let els_hir = els_hir.unwrap_or(HirBlock {
            stmts: Vec::new(),
            span,
        });
        push_stmt(
            fctx,
            HirStmt::If {
                cond: cond_hir,
                then: then_hir,
                els: els_hir,
                out: None,
                span,
            },
            span,
        );
    }

    /// Checks the capture condition call: returns its out-slot types, value
    /// HIR, call span, and callee definition (for slot names).
    fn capture_call(
        &mut self,
        fctx: &mut FuncCtx,
        cond: &Expr,
        span: Span,
    ) -> Option<CapturedCall> {
        let Expr::Call {
            callee,
            generic_args,
            args,
            span: call_span,
        } = cond
        else {
            self.diags.emit(
                Diagnostic::error("E022", "`if` capture needs a call condition").label(
                    span,
                    true,
                    "write `if f() -> name: Type`".to_owned(),
                ),
            );
            push_stmt(fctx, empty_if(span), span);
            return None;
        };
        let (outs, value_hir, callee_def) =
            match self.check_call(fctx, callee, generic_args, args, *call_span, None) {
                Ok(CallOutcome::Call(call)) => {
                    let def = match &call.callee {
                        HirCallee::Item(def) | HirCallee::Hook { def, .. } => Some(*def),
                        HirCallee::Local { .. } | HirCallee::Intrinsic { .. } => None,
                    };
                    let hir = HirExpr {
                        ty: call.outs.first().copied().unwrap_or(TypeRef {
                            id: TypeId::ERROR,
                            is_ref: false,
                        }),
                        span: *call_span,
                        kind: HirExprKind::Call {
                            callee: call.callee,
                            args: call.args,
                        },
                    };
                    (call.outs, hir, def)
                }
                Ok(CallOutcome::Inline(hir, ty)) => (vec![ty], hir, None),
                Ok(CallOutcome::CompilerError(_)) | Err(_) => {
                    self.diags.emit(
                        Diagnostic::error("E022", "cannot capture from this call").label(
                            *call_span,
                            true,
                            "capture needs a multi-return call".to_owned(),
                        ),
                    );
                    push_stmt(fctx, empty_if(span), span);
                    return None;
                }
            };
        if outs.is_empty() {
            self.diags.emit(
                Diagnostic::error("E022", "cannot capture from a call with no returns").label(
                    *call_span,
                    true,
                    "nothing to capture".to_owned(),
                ),
            );
            push_stmt(fctx, empty_if(span), span);
            return None;
        }
        Some(CapturedCall {
            outs,
            value_hir,
            call_span: *call_span,
            callee_def,
        })
    }

    /// Binds the capture: hidden multi-`let` for every out-slot plus the
    /// named capture `let`. Returns the condition expression, hidden names,
    /// and any shadowed local to restore.
    fn capture_bind(
        &mut self,
        fctx: &mut FuncCtx,
        cap: &lilac_ast::Param,
        call: CapturedCall,
        span: Span,
    ) -> Option<(HirExpr, Vec<String>, Option<TypeRef>)> {
        let outs = call.outs;
        let value_hir = call.value_hir;
        let call_span = call.call_span;
        let callee_def = call.callee_def;
        if cap.name.is_empty() {
            self.diags
                .emit(Diagnostic::error("E022", "capture needs a name").label(
                    cap.span,
                    true,
                    "write `-> name: Type`".to_owned(),
                ));
            push_stmt(fctx, empty_if(span), span);
            return None;
        }
        let slot_names: Vec<String> = match callee_def
            .and_then(|def| find_item(self.program, self.project, def))
        {
            Some((ast_item, _, _)) => match &ast_item.kind {
                ItemKind::Func(decl) => decl.returns.iter().map(|slot| slot.name.clone()).collect(),
                _ => Vec::new(),
            },
            None => Vec::new(),
        };
        let Some(pos) = slot_names.iter().position(|slot| slot == &cap.name) else {
            self.diags.emit(
                Diagnostic::error(
                    "E022",
                    format!("no out-slot named `{}` to capture", cap.name),
                )
                .label(cap.span, true, "unknown slot".to_owned()),
            );
            push_stmt(fctx, empty_if(span), span);
            return None;
        };
        let declared = self.resolve_ty(&cap.ty);
        if !is_poison(outs[pos]) && !is_poison(declared) {
            let _ = self.expect_type(outs[pos], declared.id, cap.span, call_span);
        }
        let mut hidden_names = Vec::with_capacity(outs.len());
        let mut hidden = Vec::with_capacity(outs.len());
        for (index, out_ty) in outs.iter().enumerate() {
            let name = fresh_local(fctx, &format!("cap{index}"));
            fctx.locals.insert(name.clone(), *out_ty);
            hidden_names.push(name.clone());
            hidden.push(HirParam {
                name,
                ty: *out_ty,
                span: call_span,
            });
        }
        push_stmt(
            fctx,
            HirStmt::MultiLet {
                bindings: hidden,
                value: value_hir,
                span,
            },
            span,
        );
        let saved = fctx.locals.insert(cap.name.clone(), declared);
        let capture_ident = HirExpr {
            ty: outs[pos],
            span: cap.span,
            kind: HirExprKind::Ident {
                name: hidden_names[pos].clone(),
                def: DefId::ERROR,
            },
        };
        let capture_value = self.convert_if_needed(outs[pos], declared.id, capture_ident);
        push_stmt(
            fctx,
            HirStmt::Let {
                name: cap.name.clone(),
                ty: declared,
                value: capture_value,
                span: cap.span,
            },
            cap.span,
        );
        let cond_hir = HirExpr {
            ty: outs[0],
            span: call_span,
            kind: HirExprKind::Ident {
                name: hidden_names[0].clone(),
                def: DefId::ERROR,
            },
        };
        Some((cond_hir, hidden_names, saved))
    }
    fn check_loop(
        &mut self,
        fctx: &mut FuncCtx,
        cond: Option<&Expr>,
        body: &lilac_ast::Block,
        els: Option<&lilac_ast::Block>,
        span: Span,
    ) {
        fctx.loop_depth += 1;
        let body_hir = self.check_block(fctx, body);
        match cond {
            None => {
                push_stmt(
                    fctx,
                    HirStmt::Loop {
                        body: body_hir,
                        span,
                    },
                    span,
                );
                if let Some(els) = els {
                    let els_hir = self.check_block(fctx, els);
                    for stmt in els_hir.stmts {
                        push_stmt(fctx, stmt, span);
                    }
                }
            }
            Some(cond) => {
                let (_, cond_hir) = self.check_cond(fctx, cond, "loop", cond.span());
                let els_hir = els.map(|block| self.check_block(fctx, block));
                let mut els_stmts = els_hir.map_or(Vec::new(), |block| block.stmts);
                els_stmts.push(HirStmt::Break { span });
                push_stmt(
                    fctx,
                    HirStmt::Loop {
                        body: HirBlock {
                            stmts: vec![HirStmt::If {
                                cond: cond_hir,
                                then: body_hir,
                                els: HirBlock {
                                    stmts: els_stmts,
                                    span,
                                },
                                out: None,
                                span,
                            }],
                            span,
                        },
                        span,
                    },
                    span,
                );
            }
        }
        fctx.loop_depth -= 1;
    }

    /// Checks `match` statements (arms run blocks; no values produced).
    fn check_match(
        &mut self,
        fctx: &mut FuncCtx,
        scrutinee: &Expr,
        arms: &[lilac_ast::MatchArm],
        els: Option<&lilac_ast::Block>,
        span: Span,
    ) {
        let (scrut_ty, scrut_hir) = self.check_test(fctx, scrutinee);
        let mut hir_arms = Vec::with_capacity(arms.len());
        for arm in arms {
            hir_arms.push(self.check_match_arm(fctx, arm, scrut_ty.id));
        }
        let els_hir = els.map(|block| self.check_block(fctx, block));
        let els_hir = els_hir.unwrap_or(HirBlock {
            stmts: Vec::new(),
            span,
        });
        push_stmt(
            fctx,
            HirStmt::Match {
                scrutinee: scrut_hir,
                arms: hir_arms,
                els: els_hir,
                out: None,
                span,
            },
            span,
        );
    }

    /// Checks one statement-match arm (captures bind payloads in the body).
    fn check_match_arm(
        &mut self,
        fctx: &mut FuncCtx,
        arm: &lilac_ast::MatchArm,
        scrut_ty: TypeId,
    ) -> HirArm {
        let (pattern, capture) = self.check_arm_pattern(arm, scrut_ty);
        let saved = capture.as_ref().map(|param| {
            (
                param.name.clone(),
                fctx.locals.insert(param.name.clone(), param.ty),
            )
        });
        let body = self.check_arm_body(fctx, &arm.body, arm.span);
        match saved {
            Some((name, Some(previous))) => {
                fctx.locals.insert(name, previous);
            }
            Some((name, None)) => {
                fctx.locals.remove(&name);
            }
            None => {}
        }
        HirArm {
            pattern,
            capture,
            body,
        }
    }

    /// Validates one arm pattern plus its capture; errors yield a poison
    /// pattern so the body still checks for more diagnostics.
    fn check_arm_pattern(
        &mut self,
        arm: &lilac_ast::MatchArm,
        scrut_ty: TypeId,
    ) -> (HirPattern, Option<HirParam>) {
        let poison_tag = |name: &str| {
            (
                HirPattern::Tag {
                    name: name.to_owned(),
                },
                None,
            )
        };
        match &arm.pattern {
            lilac_ast::MatchPattern::Tag(name) => {
                let Ok(payload) = self.check_tag_pattern(scrut_ty, name, arm.span) else {
                    return poison_tag(name);
                };
                let Some(cap) = &arm.capture else {
                    return (HirPattern::Tag { name: name.clone() }, None);
                };
                let declared = self.resolve_ty(&cap.ty);
                if let Some(payload_ty) = payload {
                    if !is_poison(payload_ty)
                        && !is_poison(declared)
                        && payload_ty.id != declared.id
                    {
                        self.diags.emit(
                            Diagnostic::error(
                                "E010",
                                format!(
                                    "mismatched types: capture holds {}, declared {}",
                                    self.types.display(payload_ty.id),
                                    self.types.display(declared.id),
                                ),
                            )
                            .label(
                                cap.span,
                                true,
                                "capture type here".to_owned(),
                            ),
                        );
                    }
                } else {
                    self.diags.emit(
                        Diagnostic::error(
                            "E022",
                            format!("variant `.{name}` carries no payload to capture"),
                        )
                        .label(
                            cap.span,
                            true,
                            "payload-less variant".to_owned(),
                        ),
                    );
                }
                (
                    HirPattern::Tag { name: name.clone() },
                    Some(HirParam {
                        name: cap.name.clone(),
                        ty: declared,
                        span: cap.span,
                    }),
                )
            }
            lilac_ast::MatchPattern::Const(bytes) => {
                if scrut_ty != TypeId::ERROR && !is_bit(self.types, scrut_ty) {
                    self.diags.emit(
                        Diagnostic::error(
                            "E010",
                            format!(
                                "byte patterns need a bit container scrutinee, found {}",
                                self.types.display(scrut_ty),
                            ),
                        )
                        .label(
                            arm.span,
                            true,
                            "not a bit container".to_owned(),
                        ),
                    );
                }
                if arm.capture.is_some() {
                    self.diags.emit(
                        Diagnostic::error("E022", "byte patterns capture nothing").label(
                            arm.span,
                            true,
                            "captures need `.Tag` patterns".to_owned(),
                        ),
                    );
                }
                (
                    HirPattern::Const {
                        bytes: bytes.clone(),
                    },
                    None,
                )
            }
        }
    }

    /// Checks a match-arm body (block or single `=>` statement).
    fn check_arm_body(
        &mut self,
        fctx: &mut FuncCtx,
        body: &lilac_ast::MatchArmBody,
        span: Span,
    ) -> HirBlock {
        fctx.blocks.push(Vec::new());
        match body {
            lilac_ast::MatchArmBody::Block(block) => {
                for stmt in &block.stmts {
                    self.check_stmt(fctx, stmt);
                }
            }
            lilac_ast::MatchArmBody::Inline(stmt) => self.check_stmt(fctx, stmt),
        }
        let Some(stmts) = fctx.blocks.pop() else {
            lilac_error::bug!(span, "block stack underflow");
        };
        HirBlock { stmts, span }
    }

    /// Checks `.Some(0x1)` constructions: the expected type names the
    /// enum, payloads check against the variant's payload type.
    /// Construction desugars to a value (no call emitted).
    fn check_tag_call(
        &mut self,
        fctx: &mut FuncCtx,
        name: &str,
        args: &[Expr],
        span: Span,
        expected: Option<TypeId>,
        tag_span: Span,
    ) -> Result<CallOutcome, ErrorGuaranteed> {
        let poison = HirExpr {
            ty: TypeRef {
                id: TypeId::ERROR,
                is_ref: false,
            },
            span,
            kind: HirExprKind::Poison,
        };
        let Some(want) = expected else {
            self.diags.emit(
                Diagnostic::error("E024", "tag construction needs an expected type").label(
                    tag_span,
                    true,
                    "annotate the binding".to_owned(),
                ),
            );
            return Ok(CallOutcome::Inline(poison.clone(), poison.ty));
        };
        if want == TypeId::ERROR {
            return Err(ErrorGuaranteed::new());
        }
        let Ok(payload) = self.check_tag_pattern(want, name, tag_span) else {
            return Ok(CallOutcome::Inline(poison.clone(), poison.ty));
        };
        let def = match self.types.kind(want) {
            lilac_ty::TypeKind::Object(def) => *def,
            _ => {
                return Ok(CallOutcome::Inline(poison.clone(), poison.ty));
            }
        };
        let ty = TypeRef {
            id: want,
            is_ref: false,
        };
        let tag_len = 1u32.saturating_add(u32::try_from(name.len()).unwrap_or(u32::MAX));
        let tag = Span::new(span.lo.0, span.lo.0.saturating_add(tag_len), span.file);
        if let Some(payload_ty) = payload {
            if args.len() != 1 {
                self.diags.emit(
                    Diagnostic::error(
                        "E022",
                        format!("variant `.{name}` needs exactly one payload value"),
                    )
                    .label(span, true, "arity mismatch here".to_owned()),
                );
                return Ok(CallOutcome::Inline(poison.clone(), poison.ty));
            }
            let (got, value_hir) = self.check_expr(fctx, &args[0], Some(payload_ty.id));
            if !is_poison(got) {
                let _ = self.expect_type(got, payload_ty.id, args[0].span(), args[0].span());
            }
            let value_hir = self.convert_if_needed(got, payload_ty.id, value_hir);
            self.record_use(tag, def);
            Ok(CallOutcome::Inline(
                HirExpr {
                    ty,
                    span,
                    kind: HirExprKind::Variant {
                        def,
                        variant: name.to_owned(),
                        payload: Some(Box::new(value_hir)),
                    },
                },
                ty,
            ))
        } else {
            if !args.is_empty() {
                self.diags.emit(
                    Diagnostic::error("E022", format!("variant `.{name}` takes no payload")).label(
                        span,
                        true,
                        "arity mismatch here".to_owned(),
                    ),
                );
                return Ok(CallOutcome::Inline(poison.clone(), poison.ty));
            }
            self.record_use(tag, def);
            Ok(CallOutcome::Inline(
                HirExpr {
                    ty,
                    span,
                    kind: HirExprKind::Variant {
                        def,
                        variant: name.to_owned(),
                        payload: None,
                    },
                },
                ty,
            ))
        }
    }

    /// Desugars `if` values to a branch-join over a temp.
    fn check_if_value(
        &mut self,
        fctx: &mut FuncCtx,
        cond: &Expr,
        then: &Expr,
        els: &Expr,
        span: Span,
        expected: Option<TypeId>,
    ) -> (TypeRef, HirExpr) {
        let temp = self.fresh_temp();
        let poison = || {
            (
                TypeRef {
                    id: TypeId::ERROR,
                    is_ref: false,
                },
                HirExpr {
                    ty: TypeRef {
                        id: TypeId::ERROR,
                        is_ref: false,
                    },
                    span,
                    kind: HirExprKind::Temp(temp),
                },
            )
        };
        let (_, cond_hir) = self.check_cond(fctx, cond, "if", cond.span());
        let (then_ty, then_hir, then_pre) = self.check_nested(fctx, then, expected);
        let els_expected = expected.or(if is_poison(then_ty) {
            None
        } else {
            Some(then_ty.id)
        });
        let (els_ty, els_hir, els_pre) = self.check_nested(fctx, els, els_expected);
        let result_ty = match expected {
            Some(want) => want,
            None => {
                if !is_poison(then_ty) {
                    then_ty.id
                } else if !is_poison(els_ty) {
                    els_ty.id
                } else {
                    return poison();
                }
            }
        };
        if expected.is_none()
            && !is_poison(then_ty)
            && !is_poison(els_ty)
            && then_ty.id != els_ty.id
        {
            self.diags.emit(
                Diagnostic::error(
                    "E010",
                    format!(
                        "mismatched types: `if` branches produce {} and {}",
                        self.types.display(then_ty.id),
                        self.types.display(els_ty.id),
                    ),
                )
                .label(span, true, "branches disagree here".to_owned()),
            );
            return poison();
        }
        let _ = self.expect_type(then_ty, result_ty, span, then.span());
        let _ = self.expect_type(els_ty, result_ty, span, els.span());
        let then_hir = self.convert_if_needed(then_ty, result_ty, then_hir);
        let els_hir = self.convert_if_needed(els_ty, result_ty, els_hir);
        let mut then_stmts = then_pre;
        then_stmts.push(assign_temp(temp, result_ty, then_hir));
        let mut els_stmts = els_pre;
        els_stmts.push(assign_temp(temp, result_ty, els_hir));
        push_stmt(
            fctx,
            HirStmt::If {
                cond: cond_hir,
                then: HirBlock {
                    stmts: then_stmts,
                    span: then.span(),
                },
                els: HirBlock {
                    stmts: els_stmts,
                    span: els.span(),
                },
                out: Some(temp),
                span,
            },
            span,
        );
        (
            TypeRef {
                id: result_ty,
                is_ref: false,
            },
            HirExpr {
                ty: TypeRef {
                    id: result_ty,
                    is_ref: false,
                },
                span,
                kind: HirExprKind::Temp(temp),
            },
        )
    }

    /// Checks one match-value arm: validates the pattern, binds the capture,
    /// and joins the arm value into the temp. Returns the arm plus its value
    /// type and whether it failed.
    fn check_value_arm(
        &mut self,
        fctx: &mut FuncCtx,
        arm: &lilac_ast::MatchArm,
        scrut_ty: TypeId,
        expected: Option<TypeId>,
        temp: TempId,
        assign_ty: Option<TypeId>,
    ) -> (HirArm, TypeRef, bool) {
        let (pattern, capture) = self.check_arm_pattern(arm, scrut_ty);
        let saved = capture.as_ref().map(|param| {
            (
                param.name.clone(),
                fctx.locals.insert(param.name.clone(), param.ty),
            )
        });
        let mut shape_failed = false;
        let (arm_ty, arm_hir, arm_pre) = if let lilac_ast::MatchArmBody::Inline(stmt) = &arm.body {
            if let Stmt::ExprStmt(value) = stmt.as_ref() {
                self.check_nested(fctx, value, expected)
            } else {
                self.diags.emit(
                    Diagnostic::error("E010", "match arm does not produce a value").label(
                        arm.span,
                        true,
                        "no value here".to_owned(),
                    ),
                );
                shape_failed = true;
                (
                    TypeRef {
                        id: TypeId::ERROR,
                        is_ref: false,
                    },
                    poison_expr(arm.span),
                    Vec::new(),
                )
            }
        } else {
            self.diags.emit(
                Diagnostic::error("E010", "match arm block cannot produce a value").label(
                    arm.span,
                    true,
                    "no value here".to_owned(),
                ),
            );
            shape_failed = true;
            (
                TypeRef {
                    id: TypeId::ERROR,
                    is_ref: false,
                },
                poison_expr(arm.span),
                Vec::new(),
            )
        };
        match saved {
            Some((name, Some(previous))) => {
                fctx.locals.insert(name, previous);
            }
            Some((name, None)) => {
                fctx.locals.remove(&name);
            }
            None => {}
        }
        let mut body_stmts = arm_pre;
        let arm_hir = if let Some(want) = expected {
            let _ = self.expect_type(arm_ty, want, arm.span, arm.span);
            self.convert_if_needed(arm_ty, want, arm_hir)
        } else {
            arm_hir
        };
        body_stmts.push(assign_temp(
            temp,
            assign_ty.unwrap_or(TypeId::ERROR),
            arm_hir,
        ));
        let hir_arm = HirArm {
            pattern,
            capture,
            body: HirBlock {
                stmts: body_stmts,
                span: arm.span,
            },
        };
        (hir_arm, arm_ty, shape_failed)
    }

    /// Desugars `match` values to tag-test chains over a temp.
    fn check_match_value(
        &mut self,
        fctx: &mut FuncCtx,
        scrutinee: &Expr,
        arms: &[lilac_ast::MatchArm],
        els: &Expr,
        span: Span,
        expected: Option<TypeId>,
    ) -> (TypeRef, HirExpr) {
        let temp = self.fresh_temp();
        let poison = || {
            (
                TypeRef {
                    id: TypeId::ERROR,
                    is_ref: false,
                },
                HirExpr {
                    ty: TypeRef {
                        id: TypeId::ERROR,
                        is_ref: false,
                    },
                    span,
                    kind: HirExprKind::Temp(temp),
                },
            )
        };
        let (scrut_ty, scrut_hir) = self.check_test(fctx, scrutinee);
        let mut hir_arms = Vec::with_capacity(arms.len());
        let mut first_ty = None;
        let mut failed = false;
        for arm in arms {
            let (hir_arm, arm_ty, arm_failed) =
                self.check_value_arm(fctx, arm, scrut_ty.id, expected, temp, first_ty);
            if arm_failed {
                failed = true;
            }
            if first_ty.is_none() && !is_poison(arm_ty) {
                first_ty = Some(arm_ty.id);
            }
            hir_arms.push(hir_arm);
        }
        let els_expected = expected.or(first_ty);
        let (els_ty, els_hir, els_pre) = self.check_nested(fctx, els, els_expected);
        let Ok(result_ty) = self.match_result_ty(expected, first_ty, els_ty, failed, span) else {
            return poison();
        };
        let els_hir = if let Some(want) = expected {
            let _ = self.expect_type(els_ty, want, span, els.span());
            self.convert_if_needed(els_ty, want, els_hir)
        } else {
            els_hir
        };
        let mut els_stmts = els_pre;
        els_stmts.push(HirStmt::Assign {
            target: HirExpr {
                ty: TypeRef {
                    id: result_ty,
                    is_ref: false,
                },
                span: els.span(),
                kind: HirExprKind::Temp(temp),
            },
            value: els_hir,
            span: els.span(),
        });
        push_stmt(
            fctx,
            HirStmt::Match {
                scrutinee: scrut_hir,
                arms: hir_arms,
                els: HirBlock {
                    stmts: els_stmts,
                    span: els.span(),
                },
                out: Some(temp),
                span,
            },
            span,
        );
        (
            TypeRef {
                id: result_ty,
                is_ref: false,
            },
            HirExpr {
                ty: TypeRef {
                    id: result_ty,
                    is_ref: false,
                },
                span,
                kind: HirExprKind::Temp(temp),
            },
        )
    }

    /// Resolves a match-value's result type: the expected type wins, else the
    /// first clean arm type, else the `else` type. Disagreeing arms stay `E010`.
    fn match_result_ty(
        &mut self,
        expected: Option<TypeId>,
        first_ty: Option<TypeId>,
        els_ty: TypeRef,
        failed: bool,
        span: Span,
    ) -> Result<TypeId, ErrorGuaranteed> {
        let result_ty = match expected {
            Some(want) => want,
            None => {
                if let Some(first) = first_ty {
                    first
                } else if !is_poison(els_ty) {
                    els_ty.id
                } else {
                    return Err(ErrorGuaranteed::new());
                }
            }
        };
        if expected.is_none() && !failed {
            for ty in [first_ty, Some(els_ty.id)].into_iter().flatten() {
                if ty != result_ty {
                    self.diags.emit(
                        Diagnostic::error("E010", "match arms produce different types").label(
                            span,
                            true,
                            "arms disagree here".to_owned(),
                        ),
                    );
                    return Err(ErrorGuaranteed::new());
                }
            }
        }
        Ok(result_ty)
    }

    /// Desugars `and`/`or` to short-circuit joins over a temp (never calls,
    /// never conversions: both sides must be `@bit8`).
    fn check_logic_op(
        &mut self,
        fctx: &mut FuncCtx,
        lhs: &Expr,
        op: BinOp,
        rhs: &Expr,
        span: Span,
    ) -> (TypeRef, HirExpr) {
        let bit8 = self.builtin_ty("@bit8");
        let value_ty = TypeRef {
            id: bit8,
            is_ref: false,
        };
        let poison = (
            TypeRef {
                id: TypeId::ERROR,
                is_ref: false,
            },
            poison_expr(span),
        );
        let (left_ty, left_hir) = self.check_test(fctx, lhs);
        if !is_poison(left_ty) && left_ty.id != bit8 {
            self.diags.emit(
                Diagnostic::error(
                    "E010",
                    format!(
                        "mismatched types: `{}` needs `@bit8`, found {}",
                        if op == BinOp::And { "and" } else { "or" },
                        self.types.display(left_ty.id),
                    ),
                )
                .label(lhs.span(), true, "not `@bit8`".to_owned()),
            );
            return poison;
        }
        let (right_ty, right_hir, right_pre) = self.check_nested(fctx, rhs, Some(bit8));
        if !is_poison(right_ty) && right_ty.id != bit8 {
            self.diags.emit(
                Diagnostic::error(
                    "E010",
                    format!(
                        "mismatched types: `{}` needs `@bit8`, found {}",
                        if op == BinOp::And { "and" } else { "or" },
                        self.types.display(right_ty.id),
                    ),
                )
                .label(rhs.span(), true, "not `@bit8`".to_owned()),
            );
            return poison;
        }
        let temp = self.fresh_temp();
        let bit = |bytes: &str| HirExpr {
            ty: value_ty,
            span,
            kind: HirExprKind::Number {
                bytes: bytes.to_owned(),
            },
        };
        let (then_stmts, els_stmts) = if op == BinOp::And {
            let mut then_stmts = right_pre;
            then_stmts.push(assign_temp(temp, bit8, right_hir));
            (then_stmts, vec![assign_temp(temp, bit8, bit("0x0"))])
        } else {
            let mut els_stmts = right_pre;
            els_stmts.push(assign_temp(temp, bit8, right_hir));
            (vec![assign_temp(temp, bit8, bit("0x1"))], els_stmts)
        };
        push_stmt(
            fctx,
            HirStmt::If {
                cond: left_hir,
                then: HirBlock {
                    stmts: then_stmts,
                    span: lhs.span(),
                },
                els: HirBlock {
                    stmts: els_stmts,
                    span: rhs.span(),
                },
                out: Some(temp),
                span,
            },
            span,
        );
        (
            value_ty,
            HirExpr {
                ty: value_ty,
                span,
                kind: HirExprKind::Temp(temp),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::check_bodies;
    use lilac_error::Diags;
    use lilac_lex::Lexer;
    use lilac_parse::Parser;
    use lilac_span::SourceMap;
    use lilac_ty::Types;

    fn check_src(src: &str) -> Diags {
        check_src_native(src, 64)
    }

    /// Like [`check_src`], but with an explicit target native width so the
    /// `@bit_native` literal path can be exercised at 32 bits.
    fn check_src_native(src: &str, native_bits: u32) -> Diags {
        let mut sm = SourceMap::new();
        let file = sm.add_file("test.lilac", src);
        let (tokens, lex_errors) = Lexer::lex_all(src, file);
        let mut diags = Diags::new();
        for error in lex_errors {
            diags.emit(error);
        }
        let mut parser = Parser::new(tokens, file);
        let program = parser
            .parse_program(&mut diags)
            .expect("test source parses");
        let module = lilac_resolve::collect(&program, &mut diags);
        let project = lilac_resolve::ResolvedProject::single(module, file);
        let mut types = Types::new();
        let sigs = super::super::collect_sigs(&project, &program, &mut types, &mut diags);
        check_bodies(
            &program,
            &project,
            &sigs,
            &mut types,
            &mut diags,
            native_bits,
        );
        diags
    }

    fn has_code(diags: &Diags, code: &str) -> bool {
        diags.diagnostics().iter().any(|d| d.code == code)
    }

    /// The checked type and source bytes of the literal in `main`'s first
    /// statement: a bare literal statement takes no expected type, while
    /// `x: T = lit` takes `T`.
    fn literal_type_and_bytes(src: &str) -> (String, String) {
        let mut sm = SourceMap::new();
        let file = sm.add_file("test.lilac", src);
        let (tokens, lex_errors) = Lexer::lex_all(src, file);
        let mut diags = Diags::new();
        for error in lex_errors {
            diags.emit(error);
        }
        let mut parser = Parser::new(tokens, file);
        let program = parser
            .parse_program(&mut diags)
            .expect("test source parses");
        let module = lilac_resolve::collect(&program, &mut diags);
        let project = lilac_resolve::ResolvedProject::single(module, file);
        let mut types = Types::new();
        let sigs = super::super::collect_sigs(&project, &program, &mut types, &mut diags);
        let hir = check_bodies(&program, &project, &sigs, &mut types, &mut diags, 64);
        assert!(!diags.has_errors(), "{diags:?}");
        let main = hir.funcs.iter().find(|f| f.name == "main").expect("main");
        let value = match &main.body.stmts[0] {
            lilac_hir::HirStmt::ExprStmt(value) | lilac_hir::HirStmt::Let { value, .. } => value,
            other => panic!("expected a value statement, got {other:?}"),
        };
        let bytes = match &value.kind {
            lilac_hir::HirExprKind::Number { bytes } => bytes.clone(),
            other => panic!("expected a number literal, got {other:?}"),
        };
        (types.display(value.ty.id), bytes)
    }

    /// The checked type of the literal in `main`'s first statement.
    fn literal_type(src: &str) -> String {
        literal_type_and_bytes(src).0
    }

    #[test]
    fn integer_literal_types_by_smallest_fit() {
        for (src, want) in [
            ("0", "@bit8"),
            ("0xFF", "@bit8"),
            ("0x1FF", "@bit16"),
            ("65535", "@bit16"),
            ("65536", "@bit32"),
            ("0xFFFF_FFFF", "@bit32"),
            ("0x1_0000_0000", "@bit64"),
        ] {
            let source = format!("main: function () nothing {{\n    {src}\n}}\n");
            assert_eq!(literal_type(&source), want, "{src}");
        }
    }

    #[test]
    fn integer_literal_grows_to_expected_width() {
        assert_eq!(
            literal_type("main: function () nothing {\n    x: @bit32 = 0x1\n}\n"),
            "@bit32"
        );
        assert_eq!(
            literal_type("main: function () nothing {\n    x: @bit32 = 1\n}\n"),
            "@bit32"
        );
        assert_eq!(
            literal_type("main: function () nothing {\n    x: @bit8 = 0xFF\n}\n"),
            "@bit8"
        );
    }

    #[test]
    fn integer_literal_over_64_bits_is_e060() {
        let diags = check_src("main: function () nothing {\n    0x1_0000_0000_0000_0000\n}\n");
        assert!(has_code(&diags, "E060"), "{diags:?}");
        let diags =
            check_src("main: function () nothing {\n    x: @bit64 = 0x1_0000_0000_0000_0000\n}\n");
        assert!(has_code(&diags, "E060"), "{diags:?}");
    }

    #[test]
    fn integer_literal_overflow_of_expected_width_is_e060() {
        let diags = check_src("main: function () nothing {\n    x: @bit8 = 0x1FF\n}\n");
        assert!(has_code(&diags, "E060"), "{diags:?}");
    }

    #[test]
    fn bit_native_literal_uses_target_width() {
        // `@bit_native` is the target's pointer width, not always 64: a value
        // needing 33 bits fits a 64-bit target but overflows a 32-bit one.
        let src = "main: function () nothing {\n    x: @bit_native = 0x1_0000_0000\n}\n";
        let diags = check_src_native(src, 64);
        assert!(!diags.has_errors(), "64-bit: {diags:?}");
        let diags = check_src_native(src, 32);
        assert!(has_code(&diags, "E060"), "32-bit: {diags:?}");
        // 32 bits exactly fits either target.
        let src = "main: function () nothing {\n    x: @bit_native = 0xFFFF_FFFF\n}\n";
        let diags = check_src_native(src, 32);
        assert!(!diags.has_errors(), "32-bit: {diags:?}");
        // A folded literal is range-checked at the native width too.
        let src = "main: function () nothing {\n    x: @bit_native = 0xFFFF_FFFF + 0x1\n}\n";
        let diags = check_src_native(src, 32);
        assert!(has_code(&diags, "E060"), "32-bit fold: {diags:?}");
    }

    #[test]
    fn float_literal_defaults_to_fbit64() {
        for src in ["3.14", "1e3", "2E10", "1.5e-4"] {
            let source = format!("main: function () nothing {{\n    {src}\n}}\n");
            assert_eq!(literal_type(&source), "@fbit64", "{src}");
        }
    }

    #[test]
    fn float_literal_pins_to_expected_fbit() {
        assert_eq!(
            literal_type("main: function () nothing {\n    x: @fbit32 = 3.14\n}\n"),
            "@fbit32"
        );
        let diags = check_src("main: function () nothing {\n    x: @fbit32 = 3.14\n}\n");
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn float_literal_in_bit_slot_is_e010() {
        let diags = check_src("main: function () nothing {\n    x: @bit64 = 3.14\n}\n");
        assert!(has_code(&diags, "E010"), "{diags:?}");
    }

    #[test]
    fn float_literal_out_of_range_is_e060() {
        let diags = check_src("main: function () nothing {\n    x: @fbit16 = 100000.0\n}\n");
        assert!(has_code(&diags, "E060"), "{diags:?}");
    }

    #[test]
    fn negative_literal_folds_to_twos_complement() {
        // Each is pinned to the signed-smallest-fit width so the folded value
        // is exactly the two's-complement bit pattern at that width.
        for (src, want_ty, want_bytes) in [
            ("-1", "@bit8", "0xFF"),
            ("-128", "@bit8", "0x80"),
            ("-129", "@bit16", "0xFF7F"),
            ("-32768", "@bit16", "0x8000"),
            ("-32769", "@bit32", "0xFFFF7FFF"),
            ("-0x8000_0000_0000_0000", "@bit64", "0x8000000000000000"),
        ] {
            let source = format!("main: function () nothing {{\n    x: {want_ty} = {src}\n}}\n");
            let (ty, bytes) = literal_type_and_bytes(&source);
            assert_eq!(ty, want_ty, "{src} type");
            assert_eq!(bytes, want_bytes, "{src} bytes");
        }
    }

    #[test]
    fn negative_literal_grows_to_expected_width() {
        for (src, want_ty, want_bytes) in [
            ("x: @bit16 = -1", "@bit16", "0xFFFF"),
            ("x: @bit64 = -1", "@bit64", "0xFFFFFFFFFFFFFFFF"),
            ("x: @bit32 = -129", "@bit32", "0xFFFFFF7F"),
        ] {
            let source = format!("main: function () nothing {{\n    {src}\n}}\n");
            let (ty, bytes) = literal_type_and_bytes(&source);
            assert_eq!(ty, want_ty, "{src} type");
            assert_eq!(bytes, want_bytes, "{src} bytes");
        }
    }

    #[test]
    fn negative_literal_overflow_is_e060() {
        // A signed value that does not fit the expected width.
        let diags = check_src("main: function () nothing {\n    x: @bit8 = -129\n}\n");
        assert!(has_code(&diags, "E060"), "{diags:?}");
        // `@i_less_than_8` constrains `-1` to `@bit8`; `-129` needs 16.
        let diags =
            check_src("main: function () nothing {\n    x: @bit8 = @i_less_than_8(-129, 0)\n}\n");
        assert!(has_code(&diags, "E060"), "{diags:?}");
        // Past the signed 64-bit range.
        let diags =
            check_src("main: function () nothing {\n    x: @bit64 = -0x8000_0000_0000_0001\n}\n");
        assert!(has_code(&diags, "E060"), "{diags:?}");
    }

    #[test]
    fn negative_literal_folds_in_signed_comparison() {
        // `@i_less_than_8` types its operands as `@bit8`; `-1` must fold to
        // `@bit8` (`0xFF`) for the signed comparison to type-check.
        let src = "main: function () nothing {\n    x: @bit8 = @i_less_than_8(-1, 0)\n}\n";
        let (diags, hir) = check_hir(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let main = hir.funcs.iter().find(|f| f.name == "main").expect("main");
        let value = match &main.body.stmts[0] {
            lilac_hir::HirStmt::Let { value, .. } => value,
            other => panic!("expected a let, got {other:?}"),
        };
        let lilac_hir::HirExprKind::Call { args, .. } = &value.kind else {
            panic!("expected a call, got {:?}", value.kind);
        };
        let lilac_hir::HirExprKind::Number { bytes } = &args[0].kind else {
            panic!("expected a folded number, got {:?}", args[0].kind);
        };
        assert_eq!(bytes, "0xFF");
    }

    #[test]
    fn negate_on_non_literal_is_unchanged() {
        // `@negate` on a non-literal still resolves the user hook.
        let src = "@negate: function (value: @bit32) result: @bit32 {\n    result = @i_negate_32(value)\n}\n\
                   main: function () nothing {\n    a: @bit32 = 0x1\n    b: @bit32 = -a\n}\n";
        let diags = check_src(src);
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn literal_fold_operators() {
        // Hex operands so the pre-typeck decimal fold (still present until
        // Task 5) does not consume them first: each pair reaches this fold.
        for (src, want_ty, want_bytes) in [
            ("0x2 + 0x3", "@bit8", "0x5"),
            ("0xC8 + 0x64", "@bit16", "0x12C"),
            ("0x5 * 0x3", "@bit8", "0xF"),
            ("0x10 / 0x3", "@bit8", "0x5"),
            ("0x10 % 0x3", "@bit8", "0x1"),
            ("0x2 - 0x3", "@bit8", "0xFF"),
        ] {
            let source = format!("main: function () nothing {{\n    {src}\n}}\n");
            let (ty, bytes) = literal_type_and_bytes(&source);
            assert_eq!(ty, want_ty, "{src} type");
            assert_eq!(bytes, want_bytes, "{src} bytes");
        }
    }

    #[test]
    fn nested_literal_fold() {
        // Folding is bottom-up: `2 * 3 + 4` parses as `(2 * 3) + 4`, so the
        // inner `*` must fold before the outer `+` (design §2/§7).
        let source = "main: function () nothing {\n    2 * 3 + 4\n}\n";
        let (ty, bytes) = literal_type_and_bytes(source);
        assert_eq!(ty, "@bit8");
        assert_eq!(bytes, "0xA");
    }

    #[test]
    fn nested_literal_fold_grows_to_expected_width() {
        let source = "main: function () nothing {\n    x: @bit32 = 2 * 3 + 4\n}\n";
        let (ty, bytes) = literal_type_and_bytes(source);
        assert_eq!(ty, "@bit32");
        assert_eq!(bytes, "0xA");
    }

    #[test]
    fn nested_literal_fold_negative_intermediate() {
        // `(2 - 5) * 2` folds the negative intermediate, then types by §5.
        let source = "main: function () nothing {\n    2 - 5 + 1\n}\n";
        let (ty, bytes) = literal_type_and_bytes(source);
        assert_eq!(ty, "@bit8");
        assert_eq!(bytes, "0xFE");
    }

    #[test]
    fn nested_literal_fold_overflow_is_e060() {
        // `200 * 2` needs 9 bits: a nested overflow is still `E060`.
        let diags = check_src("main: function () nothing {\n    x: @bit8 = 200 * 2\n}\n");
        assert!(has_code(&diags, "E060"), "{diags:?}");
        // An intermediate past 64 bits is `E060` even though `/ 2` would fit.
        let diags =
            check_src("main: function () nothing {\n    0xFFFF_FFFF_FFFF_FFFF * 2 / 2\n}\n");
        assert!(has_code(&diags, "E060"), "{diags:?}");
    }

    #[test]
    fn nested_literal_fold_divide_by_zero_is_e060() {
        let diags = check_src("main: function () nothing {\n    2 * (3 / 0)\n}\n");
        assert!(has_code(&diags, "E060"), "{diags:?}");
    }

    #[test]
    fn nested_literal_fold_mixed_kind_is_e010() {
        let diags = check_src("main: function () nothing {\n    2 * 3 + 4.0\n}\n");
        assert!(has_code(&diags, "E010"), "{diags:?}");
    }

    #[test]
    fn foldable_literal_anchors_non_literal_width() {
        // A non-literal operand anchors the width, so the all-literal subtree
        // `(2 + 3)` must grow to `@bit32` (not its own `@bit8` fit) for the
        // `@bit32` `@mul` hook to resolve.
        let source = "@mul: function (lhs: @bit32, rhs: @bit32) result: @bit32 {\n    result = @i_mul_32(lhs, rhs)\n}\n\
                      main: function () nothing {\n    x: @bit32 = 2\n    y: @bit32 = x * (2 + 3)\n}\n";
        let diags = check_src(source);
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn literal_fold_grows_to_expected_width() {
        let source = "main: function () nothing {\n    x: @bit32 = 0x2 + 0x3\n}\n";
        let (ty, bytes) = literal_type_and_bytes(source);
        assert_eq!(ty, "@bit32");
        assert_eq!(bytes, "0x5");
        let source = "main: function () nothing {\n    x: @bit16 = 0xC8 + 0x64\n}\n";
        let (ty, bytes) = literal_type_and_bytes(source);
        assert_eq!(ty, "@bit16");
        assert_eq!(bytes, "0x12C");
    }

    #[test]
    fn literal_fold_overflow_is_e060() {
        // 300 needs 9 bits; `@bit8` holds 8.
        let diags = check_src("main: function () nothing {\n    x: @bit8 = 0xC8 + 0x64\n}\n");
        assert!(has_code(&diags, "E060"), "{diags:?}");
        // 2^64 does not fit any 64-bit container.
        let diags = check_src("main: function () nothing {\n    0xFFFF_FFFF_FFFF_FFFF + 0x1\n}\n");
        assert!(has_code(&diags, "E060"), "{diags:?}");
    }

    #[test]
    fn literal_fold_divide_by_zero_is_e060() {
        for src in ["0x5 / 0x0", "0x5 % 0x0"] {
            let source = format!("main: function () nothing {{\n    {src}\n}}\n");
            let diags = check_src(&source);
            assert!(has_code(&diags, "E060"), "{src}: {diags:?}");
        }
    }

    #[test]
    fn literal_fold_mixed_kind_is_e010() {
        let diags = check_src("main: function () nothing {\n    0x2 + 3.0\n}\n");
        assert!(has_code(&diags, "E010"), "{diags:?}");
        let diags = check_src("main: function () nothing {\n    3.0 - 0x2\n}\n");
        assert!(has_code(&diags, "E010"), "{diags:?}");
    }

    #[test]
    fn literal_fold_float_operators() {
        let source = "main: function () nothing {\n    1.0 + 2.0\n}\n";
        let (ty, bytes) = literal_type_and_bytes(source);
        assert_eq!(ty, "@fbit64");
        let value: f64 = bytes.parse().expect("folded float parses");
        assert!((value - 3.0).abs() < f64::EPSILON, "{bytes}");
        let source = "main: function () nothing {\n    x: @fbit32 = 5.0 / 2.0\n}\n";
        let (ty, _) = literal_type_and_bytes(source);
        assert_eq!(ty, "@fbit32");
    }

    #[test]
    fn literal_fold_leaves_non_literals_to_hooks() {
        // `a + 0x3` has a non-literal operand: the operator still resolves the
        // user `@add` hook (no fold).
        let src = "@add: function (lhs: @bit32, rhs: @bit32) result: @bit32 {\n    result = @i_add_32(lhs, rhs)\n}\n\
                   main: function () nothing {\n    a: @bit32 = 0x1\n    b: @bit32 = a + 0x3\n}\n";
        let (diags, hir) = check_hir(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let main = hir.funcs.iter().find(|f| f.name == "main").expect("main");
        let value = match &main.body.stmts[1] {
            lilac_hir::HirStmt::Let { value, .. } => value,
            other => panic!("expected a let, got {other:?}"),
        };
        assert!(
            matches!(value.kind, lilac_hir::HirExprKind::Call { .. }),
            "{:?}",
            value.kind
        );
    }

    /// Numeric objects mirroring `lib/Standard`: one exact-width
    /// `@conversion(from: <bit|fbit>)` per type, no widening pairs.
    fn numeric_objects() -> &'static str {
        "Int8: object { private { data: @bit8 } @conversion: function (from: @bit8) Int8 => Int8 { data = from } }\n\
         Int16: object { private { data: @bit16 } @conversion: function (from: @bit16) Int16 => Int16 { data = from } }\n\
         Int32: object { private { data: @bit32 } @conversion: function (from: @bit32) Int32 => Int32 { data = from } }\n\
         Int64: object { private { data: @bit64 } @conversion: function (from: @bit64) Int64 => Int64 { data = from } }\n\
         Int: object { private { data: @bit_native } @conversion: function (from: @bit_native) Int => Int { data = from } }\n\
         Float32: object { private { data: @fbit32 } @conversion: function (from: @fbit32) Float32 => Float32 { data = from } }\n\
         Float64: object { private { data: @fbit64 } @conversion: function (from: @fbit64) Float64 => Float64 { data = from } }\n"
    }

    #[test]
    fn literal_grows_through_object_conversion() {
        // A literal assigned to a numeric object grows to the object's single
        // conversion source width (never shrinks), so the exact-width
        // conversion applies: `Int16 = 5` uses `@bit16`, not `@bit8`.
        let src = format!(
            "{}main: function () nothing {{\n    a: Int8 = 5\n    b: Int16 = 5\n    c: Int32 = 5\n    d: Int64 = 5\n    e: Int = 5\n    f: Float32 = 3.14\n    g: Float64 = 3.14\n}}\n",
            numeric_objects()
        );
        let diags = check_src(&src);
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn literal_overflow_through_object_is_e060() {
        // `Int8`'s source is `@bit8`; `300` needs 9 bits, so it cannot shrink.
        let src = format!(
            "{}main: function () nothing {{\n    x: Int8 = 300\n}}\n",
            numeric_objects()
        );
        let diags = check_src(&src);
        assert!(has_code(&diags, "E060"), "{diags:?}");
        assert!(!has_code(&diags, "E043"), "{diags:?}");
    }

    #[test]
    fn float_literal_into_float_object_compiles() {
        // `@fbitN -> FloatN` is the value conversion; a float literal pins to it.
        let src = format!(
            "{}main: function () nothing {{\n    f: Float32 = 3.14\n    g: Float64 = 3.14\n}}\n",
            numeric_objects()
        );
        let diags = check_src(&src);
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn integer_literal_into_float_object_is_not_a_bitcast() {
        // The float objects expose no `@bitN` conversion, so an integer literal
        // cannot silently reinterpret its bits; it is a clean conversion miss.
        for ty in ["Float32", "Float64"] {
            let src = format!(
                "{}main: function () nothing {{\n    x: {ty} = 3\n}}\n",
                numeric_objects()
            );
            let diags = check_src(&src);
            assert!(has_code(&diags, "E043"), "{ty}: {diags:?}");
        }
    }

    #[test]
    fn fbit128_literal_is_out_of_scope() {
        let diags = check_src("main: function () nothing {\n    x: @fbit128 = 3.14\n}\n");
        assert!(has_code(&diags, "E010"), "{diags:?}");
    }

    #[test]
    fn negative_literal_grows_through_object_conversion() {
        // A folded negative literal assigned to a numeric object grows to the
        // object's single `@conversion(from: @bitN)` source width, exactly as
        // a positive integer literal does (§4/§5).
        let src = format!(
            "{}main: function () nothing {{\n    a: Int8 = -1\n    b: Int16 = -1\n    c: Int32 = -1\n    d: Int64 = -1\n    e: Int = -1\n}}\n",
            numeric_objects()
        );
        let diags = check_src(&src);
        assert!(!diags.has_errors(), "{diags:?}");
        // `Int8`'s source is `@bit8`; `-129` needs a signed 16-bit width.
        let src = format!(
            "{}main: function () nothing {{\n    x: Int8 = -129\n}}\n",
            numeric_objects()
        );
        let diags = check_src(&src);
        assert!(has_code(&diags, "E060"), "{diags:?}");
    }

    fn check_hir(src: &str) -> (Diags, lilac_hir::HirProgram) {
        let mut sm = SourceMap::new();
        let file = sm.add_file("test.lilac", src);
        let (tokens, lex_errors) = Lexer::lex_all(src, file);
        let mut diags = Diags::new();
        for error in lex_errors {
            diags.emit(error);
        }
        let mut parser = Parser::new(tokens, file);
        let program = parser
            .parse_program(&mut diags)
            .expect("test source parses");
        let module = lilac_resolve::collect(&program, &mut diags);
        let project = lilac_resolve::ResolvedProject::single(module, file);
        let mut types = Types::new();
        let sigs = super::super::collect_sigs(&project, &program, &mut types, &mut diags);
        let hir = check_bodies(&program, &project, &sigs, &mut types, &mut diags, 64);
        (diags, hir)
    }

    #[test]
    fn bare_ident_use_is_recorded() {
        let src = "pi: @bit32 = 0x2A\nmain: function () nothing {\n    x: @bit32 = pi\n}\n";
        let (diags, hir) = check_hir(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let lo = u32::try_from(src.rfind("pi").expect("use site")).expect("fits");
        assert!(
            hir.uses
                .iter()
                .any(|s| s.span.lo.0 == lo && s.def != lilac_resolve::DefId::ERROR),
            "{:?}",
            hir.uses
        );
    }

    #[test]
    fn callee_use_is_recorded() {
        let src = "foo: function () nothing {\n}\nmain: function () nothing {\n    foo()\n}\n";
        let (diags, hir) = check_hir(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let lo = u32::try_from(src.rfind("foo").expect("call")).expect("fits");
        assert!(
            hir.uses
                .iter()
                .any(|s| s.span.lo.0 == lo && s.def != lilac_resolve::DefId::ERROR),
            "{:?}",
            hir.uses
        );
    }

    #[test]
    fn member_use_is_recorded() {
        let src = "Object: object {\n    member: @bit8\n}\nmain: function () nothing {\n    v: Object = Object {\n        member = 0x1\n    }\n    x: @bit8 = v.member\n}\n";
        let (diags, hir) = check_hir(src);
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(
            hir.uses.iter().any(|s| {
                &src[s.span.lo.0 as usize..s.span.hi.0 as usize] == "member"
                    && s.def != lilac_resolve::DefId::ERROR
            }),
            "{:?}",
            hir.uses
        );
    }

    #[test]
    fn variant_use_is_recorded() {
        let src = "OptionalBit32: enum {\n    Some: @bit32\n    None\n}\nmain: function () nothing {\n    v: OptionalBit32 = .Some(0x0)\n    w: OptionalBit32 = .None\n}\n";
        let (diags, hir) = check_hir(src);
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(
            hir.uses.iter().any(|s| {
                &src[s.span.lo.0 as usize..s.span.hi.0 as usize] == ".Some"
                    && s.def != lilac_resolve::DefId::ERROR
            }),
            "{:?}",
            hir.uses
        );
    }

    #[test]
    fn bare_tag_use_is_recorded() {
        let src = "OptionalBit32: enum {\n    Some: @bit32\n    None\n}\nmain: function () nothing {\n    v: OptionalBit32 = .None\n}\n";
        let (diags, hir) = check_hir(src);
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(
            hir.uses.iter().any(|s| {
                &src[s.span.lo.0 as usize..s.span.hi.0 as usize] == ".None"
                    && s.def != lilac_resolve::DefId::ERROR
            }),
            "{:?}",
            hir.uses
        );
    }

    #[test]
    fn local_and_intrinsic_uses_are_skipped() {
        let src = "pi: @bit8 = 0x1\nmain: function () nothing {\n    x: @bit8 = pi\n    y: @bit8 = @i_add_8(x, 0x1)\n}\n";
        let (diags, hir) = check_hir(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let texts: Vec<&str> = hir
            .uses
            .iter()
            .map(|s| &src[s.span.lo.0 as usize..s.span.hi.0 as usize])
            .collect();
        assert!(texts.contains(&"pi"), "{texts:?}");
        assert!(!texts.contains(&"x"), "local use recorded: {texts:?}");
        assert!(
            !texts.iter().any(|text| text.starts_with('@')),
            "intrinsic recorded: {texts:?}"
        );
    }

    #[test]
    fn semantic_sites_record_non_item_names() {
        let src = "Obj: object {\n    v: @bit8\n}\nmain: function (p: @bit32) result: @bit32 {\n    x: Obj = Obj {\n        v = 0x1\n    }\n    y: @bit32 = p\n    z: @bit8 = x.v\n    result = @i_add_32(y, y)\n}\n";
        let (diags, hir) = check_hir(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let has = |lo: usize, kind: lilac_hir::SemanticKind| {
            hir.semantics
                .iter()
                .any(|site| site.span.lo.0 as usize == lo && site.kind == kind)
        };
        // The item names (`main`, the object `Obj`, the field `v`) are the
        // index's job, not this source's.
        for lo in [
            src.find("main").expect("main"),
            src.find("Obj").expect("Obj declaration"),
            src.find("v: @bit8").expect("field declaration"),
        ] {
            assert!(
                hir.semantics
                    .iter()
                    .all(|site| site.span.lo.0 as usize != lo),
                "item name at {lo} must not be a semantic site: {:?}",
                hir.semantics
            );
        }
        // Local declaration + use.
        let x_decl = src.find("x: Obj").expect("x declaration");
        assert!(
            has(x_decl, lilac_hir::SemanticKind::Local),
            "{:?}",
            hir.semantics
        );
        let x_use = src.rfind("x.v").expect("x use");
        assert!(
            has(x_use, lilac_hir::SemanticKind::Local),
            "{:?}",
            hir.semantics
        );
        // Parameter declaration + use (a use of a parameter is still a local
        // name use).
        let p_decl = src.find("p: @bit32").expect("p parameter");
        assert!(
            has(p_decl, lilac_hir::SemanticKind::Param),
            "{:?}",
            hir.semantics
        );
        let p_use = src.find("= p\n").expect("p use") + 2;
        assert!(
            has(p_use, lilac_hir::SemanticKind::Local),
            "{:?}",
            hir.semantics
        );
        // A user type reference (the object used as a type).
        let obj_use = x_decl + 3;
        assert!(
            has(obj_use, lilac_hir::SemanticKind::UserType),
            "{:?}",
            hir.semantics
        );
        // A builtin type name in body position.
        let builtin_ty = src.find("y: @bit32").expect("y declaration") + 3;
        assert!(
            has(builtin_ty, lilac_hir::SemanticKind::BuiltinType),
            "{:?}",
            hir.semantics
        );
        // A builtin call name.
        let builtin_fn = src.find("@i_add_32").expect("intrinsic call");
        assert!(
            has(builtin_fn, lilac_hir::SemanticKind::BuiltinFunc),
            "{:?}",
            hir.semantics
        );
    }

    #[test]
    fn unused_generic_object_members_are_not_checked() {
        // A generic object is a template: its non-generic members may carry
        // `Param` slots and must not be emitted as concrete HIR (design §3.3 /
        // §3.4). Otherwise lowering sees `#T` and the verifier raises `E070`.
        let (diags, hir) = check_hir(
            "GenPut[T]: object {\n    put: function (value: T) nothing {\n    }\n}\n\nGenDrop[T]: object {\n    @on_drop: function (from: ref GenDrop[T]) nothing {\n    }\n}\n\nmain: function () nothing {\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        let names: Vec<&str> = hir.funcs.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["main"], "generic members leaked: {names:?}");
        assert!(!hir.drops.contains_key("GenDrop"), "{:?}", hir.drops);
    }

    fn messages(diags: &Diags) -> Vec<String> {
        diags.diagnostics().iter().map(|d| d.msg.clone()).collect()
    }

    #[test]
    fn mismatch_without_conversion_is_e043() {
        // Task 5: without a binding, a type mismatch is a failed conversion
        // search. The diagnostic still shows both sides.
        let diags =
            check_src("main: function () nothing {\n    y: @bit32 = 0x1\n    x: @bit8 = y\n}\n");
        assert!(has_code(&diags, "E043"), "{diags:?}");
        let msgs = messages(&diags);
        assert!(
            msgs.iter()
                .any(|m| m.contains("@bit8") && m.contains("@bit32")),
            "{msgs:?}"
        );
    }

    #[test]
    fn unresolved_name_is_e021() {
        let diags = check_src("main: function () nothing {\n    x: @bit8 = nope\n}\n");
        assert!(has_code(&diags, "E021"), "{diags:?}");
    }

    #[test]
    fn arity_mismatch_is_e022() {
        let diags = check_src(
            "get: function () result: @bit32 {\n    result = 0x1\n}\nmain: function () nothing {\n    x: @bit8 = get(0x1)\n}\n",
        );
        assert!(has_code(&diags, "E022"), "{diags:?}");
    }

    #[test]
    fn bare_literal_is_typed_not_e024() {
        let diags = check_src("main: function () nothing {\n    0x1\n}\n");
        assert!(!has_code(&diags, "E024"), "{diags:?}");
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn unassigned_slot_is_e027() {
        let diags = check_src("get: function () result: @bit32 {\n}\n");
        assert!(has_code(&diags, "E027"), "{diags:?}");
    }

    #[test]
    fn locals_shadow_globals() {
        let diags = check_src(
            "get: function () result: @bit32 {\n    result = 0x1\n}\nmain: function () nothing {\n    g: @bit32 = get()\n    x: @bit8 = g()\n}\n",
        );
        assert!(has_code(&diags, "E022"), "{diags:?}");
        assert_eq!(diags.diagnostics().len(), 1, "{diags:?}");
    }

    #[test]
    fn equality_needs_same_width() {
        let diags = check_src(
            "main: function () nothing {\n    a: @bit8 = 0x1\n    b: @bit32 = 0x2\n    c: @bit8 = a == b\n}\n",
        );
        assert!(has_code(&diags, "E010"), "{diags:?}");
        let e010: Vec<_> = diags
            .diagnostics()
            .iter()
            .filter(|d| d.code == "E010")
            .collect();
        assert_eq!(e010.len(), 1, "{diags:?}");
        assert_eq!(e010[0].labels.len(), 2, "{diags:?}");
    }

    #[test]
    fn cmp_operands_ignore_expected_result_width() {
        // Return position expects `@bit8`; operands are `@bit32`.
        let diags =
            check_src("f: function (a: @bit32, b: @bit32) @bit8 => @i_less_than_32(a, b)\n");
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn width_suffixed_numeric_intrinsics() {
        // An exact-width integer add typechecks; literals adopt the width.
        let diags =
            check_src("main: function () nothing {\n    x: @bit8 = @i_add_8(0x1, 0x2)\n}\n");
        assert!(!diags.has_errors(), "{diags:?}");
        // A width mismatch is `E010`, with no auto-unification.
        let diags = check_src(
            "main: function () nothing {\n    a: @bit16 = 0x1\n    b: @bit16 = 0x2\n    x: @bit16 = @i_add_8(a, b)\n}\n",
        );
        assert!(has_code(&diags, "E010"), "{diags:?}");
        // Float ops require real `@fbit32` operands, not integer containers.
        let diags = check_src(
            "main: function () nothing {\n    a: @bit32 = 0x1\n    b: @bit32 = 0x2\n    x: @fbit32 = @f_add_32(a, b)\n}\n",
        );
        assert!(has_code(&diags, "E010"), "{diags:?}");
    }

    #[test]
    fn old_unsuffixed_intrinsics_are_gone() {
        // The unsuffixed names are gone. The name is assembled so this source
        // itself does not contain the retired spelling.
        let old = ["@i", "_add"].concat();
        let src = format!("main: function () nothing {{\n    x: @bit8 = {old}(0x1, 0x2)\n}}\n");
        let diags = check_src(&src);
        assert!(diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn bitcast_intrinsics_round_trip() {
        // `@fbit_to_bits_W(@bits_to_fbit_W(x))` typechecks at every width that
        // has a bit container (`@bit16/32/64`).
        for (w, bit, fbit) in [
            ("16", "@bit16", "@fbit16"),
            ("32", "@bit32", "@fbit32"),
            ("64", "@bit64", "@fbit64"),
        ] {
            let diags = check_src(&format!(
                "main: function () nothing {{\n    x: {bit} = 0x1\n    f: {fbit} = @bits_to_fbit_{w}(x)\n    y: {bit} = @fbit_to_bits_{w}(f)\n}}\n"
            ));
            assert!(!diags.has_errors(), "{w}: {diags:?}");
        }
    }

    #[test]
    fn bitcast_wrong_source_is_e010() {
        // `@bits_to_fbit_32` needs an `@bit32` operand.
        let diags = check_src(
            "main: function (x: @bit16) nothing {\n    f: @fbit32 = @bits_to_fbit_32(x)\n}\n",
        );
        assert!(has_code(&diags, "E010"), "{diags:?}");
        // `@fbit_to_bits_64` needs an `@fbit64` operand.
        let diags = check_src(
            "main: function (f: @fbit32) nothing {\n    y: @bit64 = @fbit_to_bits_64(f)\n}\n",
        );
        assert!(has_code(&diags, "E010"), "{diags:?}");
    }

    #[test]
    fn ref_mismatch_labels_both_sides() {
        let diags = check_src(
            "set: function (r: ref @bit8) nothing {\n}\nmain: function () nothing {\n    v: @bit8 = 0x1\n    set(v)\n}\n",
        );
        assert!(has_code(&diags, "E010"), "{diags:?}");
        let e010: Vec<_> = diags
            .diagnostics()
            .iter()
            .filter(|d| d.code == "E010")
            .collect();
        assert_eq!(e010.len(), 1, "{diags:?}");
        assert_eq!(e010[0].labels.len(), 2, "{diags:?}");
        assert!(
            e010[0].msg.contains("ref @bit8") && e010[0].msg.contains("@bit8"),
            "{diags:?}"
        );
    }

    #[test]
    fn enum_tag_validates() {
        let diags = check_src(
            "Maybe: enum {\n    Some: @bit8\n    None\n}\nmain: function () nothing {\n    x: Maybe = .None\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn hex_literal_takes_expected_bit_type() {
        let diags = check_src("main: function () nothing {\n    x: @bit8 = 0x1\n}\n");
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn decimal_literal_is_a_bit_value() {
        let diags = check_src("main: function () nothing {\n    x: @bit8 = 5\n}\n");
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn hex_literal_overflow_is_e060() {
        let diags = check_src("main: function () nothing {\n    x: @bit8 = 0x1FF\n}\n");
        assert!(has_code(&diags, "E060"), "{diags:?}");
    }

    #[test]
    fn hex_literal_over_u64_is_e060() {
        let diags =
            check_src("main: function () nothing {\n    x: @bit8 = 0x1_0000_0000_0000_0000\n}\n");
        assert!(has_code(&diags, "E060"), "{diags:?}");
    }

    #[test]
    fn decimal_literals_adopt_intrinsic_width() {
        let diags = check_src("main: function () nothing {\n    x: @bit32 = @i_add_32(5, 6)\n}\n");
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn poison_types_never_panic() {
        // Unknown parameter types poison the signature; calling with a
        // literal must report E023, never crash the compiler.
        let diags = check_src(
            "foo: function (x: Nope) nothing {\n}\nmain: function () nothing {\n    foo(0x1)\n}\n",
        );
        assert!(has_code(&diags, "E023"), "{diags:?}");
    }

    fn method_src() -> &'static str {
        "Object: object {\n    member: @bit32\n    get: function (instance: ref Object) result: @bit32 {\n        result = instance.member\n    }\n}\nmain: function () nothing {\n    variable: Object = Object {\n        member = 0x1\n    }\n    member: @bit32 = variable.get()\n}\n"
    }

    #[test]
    fn method_call_checks() {
        let diags = check_src(method_src());
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn unknown_member_is_e021_not_e040() {
        let diags = check_src(
            "Object: object {\n    member: @bit8\n}\nmain: function () nothing {\n    variable: Object = Object {\n        member = 0x1\n    }\n    variable_2: @bit8 = variable.data\n}\n",
        );
        assert!(has_code(&diags, "E021"), "{diags:?}");
        assert!(!has_code(&diags, "E040"), "{diags:?}");
    }

    #[test]
    fn calling_a_field_is_e022() {
        let diags = check_src(
            "Object: object {\n    member: @bit8\n}\nmain: function () nothing {\n    variable: Object = Object {\n        member = 0x1\n    }\n    variable.member()\n}\n",
        );
        assert!(has_code(&diags, "E022"), "{diags:?}");
    }

    #[test]
    fn multilet_clean() {
        let diags = check_src(
            "get_multi: function () condition: @bit32, result: @bit32 {\n    condition = 0x1\n    result = 0x2\n}\nmain: function () nothing {\n    condition: @bit32, result: @bit32 = get_multi()\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn multilet_arity_mismatch_is_e022() {
        let diags = check_src(
            "get_multi: function () condition: @bit32, result: @bit32 {\n    condition = 0x1\n    result = 0x2\n}\nmain: function () nothing {\n    a: @bit32, b: @bit32, c: @bit32 = get_multi()\n}\n",
        );
        assert!(has_code(&diags, "E022"), "{diags:?}");
        let msgs = messages(&diags);
        assert!(
            msgs.iter().any(|m| m.contains('3') && m.contains('2')),
            "{msgs:?}"
        );
    }

    #[test]
    fn single_let_against_multi_return_is_e022() {
        let diags = check_src(
            "Object1: object {\n    value: @bit8\n}\nmain: function () nothing {\n    result: Object1 = get()\n}\nget: function () result: Object1, result_2: @bit32 {\n    result = Object1 {\n        value = 0x0\n    }\n    result_2 = 0x0\n}\n",
        );
        assert!(has_code(&diags, "E022"), "{diags:?}");
    }

    #[test]
    fn structlit_missing_field_is_e022() {
        let diags = check_src(
            "Object: object {\n    member: @bit8\n}\nmain: function () nothing {\n    variable: Object = Object {\n    }\n}\n",
        );
        assert!(has_code(&diags, "E022"), "{diags:?}");
        let msgs = messages(&diags);
        assert!(msgs.iter().any(|m| m.contains("member")), "{msgs:?}");
    }

    #[test]
    fn structlit_unknown_field_is_e021() {
        let diags = check_src(
            "Object: object {\n    member: @bit8\n}\nmain: function () nothing {\n    variable: Object = Object {\n        data = 0x1\n    }\n}\n",
        );
        assert!(has_code(&diags, "E021"), "{diags:?}");
    }

    #[test]
    fn func_value_call_checks() {
        let diags = check_src(
            "main: function () nothing {\n    add: function (@bit32) @bit32 = addOne\n    value: @bit32 = 0x0\n    value = add(value)\n    add = addTwo\n    value = add(value)\n}\naddOne: function (value: @bit32) result: @bit32 {\n    result = @i_add_32(value, 0x1)\n}\naddTwo: function (value: @bit32) result: @bit32 {\n    result = @i_add_32(value, 0x2)\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn recursive_inline_is_e022() {
        let diags = check_src(
            "f: function (x: @bit32) @bit32 => f(x)\nmain: function () nothing {\n    y: @bit32 = f(0x1)\n}\n",
        );
        assert!(has_code(&diags, "E022"), "{diags:?}");
        let msgs = messages(&diags);
        assert!(msgs.iter().any(|m| m.contains("recursive")), "{msgs:?}");
    }

    #[test]
    fn static_call_checks() {
        let diags = check_src(
            "Object: object {\n    member: @bit32\n    add: function (num: @bit32) result: @bit32 {\n        result = @i_add_32(instance.member, num)\n    }\n}\nmain: function () nothing {\n    variable: Object = Object {\n        member = 0x1\n    }\n    member: @bit32 = Object.add(0x2)\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn method_without_self_is_e022() {
        let diags = check_src(
            "Object: object {\n    member: @bit8\n    add: function (num: @bit32) result: @bit32 {\n        result = num\n    }\n}\nmain: function () nothing {\n    variable: Object = Object {\n        member = 0x1\n    }\n    member: @bit32 = variable.add(0x2)\n}\n",
        );
        assert!(has_code(&diags, "E022"), "{diags:?}");
    }

    use lilac_hir::{HirExprKind, HirProgram, HirStmt, TempId};

    fn check_hir_src(src: &str) -> (HirProgram, Diags) {
        let mut sm = SourceMap::new();
        let file = sm.add_file("test.lilac", src);
        let (tokens, lex_errors) = Lexer::lex_all(src, file);
        let mut diags = Diags::new();
        for error in lex_errors {
            diags.emit(error);
        }
        let mut parser = Parser::new(tokens, file);
        let program = parser
            .parse_program(&mut diags)
            .expect("test source parses");
        let module = lilac_resolve::collect(&program, &mut diags);
        let project = lilac_resolve::ResolvedProject::single(module, file);
        let mut types = Types::new();
        let sigs = super::super::collect_sigs(&project, &program, &mut types, &mut diags);
        let hir = check_bodies(&program, &project, &sigs, &mut types, &mut diags, 64);
        (hir, diags)
    }

    #[test]
    fn factory_call_checks() {
        // `Type(args)` routes to the `@init` factory hook (SPEC §8 RAII).
        let diags = check_src(
            "Object: object {\n    value: @bit8\n    @init: function (value: @bit8) result: Object {\n        result = Object {\n            value = value\n        }\n    }\n}\nmain: function () nothing {\n    variable: Object = Object(0x1)\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn factory_missing_init_is_e022() {
        let diags = check_src(
            "Object: object {\n    value: @bit8\n}\nmain: function () nothing {\n    variable: Object = Object(0x1)\n}\n",
        );
        assert!(has_code(&diags, "E022"), "{diags:?}");
    }

    #[test]
    fn factory_self_param_is_e022() {
        // Factories take plain parameters (Zig rejects `self` on `@init`).
        let diags = check_src(
            "Object: object {\n    value: @bit8\n    @init: function (self) result: Object {\n        result = Object {\n            value = 0x1\n        }\n    }\n}\nmain: function () nothing {\n    variable: Object = Object(0x1)\n}\n",
        );
        assert!(has_code(&diags, "E022"), "{diags:?}");
    }

    #[test]
    fn factory_wrong_return_is_e022() {
        let diags = check_src(
            "Object: object {\n    value: @bit8\n    @init: function (value: @bit8) result: @bit8 {\n        result = value\n    }\n}\nmain: function () nothing {\n    variable: Object = Object(0x1)\n}\n",
        );
        assert!(has_code(&diags, "E022"), "{diags:?}");
    }

    #[test]
    fn factory_selects_init_overload_by_arity() {
        // Member hooks overload: `Object(0x1)` must pick the 1-arg `@init`,
        // not the 0-arg one, and `Object()` must pick the 0-arg one.
        let diags = check_src(
            "Object: object {\n    value: @bit8\n    @init: function () result: Object {\n        result = Object { value = 0x1 }\n    }\n    @init: function (value: @bit8) result: Object {\n        result = Object { value = value }\n    }\n}\nmain: function () nothing {\n    a: Object = Object()\n    b: Object = Object(0x1)\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn factory_does_not_convert_arguments() {
        // A factory is a hook, so its arguments match exactly: `Object(b)` with
        // `b: @bit8` and an `@bit8 -> @bit32` conversion is `E010`, not a
        // conversion.
        let diags = check_src(
            "Object: object {\n    value: @bit32\n    @init: function (value: @bit32) result: Object {\n        result = Object { value = value }\n    }\n}\n@conversion: function (from: @bit8) to: @bit32 { to = @extend_zero(from) }\nmain: function () nothing {\n    b: @bit8 = 0x2\n    c: Object = Object(b)\n}\n",
        );
        assert!(has_code(&diags, "E010"), "{diags:?}");
    }

    #[test]
    fn structlit_with_init_ignores_hook() {
        // Struct literals initialize fields directly; `@init` only serves
        // `Type(params)` factory calls.
        let (hir, diags) = check_hir_src(
            "Object: object {\n    member: @bit8\n    @init: function (value: @bit8) result: Object {\n        result = Object {\n            member = value\n        }\n    }\n}\nmain: function () nothing {\n    variable: Object = Object {\n        member = 0x1\n    }\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        let main = hir
            .funcs
            .iter()
            .find(|f| f.name == "main")
            .expect("has main");
        let HirStmt::Let { value, .. } = &main.body.stmts[0] else {
            panic!("expected let, got {:?}", main.body.stmts[0]);
        };
        assert!(
            matches!(value.kind, lilac_hir::HirExprKind::StructLit { .. }),
            "expected plain struct literal, got {:?}",
            value.kind
        );
    }

    #[test]
    fn on_drop_bad_shape_is_e022() {
        let diags = check_src(
            "Object: object {\n    member: @bit8\n    @on_drop: function (member: @bit8) nothing {\n    }\n}\nmain: function () nothing {\n}\n",
        );
        assert!(has_code(&diags, "E022"), "{diags:?}");
    }

    #[test]
    fn conversion_wraps_call() {
        // A conversion mismatch becomes an explicit `@conversion` call in HIR.
        let (hir, diags) = check_hir_src(
            "@conversion: function (from: @bit32) to: @bit8 {\n    to = @truncate(from)\n}\nmain: function () nothing {\n    a: @bit32 = 0x1\n    b: @bit8 = a\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        let main = hir
            .funcs
            .iter()
            .find(|f| f.name == "main")
            .expect("has main");
        let HirStmt::Let { value, .. } = &main.body.stmts[1] else {
            panic!("expected let, got {:?}", main.body.stmts[1]);
        };
        let lilac_hir::HirExprKind::Call { callee, .. } = &value.kind else {
            panic!("expected conversion call, got {:?}", value.kind);
        };
        let lilac_hir::HirCallee::Hook { name, .. } = callee else {
            panic!("expected hook call, got {callee:?}");
        };
        assert!(name.contains("@conversion"), "{name}");
    }

    #[test]
    fn as_cast_conversion_wraps_call() {
        // `a as @bit16` (`a: @bit8`) emits the same one-step `@conversion`
        // call the implicit path emits.
        let (hir, diags) = check_hir_src(
            "@conversion: function (from: @bit8) to: @bit16 {\n    to = @extend_zero(from)\n}\nmain: function () nothing {\n    a: @bit8 = 0x1\n    b: @bit16 = a as @bit16\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        let main = hir
            .funcs
            .iter()
            .find(|f| f.name == "main")
            .expect("has main");
        let HirStmt::Let { value, .. } = &main.body.stmts[1] else {
            panic!("expected let, got {:?}", main.body.stmts[1]);
        };
        let HirExprKind::Call { callee, .. } = &value.kind else {
            panic!("expected conversion call, got {:?}", value.kind);
        };
        let lilac_hir::HirCallee::Hook { name, .. } = callee else {
            panic!("expected hook call, got {callee:?}");
        };
        assert!(name.contains("@conversion"), "{name}");
    }

    #[test]
    fn as_cast_without_conversion_is_e043() {
        let diags = check_src(
            "main: function () nothing {\n    a: @bit8 = 0x1\n    b: @bit16 = a as @bit16\n}\n",
        );
        assert!(has_code(&diags, "E043"), "{diags:?}");
    }

    #[test]
    fn as_cast_literal_pins_to_target() {
        assert_eq!(
            literal_type("main: function () nothing {\n    0x1 as @bit16\n}\n"),
            "@bit16"
        );
        assert_eq!(
            literal_type("main: function () nothing {\n    3.14 as @fbit32\n}\n"),
            "@fbit32"
        );
    }

    #[test]
    fn as_cast_redundant_is_identity() {
        let (hir, diags) = check_hir_src(
            "main: function () nothing {\n    x: @bit16 = 0x1\n    y: @bit16 = x as @bit16\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        let main = hir
            .funcs
            .iter()
            .find(|f| f.name == "main")
            .expect("has main");
        let HirStmt::Let { value, .. } = &main.body.stmts[1] else {
            panic!("expected let, got {:?}", main.body.stmts[1]);
        };
        assert!(
            matches!(value.kind, HirExprKind::Ident { .. }),
            "identity cast must not emit a conversion call: {:?}",
            value.kind
        );
    }

    #[test]
    fn as_cast_ref_unknown_is_builtin() {
        // `ref unknown` <-> any `ref T`, both directions, with no hook: the
        // operand is retyped in place (still an `Ident`, never poison).
        let (hir, diags) = check_hir_src(
            "main: function (p: ref unknown) nothing {\n    q: ref @bit8 = p as ref @bit8\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        let main = hir
            .funcs
            .iter()
            .find(|f| f.name == "main")
            .expect("has main");
        let HirStmt::Let { value, .. } = &main.body.stmts[0] else {
            panic!("expected let, got {:?}", main.body.stmts[0]);
        };
        assert!(
            matches!(value.kind, HirExprKind::Ident { .. }),
            "ref-unknown cast must retype the operand: {:?}",
            value.kind
        );
        let (hir, diags) = check_hir_src(
            "main: function (p: ref @bit8) nothing {\n    q: ref unknown = p as ref unknown\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        let main = hir
            .funcs
            .iter()
            .find(|f| f.name == "main")
            .expect("has main");
        let HirStmt::Let { value, .. } = &main.body.stmts[0] else {
            panic!("expected let, got {:?}", main.body.stmts[0]);
        };
        assert!(
            matches!(value.kind, HirExprKind::Ident { .. }),
            "ref-unknown cast must retype the operand: {:?}",
            value.kind
        );
    }

    #[test]
    fn as_cast_ref_mismatch_is_e010() {
        // A non-`unknown` ref mismatch is not the §6 exception.
        let diags = check_src(
            "main: function (p: ref @bit8) nothing {\n    q: ref @bit16 = p as ref @bit16\n}\n",
        );
        assert!(has_code(&diags, "E010"), "{diags:?}");
        assert!(!has_code(&diags, "E043"), "{diags:?}");
    }

    #[test]
    fn as_cast_ref_to_nonref_is_e010() {
        // `ref T as T` is not the §6 exception (ref-to-ref only): a `ref`
        // never silently becomes a non-`ref` value, so this is `E010`.
        let diags =
            check_src("main: function (p: ref @bit8) nothing {\n    q: @bit8 = p as @bit8\n}\n");
        assert!(has_code(&diags, "E010"), "{diags:?}");
        assert!(!has_code(&diags, "E043"), "{diags:?}");
    }

    #[test]
    fn as_cast_is_usable_as_operator_operand() {
        // `as` converts where the operator would otherwise reject the operand.
        let (hir, diags) = check_hir_src(
            "@conversion: function (from: @bit8) to: @bit32 { to = @extend_zero(from) }\n@add: function (lhs: @bit32, rhs: @bit32) result: @bit32 { result = @i_add_32(lhs, rhs) }\nmain: function () nothing {\n    a: @bit8 = 0x1\n    b: @bit32 = 0x2\n    c: @bit32 = a as @bit32 + b\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        let main = hir
            .funcs
            .iter()
            .find(|f| f.name == "main")
            .expect("has main");
        let HirStmt::Let { value, .. } = &main.body.stmts[2] else {
            panic!("expected let, got {:?}", main.body.stmts[2]);
        };
        let HirExprKind::Call { args, .. } = &value.kind else {
            panic!("expected add call, got {:?}", value.kind);
        };
        let HirExprKind::Call { callee, .. } = &args[0].kind else {
            panic!(
                "expected conversion on the cast operand, got {:?}",
                args[0].kind
            );
        };
        let lilac_hir::HirCallee::Hook { name, .. } = callee else {
            panic!("expected hook call, got {callee:?}");
        };
        assert!(name.contains("@conversion"), "{name}");
    }

    #[test]
    fn inline_substituted_nonrecursive() {
        let src = "addOne: function (value: @bit32) @bit32 => @i_add_32(value, 0x1)\nmain: function () nothing {\n    variable: @bit32 = addOne(0x1)\n}\n";
        let (hir, diags) = check_hir_src(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let main = hir
            .funcs
            .iter()
            .find(|f| f.name == "main")
            .expect("has main");
        let HirStmt::Let { value, .. } = &main.body.stmts[0] else {
            panic!("expected let, got {:?}", main.body.stmts[0]);
        };
        let HirExprKind::Call { callee, args } = &value.kind else {
            panic!("expected inlined call, got {:?}", value.kind);
        };
        let lilac_hir::HirCallee::Intrinsic { name } = callee else {
            panic!("expected intrinsic call, got {callee:?}");
        };
        assert_eq!(name, "@i_add_32");
        assert_eq!(args.len(), 2);
    }

    #[test]
    fn inline_or_body_checks_and_inlines() {
        let src = "leq: function (a: @bit8, b: @bit8) @bit8 => (a == b) or @i_less_than_8(a, b)\n\
                   main: function () nothing {\n    r: @bit8 = leq(0x1, 0x2)\n}\n";
        let (diags, _) = check_hir(src);
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn inline_if_value_body_checks_and_inlines() {
        // Value-`if` branches are expressions: `if c a else b` (no braces).
        let src = "pick: function (a: @bit8, b: @bit8) @bit8 => if a b else a\n\
                   main: function () nothing {\n    r: @bit8 = pick(0x1, 0x2)\n}\n";
        let (diags, _) = check_hir(src);
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn inline_two_or_bodies_use_distinct_temps() {
        let src = "or2: function (a: @bit8, b: @bit8) @bit8 => a or b\n\
                   use2: function (p: @bit8, q: @bit8, r: @bit8, s: @bit8) @bit8 => or2(p, q) or or2(r, s)\n\
                   main: function () nothing {\n    x: @bit8 = use2(0x1, 0x2, 0x3, 0x4)\n}\n";
        let (diags, hir) = check_hir(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let main = hir.funcs.iter().find(|f| f.name == "main").expect("main");
        let outs: Vec<TempId> = main
            .body
            .stmts
            .iter()
            .filter_map(|s| match s {
                HirStmt::If { out: Some(t), .. } => Some(*t),
                _ => None,
            })
            .collect();
        assert!(
            outs.len() >= 2,
            "expected two inlined `or` joins, got {outs:?}"
        );
        let unique: std::collections::HashSet<TempId> = outs.iter().copied().collect();
        assert_eq!(
            unique.len(),
            outs.len(),
            "inlined joins must use distinct temps: {outs:?}"
        );
    }

    #[test]
    fn inline_call_as_or_operand_places_prelude_in_branch() {
        // `a or inner(b, c)`: `inner`'s prelude must land in the outer `or`'s
        // branch block, not the outer block. `outer` is itself an inline body
        // and never reaches `hir`; it is expanded into `main`, so the outer
        // `or` join is inspected there.
        let src = "inner: function (a: @bit8, b: @bit8) @bit8 => a or b\n\
                   outer: function (a: @bit8, b: @bit8, c: @bit8) @bit8 => a or inner(b, c)\n\
                   main: function () nothing {\n    r: @bit8 = outer(0x1, 0x2, 0x3)\n}\n";
        let (diags, hir) = check_hir(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let main = hir.funcs.iter().find(|f| f.name == "main").expect("main");
        let outer_if = main
            .body
            .stmts
            .iter()
            .find_map(|s| match s {
                HirStmt::If {
                    out: Some(t),
                    then,
                    els,
                    ..
                } => Some((*t, then, els)),
                _ => None,
            })
            .expect("outer `or` join");
        let nested = |block: &lilac_hir::HirBlock| {
            block
                .stmts
                .iter()
                .any(|s| matches!(s, HirStmt::If { out: Some(_), .. }))
        };
        assert!(
            nested(outer_if.1) || nested(outer_if.2),
            "inner prelude must be nested inside the outer `or` branch"
        );
    }

    #[test]
    fn inline_control_flow_in_constant_is_e030() {
        let src = "or2: function (a: @bit8, b: @bit8) @bit8 => a or b\n\
                   c: @bit8 = or2(0x1, 0x2)\n";
        let diags = check_src(src);
        assert!(has_code(&diags, "E030"), "{diags:?}");
    }

    #[test]
    fn overloaded_hook_probe_does_not_double_emit_prelude() {
        // Two same-arity overloads force `select_hook_overload` to probe the
        // argument; the inline `or` prelude must be emitted exactly once.
        let src = "@pick: function (x: @bit8) @bit8 => x\n\
                   @pick: function (x: @bit16) @bit16 => x\n\
                   or2: function (a: @bit8, b: @bit8) @bit8 => a or b\n\
                   main: function () nothing {\n    r: @bit8 = @pick(or2(0x1, 0x2))\n}\n";
        let (diags, hir) = check_hir(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let main = hir.funcs.iter().find(|f| f.name == "main").expect("main");
        let joins = main
            .body
            .stmts
            .iter()
            .filter(|s| matches!(s, HirStmt::If { out: Some(_), .. }))
            .count();
        assert_eq!(joins, 1, "the inline `or` prelude must be emitted once");
    }

    #[test]
    fn if_any_bit_width_ok() {
        let diags =
            check_src("main: function () nothing {\n    x: @bit32 = 0x1\n    if x {\n    }\n}\n");
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn capture_binds_by_name() {
        let diags = check_src(
            "get_multi: function () condition: @bit32, result: @bit32 {\n    condition = 0x1\n    result = 0x2\n}\nmain: function () nothing {\n    if get_multi() -> result: @bit32 {\n    }\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn unknown_capture_is_e022() {
        let diags = check_src(
            "get_multi: function () condition: @bit32, result: @bit32 {\n    condition = 0x1\n    result = 0x2\n}\nmain: function () nothing {\n    if get_multi() -> nope: @bit32 {\n    }\n}\n",
        );
        assert!(has_code(&diags, "E022"), "{diags:?}");
    }

    #[test]
    fn stray_break_is_e022() {
        let diags = check_src("main: function () nothing {\n    break\n}\n");
        assert!(has_code(&diags, "E022"), "{diags:?}");
    }

    #[test]
    fn loop_if_checks() {
        let diags =
            check_src("main: function () nothing {\n    loop if 0x0 {\n    } else {\n    }\n}\n");
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn if_value_branch_converts() {
        // Branch values convert toward the expected type like `let` values.
        let (hir, diags) = check_hir_src(
            "@conversion: function (from: @bit32) to: @bit8 {\n    to = @truncate(from)\n}\nmain: function () nothing {\n    a: @bit32 = 0x1\n    x: @bit8 = if 0x1 a else 0x0\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        let main = hir
            .funcs
            .iter()
            .find(|f| f.name == "main")
            .expect("has main");
        let HirStmt::If { then, .. } = &main.body.stmts[1] else {
            panic!("expected join, got {:?}", main.body.stmts[1]);
        };
        let HirStmt::Assign { value, .. } = &then.stmts[0] else {
            panic!("expected join assign, got {:?}", then.stmts[0]);
        };
        let lilac_hir::HirExprKind::Call { callee, .. } = &value.kind else {
            panic!("expected conversion call, got {:?}", value.kind);
        };
        let lilac_hir::HirCallee::Hook { name, .. } = callee else {
            panic!("expected hook call, got {callee:?}");
        };
        assert!(name.contains("@conversion"), "{name}");
    }

    #[test]
    fn if_value_branch_mismatch_is_e043() {
        let diags = check_src(
            "main: function () nothing {\n    a: @bit32 = 0x1\n    x: @bit8 = if 0x1 a else 0x0\n}\n",
        );
        assert!(has_code(&diags, "E043"), "{diags:?}");
    }

    #[test]
    fn match_value_branch_converts() {
        let (hir, diags) = check_hir_src(
            "@conversion: function (from: @bit32) to: @bit8 {\n    to = @truncate(from)\n}\nmain: function () nothing {\n    a: @bit32 = 0x1\n    variable: @bit8 = match 0x0 {\n        0x1 => a\n    } else 0x3\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        let main = hir
            .funcs
            .iter()
            .find(|f| f.name == "main")
            .expect("has main");
        assert_eq!(main.body.stmts.len(), 3);
    }

    #[test]
    fn match_tag_capture_payload() {
        let diags = check_src(
            "OptionalBit32: enum {\n    Some: @bit32\n    None\n}\nmain: function () nothing {\n    number: OptionalBit32 = .Some(0x1)\n    match number {\n        .Some -> some: @bit32 {\n        }\n        .None => return\n    }\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn match_capture_mismatch_is_e010() {
        let diags = check_src(
            "OptionalBit32: enum {\n    Some: @bit32\n    None\n}\nmain: function () nothing {\n    number: OptionalBit32 = .Some(0x1)\n    match number {\n        .Some -> some: @bit16 {\n        }\n        .None => return\n    }\n}\n",
        );
        assert!(has_code(&diags, "E010"), "{diags:?}");
    }

    #[test]
    fn capture_on_bare_tag_is_e022() {
        let diags = check_src(
            "OptionalBit32: enum {\n    Some: @bit32\n    None\n}\nmain: function () nothing {\n    number: OptionalBit32 = .None\n    match number {\n        .Some -> some: @bit32 {\n        }\n        .None -> x: @bit8 {\n        }\n    }\n}\n",
        );
        assert!(has_code(&diags, "E022"), "{diags:?}");
    }

    #[test]
    fn match_value_unifies_branches() {
        let diags = check_src(
            "main: function () nothing {\n    variable: @bit32 = match 0x0 {\n        0x1 => 0x1\n    } else 0x3\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn and_short_circuits() {
        let src = "main: function () nothing {\n    x: @bit8 = 0x0 and 0x1\n}\n";
        let (hir, diags) = check_hir_src(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let main = hir
            .funcs
            .iter()
            .find(|f| f.name == "main")
            .expect("has main");
        assert_eq!(main.body.stmts.len(), 2);
        let HirStmt::If { out, .. } = &main.body.stmts[0] else {
            panic!("expected join, got {:?}", main.body.stmts[0]);
        };
        assert!(out.is_some());
        let HirStmt::Let { value, .. } = &main.body.stmts[1] else {
            panic!("expected let, got {:?}", main.body.stmts[1]);
        };
        assert!(
            matches!(value.kind, lilac_hir::HirExprKind::Temp(_)),
            "expected temp use, got {:?}",
            value.kind
        );
    }

    #[test]
    fn and_or_require_bit8() {
        let diags = check_src(
            "main: function () nothing {\n    y: @bit32 = 0x2\n    x: @bit8 = 0x1 and y\n}\n",
        );
        assert!(has_code(&diags, "E010"), "{diags:?}");
    }

    #[test]
    fn compile_error_recorded_silent() {
        let diags = check_src(
            "main: function () nothing {\n    if 0x1 {\n    } else {\n        @compile_error(\"dead\")\n    }\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn compile_error_value_position_silent() {
        // Folding (Task 9) decides liveness; checking stays silent.
        let diags =
            check_src("main: function () nothing {\n    x: @bit32 = @compile_error(\"boom\")\n}\n");
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn loop_break_checks() {
        let diags = check_src("main: function () nothing {\n    loop {\n        break\n    }\n}\n");
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn fstring_desugars_to_format_args() {
        let diags = check_src("main: function () nothing {\n    x: @format_args = $\"a\"\n}\n");
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn fstring_hole_missing_conversion_is_e043() {
        let diags =
            check_src("main: function () nothing {\n    name: @bit32 = 0x1\n    x: @format_args = $\"{name}\"\n}\n");
        assert!(has_code(&diags, "E043"), "{diags:?}");
    }

    #[test]
    fn fstring_piece_cap_is_e061() {
        let mut holes = String::new();
        for _ in 0..17 {
            holes.push_str("{name}");
        }
        let src = format!(
            "main: function () nothing {{\n    name: @byte_buffer = \"x\"\n    x: @format_args = $\"{holes}\"\n}}\n"
        );
        let diags = check_src(&src);
        assert!(has_code(&diags, "E061"), "{diags:?}");
    }

    #[test]
    fn fstring_desugar_emits_new_then_push() {
        let (hir, diags) =
            check_hir_src("main: function () nothing {\n    x: @format_args = $\"a{\"z\"}\"\n}\n");
        assert!(!diags.has_errors(), "{diags:?}");
        let main = hir
            .funcs
            .iter()
            .find(|f| f.name == "main")
            .expect("has main");
        let HirStmt::Let { name, value, .. } = &main.body.stmts[0] else {
            panic!("expected let, got {:?}", main.body.stmts[0]);
        };
        assert_eq!(name, "__f");
        assert!(
            matches!(
                value.kind,
                lilac_hir::HirExprKind::Call {
                    callee: lilac_hir::HirCallee::Intrinsic { ref name },
                    ..
                } if name == "@format_args_new"
            ),
            "expected @format_args_new, got {:?}",
            value.kind
        );
        let HirStmt::ExprStmt(push) = &main.body.stmts[1] else {
            panic!("expected push, got {:?}", main.body.stmts[1]);
        };
        assert!(
            matches!(
                push.kind,
                lilac_hir::HirExprKind::Call {
                    callee: lilac_hir::HirCallee::Intrinsic { ref name },
                    ..
                } if name == "@format_args_push"
            ),
            "expected @format_args_push, got {:?}",
            push.kind
        );
    }

    #[test]
    fn nested_private_object_field_resolves() {
        // Regression: a field wrapped in nested `private` blocks must flatten
        // into the object's member fields, not be routed as a top-level item
        // (which silently dropped it and produced a spurious `E021`).
        let src = "Object: object {\n    private {\n        private {\n            f: @bit8\n        }\n    }\n\n    @init: function () result: Object {\n        result = Object {\n            f = 0x1\n        }\n    }\n\n    get: function (instance: ref Object) result: @bit8 {\n        result = instance.f\n    }\n}\n\nmain: function () nothing {\n    variable: Object = Object()\n    value: @bit8 = variable.get()\n}\n";
        let diags = check_src(src);
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn nested_private_object_field_is_e026_outside() {
        let src = "Object: object {\n    private {\n        private {\n            f: @bit8\n        }\n    }\n\n    @init: function () result: Object {\n        result = Object {\n            f = 0x1\n        }\n    }\n}\n\nmain: function () nothing {\n    variable: Object = Object()\n    value: @bit8 = variable.f\n}\n";
        let diags = check_src(src);
        assert!(has_code(&diags, "E026"), "{diags:?}");
        assert!(!has_code(&diags, "E021"), "{diags:?}");
    }

    #[test]
    fn tolerant_check_poisons_signatureless_definition() {
        // A definition whose signature could not be collected (tolerant mode
        // reaches `check_bodies` with errors present) is poisoned rather than
        // `bug!`-panicking, and a sibling definition still yields checked HIR
        // with name/type sites.
        let src = "helper: function () nothing {\n}\nmain: function () nothing {\n    other()\n}\nother: function () nothing {\n}\n";
        let mut sm = SourceMap::new();
        let file = sm.add_file("test.lilac", src);
        let (tokens, lex_errors) = Lexer::lex_all(src, file);
        let mut diags = Diags::new();
        for error in lex_errors {
            diags.emit(error);
        }
        let mut parser = Parser::new(tokens, file);
        let program = parser.parse_program(&mut diags).expect("source parses");
        let module = lilac_resolve::collect(&program, &mut diags);
        let project = lilac_resolve::ResolvedProject::single(module, file);
        let mut types = Types::new();
        let mut sigs = super::super::collect_sigs(&project, &program, &mut types, &mut diags);
        assert!(!diags.has_errors(), "{diags:?}");
        let helper = project
            .all_items()
            .find(|(name, _)| *name == "helper")
            .map(|(_, item)| item.id)
            .expect("helper resolved");
        sigs.funcs.remove(&helper);
        let hir = check_bodies(&program, &project, &sigs, &mut types, &mut diags, 64);
        assert!(
            !hir.uses.is_empty(),
            "the sibling call site still yields uses"
        );
    }

    #[test]
    fn tolerant_check_poisons_poison_signature() {
        // A signature whose type failed to parse is recorded with poison slots
        // and is not usable: its body is poisoned rather than checked.
        let src =
            "broken: function (x: ) nothing {\n    other()\n}\nother: function () nothing {\n}\n";
        let mut sm = SourceMap::new();
        let file = sm.add_file("test.lilac", src);
        let (tokens, lex_errors) = Lexer::lex_all(src, file);
        let mut diags = Diags::new();
        for error in lex_errors {
            diags.emit(error);
        }
        let mut parser = Parser::new(tokens, file);
        let program = parser.parse_program(&mut diags).expect("source parses");
        let module = lilac_resolve::collect(&program, &mut diags);
        let project = lilac_resolve::ResolvedProject::single(module, file);
        let mut types = Types::new();
        let sigs = super::super::collect_sigs(&project, &program, &mut types, &mut diags);
        let hir = check_bodies(&program, &project, &sigs, &mut types, &mut diags, 64);
        let broken = hir
            .funcs
            .iter()
            .find(|f| f.name == "broken")
            .expect("the poisoned definition still has HIR");
        assert_eq!(broken.params[0].ty.id, lilac_ty::TypeId::ERROR);
        assert!(
            matches!(
                broken.body.stmts.as_slice(),
                [lilac_hir::HirStmt::ExprStmt(value)]
                    if matches!(value.kind, lilac_hir::HirExprKind::Poison)
            ),
            "the body is the poison sentinel: {:?}",
            broken.body.stmts
        );
    }
}
