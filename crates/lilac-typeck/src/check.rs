//! Body checking: statements and expressions with expected types.

use std::collections::{HashMap, HashSet};

use lilac_ast::{BinOp, Expr, Item, ItemKind, Program, Stmt, Type as AstType};
use lilac_error::{Diagnostic, Diags, ErrorGuaranteed};
use lilac_hir::{
    HirArm, HirBlock, HirCallee, HirConst, HirExpr, HirExprKind, HirField, HirFunc, HirParam,
    HirPattern, HirProgram, HirStmt, NameSite, TempId,
};
use lilac_resolve::{DefId, ResItem, ResKind, ResolvedProject, Visibility};
use lilac_span::{FileId, Span};
use lilac_ty::{FuncSig, TypeId, TypeRef, Types};

use super::{lookup_intrinsic, lookup_one, qualify_name, resolve_type, Intrinsic, Sigs};

mod generics;
use generics::FuncInstance;

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
    /// A checked `=>` body: parameter names plus the value (inlined at calls).
    Inline {
        /// Parameter names in order (for call-site substitution).
        param_names: Vec<String>,
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
) -> HirProgram {
    let mut checker = Checker {
        program,
        project,
        file: FileId(u32::MAX),
        sigs,
        types,
        diags,
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
    }
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
    checked: HashSet<DefId>,
    fields: HashMap<(String, String), AstType>,
    hir: Vec<HirFunc>,
    /// Checked module constants (inlined wherever used).
    consts: Vec<HirConst>,
    /// Drop hooks by type display name (mangled `@on_drop` callees).
    drops: HashMap<String, String>,
    /// Checked `=>` bodies by function: parameter names plus the value HIR.
    /// Inline functions never reach `hir` (every call substitutes them).
    inline_bodies: HashMap<DefId, (Vec<String>, HirExpr)>,
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
    /// Specialized `=>` bodies by instance key (generic inline functions).
    instance_inline: HashMap<(DefId, Vec<TypeRef>), (Vec<String>, HirExpr)>,
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
        if let ResKind::Object { members, .. } = &item.kind {
            if members.values().any(|member| member.id == def) {
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
                    ..
                },
            ) = (&ast.kind, &resolved.kind)
            {
                let grandparent = Some((ast.name.clone(), resolved.id));
                let member_scope = |member: &Item| resolved_members.get(&member.name).cloned();
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

/// Whether HIR contains a branch-join temp: control flow inside a value
/// that must stay straight-line (module constants and `=>` bodies fold in
/// M3, so they stay `E030` here).
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

/// Width in bits of a bit container (callers guarantee `is_bit` first).
fn bit_width(types: &Types, id: TypeId) -> u32 {
    match types.kind(id) {
        lilac_ty::TypeKind::Bit8 => 8,
        lilac_ty::TypeKind::Bit16 => 16,
        lilac_ty::TypeKind::Bit32 => 32,
        lilac_ty::TypeKind::Bit64 | lilac_ty::TypeKind::BitNative => 64,
        _ => lilac_error::bug!(Span::dummy(), "bit width of non-bit type"),
    }
}

/// True for hex/bin literals (`0x`/`0X`/`0b`/`0B`), whose bits are fully
/// known; decimals are `NumberLiteral`s typed as `@byte_buffer`.
fn is_lit_literal(bytes: &str) -> bool {
    bytes.starts_with("0x")
        || bytes.starts_with("0X")
        || bytes.starts_with("0b")
        || bytes.starts_with("0B")
}

/// Number of significant bits of a hex/bin literal (with `_` separators),
/// ignoring leading zeros; decimals (and malformed digits) return `None`.
///
/// Counting directly from the digit string keeps a literal wider than `u64`
/// (e.g. `0x1_0000_0000_0000_0000`) faithful, so a `u64` parse overflow can no
/// longer hide an out-of-range value from the `E060` check.
fn lit_bits(bytes: &str) -> Option<u32> {
    let digits = bytes.replace('_', "");
    let (digit_bits, rest) = if let Some(hex) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        (4u32, hex)
    } else {
        let bin = digits
            .strip_prefix("0b")
            .or_else(|| digits.strip_prefix("0B"))?;
        (1u32, bin)
    };
    let rest = rest.trim_start_matches('0');
    let lead = rest.chars().next()?.to_digit(16)?;
    let len = u32::try_from(rest.len()).ok()?;
    Some((len - 1) * digit_bits + (32 - lead.leading_zeros()))
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

    /// Checks one object member, flattening `private { … }` blocks so a private
    /// member body is checked in its object's context.
    fn check_member(
        &mut self,
        member: &Item,
        resolved_members: &HashMap<String, ResItem>,
        owner: &Item,
        owner_def: DefId,
    ) {
        if let ItemKind::Private { items } = &member.kind {
            for inner in items {
                self.check_member(inner, resolved_members, owner, owner_def);
            }
            return;
        }
        if let Some(member_resolved) = resolved_members.get(&member.name) {
            let parent = Some((owner.name.clone(), owner_def));
            self.check_item(member, member_resolved, parent);
        }
    }

    fn check_item(&mut self, item: &Item, resolved: &ResItem, parent: Option<(String, DefId)>) {
        self.file = item.span.file;
        match &item.kind {
            ItemKind::Func(decl) => self.check_func(item, resolved, decl, parent),
            ItemKind::Object { members, .. } => {
                if let ResKind::Object {
                    members: resolved_members,
                    generic_params,
                    ..
                } = &resolved.kind
                {
                    if !generic_params.is_empty() {
                        // A generic object body is a template: members (methods,
                        // in-body hooks) carry `Param` slots and are specialized
                        // per instantiation (M4 Tasks 4-5). Checking or emitting
                        // them as concrete items would leak `Param` into HIR and
                        // trip the verifier (`E070`), so skip the whole body.
                        return;
                    }
                    for member in members {
                        self.check_member(member, resolved_members, item, resolved.id);
                    }
                    if let Some(drop) = resolved_members.get("@on_drop") {
                        if matches!(drop.kind, ResKind::Hook { .. })
                            && self.func_sig(drop.id).is_some()
                        {
                            let owner = self.qualify_local(item.span.file, &item.name);
                            let name = self.hook_name(&format!("{owner}.@on_drop"), drop.id);
                            self.drops.insert(owner, name);
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
            | ItemKind::Field { .. } => {}
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
        resolve_type(ty, self.types, self.project, self.file, self.diags, params)
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
                    if has_temp(&value) {
                        let _ = deferred(self.diags, span, "control flow in `=>` bodies", "M3");
                        CheckedBody::Rejected
                    } else {
                        if let Some((name, _, _)) = fctx.slots.first() {
                            fctx.assigned.insert(name.clone());
                        }
                        let param_names: Vec<String> =
                            decl.params.iter().map(|param| param.name.clone()).collect();
                        CheckedBody::Inline { param_names, value }
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
        let Some(sig) = self.func_sig(resolved.id) else {
            lilac_error::bug!(item.span, "missing signature for checked function");
        };
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
            CheckedBody::Inline { param_names, value } => {
                self.inline_bodies.insert(resolved.id, (param_names, value));
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
                    Ok(ty) => Ok((
                        ty,
                        HirExpr {
                            ty,
                            span: *span,
                            kind: HirExprKind::Member {
                                obj: Box::new(obj_hir),
                                field: field.clone(),
                            },
                        },
                    )),
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
            } => self.check_multilet(fctx, bindings, value, *span),
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
            Ok(ty) => (
                ty,
                HirExpr {
                    ty,
                    span,
                    kind: HirExprKind::Member {
                        obj: Box::new(obj_hir),
                        field: field.to_owned(),
                    },
                },
            ),
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
            Expr::Member { obj, field, span } => {
                self.check_member_read(fctx, obj, field, *span, expected)
            }
            Expr::Call {
                callee,
                generic_args,
                args,
                span,
            } => self.check_call_expr(fctx, callee, generic_args, args, *span, expected),
            Expr::BinOp { lhs, op, rhs, span } => self.check_binop_expr(fctx, lhs, *op, rhs, *span),
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
        let poison = (
            TypeRef {
                id: TypeId::ERROR,
                is_ref: false,
            },
            poison_expr(span),
        );
        let number = |id: TypeId| {
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
        };
        if !is_lit_literal(bytes) {
            return number(self.builtin_ty("@byte_buffer"));
        }
        match expected {
            Some(t) if is_bit(self.types, t) => {
                if let Some(bits) = lit_bits(bytes) {
                    let width = bit_width(self.types, t);
                    let max = if width == 64 {
                        u64::MAX
                    } else {
                        (1u64 << width) - 1
                    };
                    if bits > width {
                        self.diags.emit(
                            Diagnostic::error(
                                "E060",
                                format!(
                                    "value does not fit in {} (max 0x{max:x})",
                                    self.types.display(t)
                                ),
                            )
                            .label(
                                span,
                                true,
                                "literal overflows".to_owned(),
                            ),
                        );
                        return poison;
                    }
                }
                number(t)
            }
            _ => {
                self.diags.emit(
                    Diagnostic::error("E024", "number literal needs an expected bit type").label(
                        span,
                        true,
                        "annotate the binding".to_owned(),
                    ),
                );
                poison
            }
        }
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
                    let right_hir = self.convert_if_needed(right, left.id, right_hir);
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
                                "no {} for ({}, {}); no conversion from {} to {}",
                                miss.hook,
                                self.types.display(miss.lhs),
                                self.types.display(miss.rhs),
                                self.types.display(miss.rhs),
                                self.types.display(miss.lhs),
                            ),
                        )
                        .label(
                            span,
                            true,
                            "operator and conversion search failed".to_owned(),
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
    /// width, literals take the anchor (or report `E024` without one).
    fn check_binop_sides(
        &mut self,
        fctx: &mut FuncCtx,
        lhs: &Expr,
        rhs: &Expr,
    ) -> ((TypeRef, HirExpr), (TypeRef, HirExpr)) {
        let poison = (
            TypeRef {
                id: TypeId::ERROR,
                is_ref: false,
            },
            poison_expr(lhs.span()),
        );
        let left_lit = matches!(lhs, Expr::Number(..));
        let right_lit = matches!(rhs, Expr::Number(..));
        if left_lit && right_lit {
            let _ = self.check_expr(fctx, lhs, None);
            return (poison.clone(), poison);
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
        let ResKind::Object { members, .. } = &item.kind else {
            return Err(self.diags.emit(
                Diagnostic::error("E022", format!("type `{name}` has no `@init` constructor"))
                    .label(span, true, "constructors need `@init`".to_owned()),
            ));
        };
        let Some(hook) = members.get("@init").cloned() else {
            return Err(self.diags.emit(
                Diagnostic::error("E022", format!("type `{name}` has no `@init` constructor"))
                    .label(span, true, "define `@init` or use `= { ... }`".to_owned()),
            ));
        };
        let ResKind::Hook { .. } = &hook.kind else {
            return Err(self.diags.emit(
                Diagnostic::error("E022", "`@init` must be a hook".to_owned()).label(
                    hook.span,
                    true,
                    "declared here".to_owned(),
                ),
            ));
        };
        if self.private_member_denied(&hook, item.id, span) {
            return Err(ErrorGuaranteed::new());
        }
        let Some(sig) = self.func_sig(hook.id) else {
            lilac_error::bug!(hook.span, "missing signature for `@init` hook");
        };
        let qualified = qualify_name(self.project, item.id, name);
        let obj_ty = {
            let id = self.types.intern_object(item.id);
            self.types.set_object_name(item.id, &qualified);
            id
        };
        if sig.returns.len() != 1 || sig.returns[0].id != obj_ty {
            return Err(self.diags.emit(
                Diagnostic::error("E022", "`@init` must return exactly its object type").label(
                    hook.span,
                    true,
                    "bad `@init` shape".to_owned(),
                ),
            ));
        }
        let hir_args = self.check_args(fctx, &sig, args, span)?;
        let hook_name = self.hook_name(&format!("{qualified}.@init"), hook.id);
        Ok(CallOutcome::Call(CheckedCall {
            outs: sig.returns.clone(),
            callee: HirCallee::Hook {
                name: hook_name,
                def: hook.id,
            },
            args: hir_args,
        }))
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
        let arity_ok: Vec<(DefId, FuncSig)> = overloads
            .iter()
            .filter(|(_, sig)| sig.params.len() == args.len())
            .cloned()
            .collect();
        let Some((first_def, first)) = arity_ok.first().cloned() else {
            return Err(self.diags.emit(
                Diagnostic::error(
                    "E022",
                    format!("no overload of `{name}` takes {} arguments", args.len()),
                )
                .label(span, true, "arity mismatch here".to_owned()),
            ));
        };
        if arity_ok.len() == 1 {
            let hir_args = self.check_args(fctx, &first, args, span)?;
            let name = self.hook_name(name, first_def);
            return Ok(CheckedCall {
                outs: first.returns.clone(),
                callee: HirCallee::Hook {
                    name,
                    def: first_def,
                },
                args: hir_args,
            });
        }
        let mut probed: Vec<Option<TypeRef>> = Vec::with_capacity(args.len());
        for arg in args {
            if matches!(arg, Expr::Number(..)) {
                probed.push(None);
                continue;
            }
            let (got, _) = self.check_expr(fctx, arg, None);
            probed.push(if is_poison(got) { None } else { Some(got) });
        }
        let winner = arity_ok
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
            .unwrap_or((first_def, first));
        let hir_args = self.check_args(fctx, &winner.1, args, span)?;
        let name = self.hook_name(name, winner.0);
        Ok(CheckedCall {
            outs: winner.1.returns.clone(),
            callee: HirCallee::Hook {
                name,
                def: winner.0,
            },
            args: hir_args,
        })
    }

    /// Mangled LIR name for a hook overload: the qualified hook name plus
    /// its signature (`@add:bit32:bit32`, `Parent.@init:ref_Object`).
    /// Overloads that collide here were already rejected as `E020` dupes.
    fn hook_name(&self, qual: &str, def: DefId) -> String {
        let Some(sig) = self.func_sig(def) else {
            lilac_error::bug!(Span::dummy(), "missing signature for hook name");
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

    /// Expands a call to an `=>` function by substituting the checked
    /// arguments for its parameters (textual substitution: an argument used
    /// twice evaluates twice). Recursive `=>` calls cannot inline.
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
        let Some((param_names, body_hir)) = self.inline_bodies.get(&def).cloned() else {
            return Err(ErrorGuaranteed::new());
        };
        let Some(sig) = self.func_sig(def) else {
            lilac_error::bug!(span, "missing signature for inlined function");
        };
        if sig.returns.len() != 1 {
            return Err(ErrorGuaranteed::new());
        }
        let hir_args = self.check_args(fctx, &sig, args, span)?;
        let map: HashMap<String, HirExpr> = param_names.into_iter().zip(hir_args).collect();
        let expanded = subst_ident(&body_hir, &map);
        Ok(CallOutcome::Inline(expanded, sig.returns[0]))
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

    /// Unifies same-width bit operands (literals take the width from siblings
    /// or the expected type); returns the unified width plus every argument's
    /// HIR in order.
    fn unify_width(
        &mut self,
        fctx: &mut FuncCtx,
        args: &[Expr],
        expected: Option<TypeId>,
        span: Span,
    ) -> Result<(TypeId, Vec<HirExpr>), ErrorGuaranteed> {
        let mut width = match expected {
            Some(t) if is_bit(self.types, t) => Some(t),
            _ => None,
        };
        let mut checked: Vec<Option<(TypeRef, HirExpr)>> = vec![None; args.len()];
        let mut failed = false;
        for (index, arg) in args.iter().enumerate() {
            if matches!(arg, Expr::Number(bytes, _) if is_lit_literal(bytes)) {
                continue;
            }
            let (got, hir) = self.check_expr(fctx, arg, None);
            if is_poison(got) {
                failed = true;
                continue;
            }
            if !is_bit(self.types, got.id) {
                self.diags.emit(
                    Diagnostic::error(
                        "E010",
                        format!(
                            "mismatched types: expected a bit container, found {}",
                            self.types.display(got.id),
                        ),
                    )
                    .label(arg.span(), true, "not a bit container".to_owned()),
                );
                failed = true;
                continue;
            }
            if width.is_none() {
                width = Some(got.id);
            }
            checked[index] = Some((got, hir));
        }
        let Some(width) = width else {
            self.diags.emit(
                Diagnostic::error("E024", "cannot infer the width here").label(
                    span,
                    true,
                    "annotate the binding".to_owned(),
                ),
            );
            return Err(ErrorGuaranteed::new());
        };
        let mut hir_args = Vec::with_capacity(args.len());
        for (index, arg) in args.iter().enumerate() {
            if let Some((got, hir)) = checked[index].take() {
                if got.id != width {
                    self.diags.emit(
                        Diagnostic::error(
                            "E010",
                            format!(
                                "mismatched widths: expected {}, found {}",
                                self.types.display(width),
                                self.types.display(got.id),
                            ),
                        )
                        .label(
                            arg.span(),
                            true,
                            "width mismatch here".to_owned(),
                        ),
                    );
                    failed = true;
                }
                hir_args.push(hir);
            } else {
                let (got, hir) = self.check_expr(fctx, arg, Some(width));
                if !is_poison(got) && got.id != width {
                    failed = true;
                }
                hir_args.push(hir);
            }
        }
        if failed {
            return Err(ErrorGuaranteed::new());
        }
        Ok((width, hir_args))
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
            Intrinsic::Arith | Intrinsic::Cmp | Intrinsic::Negate => {
                self.arith_out(fctx, intrinsic, args, span, expected)?
            }
            Intrinsic::AsInt | Intrinsic::AsFloat | Intrinsic::Truncate | Intrinsic::ExtendZero => {
                self.converting_out(fctx, intrinsic, args, span, expected)?
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

    fn arith_out(
        &mut self,
        fctx: &mut FuncCtx,
        intrinsic: Intrinsic,
        args: &[Expr],
        span: Span,
        expected: Option<TypeId>,
    ) -> Result<(Vec<TypeRef>, Vec<HirExpr>), ErrorGuaranteed> {
        let value = |id: TypeId| TypeRef { id, is_ref: false };
        if intrinsic == Intrinsic::Negate {
            self.expect_arity(span, "@negate", args, 1)?;
        }
        let (width, hir_args) = self.unify_width(fctx, args, expected, span)?;
        if args.is_empty() {
            let what = if intrinsic == Intrinsic::Cmp {
                "comparison"
            } else {
                "arithmetic"
            };
            return Err(self.diags.emit(
                Diagnostic::error("E022", format!("{what} needs operand values")).label(
                    span,
                    true,
                    "arity mismatch here".to_owned(),
                ),
            ));
        }
        if intrinsic == Intrinsic::Cmp {
            return Ok((vec![value(self.builtin_ty("@bit8"))], hir_args));
        }
        Ok((vec![value(width)], hir_args))
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
        let (name, want_buffer) = match intrinsic {
            Intrinsic::AsInt => ("@as_int", true),
            Intrinsic::AsFloat => ("@as_float", true),
            Intrinsic::Truncate => ("@truncate", false),
            _ => ("@extend_zero", false),
        };
        self.expect_arity(span, name, args, 1)?;
        let (got, hir) = self.check_expr(fctx, &args[0], None);
        if !is_poison(got) {
            let ok = if want_buffer {
                got.id == self.builtin_ty("@byte_buffer")
            } else {
                is_bit(self.types, got.id)
            };
            if !ok {
                let (want, note) = if want_buffer {
                    ("@byte_buffer".to_owned(), "digit buffer expected")
                } else {
                    ("a bit container".to_owned(), "not a bit container")
                };
                self.diags.emit(
                    Diagnostic::error(
                        "E010",
                        format!(
                            "mismatched types: expected {want}, found {}",
                            self.types.display(got.id),
                        ),
                    )
                    .label(args[0].span(), true, note.to_owned()),
                );
                return Err(ErrorGuaranteed::new());
            }
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
        let sigs = super::super::collect_sigs(&project, &program, &mut types, &mut diags, true);
        check_bodies(&program, &project, &sigs, &mut types, &mut diags);
        diags
    }

    fn has_code(diags: &Diags, code: &str) -> bool {
        diags.diagnostics().iter().any(|d| d.code == code)
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
        let sigs = super::super::collect_sigs(&project, &program, &mut types, &mut diags, true);
        let hir = check_bodies(&program, &project, &sigs, &mut types, &mut diags);
        (diags, hir)
    }

    #[test]
    fn bare_ident_use_is_recorded() {
        let src = "pi: @bit32 = 0x2A\nmain: function () nothing {\n    x: @bit32 = pi\n}\n";
        let (diags, hir) = check_hir(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let lo = src.rfind("pi").expect("use site") as u32;
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
        let lo = src.rfind("foo").expect("call") as u32;
        assert!(
            hir.uses
                .iter()
                .any(|s| s.span.lo.0 == lo && s.def != lilac_resolve::DefId::ERROR),
            "{:?}",
            hir.uses
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
    fn bare_literal_is_e024() {
        let diags = check_src("main: function () nothing {\n    0x1\n}\n");
        assert!(has_code(&diags, "E024"), "{diags:?}");
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
    fn decimal_literal_is_byte_buffer_not_bit() {
        let diags = check_src("main: function () nothing {\n    x: @bit8 = 5\n}\n");
        assert!(has_code(&diags, "E043"), "{diags:?}");
        let msgs = messages(&diags);
        assert!(msgs.iter().any(|m| m.contains("@byte_buffer")), "{msgs:?}");
    }

    #[test]
    fn as_int_of_decimal_checks() {
        let diags = check_src("main: function () nothing {\n    x: @bit8 = @as_int(5)\n}\n");
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
    fn decimal_in_arith_is_e010() {
        let diags = check_src("main: function () nothing {\n    x: @bit32 = @i_add(5, 6)\n}\n");
        assert!(has_code(&diags, "E010"), "{diags:?}");
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
            "main: function () nothing {\n    add: function (@bit32) @bit32 = addOne\n    value: @bit32 = 0x0\n    value = add(value)\n    add = addTwo\n    value = add(value)\n}\naddOne: function (value: @bit32) result: @bit32 {\n    result = @i_add(value, 0x1)\n}\naddTwo: function (value: @bit32) result: @bit32 {\n    result = @i_add(value, 0x2)\n}\n",
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
            "Object: object {\n    member: @bit32\n    add: function (num: @bit32) result: @bit32 {\n        result = @i_add(instance.member, num)\n    }\n}\nmain: function () nothing {\n    variable: Object = Object {\n        member = 0x1\n    }\n    member: @bit32 = Object.add(0x2)\n}\n",
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

    use lilac_hir::{HirExprKind, HirProgram, HirStmt};

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
        let sigs = super::super::collect_sigs(&project, &program, &mut types, &mut diags, true);
        let hir = check_bodies(&program, &project, &sigs, &mut types, &mut diags);
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
    fn inline_substituted_nonrecursive() {
        let src = "addOne: function (value: @bit32) @bit32 => @i_add(value, 0x1)\nmain: function () nothing {\n    variable: @bit32 = addOne(0x1)\n}\n";
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
        assert_eq!(name, "@i_add");
        assert_eq!(args.len(), 2);
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
}
