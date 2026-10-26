//! Declarations rendered as Lilac source writes them, for hovers,
//! signature help and completion details. Everything comes from the
//! definition table and the declaration's syntax tree (types as written,
//! a constant's initializer printed from its expression), never from the
//! source text, so a comment or a layout never leaks into a hover.

use std::fmt::Write as _;

use lilac_ast::{BinOp, Expr, FuncDecl, FuncType, Param, Type, TypeShape};
use lilac_hir::intrinsics::Intrinsic;
use lilac_resolve::{DefId, DefKind, Resolved};
use lilac_ty::{TypeKind, TypeRef, Types};

/// A rendered declaration and where each parameter is in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rendered {
    /// The declaration as source writes it.
    pub text: String,
    /// Each parameter's UTF-16 range in `text`, for signature help.
    pub params: Vec<(u32, u32)>,
}

/// How many fields or variants a type's declaration lists.
pub const MAX_SLOTS: usize = 20;

/// How long a constant's or global's printed initializer may be before it
/// is shown as `…`, in bytes.
const MAX_VALUE: usize = 80;

/// How deep an initializer is printed before the rest is `…`.
const MAX_VALUE_DEPTH: usize = 8;

/// `def`'s declaration: a function's signature (its owner before its name),
/// a type's head and up to `slots` of its fields or variants, a field or
/// variant's slot, a constant (a member one after its type) or global
/// with its initializer.
#[must_use]
pub fn declaration(resolved: &Resolved, def: DefId, slots: usize) -> Rendered {
    let data = resolved.def(def);
    let mut out = Out::default();
    match &data.kind {
        DefKind::Func(decl) | DefKind::Hook(decl) => {
            function(resolved, def, decl, false, &mut out);
        }
        DefKind::Object | DefKind::Enum => {
            let keyword = if matches!(data.kind, DefKind::Object) {
                "structure"
            } else {
                "either"
            };
            out.push(&generic_name(&data.name, &data.generic_params));
            out.push(": ");
            out.push(keyword);
            if slots > 0 {
                if data.slots.is_empty() {
                    out.push(" {}");
                } else {
                    out.push(" {");
                    for slot in data.slots.iter().take(slots) {
                        out.push("\n    ");
                        member(resolved, *slot, &mut out);
                    }
                    let more = data.slots.len().saturating_sub(slots);
                    if more > 0 {
                        let noun = match (keyword, more) {
                            ("structure", 1) => "field",
                            ("structure", _) => "fields",
                            (_, 1) => "variant",
                            _ => "variants",
                        };
                        out.push(&format!("\n    // {more} more {noun}"));
                    }
                    out.push("\n}");
                }
            }
        }
        DefKind::Field(_) | DefKind::Variant(_) => member(resolved, def, &mut out),
        DefKind::Alias(ty) => {
            out.push(&generic_name(&data.name, &data.generic_params));
            out.push(": alias ");
            out.push(&type_text(ty));
        }
        DefKind::Const { ty, value } => {
            // A member constant is named through its type, whatever the
            // type's arguments: `Int.MAX`.
            if let Some(owner) = data.parent {
                out.push(&resolved.def(owner).name);
                out.push(".");
            }
            out.push(&data.name);
            out.push(": ");
            out.push(&type_text(ty));
            out.push(" => ");
            out.push(&value_text(value, ty));
        }
        DefKind::Global { ty, value } => {
            out.push(&data.name);
            out.push(": ");
            out.push(&type_text(ty));
            if let Some(value) = value {
                out.push(" = ");
                out.push(&value_text(value, ty));
            }
        }
    }
    out.finish()
}

/// A function's signature for a call: [`declaration`], leaving out the
/// receiver when the call passes it by naming a value (`point.length()`).
#[must_use]
pub fn call(resolved: &Resolved, def: DefId, skip_receiver: bool) -> Option<Rendered> {
    let decl = resolved.def(def).func()?;
    let mut out = Out::default();
    function(resolved, def, decl, skip_receiver, &mut out);
    Some(out.finish())
}

/// `name: ref function (param: slot, ...) outs` for a call through a
/// function reference of type `ty`. The slots are `ty`'s own (a generic
/// instance's, not its template's); a parameter or out is named where
/// `syntax`, the function type as declared, names it with the same number
/// of slots.
#[must_use]
pub fn value_call(
    types: &Types,
    name: &str,
    ty: TypeRef,
    syntax: Option<&FuncType>,
) -> Option<Rendered> {
    let TypeKind::Func {
        params,
        returns,
        failures,
        c_abi,
    } = types.kind(ty.id)
    else {
        return None;
    };
    let names = |slots: &[TypeRef], declared: Option<&Vec<Param>>| -> Vec<String> {
        let declared = declared.filter(|declared| declared.len() == slots.len());
        slots
            .iter()
            .enumerate()
            .map(|(index, slot)| {
                let shown = types.display_ref(*slot);
                match declared.map(|declared| declared[index].name.as_str()) {
                    Some(name) if !name.is_empty() => format!("{name}: {shown}"),
                    _ => shown,
                }
            })
            .collect()
    };
    let mut out = Out::default();
    out.push(name);
    out.push(if *c_abi {
        ": ref export function ("
    } else {
        ": ref function ("
    });
    for (index, param) in names(params, syntax.map(|syntax| &syntax.params))
        .iter()
        .enumerate()
    {
        if index > 0 {
            out.push(", ");
        }
        out.param(param);
    }
    out.push(") ");
    let outs = |out: &mut Out, outs: Vec<String>| match outs.len() {
        0 => out.push("nothing"),
        1 => out.push(&outs[0]),
        _ => out.push(&format!("({})", outs.join(", "))),
    };
    outs(
        &mut out,
        names(returns, syntax.map(|syntax| &syntax.returns)),
    );
    if let Some(failures) = failures {
        out.push(" else ");
        let declared = syntax.and_then(|syntax| syntax.failures.as_ref());
        outs(&mut out, names(failures, declared));
    }
    Some(out.finish())
}

/// `@name: function (param: slot, ...) result` for an intrinsic, from its
/// table.
#[must_use]
pub fn intrinsic(name: &str, intrinsic: Intrinsic) -> Rendered {
    let sig = intrinsic.signature();
    let mut out = Out::default();
    out.push(name);
    out.push(": function (");
    for (index, param) in sig.params.iter().enumerate() {
        if index > 0 {
            out.push(", ");
        }
        out.param(&format!("{}: {}", param.name, param.slot));
    }
    out.push(") ");
    match sig.result {
        Some(slot) => out.push(&slot.to_string()),
        None => out.push("nothing"),
    }
    out.finish()
}

/// Where `def` is declared, when that is not the root namespace: its owner
/// for a field or variant (a method or constant names it already), else its
/// namespace.
#[must_use]
pub fn context(resolved: &Resolved, def: DefId) -> Option<String> {
    let data = resolved.def(def);
    if let Some(owner) = data.parent {
        if matches!(data.kind, DefKind::Field(_) | DefKind::Variant(_)) {
            let owner = resolved.def(owner);
            return Some(generic_name(&owner.name, &owner.generic_params));
        }
        return None;
    }
    let path = resolved.namespace_path(data.ns);
    (!path.is_empty()).then(|| path.to_owned())
}

/// A hover's Markdown: the declaration in a `lilac` block, where it is
/// declared, then its doc.
#[must_use]
pub fn markdown(text: &str, context: Option<&str>, doc: Option<&str>) -> String {
    let mut out = format!("```lilac\n{text}\n```");
    if let Some(context) = context {
        let _ = write!(out, "\nin `{context}`");
    }
    if let Some(doc) = doc.map(str::trim).filter(|doc| !doc.is_empty()) {
        out.push_str("\n\n---\n\n");
        out.push_str(doc);
    }
    out
}

/// `def`'s hover Markdown (its whole declaration, context and doc).
#[must_use]
pub fn item_markdown(resolved: &Resolved, def: DefId) -> String {
    let rendered = declaration(resolved, def, MAX_SLOTS);
    let doc = resolved.def(def).doc.as_ref().map(|doc| doc.text.as_str());
    markdown(&rendered.text, context(resolved, def).as_deref(), doc)
}

/// A declaration being written, with its parameters' UTF-16 ranges.
#[derive(Default)]
struct Out {
    text: String,
    params: Vec<(u32, u32)>,
}

impl Out {
    fn push(&mut self, text: &str) {
        self.text.push_str(text);
    }

    /// Writes one parameter and records its range.
    fn param(&mut self, text: &str) {
        let start = utf16_len(&self.text);
        self.text.push_str(text);
        self.params.push((start, utf16_len(&self.text)));
    }

    fn finish(self) -> Rendered {
        Rendered {
            text: self.text,
            params: self.params,
        }
    }
}

fn utf16_len(text: &str) -> u32 {
    u32::try_from(text.encode_utf16().count()).unwrap_or(u32::MAX)
}

/// `Owner.name[T]: extern "link" function (params) outs else failures`.
fn function(resolved: &Resolved, def: DefId, decl: &FuncDecl, skip_receiver: bool, out: &mut Out) {
    let data = resolved.def(def);
    if let Some(owner) = data.parent {
        let owner = resolved.def(owner);
        out.push(&generic_name(&owner.name, &owner.generic_params));
        out.push(".");
    }
    out.push(&generic_name(&data.name, &data.generic_params));
    out.push(": ");
    if decl.is_extern || decl.is_export {
        out.push(if decl.is_extern { "extern " } else { "export " });
        if let Some(link) = &decl.link_name {
            out.push(&string_text(link.as_bytes()));
            out.push(" ");
        }
    }
    out.push("function (");
    let mut first = true;
    for (index, param) in decl.params.iter().enumerate() {
        if index == 0 && skip_receiver && param.is_receiver() {
            continue;
        }
        if !first {
            out.push(", ");
        }
        first = false;
        out.param(&slot_text(param));
    }
    if decl.variadic {
        if !first {
            out.push(", ");
        }
        out.push("...");
    }
    out.push(") ");
    out.push(&outs_text(&decl.returns));
    if let Some(failures) = &decl.failures {
        out.push(" else ");
        out.push(&outs_text(failures));
    }
}

/// A field (`x: @bit8`) or a variant (`Some: T`, `Dot`).
fn member(resolved: &Resolved, def: DefId, out: &mut Out) {
    let data = resolved.def(def);
    out.push(&data.name);
    match &data.kind {
        DefKind::Field(ty) | DefKind::Variant(Some(ty)) => {
            out.push(": ");
            out.push(&type_text(ty));
        }
        _ => {}
    }
}

/// `name` with its generic parameters (`Box[T]`).
fn generic_name(name: &str, generics: &[String]) -> String {
    if generics.is_empty() {
        name.to_owned()
    } else {
        format!("{name}[{}]", generics.join(", "))
    }
}

/// A parameter slot: `name: Type`, the bare receiver `self`, or an
/// anonymous out's type.
fn slot_text(param: &Param) -> String {
    if param.is_ref_self() {
        "self".to_owned()
    } else if param.name.is_empty() {
        type_text(&param.ty)
    } else {
        format!("{}: {}", param.name, type_text(&param.ty))
    }
}

/// Outs, or `nothing`.
fn outs_text(outs: &[Param]) -> String {
    if outs.is_empty() {
        "nothing".to_owned()
    } else {
        outs.iter().map(slot_text).collect::<Vec<_>>().join(", ")
    }
}

/// A function type's outs: `nothing`, one, or more in parentheses.
fn type_outs_text(outs: &[Param]) -> String {
    if outs.len() > 1 {
        format!("({})", outs_text(outs))
    } else {
        outs_text(outs)
    }
}

/// A type as written.
#[must_use]
pub fn type_text(ty: &Type) -> String {
    let shape = match &ty.shape {
        TypeShape::Named { name, args } if args.is_empty() => name.clone(),
        TypeShape::Named { name, args } => format!(
            "{name}[{}]",
            args.iter().map(type_text).collect::<Vec<_>>().join(", ")
        ),
        TypeShape::Function(func) => {
            let params: Vec<String> = func.params.iter().map(slot_text).collect();
            let mut text = format!(
                "{}function ({}) {}",
                if func.is_export { "export " } else { "" },
                params.join(", "),
                type_outs_text(&func.returns)
            );
            if let Some(failures) = &func.failures {
                text.push_str(" else ");
                text.push_str(&type_outs_text(failures));
            }
            text
        }
        TypeShape::Array { element, len, .. } => format!("@array({}, {len})", type_text(element)),
        TypeShape::FormatArgs { writer } => format!("@format_args({})", type_text(writer)),
        TypeShape::SelfType => "self".to_owned(),
        TypeShape::Error => "…".to_owned(),
    };
    if ty.is_ref {
        format!("ref {shape}")
    } else {
        shape
    }
}

/// A constant's or global's initializer, or `…` when it is long. The
/// shorthand `name: Type.function(args)` names its declared type `ty`.
fn value_text(value: &Expr, ty: &Type) -> String {
    let mut text = Value::default();
    if matches!(value, Expr::Construct { .. }) {
        text.push_str(&type_text(ty));
    }
    expr_text(value, 0, &mut text);
    if text.cut {
        "…".to_owned()
    } else {
        text.text
    }
}

/// An initializer being printed; `cut` once it grew too long or too deep
/// to show, so none of it is shown (a part elided would misread).
#[derive(Default)]
struct Value {
    text: String,
    cut: bool,
}

impl Value {
    fn push(&mut self, ch: char) {
        self.text.push(ch);
    }

    fn push_str(&mut self, text: &str) {
        self.text.push_str(text);
    }
}

impl std::fmt::Write for Value {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        self.text.push_str(text);
        Ok(())
    }
}

/// How tightly an operator binds (the parser's `binary_power`).
fn power(op: BinOp) -> u8 {
    match op {
        BinOp::Or => 1,
        BinOp::And => 2,
        BinOp::EqEq | BinOp::NotEq => 3,
        BinOp::Lt | BinOp::Gt | BinOp::LtEq | BinOp::GtEq => 4,
        BinOp::Add | BinOp::Sub => 5,
        BinOp::Mul | BinOp::Div | BinOp::Mod => 6,
    }
}

/// How tightly `as` binds.
const CAST_POWER: u8 = 7;

/// How tightly a prefix (`-`, `ref`, `not`) binds: tighter than `as`,
/// looser than a member access or call.
const PREFIX_POWER: u8 = 8;

fn op_text(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Mod => "%",
        BinOp::Lt => "<",
        BinOp::Gt => ">",
        BinOp::LtEq => "<=",
        BinOp::GtEq => ">=",
        BinOp::EqEq => "==",
        BinOp::NotEq => "!=",
        BinOp::And => "and",
        BinOp::Or => "or",
    }
}

/// How tightly `expr` binds as an operand (higher needs no parentheses).
fn expr_power(expr: &Expr) -> u8 {
    match expr {
        Expr::BinOp { op, .. } => power(*op),
        Expr::Cast(..) => CAST_POWER,
        Expr::RefOf { .. } | Expr::Not { .. } => PREFIX_POWER,
        Expr::Call {
            callee,
            args,
            generic_args,
            ..
        } if generic_args.is_empty()
            && args.len() == 1
            && matches!(&**callee, Expr::Ident(name, _) if name == "@negate") =>
        {
            PREFIX_POWER
        }
        Expr::IfValue { .. } | Expr::MatchValue { .. } => 0,
        _ => u8::MAX,
    }
}

/// Writes `expr` as an operand that must bind tighter than `above`.
fn operand(expr: &Expr, above: u8, depth: usize, out: &mut Value) {
    if expr_power(expr) > above {
        expr_text(expr, depth, out);
    } else {
        out.push('(');
        expr_text(expr, depth, out);
        out.push(')');
    }
}

fn expr_list(exprs: &[Expr], depth: usize, out: &mut Value) {
    for (index, expr) in exprs.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        expr_text(expr, depth, out);
    }
}

/// A character literal as source writes it.
fn char_text(ch: char) -> String {
    match ch {
        '\n' => "'\\n'".to_owned(),
        '\\' => "'\\\\'".to_owned(),
        ch => format!("'{ch}'"),
    }
}

/// `[A, B]` after a generic call's or instance's callee; nothing for none.
fn generic_args_text(generic_args: &[Type], out: &mut Value) {
    if generic_args.is_empty() {
        return;
    }
    let types: Vec<String> = generic_args.iter().map(type_text).collect();
    let _ = write!(out, "[{}]", types.join(", "));
}

/// Prints an expression as source writes it; a block-shaped one (a struct
/// literal, a `match`, an f-string) is elided to `…`.
fn expr_text(expr: &Expr, depth: usize, out: &mut Value) {
    if out.cut || depth > MAX_VALUE_DEPTH || out.text.len() > MAX_VALUE {
        out.cut = true;
        return;
    }
    let depth = depth + 1;
    match expr {
        Expr::Ident(name, _) => out.push_str(name),
        Expr::Tag(name, _) => {
            out.push('.');
            out.push_str(name);
        }
        Expr::Number(text, _) => out.push_str(text),
        Expr::Str(bytes, _) => out.push_str(&string_text(bytes)),
        Expr::Char(ch, _) => out.push_str(&char_text(*ch)),
        Expr::Call {
            callee,
            generic_args,
            args,
            ..
        } => {
            if let (Expr::Ident(name, _), [value], []) = (&**callee, &args[..], &generic_args[..]) {
                if name == "@negate" {
                    out.push('-');
                    operand(value, CAST_POWER, depth, out);
                    return;
                }
            }
            operand(callee, PREFIX_POWER, depth, out);
            generic_args_text(generic_args, out);
            out.push('(');
            expr_list(args, depth, out);
            out.push(')');
        }
        Expr::Member { obj, field, .. } => {
            operand(obj, PREFIX_POWER, depth, out);
            out.push('.');
            out.push_str(field);
        }
        Expr::Instance {
            callee,
            generic_args,
            ..
        } => {
            operand(callee, PREFIX_POWER, depth, out);
            generic_args_text(generic_args, out);
        }
        Expr::BinOp { lhs, op, rhs, .. } => {
            let power = power(*op);
            operand(lhs, power - 1, depth, out);
            let _ = write!(out, " {} ", op_text(*op));
            operand(rhs, power, depth, out);
        }
        Expr::IfValue {
            cond, then, els, ..
        } => {
            out.push_str("if ");
            expr_text(cond, depth, out);
            out.push_str(" then ");
            expr_text(then, depth, out);
            out.push_str(" else ");
            expr_text(els, depth, out);
        }
        Expr::RefOf { value, .. } => {
            out.push_str("ref ");
            operand(value, CAST_POWER, depth, out);
        }
        Expr::Not { value, .. } => {
            out.push_str("not ");
            operand(value, CAST_POWER, depth, out);
        }
        Expr::Cast(value, ty) => {
            operand(value, CAST_POWER - 1, depth, out);
            out.push_str(" as ");
            out.push_str(&type_text(ty));
        }
        Expr::TypeApp(ty) => out.push_str(&type_text(ty)),
        Expr::StructLit { ty, .. } => {
            let _ = write!(out, "{} {{ … }}", type_text(ty));
        }
        // After its declared type (`value_text`).
        Expr::Construct {
            function,
            generic_args,
            args,
            ..
        } => {
            out.push('.');
            out.push_str(function);
            generic_args_text(generic_args, out);
            out.push('(');
            expr_list(args, depth, out);
            out.push(')');
        }
        Expr::FString { .. } => out.push_str("$\"…\""),
        Expr::MatchValue { .. } | Expr::Error { .. } => out.push('…'),
    }
}

/// A string literal of `bytes`: `"`, `\` and a newline escaped, a byte
/// that is not printable or not UTF-8 as `\xHH`.
fn string_text(bytes: &[u8]) -> String {
    let mut out = String::from("\"");
    for chunk in bytes.utf8_chunks() {
        for ch in chunk.valid().chars() {
            match ch {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                ch if ch.is_ascii_control() => {
                    let _ = write!(out, "\\x{:02X}", u32::from(ch));
                }
                ch => out.push(ch),
            }
        }
        for byte in chunk.invalid() {
            let _ = write!(out, "\\x{byte:02X}");
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use lilac_ast::ItemKind;
    use lilac_error::Diags;
    use lilac_lex::Lexer;
    use lilac_parse::Parser;
    use lilac_span::FileId;

    use super::*;

    /// The constant `text` declares, printed back.
    fn printed(text: &str) -> String {
        let (tokens, _) = Lexer::lex_all(text, FileId(0));
        let mut diags = Diags::new();
        let program = Parser::new(tokens, FileId(0))
            .parse_program(&mut diags)
            .expect("parses");
        let ItemKind::Const { ty, value } = &program.items[0].kind else {
            panic!("a constant");
        };
        value_text(value, ty)
    }

    #[test]
    fn initializers_print_as_written() {
        for (written, shown) in [
            ("0x10", "0x10"),
            ("a + b * c", "a + b * c"),
            ("(a + b) * c", "(a + b) * c"),
            ("a - (b - c)", "a - (b - c)"),
            ("a - b - c", "a - b - c"),
            ("-(a + b)", "-(a + b)"),
            ("x as @bit8 + 0x1", "x as @bit8 + 0x1"),
            ("(x + 0x1) as @bit8", "(x + 0x1) as @bit8"),
            ("A.f[@bit8](ref y, .Some)", "A.f[@bit8](ref y, .Some)"),
            ("if a then b else c", "if a then b else c"),
            (r#""a\nb""#, r#""a\nb""#),
            ("Point { x = 0x1 }", "Point { … }"),
            ("Point.make(0x1)", "Point.make(0x1)"),
            ("not a == b != c", "not a == b != c"),
            ("not (a == b)", "not (a == b)"),
            ("not (x as @bit8)", "not (x as @bit8)"),
            ("not - not a", "not -not a"),
            ("(not a).b", "(not a).b"),
            ("(-a).f()", "(-a).f()"),
            ("-a.f()", "-a.f()"),
        ] {
            assert_eq!(printed(&format!("k: T => {written}\n")), shown, "{written}");
        }
        let long = (0..40)
            .map(|n| format!("a{n}"))
            .collect::<Vec<_>>()
            .join(" + ");
        assert_eq!(printed(&format!("k: T => {long}\n")), "…");
    }

    #[test]
    fn strings_escape_what_the_lexer_reads_back() {
        assert_eq!(string_text(b"a\"b\\c\nd"), r#""a\"b\\c\nd""#);
        assert_eq!(string_text(b"\x01\xFF\xC3\xA9"), "\"\\x01\\xFFé\"");
    }
}
