//! Typed HIR: concrete, checked functions.
//!
//! Every expression carries its type and span. Locals are numbered per
//! function ([`LocalId`]); calls name their callee by identity
//! ([`FuncKey`]: a definition plus its generic arguments). Generic templates
//! never appear: each used `(definition, arguments)` pair is its own
//! [`HirFunc`]. Blocks are scopes: a `Let` inside a block is dropped when the
//! block exits (lowering emits the drops).
//!
//! Places are [`HirExprKind::Local`], [`HirExprKind::Field`] and
//! [`HirExprKind::Deref`]. Lowering copies a place (running copy hooks) when
//! its value moves into a new owner; every other expression is a fresh value
//! that moves.

pub mod intrinsics;

use std::collections::HashMap;

use lilac_resolve::DefId;
use lilac_span::Span;
use lilac_ty::{TypeId, TypeRef};

pub use intrinsics::Intrinsic;

/// The platform hooks the compiler calls on the program's behalf, with the
/// fixed symbols they are emitted under: the entry stub calls `@entry` then
/// `@exit(0)`, and the panic handler writes through `@compiler_print` then
/// calls `@exit(101)`. Each is top-level and defined at most once.
pub const PLATFORM_HOOKS: [(&str, &str); 3] = [
    ("@entry", ".lilac.entry"),
    ("@exit", ".lilac.exit"),
    ("@compiler_print", ".lilac.compiler_print"),
];

/// The fixed symbol of platform hook `name`, if it is one.
#[must_use]
pub fn platform_hook_symbol(name: &str) -> Option<&'static str> {
    PLATFORM_HOOKS
        .iter()
        .find(|(hook, _)| *hook == name)
        .map(|(_, symbol)| *symbol)
}

/// A function instance: a definition plus its generic arguments (empty for
/// non-generic functions; for a method of a generic type, the type's
/// arguments come first, then the method's own).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct FuncKey {
    /// The function or hook definition.
    pub def: DefId,
    /// Concrete generic arguments.
    pub args: Vec<TypeRef>,
}

impl FuncKey {
    /// A non-generic function.
    #[must_use]
    pub fn plain(def: DefId) -> Self {
        Self {
            def,
            args: Vec::new(),
        }
    }
}

/// A checked program.
#[derive(Clone, Debug, Default)]
pub struct HirProgram {
    /// Every concrete function instance.
    pub funcs: Vec<HirFunc>,
    /// Checked module constants (lowering inlines them at each use).
    pub consts: HashMap<DefId, HirExpr>,
    /// Checked global variables.
    pub globals: HashMap<DefId, HirGlobal>,
    /// Layout of every concrete object/enum type that occurs in the program.
    pub layouts: HashMap<TypeId, Layout>,
    /// RAII hooks per concrete type.
    pub hooks: HashMap<TypeId, TypeHooks>,
    /// Resolved item uses (editor hover / go-to-definition).
    pub uses: Vec<NameSite>,
    /// Non-item semantic-token sites.
    pub semantics: Vec<SemanticSite>,
    /// Every named local's declaration and uses, each linked to its
    /// declaration (editor references and rename).
    pub locals: Vec<LocalSite>,
    /// Every path head that names a `using … as` alias, linked to the
    /// alias's declaration (editor definition, references and rename).
    pub alias_uses: Vec<AliasSite>,
    /// Statements no path reaches: the rest of a block after a `return`,
    /// `break`, `continue`, `@panic` or `@trap`, one span per block (an
    /// editor hint; they are still checked and lowered).
    pub unreachable: Vec<Span>,
    /// Expression statements whose value has no effect (no call anywhere
    /// in them, like `5 + 5`): an editor hint; they are still lowered.
    pub no_effect: Vec<Span>,
    /// Every tag (`.Name`, a `.` mid-edit) with the variants it may name
    /// (editor completion).
    pub tags: Vec<TagSite>,
    /// Every member name after a `.` (`x.field`, `Type.method`,
    /// `Ns.item`, a `.` mid-edit) with what it is looked up in (editor
    /// completion).
    pub members: Vec<MemberSite>,
}

impl HirExpr {
    /// Whether evaluating this has no effect: it calls nothing (operator
    /// hooks and intrinsics are calls too) and is not a `@compile_error`.
    #[must_use]
    pub fn has_no_effect(&self) -> bool {
        let mut effect = false;
        self.walk(&mut |e| {
            effect |= matches!(
                e.kind,
                HirExprKind::Call { .. } | HirExprKind::CompileError { .. } | HirExprKind::Poison
            );
        });
        !effect
    }
}

/// A concrete type's layout.
#[derive(Clone, Debug)]
pub enum Layout {
    /// Object fields in declaration order.
    Object {
        /// `(name, type)` per field.
        fields: Vec<(String, TypeRef)>,
    },
    /// Enum variants in declaration order and the tag container.
    Enum {
        /// `(name, payload)` per variant.
        variants: Vec<(String, Option<TypeRef>)>,
        /// The tag's bit container: the smallest `@bitN` holding the count.
        tag: TypeId,
    },
}

/// One checked global variable.
#[derive(Clone, Debug)]
pub struct HirGlobal {
    /// Declared type.
    pub ty: TypeRef,
    /// Initializer (`None` starts zeroed); lowering requires it constant.
    pub value: Option<HirExpr>,
    /// Declaration span.
    pub span: Span,
}

/// The RAII hooks bound to one concrete type.
#[derive(Clone, Debug, Default)]
pub struct TypeHooks {
    /// `@on_drop(ref T)`.
    pub drop: Option<FuncKey>,
    /// `@on_copy(ref T) -> T`.
    pub copy: Option<FuncKey>,
    /// `@on_override(ref T, ref T)`.
    pub override_: Option<FuncKey>,
}

/// What a non-item semantic-token site names.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SemanticKind {
    /// A local binding (declaration and each use).
    Local,
    /// A parameter (declaration name and each use).
    Param,
    /// A user object/enum name in type position.
    UserType,
    /// An enum variant (declaration name and each use).
    Variant,
    /// A builtin type name (`@bit8`, `unknown`, ...).
    BuiltinType,
    /// A compiler intrinsic call name (`@i_add_8`, `@load_u8`, ...).
    BuiltinFunc,
    /// A namespace in a path or a `using` (`Os` in `Os.write`), or a
    /// `using … as` alias.
    Namespace,
}

/// One semantic-token site.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SemanticSite {
    /// The name token's span.
    pub span: Span,
    /// What the name is.
    pub kind: SemanticKind,
}

/// One tag position: a `.Name` value or pattern, or a bare `.` mid-edit.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TagSite {
    /// The tag, from its `.` through its name.
    pub span: Span,
    /// The variants it may name, each with its payload type displayed:
    /// the expected enum's, empty when no enum is expected. In a `match`
    /// pattern, the variants other arms name are left out.
    pub variants: Vec<(String, Option<String>)>,
}

/// One member name after a `.` and what it is looked up in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MemberSite {
    /// The name (empty, just after the `.`, mid-edit).
    pub span: Span,
    /// What the name is a member of.
    pub receiver: MemberReceiver,
}

/// What a member name after a `.` is looked up in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MemberReceiver {
    /// A value: its fields and methods.
    Value(TypeRef),
    /// An object or enum written as a type: its static functions and
    /// variants.
    Type(DefId),
    /// A namespace: its items and child namespaces.
    Namespace(lilac_resolve::NsId),
}

/// One local or parameter name: its declaration, or a use of it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct LocalSite {
    /// The name token.
    pub span: Span,
    /// The declaration's name token (`span` itself at the declaration).
    pub decl: Span,
    /// The local's type.
    pub ty: TypeRef,
    /// Where the name is in scope: its block (the function for a
    /// parameter); the name is visible from `decl` to the scope's end.
    pub scope: Span,
    /// What declares it.
    pub kind: LocalKind,
}

/// What declares a local name.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum LocalKind {
    /// A `let`, a capture or a binding in a body.
    Local,
    /// A parameter of a function with a body.
    Param,
    /// A named result, or a parameter of a function without a body
    /// (`extern`): a name that is part of the signature only.
    Signature,
}

/// One use of a `using … as` alias: the head of a path.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct AliasSite {
    /// The name token.
    pub span: Span,
    /// The alias's name token in its `using` directive.
    pub decl: Span,
}

/// One resolved item use.
#[derive(Clone, Copy, Debug)]
pub struct NameSite {
    /// The name token.
    pub span: Span,
    /// The definition it resolved to.
    pub def: DefId,
}

/// Local slot identity within one function.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct LocalId(pub u32);

/// One local slot: parameter, return slot, `let` binding, or hidden temp.
#[derive(Clone, Debug)]
pub struct HirLocal {
    /// Source name (`""` for anonymous slots and hidden temps).
    pub name: String,
    /// Slot type.
    pub ty: TypeRef,
    /// Declaration span.
    pub span: Span,
}

/// One concrete function.
#[derive(Clone, Debug)]
pub struct HirFunc {
    /// Identity (definition plus generic arguments).
    pub key: FuncKey,
    /// Parameter locals, in order.
    pub params: Vec<LocalId>,
    /// Return-slot locals, in order.
    pub returns: Vec<LocalId>,
    /// Every local, indexed by [`LocalId`].
    pub locals: Vec<HirLocal>,
    /// Body; `None` for `extern` declarations.
    pub body: Option<HirBlock>,
    /// `=>` function: always inlined by the backend.
    pub inline: bool,
    /// Whole-declaration span.
    pub span: Span,
}

impl HirFunc {
    /// The local `id` names.
    #[must_use]
    pub fn local(&self, id: LocalId) -> &HirLocal {
        &self.locals[id.0 as usize]
    }
}

/// A statement sequence and scope.
#[derive(Clone, Debug)]
pub struct HirBlock {
    /// Statements in order.
    pub stmts: Vec<HirStmt>,
    /// Whole-block span.
    pub span: Span,
}

/// Checked statements.
#[derive(Clone, Debug)]
pub enum HirStmt {
    /// `name: T = value`: initializes a fresh local owned by this block.
    Let {
        /// The local.
        local: LocalId,
        /// Initializer (already converted to the local's type).
        value: HirExpr,
    },
    /// `a: T, b: U = f()`: the call writes its outs straight into the locals.
    MultiLet {
        /// One local per bound out slot, in order (a prefix of the outs; the
        /// call's other outs are dropped at the end of the statement).
        locals: Vec<LocalId>,
        /// The call (an [`HirExprKind::Call`]).
        call: HirExpr,
    },
    /// `place = value` (runs `@on_override`, then drops the old value).
    Assign {
        /// Target place.
        place: HirExpr,
        /// New value (already converted to the place's type).
        value: HirExpr,
    },
    /// An expression evaluated for its effects.
    Expr(HirExpr),
    /// `return`.
    Return(Span),
    /// `break`.
    Break(Span),
    /// `continue`.
    Continue(Span),
    /// `if cond { } else { }`.
    If {
        /// A `@bit8` condition.
        cond: HirExpr,
        /// Taken branch.
        then: HirBlock,
        /// Other branch (empty when absent).
        els: HirBlock,
    },
    /// `loop { }`.
    Loop(HirBlock),
    /// `match`.
    Match {
        /// Inspected value (a place, or a value lowering spills).
        scrutinee: HirExpr,
        /// Arms in order.
        arms: Vec<HirArm>,
        /// `else` block (empty when absent).
        els: HirBlock,
    },
    /// A nested `{ }` scope.
    Block(HirBlock),
}

/// One statement `match` arm.
#[derive(Clone, Debug)]
pub struct HirArm {
    /// Tested pattern.
    pub pattern: HirPattern,
    /// Payload capture (a local initialized from the payload).
    pub capture: Option<LocalId>,
    /// Arm body.
    pub body: HirBlock,
}

/// One value `match` arm.
#[derive(Clone, Debug)]
pub struct HirValueArm {
    /// Tested pattern.
    pub pattern: HirPattern,
    /// Payload capture.
    pub capture: Option<LocalId>,
    /// Arm value.
    pub value: HirExpr,
}

/// A match pattern.
#[derive(Clone, Debug)]
pub enum HirPattern {
    /// An enum variant, by declaration index.
    Variant(u32),
    /// A literal compared bit-for-bit (source bytes).
    Const(String),
}

/// One checked expression.
#[derive(Clone, Debug)]
pub struct HirExpr {
    /// Type (with its `ref` flag).
    pub ty: TypeRef,
    /// Source span.
    pub span: Span,
    /// Shape.
    pub kind: HirExprKind,
}

/// A call target.
#[derive(Clone, Debug)]
pub enum HirCallee {
    /// A function instance.
    Func(FuncKey),
    /// A function value held in a local.
    Local(LocalId),
    /// A compiler intrinsic.
    Intrinsic(Intrinsic),
}

/// Checked expression shapes.
#[derive(Clone, Debug)]
pub enum HirExprKind {
    /// A local (place).
    Local(LocalId),
    /// Field `index` of an object place (place).
    Field {
        /// The object place.
        base: Box<HirExpr>,
        /// Field index in layout order.
        index: u32,
    },
    /// The place a `ref T` value points at (place of type `T`).
    Deref(Box<HirExpr>),
    /// Integer or float literal bits (source bytes, typed by `ty`).
    Number(String),
    /// String literal bytes (`@byte_buffer`).
    Str(Vec<u8>),
    /// Object literal: one value per field, in layout order.
    Struct(Vec<HirExpr>),
    /// Enum variant value.
    Variant {
        /// Variant index.
        index: u32,
        /// Payload value.
        payload: Option<Box<HirExpr>>,
    },
    /// A call: its type is its first out's type (the error type for none).
    Call {
        /// Target.
        callee: HirCallee,
        /// Arguments (already converted to parameter types).
        args: Vec<HirExpr>,
        /// The types of the outs after the first. Nothing binds them, so
        /// they are dropped at the end of the statement.
        rest: Vec<TypeRef>,
    },
    /// A function used as a value.
    FuncRef(FuncKey),
    /// `ref place`.
    RefOf(Box<HirExpr>),
    /// A pointer reinterpretation (`as ref unknown` / `as ref T`).
    Retype(Box<HirExpr>),
    /// `lhs == rhs` (bit equality, `@bit8`).
    Eq(Box<HirExpr>, Box<HirExpr>),
    /// `if c then x else y`.
    If {
        /// `@bit8` condition.
        cond: Box<HirExpr>,
        /// Value when true.
        then: Box<HirExpr>,
        /// Value when false.
        els: Box<HirExpr>,
    },
    /// `match` used as a value.
    Match {
        /// Inspected value.
        scrutinee: Box<HirExpr>,
        /// Arms.
        arms: Vec<HirValueArm>,
        /// Fallback value.
        els: Box<HirExpr>,
    },
    /// Short-circuit `and` (`true`) / `or` (`false`) over `@bit8`.
    Logic {
        /// `and` when true, `or` when false.
        and: bool,
        /// Left operand.
        lhs: Box<HirExpr>,
        /// Right operand.
        rhs: Box<HirExpr>,
    },
    /// `$"..."`: pieces pushed into a fresh `@format_args`.
    FormatArgs(Vec<HirExpr>),
    /// A module constant (inlined by lowering).
    Const(DefId),
    /// A global variable (place).
    Global(DefId),
    /// `@size_of(T)`.
    SizeOf(TypeRef),
    /// `@align_of(T)`.
    AlignOf(TypeRef),
    /// A `@compile_error` site (`E031` if lowered in a reachable function).
    CompileError {
        /// Message.
        msg: String,
        /// Message literal span.
        msg_span: Span,
    },
    /// Error recovery (never lowered).
    Poison,
}

impl HirExpr {
    /// A poison expression (error recovery; never lowered).
    #[must_use]
    pub fn poison(span: Span) -> Self {
        Self {
            ty: TypeRef::ERROR,
            span,
            kind: HirExprKind::Poison,
        }
    }

    /// Whether this expression denotes a place.
    #[must_use]
    pub fn is_place(&self) -> bool {
        matches!(
            self.kind,
            HirExprKind::Local(_)
                | HirExprKind::Field { .. }
                | HirExprKind::Deref(_)
                | HirExprKind::Global(_)
        )
    }

    /// Calls `f` on this expression and every sub-expression, depth-first.
    pub fn walk(&self, f: &mut dyn FnMut(&HirExpr)) {
        f(self);
        match &self.kind {
            HirExprKind::Field { base, .. }
            | HirExprKind::Deref(base)
            | HirExprKind::RefOf(base)
            | HirExprKind::Retype(base) => base.walk(f),
            HirExprKind::Struct(values) | HirExprKind::FormatArgs(values) => {
                for value in values {
                    value.walk(f);
                }
            }
            HirExprKind::Variant { payload, .. } => {
                if let Some(payload) = payload {
                    payload.walk(f);
                }
            }
            HirExprKind::Call { args, .. } => {
                for arg in args {
                    arg.walk(f);
                }
            }
            HirExprKind::Eq(lhs, rhs) | HirExprKind::Logic { lhs, rhs, .. } => {
                lhs.walk(f);
                rhs.walk(f);
            }
            HirExprKind::If { cond, then, els } => {
                cond.walk(f);
                then.walk(f);
                els.walk(f);
            }
            HirExprKind::Match {
                scrutinee,
                arms,
                els,
            } => {
                scrutinee.walk(f);
                for arm in arms {
                    arm.value.walk(f);
                }
                els.walk(f);
            }
            HirExprKind::Local(_)
            | HirExprKind::Number(_)
            | HirExprKind::Str(_)
            | HirExprKind::FuncRef(_)
            | HirExprKind::Const(_)
            | HirExprKind::Global(_)
            | HirExprKind::SizeOf(_)
            | HirExprKind::AlignOf(_)
            | HirExprKind::CompileError { .. }
            | HirExprKind::Poison => {}
        }
    }
}

impl HirBlock {
    /// Calls `f` on every expression in this block, depth-first.
    pub fn walk_exprs(&self, f: &mut dyn FnMut(&HirExpr)) {
        for stmt in &self.stmts {
            match stmt {
                HirStmt::Let { value, .. } | HirStmt::Expr(value) => value.walk(f),
                HirStmt::MultiLet { call, .. } => call.walk(f),
                HirStmt::Assign { place, value } => {
                    place.walk(f);
                    value.walk(f);
                }
                HirStmt::Return(_) | HirStmt::Break(_) | HirStmt::Continue(_) => {}
                HirStmt::If { cond, then, els } => {
                    cond.walk(f);
                    then.walk_exprs(f);
                    els.walk_exprs(f);
                }
                HirStmt::Loop(body) | HirStmt::Block(body) => body.walk_exprs(f),
                HirStmt::Match {
                    scrutinee,
                    arms,
                    els,
                } => {
                    scrutinee.walk(f);
                    for arm in arms {
                        arm.body.walk_exprs(f);
                    }
                    els.walk_exprs(f);
                }
            }
        }
    }
}
