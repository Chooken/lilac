//! Cargo-style progress output for `lilac build`, on stderr.
//!
//! Each line is a bold green verb right-aligned in 12 columns, then a
//! message. The driver's [`Stage`]s are grouped into the steps a user cares
//! about (Loading, Checking, Lowering), and the backend's [`CodegenStep`]s
//! into Optimizing and Writing. `--timings` also prints every individual
//! stage with its duration.

use std::io::IsTerminal as _;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use lilac_codegen::CodegenStep;
use lilac_driver::{Stage, StageEvent};

/// The `--color=auto` choice: colour when stderr is a terminal and
/// `NO_COLOR` is unset. Diagnostics and progress lines share it.
pub fn auto_color() -> bool {
    std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

/// How much the reporter prints.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verbosity {
    /// Nothing but errors (`-q`, or `--error-format=json`).
    Quiet,
    /// One line per step.
    Normal,
    /// One line per step, plus every stage with its duration.
    Timings,
}

struct State {
    verbosity: Verbosity,
    color: bool,
    started: Instant,
    /// The open timed step: its label and start.
    open: Option<(String, Instant)>,
}

/// The progress reporter. Clones share one state, so the driver's hook and
/// the build command print through the same reporter.
#[derive(Clone)]
pub struct Reporter(Arc<Mutex<State>>);

impl Reporter {
    /// A reporter printing at `verbosity`, coloured when `color`.
    pub fn new(verbosity: Verbosity, color: bool) -> Self {
        Self(Arc::new(Mutex::new(State {
            verbosity,
            color,
            started: Instant::now(),
            open: None,
        })))
    }

    /// The shared state (a panic while printing poisons nothing worth
    /// refusing).
    fn state(&self) -> MutexGuard<'_, State> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn verbosity(&self) -> Verbosity {
        self.state().verbosity
    }

    /// Prints one status line (not in quiet mode), closing the open timed
    /// step first so its duration prints above the new line.
    pub fn status(&self, verb: &str, message: &str) {
        self.end();
        let state = self.state();
        if state.verbosity == Verbosity::Quiet {
            return;
        }
        eprintln!("{}", line(verb, message, state.color));
    }

    /// Prints `error: <message>` (always, even in quiet mode).
    pub fn error(&self, message: &str) {
        let color = self.state().color;
        if color {
            eprintln!("\x1b[1;31merror\x1b[0m\x1b[1m: {message}\x1b[0m");
        } else {
            eprintln!("error: {message}");
        }
    }

    /// Starts a timed step, closing the open one. Only `--timings` prints
    /// the durations.
    fn begin(&self, label: &str) {
        self.end();
        self.state().open = Some((label.to_owned(), Instant::now()));
    }

    /// Closes the open timed step, printing its duration under `--timings`.
    pub fn end(&self) {
        let open = self.state().open.take();
        if let Some((label, start)) = open {
            if self.verbosity() == Verbosity::Timings {
                let color = self.state().color;
                let text = format!("{:>12} {label} {}", "", format_duration(start.elapsed()));
                if color {
                    eprintln!("\x1b[2m{text}\x1b[0m");
                } else {
                    eprintln!("{text}");
                }
            }
        }
    }

    /// The driver's progress hook.
    pub fn stage(&self, stage: Stage, event: StageEvent) {
        match event {
            StageEvent::Begin => {
                if stage == Stage::Lower {
                    self.status("Lowering", "");
                }
                self.begin(stage_label(stage));
            }
            StageEvent::End { count } => {
                self.end();
                match (stage, count) {
                    (Stage::Load, Some(n)) => self.status("Loading", &plural(n, "file")),
                    (Stage::Check, Some(n)) => self.status("Checking", &plural(n, "function")),
                    _ => {}
                }
            }
        }
    }

    /// The backend's step hook; `mode` names the build mode.
    pub fn codegen(&self, step: CodegenStep, mode: &str, object: &str) {
        match step {
            CodegenStep::Lower => {
                self.status("Optimizing", mode);
                self.begin("codegen lower");
            }
            CodegenStep::Verify => self.begin("codegen verify"),
            CodegenStep::Optimize => self.begin("optimize"),
            CodegenStep::Emit => {
                self.status("Writing", object);
                self.begin("emit");
            }
        }
    }

    /// Starts the link step; `--timings` also prints the command line.
    pub fn linking(&self, exe: &str, linker: &str, command: &str) {
        self.status("Linking", &format!("{exe} ({linker})"));
        if self.verbosity() == Verbosity::Timings {
            eprintln!("{:>12} {command}", "");
        }
        self.begin("link");
    }

    /// Prints the final `Finished` line.
    pub fn finished(&self, mode: &str, exe: &str) {
        self.end();
        let elapsed = self.state().started.elapsed();
        self.status(
            "Finished",
            &format!("{mode} in {} → {exe}", format_duration(elapsed)),
        );
    }
}

/// One status line: the verb right-aligned in 12 columns.
pub fn line(verb: &str, message: &str, color: bool) -> String {
    let text = if color {
        format!("\x1b[1;32m{verb:>12}\x1b[0m {message}")
    } else {
        format!("{verb:>12} {message}")
    };
    text.trim_end().to_owned()
}

fn plural(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

fn stage_label(stage: Stage) -> &'static str {
    match stage {
        Stage::Load => "load",
        Stage::Parse => "parse",
        Stage::Resolve => "resolve",
        Stage::Check => "check",
        Stage::Lower => "lower",
        Stage::Runtime => "runtime",
        Stage::Prune => "prune",
        Stage::Verify => "verify",
    }
}

/// `0.84s` above a second, else `12.3ms`.
pub fn format_duration(duration: Duration) -> String {
    let secs = duration.as_secs_f64();
    if secs >= 1.0 {
        format!("{secs:.2}s")
    } else {
        format!("{:.1}ms", secs * 1000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbs_right_align_in_twelve_columns() {
        assert_eq!(line("Linking", "a.exe", false), "     Linking a.exe");
        assert_eq!(line("Lowering", "", false), "    Lowering");
        assert_eq!(
            line("Finished", "x", true),
            "\x1b[1;32m    Finished\x1b[0m x"
        );
    }

    #[test]
    fn counts_and_durations_format() {
        assert_eq!(plural(1, "file"), "1 file");
        assert_eq!(plural(3, "file"), "3 files");
        assert_eq!(format_duration(Duration::from_millis(840)), "840.0ms");
        assert_eq!(format_duration(Duration::from_millis(1500)), "1.50s");
    }
}
