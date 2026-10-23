//! `lilac-layout` against the C compiler: for every supported target, a
//! corpus of types is declared in C with a `_Static_assert` on each
//! `sizeof`, `_Alignof` and `offsetof` that `lilac-layout` computes, and
//! the toolchain Clang (`$LLVM_SYS_181_PREFIX/bin/clang`) checks them with
//! `-fsyntax-only --target=<triple>`. Skipped, with a message, when that
//! Clang is missing.
//!
//! The corpus covers every scalar after every other, nested structs, arrays
//! of structs and arrays, trailing padding, empty structs (a GCC/Clang
//! extension, size 0), and tagged unions as their C
//! `struct { tag; union { … } payload; }` (enums: mixed payload alignments,
//! nested, in arrays and in structs), plus a fixed-seed random set.
//! `lilac-lower`'s `enum_c_layout` test checks that the compiler gives real
//! Lilac enums these layouts.
//! `_Float16` and the 128-bit float are declared where the target's C has
//! them (`__float128`, else a binary128 `long double`), which Clang's
//! predefined macros say.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use lilac_layout::{layout, Shape};
use lilac_target::{Abi, Os, Scalar, Target};

/// The toolchain Clang, when it is installed.
fn clang() -> Option<PathBuf> {
    let prefix = std::env::var_os("LLVM_SYS_181_PREFIX")?;
    let exe = if cfg!(windows) { "clang.exe" } else { "clang" };
    let path = Path::new(&prefix).join("bin").join(exe);
    path.is_file().then_some(path)
}

/// The targets checked: every supported one, plus the ABI variants whose
/// C differs in what it offers (`__float128` on MinGW, musl).
fn targets() -> Vec<Target> {
    let mut targets = Target::supported();
    for extra in [
        "x86-pc-windows-gnu",
        "x86_64-pc-windows-gnu",
        "x86_64-unknown-linux-musl",
        "aarch64-unknown-linux-musl",
        "arm-unknown-linux-musl",
        "arm-unknown-none",
    ] {
        let target = Target::parse(extra).expect("valid triple");
        if !targets.contains(&target) {
            targets.push(target);
        }
    }
    targets
}

/// What the target's C offers beyond the always-present scalars.
struct CFloats {
    /// `_Float16` exists.
    float16: bool,
    /// The spelling of a binary128 float, when one exists.
    float128: Option<&'static str>,
}

/// Reads the floats the target's C has from Clang's predefined macros.
fn c_floats(clang: &Path, target: Target) -> CFloats {
    let output = Command::new(clang)
        .args(["-ffreestanding", "-E", "-dM", "-x", "c", "-"])
        .arg(format!("--target={target}"))
        .stdin(Stdio::null())
        .output()
        .expect("clang runs");
    assert!(
        output.status.success(),
        "clang refused `{target}`:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let macros = String::from_utf8_lossy(&output.stdout);
    let defines = |name: &str, value: Option<&str>| {
        macros.lines().any(|line| {
            let mut words = line.split_whitespace();
            words.next() == Some("#define")
                && words.next() == Some(name)
                && value.is_none_or(|value| words.next() == Some(value))
        })
    };
    let float128 = if defines("__SIZEOF_FLOAT128__", None) {
        Some("__float128")
    } else if defines("__LDBL_MANT_DIG__", Some("113")) {
        Some("long double")
    } else {
        None
    };
    CFloats {
        float16: defines("__FLT16_MANT_DIG__", None),
        float128,
    }
}

/// C declarations of a corpus, with their asserts.
struct Corpus<'a> {
    target: &'a Target,
    floats: &'a CFloats,
    /// Whether C has empty structs here: not under the Microsoft C ABI,
    /// where C requires a member (Clang's emulation gives one size 4),
    /// so no C declaration exists to disagree with Lilac's size 0.
    empty_structs: bool,
    text: String,
    count: usize,
    asserts: usize,
}

impl Corpus<'_> {
    /// The C spelling of a scalar (`None` when the target's C lacks it).
    /// `uintptr_t` for `pointer_int`, else `void *`, for a pointer.
    fn scalar(&self, scalar: Scalar, pointer_int: bool) -> Option<&'static str> {
        Some(match scalar {
            Scalar::Bit8 => "uint8_t",
            Scalar::Bit16 => "uint16_t",
            Scalar::Bit32 => "uint32_t",
            Scalar::Bit64 => "uint64_t",
            Scalar::Float16 => self.floats.float16.then_some("_Float16")?,
            Scalar::Float32 => "float",
            Scalar::Float64 => "double",
            Scalar::Float128 => self.floats.float128?,
            Scalar::Pointer if pointer_int => "uintptr_t",
            Scalar::Pointer => "void *",
        })
    }

    /// Whether every scalar of `shape` exists in the target's C.
    fn expressible(&self, shape: &Shape) -> bool {
        match shape {
            Shape::Scalar(scalar) => self.scalar(*scalar, false).is_some(),
            Shape::Array(elem, _) => self.expressible(elem),
            Shape::Struct(fields) => {
                (self.empty_structs || !fields.is_empty())
                    && fields.iter().all(|field| self.expressible(field))
            }
            Shape::Tagged { tag, payloads } => {
                self.scalar(*tag, false).is_some()
                    && payloads.iter().all(|payload| self.expressible(payload))
            }
        }
    }

    fn assert(&mut self, condition: &str) {
        self.asserts += 1;
        let _ = writeln!(self.text, "_Static_assert({condition}, \"{condition}\");");
    }

    /// Declares `shape` (when the target's C can) and asserts its layout.
    fn add(&mut self, shape: &Shape) {
        if self.expressible(shape) {
            self.declare(shape);
        }
    }

    /// The C type name of `shape`, declaring it (and asserting its
    /// layout) first when it is an aggregate.
    fn declare(&mut self, shape: &Shape) -> String {
        let layout = layout(self.target, shape).expect("the corpus fits every target");
        let name = match shape {
            Shape::Scalar(scalar) => {
                let pointer_int = self.count.is_multiple_of(2);
                self.count += 1;
                self.scalar(*scalar, pointer_int)
                    .expect("expressible")
                    .to_owned()
            }
            Shape::Array(elem, len) => {
                let elem = self.declare(elem);
                let name = self.fresh("A");
                let _ = writeln!(self.text, "typedef {elem} {name}[{len}];");
                name
            }
            Shape::Struct(fields) => {
                let fields: Vec<String> = fields.iter().map(|field| self.declare(field)).collect();
                let name = self.fresh("S");
                let mut body = String::new();
                for (index, field) in fields.iter().enumerate() {
                    let _ = write!(body, " {field} f{index};");
                }
                let _ = writeln!(self.text, "typedef struct {name} {{{body} }} {name};");
                for (index, offset) in layout.fields.iter().enumerate() {
                    self.assert(&format!("offsetof({name}, f{index}) == {offset}"));
                }
                name
            }
            Shape::Tagged { tag, payloads } => {
                let tag = self.scalar(*tag, false).expect("an integer tag");
                let payloads: Vec<String> = payloads
                    .iter()
                    .map(|payload| self.declare(payload))
                    .collect();
                let name = self.fresh("U");
                let mut union = String::new();
                if !payloads.is_empty() {
                    union.push_str(" union {");
                    for (index, payload) in payloads.iter().enumerate() {
                        let _ = write!(union, " {payload} p{index};");
                    }
                    union.push_str(" } payload;");
                }
                let _ = writeln!(
                    self.text,
                    "typedef struct {name} {{ {tag} tag;{union} }} {name};"
                );
                self.assert(&format!("offsetof({name}, tag) == {}", layout.fields[0]));
                for (index, offset) in layout.fields.iter().skip(1).enumerate() {
                    self.assert(&format!("offsetof({name}, payload.p{index}) == {offset}"));
                }
                name
            }
        };
        self.assert(&format!("sizeof({name}) == {}", layout.size));
        self.assert(&format!("_Alignof({name}) == {}", layout.align));
        name
    }

    fn fresh(&mut self, prefix: &str) -> String {
        self.count += 1;
        format!("{prefix}{}", self.count)
    }
}

fn s(scalar: Scalar) -> Shape {
    Shape::Scalar(scalar)
}

fn object(fields: Vec<Shape>) -> Shape {
    Shape::Struct(fields)
}

fn array(elem: Shape, len: u64) -> Shape {
    Shape::Array(Box::new(elem), len)
}

fn tagged(tag: Scalar, payloads: Vec<Shape>) -> Shape {
    Shape::Tagged { tag, payloads }
}

/// The fixed corpus.
fn corpus() -> Vec<Shape> {
    let mut shapes = Vec::new();
    for a in Scalar::ALL {
        shapes.push(object(vec![s(a)]));
        for b in Scalar::ALL {
            shapes.push(object(vec![s(a), s(b)]));
            shapes.push(object(vec![s(Scalar::Bit8), s(a), s(b), s(Scalar::Bit8)]));
        }
    }
    let padded = object(vec![s(Scalar::Bit64), s(Scalar::Bit8)]);
    let empty = object(Vec::new());
    shapes.extend([
        // Trailing padding.
        padded.clone(),
        object(vec![s(Scalar::Float128), s(Scalar::Bit8)]),
        object(vec![s(Scalar::Pointer), s(Scalar::Bit16), s(Scalar::Bit8)]),
        // Empty structs (size 0), alone, between fields and in arrays.
        empty.clone(),
        object(vec![s(Scalar::Bit8), empty.clone(), s(Scalar::Bit64)]),
        object(vec![empty.clone(), empty.clone()]),
        array(empty.clone(), 4),
        // Nesting.
        object(vec![s(Scalar::Bit8), padded.clone(), s(Scalar::Bit16)]),
        object(vec![
            object(vec![s(Scalar::Bit8), object(vec![s(Scalar::Float64)])]),
            s(Scalar::Bit8),
        ]),
        // Arrays of structs and of arrays.
        array(padded.clone(), 3),
        object(vec![
            s(Scalar::Bit8),
            array(object(vec![s(Scalar::Bit16), s(Scalar::Bit8)]), 5),
        ]),
        array(array(s(Scalar::Bit16), 3), 5),
        object(vec![
            s(Scalar::Bit8),
            array(s(Scalar::Float64), 3),
            s(Scalar::Bit8),
        ]),
        object(vec![array(s(Scalar::Float128), 2), s(Scalar::Bit32)]),
        // Tagged unions.
        tagged(Scalar::Bit8, Vec::new()),
        tagged(Scalar::Bit16, Vec::new()),
        tagged(Scalar::Bit32, Vec::new()),
        tagged(Scalar::Bit8, vec![s(Scalar::Bit64), s(Scalar::Bit8)]),
        tagged(
            Scalar::Bit8,
            vec![
                array(s(Scalar::Bit8), 256),
                array(s(Scalar::Bit8), 256),
                array(s(Scalar::Bit8), 256),
            ],
        ),
        tagged(Scalar::Bit8, vec![empty.clone(), s(Scalar::Bit32)]),
        tagged(Scalar::Bit32, vec![empty]),
        tagged(Scalar::Bit16, vec![s(Scalar::Float128), s(Scalar::Bit8)]),
        tagged(Scalar::Bit8, vec![s(Scalar::Float64)]),
        tagged(Scalar::Bit16, vec![array(s(Scalar::Bit8), 3)]),
        object(vec![
            s(Scalar::Bit8),
            tagged(Scalar::Bit8, vec![padded, s(Scalar::Pointer)]),
            s(Scalar::Bit8),
        ]),
        array(tagged(Scalar::Bit16, vec![s(Scalar::Bit64)]), 3),
    ]);
    shapes.extend(enums());
    shapes
}

/// Enums as Lilac lays them out (robustness plan phase 4): mixed payloads
/// of every alignment, nested enums, arrays of enums and enums in objects.
fn enums() -> Vec<Shape> {
    let mixed = tagged(
        Scalar::Bit8,
        vec![
            s(Scalar::Bit8),
            s(Scalar::Bit16),
            s(Scalar::Bit64),
            s(Scalar::Pointer),
            s(Scalar::Float32),
            s(Scalar::Float64),
        ],
    );
    let lone = tagged(Scalar::Bit8, vec![s(Scalar::Bit16)]);
    let odd = tagged(
        Scalar::Bit8,
        vec![array(s(Scalar::Bit8), 3), s(Scalar::Bit16)],
    );
    vec![
        mixed.clone(),
        lone.clone(),
        odd.clone(),
        tagged(Scalar::Bit16, vec![s(Scalar::Bit64), s(Scalar::Bit8)]),
        tagged(Scalar::Bit8, vec![s(Scalar::Float16), s(Scalar::Bit8)]),
        tagged(Scalar::Bit8, vec![s(Scalar::Float128), s(Scalar::Bit32)]),
        tagged(
            Scalar::Bit8,
            vec![
                object(vec![s(Scalar::Bit32), s(Scalar::Bit32)]),
                s(Scalar::Bit8),
            ],
        ),
        // Nested.
        tagged(
            Scalar::Bit8,
            vec![mixed.clone(), s(Scalar::Bit8), odd.clone()],
        ),
        tagged(
            Scalar::Bit8,
            vec![tagged(Scalar::Bit8, vec![lone.clone()]), s(Scalar::Bit8)],
        ),
        tagged(
            Scalar::Bit32,
            vec![tagged(Scalar::Bit8, vec![s(Scalar::Bit8)])],
        ),
        // Arrays of enums, and enums in objects.
        array(mixed.clone(), 3),
        array(odd.clone(), 5),
        tagged(Scalar::Bit8, vec![array(mixed.clone(), 2), lone.clone()]),
        object(vec![
            s(Scalar::Bit8),
            mixed,
            s(Scalar::Bit8),
            array(odd, 3),
            s(Scalar::Bit16),
            lone,
        ]),
    ]
}

/// A small xorshift generator: the same shapes on every run.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    fn index(&mut self, len: usize) -> usize {
        usize::try_from(self.below(len as u64)).unwrap_or(0)
    }

    fn shape(&mut self, depth: u32) -> Shape {
        let tags = [Scalar::Bit8, Scalar::Bit16, Scalar::Bit32];
        let choice = if depth == 0 { 0 } else { self.below(8) };
        match choice {
            0..=3 => s(Scalar::ALL[self.index(Scalar::ALL.len())]),
            4 => array(self.shape(depth - 1), 1 + self.below(4)),
            5 => tagged(
                tags[self.index(tags.len())],
                (0..self.below(4)).map(|_| self.shape(depth - 1)).collect(),
            ),
            _ => object((0..self.below(5)).map(|_| self.shape(depth - 1)).collect()),
        }
    }
}

#[test]
fn layouts_match_clang_on_every_target() {
    let Some(clang) = clang() else {
        eprintln!(
            "skipping the C layout conformance test: no clang under `$LLVM_SYS_181_PREFIX/bin`"
        );
        return;
    };
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("c_conformance");
    std::fs::create_dir_all(&scratch).expect("scratch folder");
    let mut shapes = corpus();
    let mut random = Random(0x9E37_79B9_7F4A_7C15);
    shapes.extend((0..200).map(|_| random.shape(3)));
    let mut float128 = Vec::new();
    for target in targets() {
        let floats = c_floats(&clang, target);
        if floats.float128.is_some() {
            float128.push(target.to_triple());
        }
        let mut corpus = Corpus {
            target: &target,
            floats: &floats,
            empty_structs: !(target.os == Os::Windows
                && matches!(target.abi, Abi::Msvc | Abi::None)),
            text: "#include <stddef.h>\n#include <stdint.h>\n".to_owned(),
            count: 0,
            asserts: 0,
        };
        for shape in &shapes {
            corpus.add(shape);
        }
        assert!(
            corpus.asserts > 1500,
            "{target}: {} asserts",
            corpus.asserts
        );
        let file = scratch.join(format!("{target}.c"));
        std::fs::write(&file, &corpus.text).expect("write the corpus");
        let output = Command::new(&clang)
            .args([
                "-ffreestanding",
                "-nostdlibinc",
                "-fsyntax-only",
                "-std=c11",
                "-w",
            ])
            .arg(format!("--target={target}"))
            .arg(&file)
            .output()
            .expect("clang runs");
        assert!(
            output.status.success(),
            "C disagrees with lilac-layout on `{target}` ({}):\n{}",
            file.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    // The 128-bit float is covered where C has one.
    for triple in [
        "x86_64-unknown-linux-gnu",
        "i686-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "riscv32-unknown-none",
        "riscv64-unknown-linux-gnu",
        "wasm32-unknown-unknown",
    ] {
        assert!(
            float128.iter().any(|t| t == triple),
            "{triple}: {float128:?}"
        );
    }
}
