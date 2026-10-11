//! The lexer's progress property (robustness plan 0.3): for random input,
//! lexing ends, yields at most one item per byte plus the end of file, and
//! every error covers at least one byte past where the previous item ended,
//! so no error repeats in place.
//!
//! Inputs are random strings over an alphabet weighted towards what the
//! lexer branches on, from a fixed seed, so a failure reproduces exactly.

use lilac_lex::Lexer;
use lilac_span::SourceMap;

/// Inputs per property run.
const CASES: usize = 100_000;
/// The longest input, in pieces of the alphabet.
const MAX_PIECES: usize = 24;

/// The pieces inputs are built from: every byte the lexer branches on,
/// keywords and literals, and characters it rejects (multi-byte ones too).
const PIECES: &[&str] = &[
    " ", "\n", "\t", "\r", "//", "/", "a", "_", "Z", "x", "main", "if", "0", "1", "9", "0x", "0b",
    "1.5", "_1", "F", "g", ".", "...", ":", ",", "=", "==", "->", "=>", "{", "}", "(", ")", "[",
    "]", "+", "-", "*", "%", "<", ">", "<=", "!", "@", "\"", "'", "#", "~", "^", "é", "\u{FEFF}",
    "\u{0}",
];

/// Pieces that reach the f-string lexer, and escapes.
const ESCAPE_PIECES: &[&str] = &["$", "$\"", "\\", "\\x", "\\n", "\\\"", "\\q", "}}", "{{"];

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

/// Checks the property on one input; `Err` says how it failed.
fn check_progress(src: &str) -> Result<(), String> {
    let mut map = SourceMap::new();
    let file = map.add_file("fuzz.lilac", src);
    let cap = src.len() + 1;
    let mut items = 0;
    let mut previous_end = 0;
    for item in Lexer::new(src, file) {
        items += 1;
        if items > cap {
            return Err(format!("more than {cap} items (the lexer is not ending)"));
        }
        let end = match item {
            Ok(token) => token.span.hi.0,
            Err(diagnostic) => {
                let Some(label) = diagnostic.labels.iter().find(|label| label.primary) else {
                    return Err(format!("error with no primary label: {diagnostic:?}"));
                };
                let end = label.span.hi.0;
                if end <= previous_end {
                    return Err(format!(
                        "error ending at {end} does not pass the previous item's end \
                         {previous_end}: {:?}",
                        diagnostic.msg
                    ));
                }
                end
            }
        };
        previous_end = previous_end.max(end);
    }
    Ok(())
}

/// Runs the property over `CASES` inputs built from `pieces`.
fn check_random_inputs(seed: u64, pieces: &[&str]) {
    let mut rng = Rng(seed);
    for case in 0..CASES {
        let mut src = String::new();
        for _ in 0..rng.below(MAX_PIECES + 1) {
            src += pieces[rng.below(pieces.len())];
        }
        if let Err(why) = check_progress(&src) {
            panic!("case {case} (seed {seed:#x}), input {src:?}: {why}");
        }
    }
}

#[test]
fn lexing_makes_progress() {
    // No `$` or `\`: f-strings and escapes recover in robustness plan
    // phase 1, and `lexing_makes_progress_through_escapes` covers them.
    check_random_inputs(0x9E37_79B9_7F4A_7C15, PIECES);
}

#[test]
#[ignore = "robustness plan phase 1"]
fn lexing_makes_progress_through_escapes() {
    let pieces: Vec<&str> = PIECES.iter().chain(ESCAPE_PIECES).copied().collect();
    check_random_inputs(0xD1B5_4A32_D192_ED03, &pieces);
}

#[test]
#[ignore = "robustness plan phase 1"]
fn fstring_errors_make_progress() {
    // The review's inputs: each lexed forever.
    for src in ["$\"a}b\"", "$\"\\q\"", "$\"a\\\"\n}\n"] {
        if let Err(why) = check_progress(src) {
            panic!("{src:?}: {why}");
        }
    }
}
