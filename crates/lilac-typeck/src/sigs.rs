//! Function signatures and the unit-global hook table.
//!
//! Hooks are one table for the whole unit, keyed by name; each overload is a
//! concrete instance with its concrete signature. Two overloads of one name
//! with identical signatures are `E020`. Hooks of a generic type are added per
//! instance when the type is instantiated (`types.rs`).

use lilac_error::{codes, Diagnostic};
use lilac_hir::{FuncKey, SemanticKind};
use lilac_resolve::{DefId, DefKind};
use lilac_span::Span;
use lilac_ty::{Builtin, FuncSig, TypeRef};

use crate::{Checker, Hook};

/// Operator hooks: the binary ones and `@negate`.
pub(crate) const BINARY_HOOKS: [&str; 9] = [
    "@add",
    "@sub",
    "@mul",
    "@div",
    "@mod",
    "@less_than",
    "@greater_than",
    "@less_than_or_equal",
    "@greater_than_or_equal",
];

impl Checker<'_> {
    /// Resolves the declared signature of a function or hook: a template whose
    /// generic parameters (the owning type's, then its own) are `Param`.
    /// `None` after a diagnostic.
    pub(crate) fn collect_sig(&mut self, def: DefId) -> Option<FuncSig> {
        let d = self.res.def(def);
        let decl = d.func()?.clone();
        if matches!(d.kind, DefKind::Hook(_)) && !d.generic_params.is_empty() {
            self.diags.emit(
                Diagnostic::error(
                    codes::E052,
                    "hook bindings must be concrete; generic hooks are rejected",
                )
                .label(d.span, true, "generic parameters here"),
            );
            return None;
        }
        let parent = d.parent;
        let env = self.env_of(def, Vec::new(), None);
        self.with_env(env, |c| {
            let mut ok = true;
            let mut slot = |c: &mut Self, param: &lilac_ast::Param| -> TypeRef {
                if !param.name.is_empty() {
                    c.record_semantic(leading(param.span, &param.name), SemanticKind::Param);
                }
                let is_self = param.name.is_empty() && param.ty.is_self();
                let ty = match (is_self, parent) {
                    (true, Some(owner)) => {
                        let mut ty = c.template_of(owner);
                        ty.is_ref = true;
                        ty
                    }
                    _ => c.resolve_syntax(&param.ty),
                };
                ok &= !ty.is_error();
                ty
            };
            let params = decl.params.iter().map(|p| slot(c, p)).collect();
            let returns = decl.returns.iter().map(|p| slot(c, p)).collect();
            ok.then_some(FuncSig { params, returns })
        })
    }

    /// Collects every hook of a non-generic owner (or top level) into the
    /// table and queues its body.
    pub(crate) fn collect_hooks(&mut self) {
        for def in self.res.hooks.clone() {
            let d = self.res.def(def);
            let generic_owner = d
                .parent
                .is_some_and(|parent| !self.res.def(parent).generic_params.is_empty());
            if generic_owner {
                // Validated per instance; a generic own parameter list is still
                // rejected up front.
                if !d.generic_params.is_empty() {
                    let _ = self.sig_of(def);
                }
                continue;
            }
            let span = d.span;
            let key = FuncKey::plain(def);
            if let Ok(sig) = self.request(key.clone(), span) {
                self.add_hook(&key, sig);
            }
        }
    }

    /// Validates a hook's shape and adds it to the table (`E020` on an
    /// identical signature).
    pub(crate) fn add_hook(&mut self, key: &FuncKey, sig: FuncSig) {
        let def = self.res.def(key.def);
        let name = def.name.clone();
        let span = def.span;
        if !self.hook_shape_ok(&name, key, &sig) {
            return;
        }
        let overloads = self.hooks.entry(name.clone()).or_default();
        if let Some(previous) = overloads
            .iter()
            .find(|hook| hook.sig.params == sig.params && hook.sig.returns == sig.returns)
        {
            let previous = previous.span;
            let diagnostic = if is_platform_hook(&name) {
                Diagnostic::error(
                    codes::E034,
                    format!("`{name}` is defined more than once in the program"),
                )
                .label(span, true, "defined again here")
                .label(previous, false, "first defined here")
                .note("a program has one of each platform hook; mount at most one runtime library")
            } else {
                Diagnostic::error(codes::E020, format!("duplicate definition of `{name}`"))
                    .label(span, true, "redefined here")
                    .label(previous, false, "first defined here")
            };
            self.diags.emit(diagnostic);
            return;
        }
        overloads.push(Hook {
            key: key.clone(),
            sig,
            span,
        });
    }

    /// Checks the fixed shapes of `@conversion` and the RAII hooks.
    fn hook_shape_ok(&mut self, name: &str, key: &FuncKey, sig: &FuncSig) -> bool {
        let def = self.res.def(key.def);
        let span = def.span;
        let owner = def
            .parent
            .map(|parent| self.type_of_def(parent, key.args.clone()));
        let fail = |c: &mut Self, msg: &str, label: &str| {
            c.diags
                .emit(Diagnostic::error(codes::E022, msg.to_owned()).label(
                    span,
                    true,
                    label.to_owned(),
                ));
            false
        };
        let by_value = |ty: &TypeRef| !ty.is_ref;
        if is_platform_hook(name) {
            return self.platform_hook_shape_ok(name, key, sig);
        }
        match name {
            "@conversion" => {
                let shaped = sig.params.len() == 1
                    && sig.returns.len() == 1
                    && by_value(&sig.params[0])
                    && by_value(&sig.returns[0]);
                if !shaped {
                    return fail(
                        self,
                        "`@conversion` needs exactly one by-value parameter and one by-value return",
                        "bad conversion shape",
                    );
                }
            }
            "@init" => {
                self.diags.emit(
                    Diagnostic::error(codes::E056, "there is no `@init` hook")
                        .label(
                            self.res.def(key.def).name_span,
                            true,
                            "make this a static function",
                        )
                        .help(
                            "name it (`create: function (...) value: Type`) and declare \
                             `variable: Type.create(...)`",
                        ),
                );
                return false;
            }
            "@on_drop" | "@on_copy" | "@on_override" => {
                let target = owner.map(|owner| owner.id).or_else(|| {
                    sig.params
                        .first()
                        .filter(|param| param.is_ref)
                        .map(|param| param.id)
                });
                let self_ref = |ty: &TypeRef| ty.is_ref && Some(ty.id) == target;
                let (shaped, msg) = match name {
                    "@on_drop" => (
                        sig.params.len() == 1 && self_ref(&sig.params[0]) && sig.returns.is_empty(),
                        "`@on_drop` needs exactly one `ref Self` parameter and no returns",
                    ),
                    "@on_copy" => (
                        sig.params.len() == 1
                            && self_ref(&sig.params[0])
                            && sig.returns.len() == 1
                            && by_value(&sig.returns[0])
                            && Some(sig.returns[0].id) == target,
                        "`@on_copy` needs exactly one `ref Self` parameter and returns `Self`",
                    ),
                    _ => (
                        sig.params.len() == 2
                            && self_ref(&sig.params[0])
                            && self_ref(&sig.params[1])
                            && sig.returns.is_empty(),
                        "`@on_override` needs exactly two `ref Self` parameters and no returns",
                    ),
                };
                if !shaped {
                    let label = format!("bad `{name}` shape");
                    return fail(self, msg, &label);
                }
            }
            _ => {}
        }
        true
    }

    /// Checks a platform hook: top-level, with its fixed signature
    /// (`@entry()`, `@exit(@bit32)`, `@compiler_print(@byte_buffer)`, no
    /// returns), else `E022`.
    fn platform_hook_shape_ok(&mut self, name: &str, key: &FuncKey, sig: &FuncSig) -> bool {
        let def = self.res.def(key.def);
        let span = def.span;
        let (message, label) = if def.parent.is_some() {
            (
                format!("`{name}` must be defined at the top level"),
                "platform hooks belong to the program, not a type".to_owned(),
            )
        } else {
            // Each parameter: its builtin type, and whether it is a `ref`.
            let (expected, want): (&[(Builtin, bool)], _) = match name {
                "@entry" => (
                    &[
                        (Builtin::BitNative, false),
                        (Builtin::Unknown, true),
                        (Builtin::Unknown, true),
                    ],
                    "`@entry: function(count: @bit_native, arguments: ref unknown, \
                     environment: ref unknown) nothing`",
                ),
                "@exit" => (
                    &[(Builtin::Bit32, false)],
                    "`@exit: function(code: @bit32) nothing`",
                ),
                _ => (
                    &[(Builtin::ByteBuffer, false)],
                    "`@compiler_print: function(text: @byte_buffer) nothing`",
                ),
            };
            let shaped = sig.returns.is_empty()
                && sig.params.len() == expected.len()
                && sig
                    .params
                    .iter()
                    .zip(expected)
                    .all(|(param, (ty, is_ref))| param.is_ref == *is_ref && param.id == ty.id());
            if shaped {
                return true;
            }
            (
                format!("`{name}` must be {want}"),
                format!("bad `{name}` shape"),
            )
        };
        self.diags
            .emit(Diagnostic::error(codes::E022, message).label(span, true, label));
        false
    }

    /// The overloads of hook `name` (empty when none exist).
    pub(crate) fn overloads(&self, name: &str) -> Vec<Hook> {
        self.hooks.get(name).cloned().unwrap_or_default()
    }
}

/// The leading `len(name)` bytes of a slot span (slots begin at their name).
pub(crate) fn leading(span: Span, name: &str) -> Span {
    let len = u32::try_from(name.len()).unwrap_or(u32::MAX);
    Span::new(span.lo.0, span.lo.0.saturating_add(len), span.file)
}

/// Whether `name` is a platform hook ([`lilac_hir::platform_hook_symbol`]).
pub(crate) fn is_platform_hook(name: &str) -> bool {
    lilac_hir::platform_hook_symbol(name).is_some()
}
