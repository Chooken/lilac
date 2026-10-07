//! Target identity and the pointer-width rule.
//!
//! This crate is the single source of truth for target triples. It parses a
//! triple into its four structured components (`arch`, `vendor`, `os`, `abi`)
//! and renders it back in canonical form.

/// Instruction-set architecture component of a target triple.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum Arch {
    /// 32-bit x86.
    X86,
    /// 64-bit x86.
    X86_64,
    /// 32-bit ARM.
    Arm,
    /// 64-bit ARM.
    Aarch64,
    /// 32-bit RISC-V.
    Riscv32,
    /// 64-bit RISC-V.
    Riscv64,
    /// 32-bit WebAssembly.
    Wasm32,
    /// 64-bit WebAssembly.
    Wasm64,
}

impl Arch {
    /// Parses an architecture name.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "x86" => Some(Self::X86),
            "x86_64" => Some(Self::X86_64),
            "arm" => Some(Self::Arm),
            "aarch64" => Some(Self::Aarch64),
            "riscv32" => Some(Self::Riscv32),
            "riscv64" => Some(Self::Riscv64),
            "wasm32" => Some(Self::Wasm32),
            "wasm64" => Some(Self::Wasm64),
            _ => None,
        }
    }

    /// Returns the canonical name of the architecture.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::X86 => "x86",
            Self::X86_64 => "x86_64",
            Self::Arm => "arm",
            Self::Aarch64 => "aarch64",
            Self::Riscv32 => "riscv32",
            Self::Riscv64 => "riscv64",
            Self::Wasm32 => "wasm32",
            Self::Wasm64 => "wasm64",
        }
    }

    /// Returns whether pointers on this architecture are 64 bits wide.
    #[must_use]
    pub fn is_64_bit(self) -> bool {
        matches!(self, Self::X86_64 | Self::Aarch64 | Self::Riscv64 | Self::Wasm64)
    }
}

/// Vendor component of a target triple.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum Vendor {
    /// No specific vendor.
    Unknown,
    /// PC / generic personal computer vendor.
    Pc,
    /// Apple.
    Apple,
}

impl Vendor {
    /// Parses a vendor name.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "unknown" => Some(Self::Unknown),
            "pc" => Some(Self::Pc),
            "apple" => Some(Self::Apple),
            _ => None,
        }
    }

    /// Returns the canonical name of the vendor.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Pc => "pc",
            Self::Apple => "apple",
        }
    }
}

/// Operating-system component of a target triple.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum Os {
    /// No operating system (bare metal).
    None,
    /// Unknown operating system.
    Unknown,
    /// Linux.
    Linux,
    /// Windows.
    Windows,
    /// macOS (rendered as `darwin`).
    Macos,
}

impl Os {
    /// Parses an operating-system name.
    ///
    /// `"macos"` is accepted as an input alias; [`Os::as_str`] always renders
    /// the canonical `darwin`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "none" => Some(Self::None),
            "unknown" => Some(Self::Unknown),
            "linux" => Some(Self::Linux),
            "windows" => Some(Self::Windows),
            "darwin" | "macos" => Some(Self::Macos),
            _ => None,
        }
    }

    /// Returns the canonical name of the operating system.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Unknown => "unknown",
            Self::Linux => "linux",
            Self::Windows => "windows",
            Self::Macos => "darwin",
        }
    }
}

/// ABI component of a target triple.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum Abi {
    /// No ABI suffix.
    None,
    /// GNU ABI.
    Gnu,
    /// Microsoft Visual C++ ABI.
    Msvc,
    /// musl ABI.
    Musl,
}

impl Abi {
    /// Parses an ABI name.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "none" => Some(Self::None),
            "gnu" => Some(Self::Gnu),
            "msvc" => Some(Self::Msvc),
            "musl" => Some(Self::Musl),
            _ => None,
        }
    }

    /// Returns the canonical name of the ABI.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Gnu => "gnu",
            Self::Msvc => "msvc",
            Self::Musl => "musl",
        }
    }
}

/// A structured target triple.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct Target {
    /// Instruction-set architecture.
    pub arch: Arch,
    /// Vendor.
    pub vendor: Vendor,
    /// Operating system.
    pub os: Os,
    /// ABI.
    pub abi: Abi,
}

impl Target {
    /// Parses a target triple into its structured components.
    ///
    /// Canonical output is produced by [`Target::to_triple`]; the parser also
    /// accepts vendor-omitted forms such as `x86_64-linux-gnu`.
    ///
    /// # Errors
    ///
    /// Returns [`ParseError`] if the triple is empty, has too many components,
    /// or contains an unrecognized architecture, vendor, OS, or ABI.
    pub fn parse(s: &str) -> Result<Self, ParseError> {
        let parts: Vec<&str> = s.split('-').collect();
        if s.is_empty() {
            return Err(ParseError::Empty);
        }
        if parts.len() > 4 {
            return Err(ParseError::TooManyComponents);
        }
        let arch =
            Arch::parse(parts[0]).ok_or_else(|| ParseError::UnknownArch(parts[0].to_owned()))?;
        match parts.len() {
            1 => Ok(Self {
                arch,
                vendor: Vendor::Unknown,
                os: Os::None,
                abi: Abi::None,
            }),
            2 => {
                let os = Os::parse(parts[1])
                    .ok_or_else(|| ParseError::UnknownOs(parts[1].to_owned()))?;
                Ok(Self {
                    arch,
                    vendor: Vendor::Unknown,
                    os,
                    abi: Abi::None,
                })
            }
            3 => {
                if let (Some(v), Some(o)) = (Vendor::parse(parts[1]), Os::parse(parts[2])) {
                    Ok(Self {
                        arch,
                        vendor: v,
                        os: o,
                        abi: Abi::None,
                    })
                } else if let Some(o) = Os::parse(parts[1]) {
                    let abi = Abi::parse(parts[2])
                        .ok_or_else(|| ParseError::UnknownAbi(parts[2].to_owned()))?;
                    Ok(Self {
                        arch,
                        vendor: Vendor::Unknown,
                        os: o,
                        abi,
                    })
                } else {
                    Err(ParseError::UnknownVendor(parts[1].to_owned()))
                }
            }
            _ => {
                let vendor = Vendor::parse(parts[1])
                    .ok_or_else(|| ParseError::UnknownVendor(parts[1].to_owned()))?;
                let os = Os::parse(parts[2])
                    .ok_or_else(|| ParseError::UnknownOs(parts[2].to_owned()))?;
                let abi = Abi::parse(parts[3])
                    .ok_or_else(|| ParseError::UnknownAbi(parts[3].to_owned()))?;
                Ok(Self {
                    arch,
                    vendor,
                    os,
                    abi,
                })
            }
        }
    }

    /// Renders the target in canonical `arch-vendor-os[-abi]` form.
    #[must_use]
    pub fn to_triple(&self) -> String {
        let arch = self.arch.as_str();
        let vendor = self.vendor.as_str();
        let os = self.os.as_str();
        if self.abi == Abi::None {
            format!("{arch}-{vendor}-{os}")
        } else {
            format!("{arch}-{vendor}-{os}-{}", self.abi.as_str())
        }
    }

    /// Returns the target for the host platform, or [`Target::default`] if the
    /// host is not recognized.
    #[must_use]
    pub fn host() -> Self {
        let arch = match std::env::consts::ARCH {
            "x86_64" => Arch::X86_64,
            "x86" => Arch::X86,
            "aarch64" => Arch::Aarch64,
            "arm" => Arch::Arm,
            _ => return Self::default(),
        };
        let (vendor, os, abi) = match std::env::consts::OS {
            "windows" => (Vendor::Pc, Os::Windows, Abi::Msvc),
            "linux" => (Vendor::Unknown, Os::Linux, Abi::Gnu),
            "macos" => (Vendor::Apple, Os::Macos, Abi::None),
            _ => return Self::default(),
        };
        Self {
            arch,
            vendor,
            os,
            abi,
        }
    }

    /// Returns the native pointer width in bits for this target.
    #[must_use]
    pub fn bit_native_width(&self) -> u32 {
        if self.arch.is_64_bit() {
            64
        } else {
            32
        }
    }
}

impl Default for Target {
    /// The bare-metal default (`x86_64-unknown-none`) is deliberately
    /// host-independent so default `emit-ir` output is stable across build
    /// machines.
    fn default() -> Self {
        Self {
            arch: Arch::X86_64,
            vendor: Vendor::Unknown,
            os: Os::None,
            abi: Abi::None,
        }
    }
}

/// Failure modes of [`Target::parse`].
#[non_exhaustive]
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ParseError {
    /// The input was empty.
    Empty,
    /// The input had more than four dash-separated components.
    TooManyComponents,
    /// The architecture component was not recognized.
    UnknownArch(String),
    /// The vendor component was not recognized.
    UnknownVendor(String),
    /// The operating-system component was not recognized.
    UnknownOs(String),
    /// The ABI component was not recognized.
    UnknownAbi(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "empty target"),
            Self::TooManyComponents => write!(f, "too many target components"),
            Self::UnknownArch(v) => write!(f, "unknown architecture `{v}`"),
            Self::UnknownVendor(v) => write!(f, "unknown vendor `{v}`"),
            Self::UnknownOs(v) => write!(f, "unknown os `{v}`"),
            Self::UnknownAbi(v) => write!(f, "unknown abi `{v}`"),
        }
    }
}

impl std::error::Error for ParseError {}

/// Returns whether the target triple denotes a 64-bit architecture.
#[must_use]
pub fn is_64_bit_triple(s: &str) -> bool {
    let arch = s.split('-').next().unwrap_or("");
    if let Some(arch) = Arch::parse(arch) {
        arch.is_64_bit()
    } else {
        arch.contains("64")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_canonical_triples() {
        for t in [
            "x86_64-unknown-none",
            "x86_64-pc-windows-msvc",
            "x86-unknown-linux-gnu",
            "aarch64-apple-darwin",
            "riscv32-unknown-none",
        ] {
            let parsed = Target::parse(t).unwrap_or_else(|e| panic!("{t}: {e}"));
            assert_eq!(parsed.to_triple(), t, "{t}");
            assert_eq!(
                Target::parse(&parsed.to_triple()).expect("reparse"),
                parsed,
                "{t}"
            );
        }
    }

    #[test]
    fn vendor_omitted_forms_normalize() {
        // Vendor-omitted inputs parse correctly and render the canonical vendor.
        assert_eq!(
            Target::parse("x86_64-linux-gnu")
                .expect("valid")
                .to_triple(),
            "x86_64-unknown-linux-gnu"
        );
        assert_eq!(
            Target::parse("x86_64-linux").expect("valid").to_triple(),
            "x86_64-unknown-linux"
        );
    }

    #[test]
    fn unknown_is_vendor_os_first() {
        let t = Target::parse("x86_64-unknown-none").expect("valid");
        assert_eq!((t.vendor, t.os), (Vendor::Unknown, Os::None));
        assert_eq!(t.to_triple(), "x86_64-unknown-none");
    }

    #[test]
    fn os_unknown_with_vendor_parses() {
        let t = Target::parse("x86_64-unknown-unknown").expect("valid");
        assert_eq!(
            (t.arch, t.vendor, t.os, t.abi),
            (Arch::X86_64, Vendor::Unknown, Os::Unknown, Abi::None)
        );
        assert_eq!(t.to_triple(), "x86_64-unknown-unknown");
    }

    #[test]
    fn vendor_omitted_musl_normalizes() {
        let t = Target::parse("x86_64-linux-musl").expect("valid");
        assert_eq!(
            (t.arch, t.vendor, t.os, t.abi),
            (Arch::X86_64, Vendor::Unknown, Os::Linux, Abi::Musl)
        );
        assert_eq!(t.to_triple(), "x86_64-unknown-linux-musl");
    }

    #[test]
    fn macos_renders_darwin() {
        let t = Target::parse("aarch64-apple-darwin").expect("valid");
        assert_eq!(t.os, Os::Macos);
        assert_eq!(t.to_triple(), "aarch64-apple-darwin");
    }

    #[test]
    fn rejects_unknown_and_malformed() {
        assert!(matches!(
            Target::parse("foo"),
            Err(ParseError::UnknownArch(_))
        ));
        assert!(matches!(
            Target::parse("x86_64-foo-linux"),
            Err(ParseError::UnknownVendor(_))
        ));
        assert!(matches!(
            Target::parse("x86_64-linux-foo"),
            Err(ParseError::UnknownAbi(_))
        ));
        assert!(matches!(
            Target::parse("a-b-c-d-e"),
            Err(ParseError::TooManyComponents)
        ));
        assert!(matches!(Target::parse(""), Err(ParseError::Empty)));
    }

    #[test]
    #[allow(clippy::bool_assert_comparison)]
    fn width_agrees_for_every_arch() {
        let expected = [
            (Arch::X86, 32),
            (Arch::X86_64, 64),
            (Arch::Arm, 32),
            (Arch::Aarch64, 64),
            (Arch::Riscv32, 32),
            (Arch::Riscv64, 64),
            (Arch::Wasm32, 32),
            (Arch::Wasm64, 64),
        ];
        for (arch, width) in expected {
            let t = Target {
                arch,
                vendor: Vendor::Unknown,
                os: Os::None,
                abi: Abi::None,
            };
            assert_eq!(t.bit_native_width(), width, "{arch:?}");
            assert_eq!(arch.is_64_bit(), width == 64, "{arch:?}");
            assert_eq!(
                is_64_bit_triple(&t.to_triple()),
                arch.is_64_bit(),
                "{arch:?}"
            );
        }
        assert_eq!(is_64_bit_triple("riscv64-unknown-none"), true);
        assert_eq!(is_64_bit_triple("riscv32-unknown-none"), false);
    }

    #[test]
    fn default_is_canonical_bare_metal() {
        assert_eq!(Target::default().to_triple(), "x86_64-unknown-none");
    }

    #[test]
    fn host_round_trips() {
        assert_eq!(
            Target::parse(&Target::host().to_triple()).expect("host triple"),
            Target::host()
        );
    }
}
