//! Lilac's real enum layouts against the C compiler: a corpus of Lilac
//! enums (no payloads, one payload, payloads of different sizes and
//! alignments, nested enums, arrays of enums, enums in objects, generic
//! instances and a 16-bit tag) is checked and lowered for every supported
//! target, and each layout lowering produced is declared in C as its
//! equivalent (an object as the struct of its fields, an enum as
//! `struct { uintN_t tag; union { P0 p0; P1 p1; … } payload; }`) with a
//! `_Static_assert` on its `sizeof`, `_Alignof` and every `offsetof`. The
//! toolchain Clang (`$LLVM_SYS_181_PREFIX/bin/clang`) checks them with
//! `-fsyntax-only --target=<triple>`. Skipped, with a message, when that
//! Clang is missing.
//!
//! `lilac-layout`'s own conformance test checks the layout function over
//! every scalar mix; this one checks that the compiler hands enums to it as
//! tagged unions, so a Lilac enum crosses an `extern` as that C struct.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use lilac_error::Diags;
use lilac_hir::Layout;
use lilac_ir::{LirLayout, LirModule, LirTy};
use lilac_span::SourceMap;
use lilac_target::Target;
use lilac_ty::Types;

/// The toolchain Clang, when it is installed.
fn clang() -> Option<PathBuf> {
    let prefix = std::env::var_os("LLVM_SYS_181_PREFIX")?;
    let exe = if cfg!(windows) { "clang.exe" } else { "clang" };
    let path = Path::new(&prefix).join("bin").join(exe);
    path.is_file().then_some(path)
}

/// The targets checked: every supported one, plus the ABI variants whose C
/// differs (MinGW, musl, Arm bare metal).
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

/// The Lilac corpus. Every type is concrete (a generic enum through the
/// instances `uses` names), and no object is empty (C under the Microsoft
/// ABI has no empty struct to compare with).
fn source() -> String {
    let mut src = String::from(
        "Point: structure {\n    x: @bit32\n    y: @bit32\n}\n\
         Padded: structure {\n    a: @bit64\n    b: @bit8\n}\n\
         Plain: either {\n    A\n    B\n    C\n}\n\
         Lone: either {\n    Only: @bit16\n}\n\
         Mixed: either {\n    Nothing\n    Byte: @bit8\n    Half: @bit16\n    Word: @bit64\n    \
         Pointer: ref Point\n    Native: @bit_native\n    Real: @fbit64\n    Short: @fbit32\n}\n\
         Aligned: either {\n    Small: @bit8\n    Struct: Padded\n    Pair: Point\n}\n\
         Nested: either {\n    Inner: Mixed\n    Flag: @bit8\n    Again: Aligned\n}\n\
         Deep: either {\n    Wrap: Nested\n    Empty\n}\n\
         Bytes: either {\n    A: @array(@bit8, 0x100)\n    B: @array(@bit8, 0x100)\n    \
         C: @array(@bit8, 0x100)\n}\n\
         Odd: either {\n    Three: @array(@bit8, 0x3)\n    Two: @bit16\n}\n\
         Holder: structure {\n    a: @bit8\n    m: Mixed\n    b: @bit8\n    \
         list: @array(Aligned, 0x3)\n    c: @bit16\n    odd: Odd\n}\n\
         Many: either {\n    List: @array(Mixed, 0x2)\n    One: Lone\n    Held: Holder\n}\n\
         Maybe[T]: either {\n    Some: T\n    None\n}\n",
    );
    // 257 variants: a 16-bit tag.
    src.push_str("Wide: either {\n    First: @bit64\n    Second: @bit8\n");
    for index in 0..255 {
        let _ = writeln!(src, "    V{index}");
    }
    src.push_str("}\n");
    src.push_str(
        "uses: function (a: Maybe[@bit64], b: Maybe[ref Point], c: Maybe[Maybe[@bit8]], \
         d: Maybe[Holder], e: Maybe[Plain], f: Maybe[Wide], g: @array(Maybe[@bit16], 0x3)) nothing {\n}\n",
    );
    src
}

/// Checks and lowers `src` for `target`: the module, and whether each of
/// its layouts (by name) is a Lilac enum, as the checked program says.
fn lower_on(src: &str, target: Target) -> (LirModule, HashMap<String, bool>) {
    let mut sources = SourceMap::new();
    let mut diags = Diags::new();
    let project = lilac_module::single_file("enums.lilac", src, &mut sources);
    let (programs, _) = lilac_module::parse_all(&project, &mut diags, false);
    let res = lilac_resolve::resolve(&project, &programs, &mut diags);
    let mut types = Types::new();
    let options = lilac_typeck::Options {
        target,
        require_entry: false,
        cancel: None,
    };
    let checked = lilac_typeck::check(&res, &mut types, &mut diags, options);
    assert!(!diags.has_errors(), "{target}: {diags:?}");
    // Lowering names each layout by its type's mangled name.
    let enums = checked
        .hir
        .layouts
        .iter()
        .map(|(id, layout)| (types.mangle(*id), matches!(layout, Layout::Enum { .. })))
        .collect();
    let module = lilac_lower::lower(&checked.hir, &res, &types, &sources, target, None).module;
    (module, enums)
}

/// C declarations of a module's layouts, with their asserts.
struct Corpus<'a> {
    layouts: &'a HashMap<String, LirLayout>,
    /// Whether each layout is a Lilac enum: the C shape to declare comes
    /// from the language, not from the layout under test.
    enums: &'a HashMap<String, bool>,
    /// The C name of each layout declared so far.
    names: HashMap<String, String>,
    text: String,
    count: usize,
    asserts: usize,
    /// Enums declared.
    enums_declared: usize,
}

impl Corpus<'_> {
    fn assert(&mut self, condition: &str) {
        self.asserts += 1;
        let _ = writeln!(self.text, "_Static_assert({condition}, \"{condition}\");");
    }

    fn fresh(&mut self, prefix: &str) -> String {
        self.count += 1;
        format!("{prefix}{}", self.count)
    }

    /// The C spelling of `ty`, declaring what it names first.
    fn c_type(&mut self, ty: &LirTy) -> String {
        match ty {
            LirTy::Bit(bits @ (8 | 16 | 32 | 64)) => format!("uint{bits}_t"),
            LirTy::BitNative => "uintptr_t".to_owned(),
            LirTy::FBit(32) => "float".to_owned(),
            LirTy::FBit(64) => "double".to_owned(),
            LirTy::Ref(_) => "void *".to_owned(),
            LirTy::Array(elem, len) => {
                let elem = self.c_type(elem);
                let name = self.fresh("A");
                let _ = writeln!(self.text, "typedef {elem} {name}[{len}];");
                name
            }
            LirTy::Named(name) => self.declare(name),
            other => panic!("`{other}` is not in the corpus"),
        }
    }

    /// Declares the layout `name` (once) and asserts it; returns its C name.
    fn declare(&mut self, name: &str) -> String {
        if let Some(c_name) = self.names.get(name) {
            return c_name.clone();
        }
        let layout = self
            .layouts
            .get(name)
            .unwrap_or_else(|| panic!("no layout `{name}`"))
            .clone();
        let fields: Vec<String> = layout
            .fields
            .iter()
            .map(|field| self.c_type(&field.ty))
            .collect();
        let is_enum = *self
            .enums
            .get(name)
            .unwrap_or_else(|| panic!("`{name}` is no Lilac type"));
        let c_name = if is_enum {
            // `struct { tag; union { payloads… } payload; }`.
            self.enums_declared += 1;
            let c_name = self.fresh("U");
            let (tag, payloads) = fields.split_first().expect("a tag");
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
                "typedef struct {c_name} {{ {tag} tag;{union} }} {c_name};"
            );
            self.assert(&format!(
                "offsetof({c_name}, tag) == {}",
                layout.fields[0].offset
            ));
            for (index, field) in layout.fields.iter().skip(1).enumerate() {
                self.assert(&format!(
                    "offsetof({c_name}, payload.p{index}) == {}",
                    field.offset
                ));
            }
            c_name
        } else {
            // The struct of the fields, in order.
            let c_name = self.fresh("S");
            let mut body = String::new();
            for (index, field) in fields.iter().enumerate() {
                let _ = write!(body, " {field} f{index};");
            }
            let _ = writeln!(self.text, "typedef struct {c_name} {{{body} }} {c_name};");
            for (index, field) in layout.fields.iter().enumerate() {
                self.assert(&format!("offsetof({c_name}, f{index}) == {}", field.offset));
            }
            c_name
        };
        self.assert(&format!("sizeof({c_name}) == {}", layout.size));
        self.assert(&format!("_Alignof({c_name}) == {}", layout.align));
        self.names.insert(name.to_owned(), c_name.clone());
        c_name
    }
}

#[test]
fn enum_layouts_match_clang_on_every_target() {
    let Some(clang) = clang() else {
        eprintln!("skipping the either C layout test: no clang under `$LLVM_SYS_181_PREFIX/bin`");
        return;
    };
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("enum_c_layout");
    std::fs::create_dir_all(&scratch).expect("scratch folder");
    let src = source();
    for target in targets() {
        let (module, enums) = lower_on(&src, target);
        assert_eq!(
            enums.len(),
            module.layouts.len(),
            "{target}: a layout per type"
        );
        let mut corpus = Corpus {
            layouts: &module.layouts,
            enums: &enums,
            names: HashMap::new(),
            text: "#include <stddef.h>\n#include <stdint.h>\n".to_owned(),
            count: 0,
            asserts: 0,
            enums_declared: 0,
        };
        let mut names: Vec<&String> = module.layouts.keys().collect();
        names.sort();
        for name in names {
            corpus.declare(name);
        }
        // 10 enums and 8 instances of `Maybe`, each declared as its C
        // struct-with-union.
        assert_eq!(corpus.enums_declared, 18, "{target}");
        assert!(corpus.asserts > 100, "{target}: {} asserts", corpus.asserts);
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
            "C disagrees with Lilac's either layouts on `{target}` ({}):\n{}",
            file.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
