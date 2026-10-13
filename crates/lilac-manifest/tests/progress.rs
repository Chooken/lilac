//! The TOML reader on random input, like `lilac-lex/tests/progress.rs`: it
//! never panics and always finishes, returns a table exactly when it
//! reported nothing, and every error is `E075` with a primary label inside
//! the text, on character boundaries.
//!
//! Tests build with debug assertions, so the reader's own progress check
//! (`debug_assert!` in `read_all`) runs on every statement here too.
//!
//! Inputs are random strings over pieces weighted towards what the reader
//! branches on, from a fixed seed, so a failure reproduces exactly.

use lilac_error::Diags;
use lilac_manifest::toml::read;
use lilac_span::SourceMap;

/// Inputs per property run.
const CASES: usize = 100_000;
/// The longest input, in pieces.
const MAX_PIECES: usize = 24;

/// The pieces inputs are built from: every byte the reader branches on,
/// keywords, strings, escapes, headers and characters it rejects.
const PIECES: &[&str] = &[
    " ", "\t", "\n", "\r\n", "\r", "#", "a", "b", "-", "_", "1", "=", ".", ",", "[", "]", "[[",
    "]]", "{", "}", "\"", "'", "\"\"\"", "'''", "\\", "\\u", "\\U", "00e9", "true", "false",
    "1979-05", ":", "é", "\u{FEFF}", "\u{0}", "a = ", "[a]", "a.b = ", "\"s\"", "'s'", "x.y",
    "{ k = ", "[\"", "\\n",
];

/// A xorshift64 generator: deterministic, and enough to pick pieces.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        // The bound is small, so the modulo bias is immaterial.
        usize::try_from(self.next() % u64::try_from(bound).expect("bound fits u64"))
            .expect("below a usize bound")
    }
}

fn check(text: &str) -> Result<(), String> {
    let mut sources = SourceMap::new();
    let file = sources.add_file("lilac.toml", text);
    let mut diags = Diags::new();
    let table = read(file, text, &mut diags);
    if table.is_some() != diags.diagnostics().is_empty() {
        return Err(format!(
            "a table: {}, errors: {:?}",
            table.is_some(),
            diags.diagnostics()
        ));
    }
    for diagnostic in diags.diagnostics() {
        if diagnostic.code != "E075" {
            return Err(format!("not E075: {diagnostic:?}"));
        }
        if diagnostic
            .labels
            .iter()
            .filter(|label| label.primary)
            .count()
            != 1
        {
            return Err(format!("not one primary label: {diagnostic:?}"));
        }
        for label in &diagnostic.labels {
            let (lo, hi) = (label.span.lo.0 as usize, label.span.hi.0 as usize);
            if lo > hi
                || hi > text.len()
                || !text.is_char_boundary(lo)
                || !text.is_char_boundary(hi)
            {
                return Err(format!("a label outside the text: {diagnostic:?}"));
            }
        }
    }
    Ok(())
}

#[test]
fn reading_random_input_finishes() {
    let seed = 0x2545_F491_4F6C_DD1D;
    let mut rng = Rng(seed);
    for case in 0..CASES {
        let mut text = String::new();
        for _ in 0..rng.below(MAX_PIECES + 1) {
            text += PIECES[rng.below(PIECES.len())];
        }
        if let Err(why) = check(&text) {
            panic!("case {case} (seed {seed:#x}), input {text:?}: {why}");
        }
    }
}
