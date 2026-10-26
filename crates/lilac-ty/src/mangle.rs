//! Symbol names derived from types, after Rust's v0 mangling scheme, and
//! their demangler (`IR_SPEC.md`, "Symbol names").
//!
//! A symbol keeps its readable path (`A.B.foo`, `Holder.make`, `@add`); the
//! types in it are written in *regions*: `[…]` after a path segment holds
//! that segment's generic arguments, and `(…)` after a hook's name holds its
//! signature. A region uses only `[A-Za-z0-9_]`, and every element in it is
//! self-delimiting, so a repeated type or path is a backreference to the
//! offset of its first encoding: a region's length is linear in the number
//! of distinct types it names, however large their written form.

use std::collections::HashMap;

use crate::{TypeId, TypeKind, TypeRef, Types};

/// Demangled text past this many bytes is cut, ending in `...`.
pub const DEMANGLE_LIMIT: usize = 4096;

/// The basic types: each is one letter in a region (the letters Rust's v0
/// gives `u8`, `u16`, `u32`, `u64`, `usize`, `f32`, `f64` and `str`, and
/// letters v0 leaves unused for the rest).
const BASIC: [(u8, &str); 12] = [
    (b'h', "@bit8"),
    (b't', "@bit16"),
    (b'm', "@bit32"),
    (b'y', "@bit64"),
    (b'j', "@bit_native"),
    (b'k', "@fbit16"),
    (b'f', "@fbit32"),
    (b'd', "@fbit64"),
    (b'q', "@fbit128"),
    (b'e', "@byte_buffer"),
    (b'g', "unknown"),
    (b'r', "<error>"),
];

/// The base-62 digits of v0.
const DIGITS: &[u8; 62] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";

impl Types {
    /// A type's name in symbols and LIR layouts: an object or enum is its
    /// qualified name (`A.Holder`), and an instance adds its arguments as a
    /// region (`A.Holder[m]` for `A.Holder[@bit32]`). Any other type is a
    /// region of its own (`[m]`), so every type has a name and no two types
    /// share one.
    pub fn mangle(&self, id: TypeId) -> String {
        match self.kind(id) {
            TypeKind::Object(def) => self.object_name(*def),
            TypeKind::Instance { base, args } => self.object_name(*base) + &self.mangle_args(args),
            _ => self.mangle_args(&[TypeRef::value(id)]),
        }
    }

    /// The region of a generic argument list, brackets included
    /// (`[mh]` for `[@bit32, @bit8]`).
    pub fn mangle_args(&self, args: &[TypeRef]) -> String {
        let mut region = Region::new(self);
        for arg in args {
            region.ty(*arg);
        }
        format!("[{}]", region.out)
    }

    /// The region of a hook's signature, parentheses included: its
    /// parameter types, `E`, then its return types (`(R6ObjectE)` for
    /// `(ref Object)`).
    pub fn mangle_signature(&self, params: &[TypeRef], returns: &[TypeRef]) -> String {
        let mut region = Region::new(self);
        for param in params {
            region.ty(*param);
        }
        region.out.push('E');
        for ret in returns {
            region.ty(*ret);
        }
        format!("({})", region.out)
    }
}

/// One region being encoded.
struct Region<'t> {
    types: &'t Types,
    out: String,
    /// Each type written in full so far: its offset and length.
    written: HashMap<TypeRef, (usize, usize)>,
    /// Each path (by its dotted name) written in full so far.
    paths: HashMap<String, (usize, usize)>,
}

impl<'t> Region<'t> {
    fn new(types: &'t Types) -> Self {
        Self {
            types,
            out: String::new(),
            written: HashMap::new(),
            paths: HashMap::new(),
        }
    }

    /// Writes `ty`. Recursion follows the type's nesting, which typeck
    /// bounds (`MAX_TYPE_NESTING`).
    fn ty(&mut self, ty: TypeRef) {
        let kind = self.types.kind(ty.id);
        if !ty.is_ref {
            if let Some(letter) = basic_letter(kind) {
                self.out.push(char::from(letter));
                return;
            }
            if let TypeKind::Object(def) = kind {
                let name = self.types.object_name(*def);
                self.path(&name);
                return;
            }
        }
        if let Some(&(at, len)) = self.written.get(&ty) {
            if self.backref(at, len) {
                return;
            }
        }
        let start = self.out.len();
        if ty.is_ref {
            self.out.push('R');
            self.ty(TypeRef::value(ty.id));
        } else {
            match kind {
                TypeKind::Instance { base, args } => {
                    self.out.push('I');
                    let name = self.types.object_name(*base);
                    self.path(&name);
                    for arg in args {
                        self.ty(*arg);
                    }
                    self.out.push('E');
                }
                TypeKind::Array { elem, len } => {
                    self.out.push('A');
                    self.ty(TypeRef::value(*elem));
                    push_base62(&mut self.out, *len);
                }
                TypeKind::FormatArgs { writer } => {
                    self.out.push('w');
                    self.ty(TypeRef::value(*writer));
                }
                TypeKind::Func {
                    params,
                    returns,
                    failures,
                    c_abi,
                } => {
                    self.out.push('F');
                    if *c_abi {
                        self.out.push_str("KC");
                    }
                    if failures.is_some() {
                        self.out.push_str("KF");
                    }
                    for param in params {
                        self.ty(*param);
                    }
                    self.out.push('E');
                    for ret in returns {
                        self.ty(*ret);
                    }
                    self.out.push('E');
                    if let Some(failures) = failures {
                        for failure in failures {
                            self.ty(*failure);
                        }
                        self.out.push('E');
                    }
                }
                TypeKind::Param { index, name } => {
                    self.out.push('p');
                    push_base62(&mut self.out, u64::from(*index));
                    push_ident(&mut self.out, name);
                }
                // Basic types and objects returned above.
                _ => {}
            }
        }
        self.written
            .entry(ty)
            .or_insert((start, self.out.len() - start));
    }

    /// Writes the path of the dotted name `name`: `<ident>` at the root,
    /// else `N <parent> <ident>`.
    fn path(&mut self, name: &str) {
        if let Some(&(at, len)) = self.paths.get(name) {
            if self.backref(at, len) {
                return;
            }
        }
        let start = self.out.len();
        match name.rsplit_once('.') {
            Some((parent, last)) => {
                self.out.push('N');
                self.path(parent);
                push_ident(&mut self.out, last);
            }
            None => push_ident(&mut self.out, name),
        }
        self.paths
            .entry(name.to_owned())
            .or_insert((start, self.out.len() - start));
    }

    /// Writes a backreference to the element at `at`, when it is shorter
    /// than the element's `len` bytes. A backreference never points at
    /// another: one to a later offset is never shorter.
    fn backref(&mut self, at: usize, len: usize) -> bool {
        let mut text = String::from("B");
        push_base62(&mut text, at as u64);
        if text.len() < len {
            self.out.push_str(&text);
            true
        } else {
            false
        }
    }
}

/// A basic type's letter.
fn basic_letter(kind: &TypeKind) -> Option<u8> {
    let name = match kind {
        TypeKind::Bit8 => "@bit8",
        TypeKind::Bit16 => "@bit16",
        TypeKind::Bit32 => "@bit32",
        TypeKind::Bit64 => "@bit64",
        TypeKind::BitNative => "@bit_native",
        TypeKind::FBit16 => "@fbit16",
        TypeKind::FBit32 => "@fbit32",
        TypeKind::FBit64 => "@fbit64",
        TypeKind::FBit128 => "@fbit128",
        TypeKind::ByteBuffer => "@byte_buffer",
        TypeKind::Unknown => "unknown",
        TypeKind::Error => "<error>",
        _ => return None,
    };
    BASIC
        .iter()
        .find(|(_, text)| *text == name)
        .map(|(letter, _)| *letter)
}

/// v0's base-62 number: `_` is 0, else `n - 1` in base 62, then `_`.
fn push_base62(out: &mut String, n: u64) {
    if n > 0 {
        let mut rest = n - 1;
        let mut digits = Vec::new();
        loop {
            digits.push(DIGITS[(rest % 62) as usize]);
            rest /= 62;
            if rest == 0 {
                break;
            }
        }
        out.extend(digits.iter().rev().map(|digit| char::from(*digit)));
    }
    out.push('_');
}

/// An identifier: its length in decimal, a `_` when it starts with a digit
/// or `_`, then its bytes. One with a byte outside `[A-Za-z0-9_]` (the
/// `@` of `@builtin`) is `u` and the same of its escaped form, where such a
/// byte, and `_`, is `_` and two hex digits.
fn push_ident(out: &mut String, name: &str) {
    if name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        push_plain(out, name);
        return;
    }
    let mut escaped = String::new();
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() {
            escaped.push(char::from(byte));
        } else {
            escaped.push('_');
            escaped.push(char::from(DIGITS[usize::from(byte >> 4)]));
            escaped.push(char::from(DIGITS[usize::from(byte & 0xf)]));
        }
    }
    out.push('u');
    push_plain(out, &escaped);
}

fn push_plain(out: &mut String, bytes: &str) {
    out.push_str(&bytes.len().to_string());
    if bytes
        .bytes()
        .next()
        .is_some_and(|first| first.is_ascii_digit() || first == b'_')
    {
        out.push('_');
    }
    out.push_str(bytes);
}

/// `symbol` with each region written as Lilac types: `Holder[m].make` is
/// `Holder[@bit32].make`, and a hook's `(R6ObjectE)` is `(ref Object)`.
/// The text is cut past [`DEMANGLE_LIMIT`] bytes, ending in `...`. `None`
/// when `symbol` has no region (it is readable as it is) or a malformed
/// one. Never panics, and takes time linear in `symbol` plus the limit.
pub fn demangle(symbol: &str) -> Option<String> {
    demangle_bounded(symbol, DEMANGLE_LIMIT)
}

/// [`demangle`] cut past `limit` bytes.
pub fn demangle_bounded(symbol: &str, limit: usize) -> Option<String> {
    let bytes = symbol.as_bytes();
    let mut regions = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'[' | b'(' => {
                let region = Parsed::parse(&bytes[at..])?;
                at += region.len;
                regions.push(region);
            }
            b']' | b')' => return None,
            _ => at += 1,
        }
    }
    if regions.is_empty() {
        return None;
    }
    let mut out = Out::new(limit);
    let mut at = 0;
    let mut next = regions.iter();
    while at < bytes.len() && !out.full() {
        let readable = bytes[at..]
            .iter()
            .position(|byte| matches!(byte, b'[' | b'('))
            .map_or(bytes.len(), |offset| at + offset);
        out.push(&symbol[at..readable]);
        at = readable;
        if let Some(region) = next.next() {
            region.print(&mut out);
            at += region.len;
        }
    }
    Some(out.finish())
}

/// `text` with every region that follows a symbol character (a name's
/// last letter, or another region) written as Lilac types, each cut past
/// [`DEMANGLE_LIMIT`] bytes: the filter `lilac demangle` runs over its
/// input. Everything else is copied as it is.
pub fn demangle_text(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut at = 0;
    while at < bytes.len() {
        let opens = matches!(bytes[at], b'[' | b'(')
            && at > 0
            && matches!(bytes[at - 1], b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'@' | b']' | b')');
        if let Some(region) = opens.then(|| Parsed::parse(&bytes[at..])).flatten() {
            out.push_str(&text[copied..at]);
            let mut shown = Out::new(DEMANGLE_LIMIT);
            region.print(&mut shown);
            out.push_str(&shown.finish());
            at += region.len;
            copied = at;
        } else {
            at += 1;
        }
    }
    out.push_str(&text[copied..]);
    out
}

/// Demangled text, cut once it passes its limit.
struct Out {
    text: String,
    limit: usize,
}

impl Out {
    fn new(limit: usize) -> Self {
        Self {
            text: String::new(),
            limit,
        }
    }

    fn full(&self) -> bool {
        self.text.len() > self.limit
    }

    /// Appends `part`, or as much of it as passes the limit.
    fn push(&mut self, part: &str) {
        if self.full() {
            return;
        }
        let room = self.limit + 1 - self.text.len();
        if part.len() <= room {
            self.text.push_str(part);
        } else {
            let mut end = room;
            while !part.is_char_boundary(end) {
                end += 1;
            }
            self.text.push_str(&part[..end]);
        }
    }

    fn finish(mut self) -> String {
        if self.full() {
            let mut end = self.limit;
            while !self.text.is_char_boundary(end) {
                end -= 1;
            }
            self.text.truncate(end);
            self.text.push_str("...");
        }
        self.text
    }
}

/// What an element of a region is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum What {
    /// A type that is not a path.
    Type,
    /// A path (which is also a type: an object or enum).
    Path,
}

/// One element of a parsed region.
#[derive(Clone, Copy, Debug)]
struct Elem {
    what: What,
    /// The offset just past it.
    end: usize,
    /// For a backreference, the element it stands for (never itself a
    /// backreference).
    target: Option<usize>,
}

/// A region checked in full, with each element's extent, so printing it
/// never parses again.
struct Parsed<'s> {
    /// The region's content, from just past its opening bracket.
    src: &'s [u8],
    /// `]` or `)`.
    close: u8,
    /// The region's length, brackets included.
    len: usize,
    elems: HashMap<usize, Elem>,
}

/// What the parser expects next. The parser keeps them on a stack of its
/// own, so a deeply nested region cannot overflow the native one.
#[derive(Clone, Copy)]
enum Step {
    /// Types until this byte, which it consumes.
    Types(u8),
    Type,
    Path,
    /// The type element starting at this offset ends here.
    EndType(usize),
    /// The path element starting at this offset ends here.
    EndPath(usize),
    Ident,
    Number,
}

impl<'s> Parsed<'s> {
    /// Checks the region that starts at `text[0]` (its `[` or `(`).
    fn parse(text: &'s [u8]) -> Option<Self> {
        let close = match text.first()? {
            b'[' => b']',
            b'(' => b')',
            _ => return None,
        };
        let mut parser = Parser {
            src: &text[1..],
            pos: 0,
            elems: HashMap::new(),
        };
        let mut steps = vec![Step::Types(close)];
        if close == b')' {
            steps.push(Step::Types(b'E'));
        }
        while let Some(step) = steps.pop() {
            parser.step(step, &mut steps)?;
        }
        Some(Self {
            src: parser.src,
            close,
            len: parser.pos + 1,
            elems: parser.elems,
        })
    }

    /// Writes the region as Lilac types.
    fn print(&self, out: &mut Out) {
        let mut tasks = Vec::new();
        if self.close == b']' {
            tasks.push(Task::Text("]"));
            tasks.push(Task::List(0, true));
            tasks.push(Task::Text("["));
        } else {
            tasks.push(Task::SignatureReturns);
            tasks.push(Task::List(0, true));
            tasks.push(Task::Text("("));
        }
        // Where the last list ended (just past its terminator), for what
        // follows it.
        let mut after = 0;
        while let Some(task) = tasks.pop() {
            if out.full() {
                return;
            }
            match task {
                Task::Text(text) => out.push(text),
                Task::List(at, first) => {
                    let Some(&byte) = self.src.get(at) else {
                        return;
                    };
                    if matches!(byte, b'E' | b']' | b')') {
                        after = at + 1;
                        continue;
                    }
                    tasks.push(Task::List(self.end(at), false));
                    tasks.push(Task::Type(at));
                    if !first {
                        tasks.push(Task::Text(", "));
                    }
                }
                Task::SignatureReturns => {
                    out.push(")");
                    if self.src.get(after) != Some(&b')') {
                        out.push(" -> ");
                        tasks.push(Task::List(after, true));
                    }
                }
                Task::FunctionReturns(fails) => {
                    if fails {
                        tasks.push(Task::FunctionFailures);
                    }
                    self.print_outs(&mut after, &mut tasks, out);
                }
                Task::FunctionFailures => {
                    out.push(" else ");
                    self.print_outs(&mut after, &mut tasks, out);
                }
                Task::Type(at) => self.print_type(self.resolve(at), &mut tasks, out),
                Task::Path(at) => {
                    let at = self.resolve(at);
                    if self.src.get(at) == Some(&b'N') {
                        tasks.push(Task::Ident(self.end(at + 1)));
                        tasks.push(Task::Text("."));
                        tasks.push(Task::Path(at + 1));
                    } else {
                        tasks.push(Task::Ident(at));
                    }
                }
                Task::Ident(at) => {
                    if let Some((text, _)) = ident(self.src, at) {
                        out.push(&text);
                    }
                }
                Task::Number(at) => {
                    if let Some((value, _)) = base62(self.src, at) {
                        out.push(&value.to_string());
                    }
                }
            }
        }
    }

    /// Queues the tasks that print the type at `at` (not a backreference).
    fn print_type(&self, at: usize, tasks: &mut Vec<Task>, out: &mut Out) {
        let Some(&tag) = self.src.get(at) else {
            return;
        };
        if let Some((_, name)) = BASIC.iter().find(|(letter, _)| *letter == tag) {
            out.push(name);
            return;
        }
        match tag {
            b'R' => {
                out.push("ref ");
                tasks.push(Task::Type(at + 1));
            }
            b'I' => {
                tasks.push(Task::Text("]"));
                tasks.push(Task::List(self.end(at + 1), true));
                tasks.push(Task::Text("["));
                tasks.push(Task::Path(at + 1));
            }
            b'A' => {
                out.push("@array(");
                tasks.push(Task::Text(")"));
                tasks.push(Task::Number(self.end(at + 1)));
                tasks.push(Task::Text(", "));
                tasks.push(Task::Type(at + 1));
            }
            b'w' => {
                out.push("@format_args(");
                tasks.push(Task::Text(")"));
                tasks.push(Task::Type(at + 1));
            }
            b'F' => {
                let mut params = at + 1;
                if self.src.get(params..params + 2) == Some(b"KC") {
                    params += 2;
                    out.push("export ");
                }
                let fails = self.src.get(params..params + 2) == Some(b"KF");
                if fails {
                    params += 2;
                }
                out.push("function (");
                tasks.push(Task::FunctionReturns(fails));
                tasks.push(Task::Text(") "));
                tasks.push(Task::List(params, true));
            }
            b'p' => {
                if let Some((_, end)) = base62(self.src, at + 1) {
                    tasks.push(Task::Ident(end));
                }
            }
            _ => tasks.push(Task::Path(at)),
        }
    }

    /// Queues the tasks that print a function type's outs, the list at
    /// `after`: `nothing`, one type, or more parenthesized, as written.
    /// An empty list moves `after` past its `E` itself, since no list
    /// task ends there.
    fn print_outs(&self, after: &mut usize, tasks: &mut Vec<Task>, out: &mut Out) {
        if self.src.get(*after) == Some(&b'E') {
            out.push("nothing");
            *after += 1;
        } else if self.src.get(self.end(*after)) == Some(&b'E') {
            tasks.push(Task::List(*after, true));
        } else {
            out.push("(");
            tasks.push(Task::Text(")"));
            tasks.push(Task::List(*after, true));
        }
    }

    /// The element a backreference at `at` stands for, else `at`.
    fn resolve(&self, at: usize) -> usize {
        self.elems
            .get(&at)
            .and_then(|elem| elem.target)
            .unwrap_or(at)
    }

    /// The offset just past the element at `at`.
    fn end(&self, at: usize) -> usize {
        self.elems.get(&at).map_or(self.src.len(), |elem| elem.end)
    }
}

/// One step of printing a region.
#[derive(Clone, Copy)]
enum Task {
    Text(&'static str),
    /// The types of a list from this offset, the first when `true`.
    List(usize, bool),
    /// A hook signature's `)` and its returns, which follow its parameters.
    SignatureReturns,
    /// A function type's returns, which follow its parameters, and its
    /// failure outs after them when `true`.
    FunctionReturns(bool),
    /// A failing function type's `else` outs, which follow its returns.
    FunctionFailures,
    Type(usize),
    Path(usize),
    Ident(usize),
    Number(usize),
}

/// The state of checking one region.
struct Parser<'s> {
    src: &'s [u8],
    pos: usize,
    elems: HashMap<usize, Elem>,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn step(&mut self, step: Step, steps: &mut Vec<Step>) -> Option<()> {
        match step {
            Step::Types(end) => {
                if self.peek()? == end {
                    self.pos += 1;
                } else {
                    steps.push(Step::Types(end));
                    steps.push(Step::Type);
                }
            }
            Step::Type => self.ty(steps)?,
            Step::Path => self.path(steps)?,
            Step::EndType(at) => self.record(at, What::Type, None),
            Step::EndPath(at) => self.record(at, What::Path, None),
            Step::Ident => {
                let (_, end) = ident(self.src, self.pos)?;
                self.pos = end;
            }
            Step::Number => {
                let (_, end) = base62(self.src, self.pos)?;
                self.pos = end;
            }
        }
        Some(())
    }

    fn ty(&mut self, steps: &mut Vec<Step>) -> Option<()> {
        let at = self.pos;
        let tag = self.peek()?;
        if BASIC.iter().any(|(letter, _)| *letter == tag) {
            self.pos += 1;
            self.record(at, What::Type, None);
            return Some(());
        }
        match tag {
            b'0'..=b'9' | b'u' | b'N' => steps.push(Step::Path),
            b'B' => self.backref(&[What::Type, What::Path])?,
            // A `ref` or a format: one type follows.
            b'R' | b'w' => {
                self.pos += 1;
                steps.push(Step::EndType(at));
                steps.push(Step::Type);
            }
            b'I' => {
                self.pos += 1;
                steps.push(Step::EndType(at));
                steps.push(Step::Types(b'E'));
                steps.push(Step::Path);
            }
            b'A' => {
                self.pos += 1;
                steps.push(Step::EndType(at));
                steps.push(Step::Number);
                steps.push(Step::Type);
            }
            b'F' => {
                self.pos += 1;
                if self.peek() == Some(b'K') && self.src.get(self.pos + 1) == Some(&b'C') {
                    self.pos += 2;
                }
                let fails = self.peek() == Some(b'K');
                if fails {
                    if self.src.get(self.pos + 1) != Some(&b'F') {
                        return None;
                    }
                    self.pos += 2;
                }
                steps.push(Step::EndType(at));
                if fails {
                    steps.push(Step::Types(b'E'));
                }
                steps.push(Step::Types(b'E'));
                steps.push(Step::Types(b'E'));
            }
            b'p' => {
                self.pos += 1;
                steps.push(Step::EndType(at));
                steps.push(Step::Ident);
                steps.push(Step::Number);
            }
            _ => return None,
        }
        Some(())
    }

    fn path(&mut self, steps: &mut Vec<Step>) -> Option<()> {
        let at = self.pos;
        match self.peek()? {
            b'0'..=b'9' | b'u' => {
                let (_, end) = ident(self.src, at)?;
                self.pos = end;
                self.record(at, What::Path, None);
            }
            b'N' => {
                self.pos += 1;
                steps.push(Step::EndPath(at));
                steps.push(Step::Ident);
                steps.push(Step::Path);
            }
            b'B' => self.backref(&[What::Path])?,
            _ => return None,
        }
        Some(())
    }

    /// A backreference here, to a whole element before it of a kind in
    /// `accept` (one still open, such as the type it is inside, has no
    /// entry yet).
    fn backref(&mut self, accept: &[What]) -> Option<()> {
        let at = self.pos;
        let (offset, end) = base62(self.src, at + 1)?;
        let offset = usize::try_from(offset).ok()?;
        if offset >= at {
            return None;
        }
        let elem = *self.elems.get(&offset)?;
        if elem.end > at || !accept.contains(&elem.what) {
            return None;
        }
        self.pos = end;
        let target = elem.target.unwrap_or(offset);
        self.record(at, elem.what, Some(target));
        Some(())
    }

    fn record(&mut self, at: usize, what: What, target: Option<usize>) {
        self.elems.insert(
            at,
            Elem {
                what,
                end: self.pos,
                target,
            },
        );
    }
}

/// The base-62 number at `at`, and the offset past it.
fn base62(src: &[u8], at: usize) -> Option<(u64, usize)> {
    let mut value: u64 = 0;
    let mut pos = at;
    loop {
        let byte = *src.get(pos)?;
        pos += 1;
        let digit = match byte {
            b'_' => break,
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'z' => byte - b'a' + 10,
            b'A'..=b'Z' => byte - b'A' + 36,
            _ => return None,
        };
        value = value.checked_mul(62)?.checked_add(u64::from(digit))?;
    }
    if pos == at + 1 {
        Some((0, pos))
    } else {
        Some((value.checked_add(1)?, pos))
    }
}

/// The identifier at `at`, decoded, and the offset past it.
fn ident(src: &[u8], at: usize) -> Option<(String, usize)> {
    let mut pos = at;
    let escaped = src.get(pos) == Some(&b'u');
    if escaped {
        pos += 1;
    }
    let mut len: usize = 0;
    let first = *src.get(pos)?;
    if !first.is_ascii_digit() {
        return None;
    }
    pos += 1;
    len += usize::from(first - b'0');
    if first != b'0' {
        while let Some(&byte) = src.get(pos).filter(|byte| byte.is_ascii_digit()) {
            len = len.checked_mul(10)?.checked_add(usize::from(byte - b'0'))?;
            pos += 1;
        }
    }
    if src.get(pos) == Some(&b'_') {
        pos += 1;
    }
    let end = pos.checked_add(len)?;
    let bytes = src.get(pos..end)?;
    if !bytes
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
    {
        return None;
    }
    if !escaped {
        // Checked to be ASCII.
        return Some((String::from_utf8_lossy(bytes).into_owned(), end));
    }
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'_' {
            let high = hex(*bytes.get(index + 1)?)?;
            let low = hex(*bytes.get(index + 2)?)?;
            decoded.push(high << 4 | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    Some((String::from_utf8(decoded).ok()?, end))
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use lilac_resolve::DefId;

    use super::*;
    use crate::{Builtin, TypeRef};

    /// A table with a few named objects: `Number`, `A.B.Holder`, the
    /// generic `P` and `Q`, and `@builtin.Target.Os`.
    fn table() -> Types {
        let mut types = Types::new();
        for (def, name) in [
            (1, "Number"),
            (2, "A.B.Holder"),
            (3, "P"),
            (4, "Q"),
            (5, "@builtin.Target.Os"),
        ] {
            types.set_object_name(DefId(def), name);
        }
        types
    }

    /// A spread of types of every kind, each distinct.
    fn sample(types: &mut Types) -> Vec<TypeRef> {
        let mut out: Vec<TypeRef> = Builtin::ALL.iter().map(|builtin| builtin.value()).collect();
        out.push(TypeRef::ERROR);
        for def in 1..=5 {
            out.push(TypeRef::value(types.intern_object(DefId(def))));
        }
        out.push(TypeRef::value(types.intern_param(0, "T")));
        out.push(TypeRef::value(types.intern_param(1, "U")));
        let base = out.clone();
        for (index, ty) in base.iter().enumerate() {
            let other = base[(index * 7 + 3) % base.len()];
            if !ty.is_error() {
                out.push(TypeRef::reference(ty.id));
            }
            out.push(TypeRef::value(types.intern_instance(DefId(3), vec![*ty])));
            out.push(TypeRef::value(
                types.intern_instance(DefId(4), vec![*ty, other]),
            ));
            out.push(TypeRef::value(
                types.intern_instance(DefId(3), vec![TypeRef::reference(ty.id)]),
            ));
            out.push(TypeRef::value(types.intern_array(ty.id, index as u64 + 1)));
            out.push(TypeRef::value(types.intern_format_args(ty.id)));
            out.push(TypeRef::value(types.intern_func(vec![*ty], vec![other])));
            out.push(TypeRef::value(
                types.intern_func(vec![other], vec![*ty, other]),
            ));
            out.push(TypeRef::value(types.intern_func_abi(
                vec![*ty, other],
                Vec::new(),
                true,
            )));
            out.push(TypeRef::value(types.intern_function(
                vec![*ty],
                vec![other],
                Some(Vec::new()),
                false,
            )));
            out.push(TypeRef::value(types.intern_function(
                Vec::new(),
                Vec::new(),
                Some(vec![*ty, other]),
                false,
            )));
            out.push(TypeRef::value(types.intern_function(
                vec![other],
                vec![*ty, other],
                Some(vec![other]),
                index % 2 == 0,
            )));
        }
        let nested = out.clone();
        for pair in nested.chunks(2) {
            out.push(TypeRef::value(
                types.intern_instance(DefId(4), pair.to_vec()),
            ));
        }
        for len in [62, 63, u64::MAX] {
            out.push(TypeRef::value(types.intern_array(Builtin::Bit8.id(), len)));
        }
        out
    }

    /// `Q[x, x]` applied `depth` times: `depth` table entries, a tree of
    /// `2^depth` leaves.
    fn doubled(types: &mut Types, depth: usize) -> TypeRef {
        (0..depth).fold(Builtin::Bit32.value(), |inner, _| {
            TypeRef::value(types.intern_instance(DefId(4), vec![inner, inner]))
        })
    }

    /// [`sample`], then `count` types built at random (fixed seed) from the
    /// ones before them, so they nest and share parts.
    fn random(types: &mut Types, count: usize) -> Vec<TypeRef> {
        let mut pool = sample(types);
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = |bound: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            usize::try_from(state % bound as u64).unwrap_or(0)
        };
        for _ in 0..count {
            let mut pick = || pool[next(pool.len())];
            let (a, b) = (pick(), pick());
            let ty = match next(9) {
                0 => TypeRef::reference(a.id),
                1 => TypeRef::value(types.intern_instance(DefId(3), vec![a])),
                2 => TypeRef::value(types.intern_instance(DefId(4), vec![a, b])),
                3 => TypeRef::value(types.intern_array(a.id, next(200) as u64 + 1)),
                4 => TypeRef::value(types.intern_func(vec![a, b], vec![b])),
                5 => TypeRef::value(types.intern_format_args(a.id)),
                6 => TypeRef::value(types.intern_func(Vec::new(), vec![a, b])),
                7 => TypeRef::value(types.intern_function(vec![a], vec![b], Some(vec![a]), false)),
                _ => TypeRef::value(types.intern_func_abi(vec![a], Vec::new(), true)),
            };
            pool.push(ty);
        }
        pool
    }

    #[test]
    fn every_type_demangles_to_its_display() {
        let mut types = table();
        let mut checked = 0;
        for ty in random(&mut types, 3_000) {
            let shown = types.display_bounded(ty, DEMANGLE_LIMIT);
            if shown.ends_with("...") {
                continue;
            }
            let mangled = types.mangle_args(&[ty]);
            assert_eq!(demangle(&mangled), Some(format!("[{shown}]")), "{mangled}");
            checked += 1;
        }
        assert!(checked > 1_000, "{checked}");
    }

    #[test]
    fn region_mangles_are_injective() {
        let mut types = table();
        let sample = random(&mut types, 3_000);
        let mut seen: HashMap<String, TypeRef> = HashMap::new();
        for ty in &sample {
            if let Some(other) = seen.insert(types.mangle_args(&[*ty]), *ty) {
                assert_eq!(other, *ty, "two types share a mangle");
            }
        }
        // Lists too: a region's elements are self-delimiting.
        let mut lists: HashMap<String, Vec<TypeRef>> = HashMap::new();
        for list in sample.windows(2).chain(sample.chunks(3)) {
            let mangled = types.mangle_args(list);
            if let Some(other) = lists.insert(mangled.clone(), list.to_vec()) {
                assert_eq!(other, list, "{mangled}");
            }
        }
    }

    #[test]
    fn regions_use_the_symbol_alphabet() {
        let mut types = table();
        for ty in sample(&mut types) {
            let mangled = types.mangle_args(&[ty]);
            let inner = &mangled[1..mangled.len() - 1];
            assert!(
                inner
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
                "{mangled}"
            );
        }
    }

    #[test]
    fn mangles_read_as_designed() {
        let mut types = table();
        let number = TypeRef::value(types.intern_object(DefId(1)));
        let holder = TypeRef::value(types.intern_object(DefId(2)));
        let os = TypeRef::value(types.intern_object(DefId(5)));
        let p_number = TypeRef::value(types.intern_instance(DefId(3), vec![number]));
        assert_eq!(types.mangle(p_number.id), "P[6Number]");
        assert_eq!(types.mangle_args(&[holder]), "[NN1A1B6Holder]");
        // A repeated path is a backreference to its offset.
        let pair = types.intern_instance(DefId(4), vec![holder, holder]);
        assert_eq!(types.mangle(pair), "Q[NN1A1B6HolderB_]");
        // `@builtin` escapes its `@`.
        assert_eq!(types.mangle_args(&[os]), "[NNu10__40builtin6Target2Os]");
        assert_eq!(
            types.mangle_signature(&[TypeRef::reference(p_number.id)], &[]),
            "(RI1P6NumberEE)"
        );
        assert_eq!(
            types.mangle_signature(&[Builtin::Bit32.value()], &[Builtin::Bit8.value()]),
            "(mEh)"
        );
        assert_eq!(
            demangle("@conversion(mEh)").as_deref(),
            Some("@conversion(@bit32) -> @bit8")
        );
        assert_eq!(
            demangle("P[6Number].@on_drop(RI1P6NumberEE)").as_deref(),
            Some("P[Number].@on_drop(ref P[Number])")
        );
        assert_eq!(demangle(".root.id[m]").as_deref(), Some(".root.id[@bit32]"));
        let func = types.intern_func(vec![number, holder], Vec::new());
        assert_eq!(types.mangle(func), "[F6NumberNN1A1B6HolderEE]");
        assert_eq!(
            demangle(&types.mangle(func)).as_deref(),
            Some("[function (Number, A.B.Holder) nothing]")
        );
        let c_func = types.intern_func_abi(
            vec![TypeRef::reference(number.id)],
            vec![Builtin::Bit32.value()],
            true,
        );
        assert_eq!(types.mangle(c_func), "[FKCR6NumberEmE]");
        // A failing one flags `KF` and adds its `else` outs as a third list.
        let failing = types.intern_function(
            vec![TypeRef::reference(number.id)],
            vec![Builtin::Bit32.value()],
            Some(Vec::new()),
            false,
        );
        assert_eq!(types.mangle(failing), "[FKFR6NumberEmEE]");
        assert_eq!(
            demangle(&types.mangle(failing)).as_deref(),
            Some("[function (ref Number) @bit32 else nothing]")
        );
        let both = types.intern_function(Vec::new(), Vec::new(), Some(vec![number, holder]), false);
        assert_eq!(
            types.display(both),
            "function () nothing else (Number, A.B.Holder)"
        );
        assert_eq!(
            demangle(&types.mangle(both)).as_deref(),
            Some("[function () nothing else (Number, A.B.Holder)]")
        );
        let array = types.intern_array(holder.id, 63);
        assert_eq!(types.mangle(array), "[ANN1A1B6Holder10_]");
        let arrays = types.intern_instance(DefId(4), vec![TypeRef::value(array), holder]);
        // The path starts at offset 1, after the `A`.
        assert_eq!(types.mangle(arrays), "Q[ANN1A1B6Holder10_B0_]");
        assert_eq!(
            demangle("Q[ANN1A1B6Holder10_B0_]").as_deref(),
            Some("Q[@array(A.B.Holder, 63), A.B.Holder]")
        );
        let format = types.intern_format_args(holder.id);
        assert_eq!(types.mangle(format), "[wNN1A1B6Holder]");
        assert_eq!(
            demangle(&types.mangle(format)).as_deref(),
            Some("[@format_args(A.B.Holder)]")
        );
    }

    #[test]
    fn doubling_types_mangle_in_linear_space() {
        let mut types = table();
        let mut last = 0;
        for depth in [10, 100, 1000] {
            let ty = doubled(&mut types, depth);
            let mangled = types.mangle(ty.id);
            // Each level is `I1Q`, its argument, a backreference and `E`.
            assert!(mangled.len() < 16 * depth, "{depth}: {}", mangled.len());
            assert!(mangled.len() > last);
            last = mangled.len();
            let shown = demangle(&mangled).expect("valid");
            assert_eq!(shown.len(), DEMANGLE_LIMIT + 3);
            assert!(shown.ends_with("..."));
            assert!(shown.starts_with("Q[Q[Q["));
        }
        let small = doubled(&mut types, 3);
        assert_eq!(
            demangle(&types.mangle(small.id)),
            Some(types.display(small.id))
        );
    }

    #[test]
    fn demangle_cuts_at_its_limit() {
        let mut types = table();
        let ty = doubled(&mut types, 4);
        let full = types.display(ty.id);
        let mangled = types.mangle(ty.id);
        assert_eq!(demangle_bounded(&mangled, full.len()), Some(full.clone()));
        let cut = demangle_bounded(&mangled, 10).expect("valid");
        assert_eq!(cut, format!("{}...", &full[..10]));
    }

    #[test]
    fn malformed_regions_are_not_demangled() {
        for symbol in [
            "plain.name",
            "x[",
            "x[m",
            "x]",
            "x[m]]",
            "x[Q]",
            "x()",
            "x(m)",
            "x(mEmEm)",
            // A backreference forwards, to itself, inside the type it is
            // in, or to a type from a path position.
            "x[B_]",
            "x[mB0_]",
            "x[I1PB_E]",
            "x[mI1PB0_E]",
            "x[mNB_1A]",
            // Numbers and lengths that overflow or run past the end.
            "x[AmZZZZZZZZZZZZZZZZZZZZZZ_]",
            "x[99999999999999999999999A]",
            "x[5abc]",
            "x[u3__zz]",
            "x[u3__ff]",
            "x[u2__4]",
            "x[FKmEE]",
            "x[p_]",
        ] {
            assert_eq!(demangle(symbol), None, "{symbol}");
        }
    }

    #[test]
    fn demangle_never_panics() {
        // Every string over a region alphabet up to length 4, then a
        // pseudo-random sample of longer ones.
        let alphabet: &[u8] = b"[]()EBNIRAFKCpu_0123456789hmgaZ";
        let mut words: Vec<Vec<u8>> = vec![Vec::new()];
        for _ in 0..4 {
            let mut longer = Vec::new();
            for word in &words {
                for byte in alphabet {
                    let mut next = word.clone();
                    next.push(*byte);
                    longer.push(next);
                }
            }
            for word in &longer {
                let text = format!("x[{}", String::from_utf8_lossy(word));
                let _ = demangle(&text);
                let _ = demangle_text(&text);
            }
            words = longer;
        }
        let mut state: u64 = 0x2545_f491_4f6c_dd1d;
        for _ in 0..20_000 {
            let mut text = String::from("s");
            for _ in 0..(state % 40) {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let index = usize::try_from(state % alphabet.len() as u64).unwrap_or(0);
                text.push(char::from(alphabet[index]));
            }
            let _ = demangle(&text);
            let _ = demangle_text(&text);
        }
    }

    #[test]
    fn demangle_text_rewrites_only_symbols() {
        let text = concat!(
            "    call @P[6Number].@on_drop(RI1P6NumberEE) (in %tmp.2) (out)\n",
            "    alloca [4 x #bit32] %a : zeroed\n",
            "define void @\"Q[mm].get\"(ptr %in0) {\n",
            "see x[m] and [m] and f(mEh)\n",
        );
        assert_eq!(
            demangle_text(text),
            concat!(
                "    call @P[Number].@on_drop(ref P[Number]) (in %tmp.2) (out)\n",
                "    alloca [4 x #bit32] %a : zeroed\n",
                "define void @\"Q[@bit32, @bit32].get\"(ptr %in0) {\n",
                "see x[@bit32] and [m] and f(@bit32) -> @bit8\n",
            )
        );
        assert_eq!(demangle_text("no symbols here"), "no symbols here");
        assert_eq!(demangle_text("bad x[Q] kept"), "bad x[Q] kept");
    }
}
