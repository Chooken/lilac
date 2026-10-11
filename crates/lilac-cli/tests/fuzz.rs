//! Mutation smoke test (robustness plan 0.2): mutants of every `tests/run`
//! program and every `lib/Standard` file are compiled in process, tolerant
//! (as the language server compiles) and strict (as `lilac build` does,
//! without the backend). A compiler panic, or a compile still running after
//! [`TIME_LIMIT`], fails the test and prints the mutant.
//!
//! Mutants come from a fixed seed: mutant `i` is the same on every run and
//! every machine, whatever the iteration count. `LILAC_FUZZ_ITERATIONS`
//! raises the count for a long local run:
//!
//! ```text
//! LILAC_FUZZ_ITERATIONS=20000 cargo test --release -p lilac-cli --test fuzz
//! ```

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::Duration;

use lilac_driver::{Entry, Session};
use lilac_module::Fs;

/// Mutants per `cargo test` run.
const DEFAULT_ITERATIONS: usize = 400;
/// The longest a compile may take before it counts as a hang.
const TIME_LIMIT: Duration = Duration::from_secs(10);
/// The seed of mutant 0; mutant `i` mixes `i` into it.
const SEED: u64 = 0x5DEE_CE66_D1CE_4E5B;

/// Text spliced into mutants: delimiters, the bytes the lexer branches on,
/// keywords and builtins, so mutants reach past the parser.
const TOKENS: &[&str] = &[
    "$\"",
    "\"",
    "'",
    "}",
    "{",
    "\\",
    "@",
    "(",
    ")",
    "[",
    "]",
    ":",
    "=",
    ".",
    ",",
    "->",
    "=>",
    "...",
    "//",
    "\n",
    " ",
    "ref ",
    "function",
    "object",
    "enum",
    "match",
    "if",
    "else",
    "loop",
    "break",
    "continue",
    "return",
    "fail",
    "using",
    "private",
    "export",
    "extern",
    "nothing",
    "self",
    "T",
    "[T]",
    "0x0",
    "0xFFFFFFFFFFFFFFFF",
    "1.5",
    "@bit32",
    "@bit_native",
    "@move(",
    "@array(",
    "@size_of(",
    "@panic(",
    "@discard(",
    "@entry",
    "@on_drop",
    " as ",
    " and ",
    " or ",
];

/// A xorshift64 generator: deterministic and dependency-free.
struct Rng(u64);

impl Rng {
    /// The generator of mutant `index`.
    fn for_mutant(index: usize) -> Self {
        let index = u64::try_from(index).expect("index fits u64");
        // SplitMix64's finalizer spreads neighbouring indices apart; `| 1`
        // keeps the state nonzero, which xorshift needs.
        let mut z = SEED ^ index.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        Self((z ^ (z >> 31)) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// A number in `0..bound` (`bound > 0`; the modulo bias is immaterial).
    fn below(&mut self, bound: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(bound).expect("bound fits u64"))
            .expect("below a usize bound")
    }
}

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// One file to mutate, and how to compile a mutant of it.
struct Seed {
    /// The file the mutant replaces.
    path: PathBuf,
    /// Its original text.
    text: String,
    /// Mounted libraries.
    libraries: Vec<(String, PathBuf)>,
    /// `None`: the mutant is the program. `Some(program)`: the mutant is a
    /// library file, and this program (which mounts that library) is
    /// compiled with the mutant in place of the file.
    host: Option<&'static str>,
}

/// A program that mounts `Standard`; its whole tree loads, so a mutated
/// `Standard` file is always compiled.
const STANDARD_HOST: &str =
    "using Standard.Io\n\nmain: function () nothing {\n    print_line(\"hi\")\n}\n";

/// Every `*.lilac` under `dir`, sorted, skipping `build` output folders.
fn lilac_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("reads a corpus folder")
        .map(|entry| entry.expect("reads a folder entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name != "build") {
                lilac_files(&path, out);
            }
        } else if path.extension().is_some_and(|ext| ext == "lilac") {
            out.push(path);
        }
    }
}

/// The libraries a run-test program mounts (its `// lib:` and
/// `// no-standard` header lines, as `tests/run.rs` reads them).
fn libraries_of(text: &str, standard: &Path) -> Vec<(String, PathBuf)> {
    let mut libraries = Vec::new();
    let mut no_standard = false;
    for line in text.lines() {
        let Some(comment) = line.trim().strip_prefix("//") else {
            continue;
        };
        let comment = comment.trim();
        if comment == "no-standard" {
            no_standard = true;
        } else if let Some(rest) = comment.strip_prefix("lib:") {
            if let Some((name, dir)) = rest.trim().split_once('=') {
                libraries.push((name.to_owned(), repo().join(dir)));
            }
        }
    }
    if !no_standard {
        libraries.push(("Standard".to_owned(), standard.to_path_buf()));
    }
    libraries
}

fn corpus() -> Vec<Seed> {
    let standard = repo().join("lib/Standard");
    let mut seeds = Vec::new();
    let mut programs = Vec::new();
    lilac_files(&repo().join("tests/run"), &mut programs);
    for path in programs {
        let text = std::fs::read_to_string(&path).expect("reads a run test");
        // A pending program pins a known bug, which its phase fixes; it
        // joins the corpus when its `// pending:` line goes.
        if text
            .lines()
            .any(|line| line.trim_start().starts_with("// pending:"))
        {
            continue;
        }
        // A folder program's other files are compiled alone; their header
        // is the folder's `main.lilac`.
        let header = match path.parent().map(|dir| dir.join("main.lilac")) {
            Some(main) if main.is_file() && main != path => {
                std::fs::read_to_string(main).expect("reads main.lilac")
            }
            _ => text.clone(),
        };
        seeds.push(Seed {
            libraries: libraries_of(&header, &standard),
            path,
            text,
            host: None,
        });
    }
    let mut library = Vec::new();
    lilac_files(&standard, &mut library);
    for path in library {
        seeds.push(Seed {
            text: std::fs::read_to_string(&path).expect("reads a Standard file"),
            path,
            libraries: vec![("Standard".to_owned(), standard.clone())],
            host: Some(STANDARD_HOST),
        });
    }
    seeds
}

/// Line ranges of `bytes`, each including its `\n`.
fn lines(bytes: &[u8]) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            ranges.push((start, index + 1));
            start = index + 1;
        }
    }
    if start < bytes.len() {
        ranges.push((start, bytes.len()));
    }
    ranges
}

/// Applies one random mutation to `bytes`.
fn mutate(bytes: &mut Vec<u8>, rng: &mut Rng) {
    let lines = lines(bytes);
    match rng.below(9) {
        0 if !lines.is_empty() => {
            let (start, end) = lines[rng.below(lines.len())];
            bytes.drain(start..end);
        }
        1 if !lines.is_empty() => {
            let (start, end) = lines[rng.below(lines.len())];
            let line = bytes[start..end].to_vec();
            bytes.splice(end..end, line);
        }
        2 if lines.len() > 1 => {
            let mut first = lines[rng.below(lines.len())];
            let mut second = lines[rng.below(lines.len())];
            if first.0 > second.0 {
                std::mem::swap(&mut first, &mut second);
            }
            if first != second {
                let mut swapped = bytes[..first.0].to_vec();
                swapped.extend_from_slice(&bytes[second.0..second.1]);
                swapped.extend_from_slice(&bytes[first.1..second.0]);
                swapped.extend_from_slice(&bytes[first.0..first.1]);
                swapped.extend_from_slice(&bytes[second.1..]);
                *bytes = swapped;
            }
        }
        3 | 4 => {
            let at = rng.below(bytes.len() + 1);
            let token = TOKENS[rng.below(TOKENS.len())].as_bytes();
            bytes.splice(at..at, token.iter().copied());
        }
        5 => {
            let at = rng.below(bytes.len() + 1);
            bytes.insert(at, random_byte(rng));
        }
        6 if !bytes.is_empty() => {
            bytes.remove(rng.below(bytes.len()));
        }
        7 if !bytes.is_empty() => {
            let at = rng.below(bytes.len());
            bytes[at] = random_byte(rng);
        }
        8 => bytes.truncate(rng.below(bytes.len() + 1)),
        // The chosen mutation needs lines or bytes the text lacks.
        _ => {}
    }
}

/// Mostly printable ASCII; sometimes any byte (the mutant is read lossily).
fn random_byte(rng: &mut Rng) -> u8 {
    let byte = rng.next().to_le_bytes()[0];
    if rng.below(4) == 0 {
        byte
    } else {
        b' ' + byte % 95
    }
}

/// Mutant `index`: the corpus file it came from, and its text.
fn mutant(corpus: &[Seed], index: usize) -> (&Seed, String) {
    let mut rng = Rng::for_mutant(index);
    let seed = &corpus[rng.below(corpus.len())];
    let mut bytes = seed.text.clone().into_bytes();
    for _ in 0..=rng.below(3) {
        mutate(&mut bytes, &mut rng);
    }
    (seed, String::from_utf8_lossy(&bytes).into_owned())
}

/// Reads the disk, except for one path whose text is given.
struct OverlayFs {
    /// The replaced file, canonical.
    path: Option<PathBuf>,
    text: String,
}

impl Fs for OverlayFs {
    fn read(&mut self, path: &Path) -> io::Result<String> {
        if self.path.is_some() && std::fs::canonicalize(path).ok() == self.path {
            return Ok(self.text.clone());
        }
        std::fs::read_to_string(path)
    }

    fn list_dir(&mut self, dir: &Path) -> io::Result<Vec<PathBuf>> {
        std::fs::read_dir(dir)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect()
    }
}

#[derive(Clone, Copy, Debug)]
enum Mode {
    /// As the language server compiles: past errors, keeping the analysis.
    Tolerant,
    /// As `lilac build` compiles for the host, up to the backend.
    Strict,
}

/// Compiles a mutant once.
fn compile(seed: &Seed, text: &str, mode: Mode) {
    let mut session = Session::new();
    session.libraries.clone_from(&seed.libraries);
    match mode {
        Mode::Tolerant => session.retain_partial_analysis = true,
        Mode::Strict => {
            session.target = session.host;
            session.require_entry = true;
            session.entry = Entry::Native;
            session.stack_traces = true;
        }
    }
    let (name, program, mut fs) = match seed.host {
        None => (
            seed.path.display().to_string(),
            text.to_owned(),
            OverlayFs {
                path: None,
                text: String::new(),
            },
        ),
        Some(host) => (
            "main.lilac".to_owned(),
            host.to_owned(),
            OverlayFs {
                path: Some(std::fs::canonicalize(&seed.path).expect("canonical corpus path")),
                text: text.to_owned(),
            },
        ),
    };
    // Errors are expected; only panics and hangs matter.
    let _ = session.compile_file_with_stages(&name, &program, &mut fs, &mut |_, _| {});
}

/// How a mutant's compile went wrong.
enum Failure {
    Panicked,
    TimedOut,
}

/// Compiles on a thread with the compiler's stack, under the time limit.
fn compile_watched(seed: &'static Seed, text: String, mode: Mode) -> Result<(), Failure> {
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name(format!("fuzz-{mode:?}"))
        .stack_size(lilac_driver::COMPILE_STACK_BYTES)
        .spawn(move || {
            compile(seed, &text, mode);
            // The receiver is gone only when the watchdog gave up.
            let _ = done.send(());
        })
        .expect("spawns a compile thread");
    match finished.recv_timeout(TIME_LIMIT) {
        Ok(()) => Ok(()),
        // The thread ended without sending: it panicked.
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(Failure::Panicked),
        // A hung compile thread is left running; the test fails anyway.
        Err(mpsc::RecvTimeoutError::Timeout) => Err(Failure::TimedOut),
    }
}

fn iterations() -> usize {
    match std::env::var("LILAC_FUZZ_ITERATIONS") {
        Ok(value) => value
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("LILAC_FUZZ_ITERATIONS is not a count: {value:?}")),
        Err(_) => DEFAULT_ITERATIONS,
    }
}

#[test]
fn mutants_compile_without_panics_or_hangs() {
    // Leaked so compile threads a hang leaves behind may outlive the test.
    let corpus: &'static [Seed] = Vec::leak(corpus());
    assert!(!corpus.is_empty(), "the corpus is empty");
    let count = iterations();
    let next = AtomicUsize::new(0);
    let failures: Mutex<HashMap<usize, String>> = Mutex::new(HashMap::new());
    let workers = std::thread::available_parallelism().map_or(1, usize::from);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                if index >= count {
                    break;
                }
                let (seed, text) = mutant(corpus, index);
                for mode in [Mode::Tolerant, Mode::Strict] {
                    let why = match compile_watched(seed, text.clone(), mode) {
                        Ok(()) => continue,
                        Err(Failure::Panicked) => "panicked",
                        Err(Failure::TimedOut) => "ran past the time limit",
                    };
                    let report = format!(
                        "mutant {index} of {} ({mode:?}) {why}; its text:\n\
                         ----\n{text}\n----",
                        seed.path.display()
                    );
                    failures
                        .lock()
                        .expect("no worker panicked holding the lock")
                        .insert(index, report);
                    break;
                }
            });
        }
    });
    let failures = failures.into_inner().expect("no worker panicked");
    let mut indices: Vec<&usize> = failures.keys().collect();
    indices.sort();
    eprintln!("{count} mutants, {} failed", failures.len());
    assert!(
        failures.is_empty(),
        "{} of {count} mutants failed (indices {indices:?}); the first:\n{}",
        failures.len(),
        failures[indices[0]]
    );
}
