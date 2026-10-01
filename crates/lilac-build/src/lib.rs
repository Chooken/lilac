//! Build-step graph types: pure data + hashing, no process spawning (M6).

/// Optimization level.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Optimize {
    /// Debug builds.
    #[default]
    Debug,
    /// Release builds.
    Release,
}

/// One build step.
#[derive(Clone, Debug)]
pub enum Step {
    /// Compile a unit.
    Compile {
        /// Entry file.
        root: String,
    },
    /// Run tests.
    Test,
}
