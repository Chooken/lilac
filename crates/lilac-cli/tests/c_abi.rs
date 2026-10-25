//! Objects by value across the C ABI, against the C compiler: for every
//! target that passes aggregates (`Target::passes_c_aggregates`), a corpus
//! of objects and enums (a fixed-seed random set of structs with nested
//! structs and arrays, plus hand-written enums, whose C is a tagged union)
//! is declared in Lilac and in C, each with an `extern` taking and returning
//! it, one taking eight of it (registers run out), one mixing it with
//! scalars, and an `export` calling each. `lilac emit-llvm` and the
//! toolchain Clang (`-S -emit-llvm`) must give every one the same LLVM
//! signature: the same flattened parameter and result types and the same
//! `byval`, `sret`, `align`, `alignstack` and `inreg`. Skipped, with a
//! message, when Clang is missing.
//!
//! A top-level object of one scalar field is left out: Lilac passes it as
//! that field (its numeric types are such objects), where C passes the
//! struct. So are 8- and 16-bit scalar slots, whose C extension attributes
//! Lilac does not give.

mod support;

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use lilac_target::{Arch, Target};

/// One field type of the corpus.
#[derive(Clone)]
enum Ty {
    /// A scalar: its Lilac and C spellings.
    Scalar(&'static str, &'static str),
    /// A declared object or enum, by index.
    Named(usize),
    /// An array of a field type.
    Array(Box<Ty>, u32),
}

/// A declared type: an object's fields, or an enum's variants (each with
/// a payload or not).
enum Decl {
    Object(Vec<Ty>),
    Enum(Vec<Option<Ty>>),
}

const SCALARS: [(&str, &str); 8] = [
    ("@bit8", "uint8_t"),
    ("@bit16", "uint16_t"),
    ("@bit32", "uint32_t"),
    ("@bit64", "uint64_t"),
    ("@fbit32", "float"),
    ("@fbit64", "double"),
    ("ref @bit8", "uint8_t *"),
    ("@bit_native", "uintptr_t"),
];

const HALF: (&str, &str) = ("@fbit16", "_Float16");

/// A fixed-seed generator (64-bit LCG).
struct Random(u64);

impl Random {
    fn below(&mut self, n: u32) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        u32::try_from((self.0 >> 33) % u64::from(n)).expect("below n")
    }
}

struct Corpus {
    decls: Vec<Decl>,
    /// The types the signatures take: never one scalar field.
    tops: Vec<usize>,
}

impl Corpus {
    fn scalar(random: &mut Random, halves: bool, small: bool) -> Ty {
        // A small type mostly holds bytes, halves, shorts, words and floats.
        if small && random.below(3) != 0 {
            let narrow = [0, 1, 2, 4];
            let pick = random.below(4 + u32::from(halves)) as usize;
            let (lilac, c) = narrow.get(pick).map_or(HALF, |&at| SCALARS[at]);
            return Ty::Scalar(lilac, c);
        }
        let pick = random
            .below(u32::try_from(SCALARS.len()).expect("few scalars") + u32::from(halves) * 2)
            as usize;
        let (lilac, c) = SCALARS.get(pick).copied().unwrap_or(HALF);
        Ty::Scalar(lilac, c)
    }

    fn field(&mut self, random: &mut Random, depth: u32, halves: bool, small: bool) -> Ty {
        let deepest = if small { 1 } else { 2 };
        match random.below(10) {
            0 | 1 if depth < deepest => Ty::Named(self.object(random, depth + 1, halves, small)),
            2 => {
                // An array element is never a `ref` (`E010`).
                let mut elem = Self::scalar(random, halves, small);
                while matches!(elem, Ty::Scalar(lilac, _) if lilac.starts_with("ref")) {
                    elem = Self::scalar(random, halves, small);
                }
                Ty::Array(Box::new(elem), 1 + random.below(if small { 2 } else { 4 }))
            }
            _ => Self::scalar(random, halves, small),
        }
    }

    fn object(&mut self, random: &mut Random, depth: u32, halves: bool, small: bool) -> usize {
        let count = 1 + random.below(if small { 3 } else { 5 });
        let mut fields: Vec<Ty> = (0..count)
            .map(|_| self.field(random, depth, halves, small))
            .collect();
        if depth == 0 && matches!(fields.as_slice(), [Ty::Scalar(..)]) {
            fields.push(Self::scalar(random, halves, small));
        }
        self.decls.push(Decl::Object(fields));
        self.decls.len() - 1
    }

    fn new(halves: bool) -> Self {
        let mut corpus = Self {
            decls: Vec::new(),
            tops: Vec::new(),
        };
        let mut random = Random(0x1ac_c0de);
        // Half the types are small, so most of those travel in registers.
        for index in 0..160 {
            let top = corpus.object(&mut random, 0, halves, index % 2 == 0);
            corpus.tops.push(top);
        }
        let scalar = |at: usize| Ty::Scalar(SCALARS[at].0, SCALARS[at].1);
        let pair = |corpus: &mut Self, ty: Ty| {
            corpus.decls.push(Decl::Object(vec![ty.clone(), ty]));
            Ty::Named(corpus.decls.len() - 1)
        };
        let floats = pair(&mut corpus, scalar(4));
        let doubles = pair(&mut corpus, scalar(5));
        let enums = vec![
            vec![Some(scalar(2)), Some(scalar(4))],
            vec![None, Some(scalar(3))],
            vec![Some(scalar(4)), Some(scalar(4))],
            vec![Some(floats.clone()), Some(scalar(5))],
            vec![Some(doubles), None, Some(scalar(0))],
            vec![None, None, None],
            vec![Some(floats), Some(Ty::Array(Box::new(scalar(1)), 3))],
            vec![Some(scalar(6)), Some(scalar(7))],
        ];
        for variants in enums {
            corpus.decls.push(Decl::Enum(variants));
            corpus.tops.push(corpus.decls.len() - 1);
        }
        corpus
    }

    fn lilac_ty(ty: &Ty) -> String {
        match ty {
            Ty::Scalar(lilac, _) => (*lilac).to_owned(),
            Ty::Named(index) => format!("T{index}"),
            Ty::Array(elem, len) => format!("@array({}, 0x{len:x})", Self::lilac_ty(elem)),
        }
    }

    /// A C declaration of `name` with field type `ty`.
    fn c_field(ty: &Ty, name: &str) -> String {
        match ty {
            Ty::Scalar(_, c) => format!("{c} {name};"),
            Ty::Named(index) => format!("T{index} {name};"),
            Ty::Array(elem, len) => Self::c_field(elem, &format!("{name}[{len}]")),
        }
    }

    fn lilac(&self) -> String {
        let mut src = String::new();
        for (index, decl) in self.decls.iter().enumerate() {
            match decl {
                Decl::Object(fields) => {
                    let _ = writeln!(src, "T{index}: structure {{");
                    for (at, field) in fields.iter().enumerate() {
                        let _ = writeln!(src, "    f{at}: {}", Self::lilac_ty(field));
                    }
                }
                Decl::Enum(variants) => {
                    let _ = writeln!(src, "T{index}: either {{");
                    for (at, payload) in variants.iter().enumerate() {
                        match payload {
                            Some(ty) => {
                                let _ = writeln!(src, "    V{at}: {}", Self::lilac_ty(ty));
                            }
                            None => {
                                let _ = writeln!(src, "    V{at}");
                            }
                        }
                    }
                }
            }
            src.push_str("}\n");
        }
        for top in &self.tops {
            let t = format!("T{top}");
            let eight = "a, b, c, d, e, f, g, h";
            let eight_params: Vec<String> =
                eight.split(", ").map(|p| format!("{p}: {t}")).collect();
            let eight_params = eight_params.join(", ");
            let _ = write!(
                src,
                "take_{t}: extern function (x: {t}) r: {t}\n\
                 many_{t}: extern function ({eight_params}, n: @bit32) nothing\n\
                 mix_{t}: extern function (a: @fbit64, x: {t}, b: @bit64, y: {t}) r: {t}\n\
                 e_take_{t}: export function (x: {t}) r: {t} {{\n    r = take_{t}(x)\n}}\n\
                 e_many_{t}: export function ({eight_params}, n: @bit32) nothing {{\n    \
                 many_{t}({eight}, n)\n}}\n\
                 e_mix_{t}: export function (a: @fbit64, x: {t}, b: @bit64, y: {t}) r: {t} {{\n    \
                 r = mix_{t}(a, x, b, y)\n}}\n"
            );
        }
        src.push_str("main: function () nothing {}\n");
        src
    }

    fn c(&self) -> String {
        let mut src = String::from("#include <stdint.h>\n");
        for (index, decl) in self.decls.iter().enumerate() {
            match decl {
                Decl::Object(fields) => {
                    let fields: Vec<String> = fields
                        .iter()
                        .enumerate()
                        .map(|(at, field)| Self::c_field(field, &format!("f{at}")))
                        .collect();
                    let _ = writeln!(src, "typedef struct {{ {} }} T{index};", fields.join(" "));
                }
                Decl::Enum(variants) => {
                    let members: Vec<String> = variants
                        .iter()
                        .enumerate()
                        .filter_map(|(at, payload)| {
                            Some(Self::c_field(payload.as_ref()?, &format!("p{at}")))
                        })
                        .collect();
                    let union = if members.is_empty() {
                        String::new()
                    } else {
                        format!(" union {{ {} }} payload;", members.join(" "))
                    };
                    let _ = writeln!(src, "typedef struct {{ uint8_t tag;{union} }} T{index};");
                }
            }
        }
        for top in &self.tops {
            let t = format!("T{top}");
            let _ = write!(
                src,
                "{t} take_{t}({t});\n\
                 void many_{t}({t}, {t}, {t}, {t}, {t}, {t}, {t}, {t}, uint32_t);\n\
                 {t} mix_{t}(double, {t}, uint64_t, {t});\n\
                 {t} e_take_{t}({t} x) {{ return take_{t}(x); }}\n\
                 void e_many_{t}({t} a, {t} b, {t} c, {t} d, {t} e, {t} f, {t} g, {t} h, \
                 uint32_t n) {{ many_{t}(a, b, c, d, e, f, g, h, n); }}\n\
                 {t} e_mix_{t}(double a, {t} x, uint64_t b, {t} y) {{ return mix_{t}(a, x, b, y); }}\n"
            );
        }
        src
    }
}

/// The flags that make Clang's C ABI the one Lilac targets: RISC-V Lilac
/// is soft-float.
fn clang_flags(target: Target) -> Vec<String> {
    let mut flags = vec![format!("--target={}", target.to_triple())];
    match target.arch {
        Arch::Riscv64 => flags.extend(["-mabi=lp64".into(), "-march=rv64imac".into()]),
        Arch::Riscv32 => flags.extend(["-mabi=ilp32".into(), "-march=rv32imac".into()]),
        _ => {}
    }
    flags
}

/// Whether the target's C has `_Float16`.
fn has_half(clang: &Path, target: Target) -> bool {
    let output = Command::new(clang)
        .args(["-ffreestanding", "-E", "-dM", "-x", "c", "-"])
        .args(clang_flags(target))
        .stdin(Stdio::null())
        .output()
        .expect("clang runs");
    String::from_utf8_lossy(&output.stdout).contains("#define __FLT16_MAX__")
}

/// Splits `text` at commas outside brackets.
fn split_top(text: &str, at: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (index, ch) in text.char_indices() {
        match ch {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' => depth -= 1,
            _ if ch == at && depth == 0 => {
                parts.push(&text[start..index]);
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
        .into_iter()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect()
}

/// The scalar types an LLVM type holds, in order: structs and arrays
/// flattened (`%struct.T` through its definition), vectors kept whole.
fn flatten(ty: &str, defs: &HashMap<String, String>, out: &mut Vec<String>) {
    let ty = ty.trim();
    if let Some(inner) = ty.strip_prefix("<{").and_then(|t| t.strip_suffix("}>")) {
        for part in split_top(inner, ',') {
            flatten(part, defs, out);
        }
    } else if let Some(inner) = ty.strip_prefix('{').and_then(|t| t.strip_suffix('}')) {
        for part in split_top(inner, ',') {
            flatten(part, defs, out);
        }
    } else if let Some(inner) = ty.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
        let (count, elem) = inner.split_once(" x ").expect("an array type");
        for _ in 0..count.trim().parse::<u32>().expect("an array length") {
            flatten(elem, defs, out);
        }
    } else if ty.starts_with('%') {
        flatten(&defs[ty], defs, out);
    } else {
        out.push(ty.split_whitespace().collect::<Vec<_>>().join(" "));
    }
}

/// The length of the type at the start of `text`.
fn type_len(text: &str) -> usize {
    let mut depth = 0i32;
    for (index, ch) in text.char_indices() {
        match ch {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' => depth -= 1,
            ' ' if depth == 0 => return index,
            _ => {}
        }
    }
    text.len()
}

/// One parameter as its flattened types and the attributes that matter.
fn param(text: &str, defs: &HashMap<String, String>) -> String {
    let len = type_len(text);
    let mut types = Vec::new();
    flatten(&text[..len], defs, &mut types);
    let words = split_top(&text[len..], ' ');
    let mut kept = Vec::new();
    let mut words = words.into_iter();
    while let Some(word) = words.next() {
        if word.starts_with("byval") {
            kept.push("byval".to_owned());
        } else if word.starts_with("sret") {
            kept.push("sret".to_owned());
        } else if word == "align" {
            kept.push(format!("align {}", words.next().unwrap_or("?")));
        } else if word.starts_with("alignstack") || word == "inreg" {
            kept.push(word.to_owned());
        }
    }
    format!("{} {}", types.join(","), kept.join(" "))
        .trim()
        .to_owned()
}

/// Every `declare`/`define` of a corpus function in an LLVM module, by
/// name, as its result's flattened types and its parameters.
fn signatures(ir: &str) -> BTreeMap<String, String> {
    let mut defs = HashMap::new();
    for line in ir.lines() {
        if let Some((name, body)) = line.split_once(" = type ") {
            defs.insert(name.trim().to_owned(), body.trim().to_owned());
        }
    }
    let mut found = BTreeMap::new();
    for line in ir.lines() {
        if !(line.starts_with("declare ") || line.starts_with("define ")) {
            continue;
        }
        let Some(at) = line.find(" @") else { continue };
        let rest = &line[at + 2..];
        let Some(open) = rest.find('(') else { continue };
        let name = &rest[..open];
        if !["take_", "many_", "mix_", "e_"]
            .iter()
            .any(|p| name.starts_with(p))
        {
            continue;
        }
        let head: Vec<&str> = line[..at]
            .split_whitespace()
            .skip(1)
            .filter(|word| {
                !matches!(
                    *word,
                    "dso_local"
                        | "hidden"
                        | "protected"
                        | "dllexport"
                        | "dllimport"
                        | "noundef"
                        | "zeroext"
                        | "signext"
                        | "inreg"
                        | "nonnull"
                ) && !word.ends_with("cc")
            })
            .collect();
        let mut returns = Vec::new();
        flatten(&head.join(" "), &defs, &mut returns);
        let args = &rest[open + 1..];
        let close = {
            let mut depth = 1;
            args.char_indices()
                .find(|&(_, ch)| {
                    match ch {
                        '(' => depth += 1,
                        ')' => depth -= 1,
                        _ => {}
                    }
                    depth == 0
                })
                .map_or(args.len(), |(index, _)| index)
        };
        let params: Vec<String> = split_top(&args[..close], ',')
            .into_iter()
            .map(|p| param(p, &defs))
            .collect();
        found.insert(
            name.to_owned(),
            format!("{} ({})", returns.join(","), params.join("; ")),
        );
    }
    found
}

#[test]
fn objects_cross_the_c_abi_as_clang_passes_them() {
    let Some(clang) = support::clang() else {
        eprintln!("skipping: no clang");
        return;
    };
    let dir = std::env::temp_dir().join(format!("lilac-c-abi-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch folder");
    let mut failures = Vec::new();
    let mut checked = 0;
    for target in Target::supported() {
        if !target.passes_c_aggregates() {
            continue;
        }
        let corpus = Corpus::new(has_half(&clang, target));
        let lilac_file = dir.join("corpus.lilac");
        std::fs::write(&lilac_file, corpus.lilac()).expect("writes the Lilac corpus");
        let lilac = Command::new(env!("CARGO_BIN_EXE_lilac"))
            .args(["emit-llvm", "-q"])
            .arg(format!("--target={target}"))
            .arg(&lilac_file)
            .output()
            .expect("runs lilac");
        assert!(
            lilac.status.success(),
            "{target}: lilac failed:\n{}",
            String::from_utf8_lossy(&lilac.stderr)
        );
        let c_file = dir.join("corpus.c");
        std::fs::write(&c_file, corpus.c()).expect("writes the C corpus");
        let c = Command::new(&clang)
            .args(["-S", "-emit-llvm", "-O0", "-ffreestanding", "-o", "-"])
            .args(clang_flags(target))
            .arg(&c_file)
            .output()
            .expect("runs clang");
        assert!(
            c.status.success(),
            "{target}: clang failed:\n{}",
            String::from_utf8_lossy(&c.stderr)
        );
        let ours = signatures(&String::from_utf8_lossy(&lilac.stdout));
        let theirs = signatures(&String::from_utf8_lossy(&c.stdout));
        assert_eq!(
            ours.keys().collect::<Vec<_>>(),
            theirs.keys().collect::<Vec<_>>(),
            "{target}: the modules declare different functions"
        );
        for (name, sig) in &ours {
            checked += 1;
            if theirs[name] != *sig {
                failures.push(format!(
                    "{target} `{name}`:\n  lilac {sig}\n  clang {}",
                    theirs[name]
                ));
            }
        }
    }
    assert!(checked > 0, "nothing was compared");
    assert!(
        failures.is_empty(),
        "{} of {checked} signatures differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
