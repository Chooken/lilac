//! Shadow-stack stack traces: the call-site text, and the pass that records
//! it.

/// One stack-trace line, newline included: `"  at main (main.lilac:3:5)\n"`,
/// or `"  at main\n"` for a compiler-generated call with no location. The
/// panic site (lowering) and every call site (the pass) are formatted here.
#[must_use]
pub fn site_text(func: &str, location: Option<&str>) -> String {
    match location {
        Some(location) => format!("  at {func} ({location})\n"),
        None => format!("  at {func}\n"),
    }
}
