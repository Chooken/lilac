//! Target identity, the pointer-width rule, and the per-target link facts.
//!
//! This crate is the single source of truth for target triples. It parses a
//! triple into its four structured components (`arch`, `vendor`, `os`, `abi`)
//! and renders it back in canonical form. [`Target::link_info`] is the one
//! table of per-OS linking facts, and [`Target::scalar_layout`] the one table
//! of the target C ABI's scalar sizes and alignments, from which
//! `lilac-layout` builds every aggregate layout.

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
    /// Every architecture, in declaration order.
    pub const ALL: [Self; 8] = [
        Self::X86,
        Self::X86_64,
        Self::Arm,
        Self::Aarch64,
        Self::Riscv32,
        Self::Riscv64,
        Self::Wasm32,
        Self::Wasm64,
    ];

    /// The variant name in `@builtin.Arch` (`X86_64`).
    #[must_use]
    pub fn builtin_name(self) -> &'static str {
        match self {
            Self::X86 => "X86",
            Self::X86_64 => "X86_64",
            Self::Arm => "Arm",
            Self::Aarch64 => "Aarch64",
            Self::Riscv32 => "Riscv32",
            Self::Riscv64 => "Riscv64",
            Self::Wasm32 => "Wasm32",
            Self::Wasm64 => "Wasm64",
        }
    }

    /// Parses an architecture name. `i686`, LLVM's name for 32-bit x86, is
    /// accepted so that a rendered triple ([`Target::to_triple`]) reads back.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "x86" | "i686" => Some(Self::X86),
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

    /// The architecture's name in an LLVM (and Clang) triple: `i686` for
    /// 32-bit x86, whose user-facing name `x86` LLVM does not know; every
    /// other name is LLVM's already.
    #[must_use]
    pub fn llvm_name(self) -> &'static str {
        match self {
            Self::X86 => "i686",
            other => other.as_str(),
        }
    }

    /// Whether this is a WebAssembly architecture.
    #[must_use]
    pub fn is_wasm(self) -> bool {
        matches!(self, Self::Wasm32 | Self::Wasm64)
    }

    /// Returns whether pointers on this architecture are 64 bits wide.
    #[must_use]
    pub fn is_64_bit(self) -> bool {
        matches!(
            self,
            Self::X86_64 | Self::Aarch64 | Self::Riscv64 | Self::Wasm64
        )
    }

    /// The LLVM CPU code is built for: `pentium4` on 32-bit x86, so float
    /// arithmetic uses SSE2 (x87 rounds twice), else `generic`.
    #[must_use]
    pub fn baseline_cpu(self) -> &'static str {
        match self {
            Self::X86 => "pentium4",
            _ => "generic",
        }
    }

    /// The LLVM features added to [`Self::baseline_cpu`]: `VFPv2` on 32-bit
    /// Arm, so floats use the FPU (the C ABI still passes them in integer
    /// registers), else none.
    #[must_use]
    pub fn baseline_features(self) -> &'static str {
        match self {
            Self::Arm => "+vfp2",
            _ => "",
        }
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
    /// Every vendor, in declaration order.
    pub const ALL: [Self; 3] = [Self::Unknown, Self::Pc, Self::Apple];

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
    /// Every operating system, in declaration order.
    pub const ALL: [Self; 5] = [
        Self::None,
        Self::Unknown,
        Self::Linux,
        Self::Windows,
        Self::Macos,
    ];

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

    /// The variant name in `@builtin.Os` (`Windows`, `Linux`, `MacOs`, or
    /// `None` for no or an unknown operating system).
    #[must_use]
    pub fn builtin_name(self) -> &'static str {
        match self {
            Self::None | Self::Unknown => "None",
            Self::Linux => "Linux",
            Self::Windows => "Windows",
            Self::Macos => "MacOs",
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
    /// Every ABI, in declaration order.
    pub const ALL: [Self; 4] = [Self::None, Self::Gnu, Self::Msvc, Self::Musl];

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
    /// contains an unrecognized architecture, vendor, OS, or ABI, or names a
    /// combination with no C ABI ([`ParseError::Unsupported`]).
    pub fn parse(s: &str) -> Result<Self, ParseError> {
        let target = Self::parse_components(s)?;
        // WebAssembly's C ABI (and LLVM's and Clang's support for it) exists
        // only without an operating system or vendor.
        if target.arch.is_wasm()
            && (target.vendor != Vendor::Unknown || !matches!(target.os, Os::None | Os::Unknown))
        {
            return Err(ParseError::Unsupported(format!(
                "`{}` has no operating system or vendor: write `{}-unknown-unknown`",
                target.arch.as_str(),
                target.arch.as_str()
            )));
        }
        Ok(target)
    }

    /// Every target [`Target::parse`] accepts, one per architecture and
    /// operating system: that system's usual vendor and ABI (`pc` and
    /// `msvc` on Windows, `apple` on macOS, `gnu` on Linux).
    #[must_use]
    pub fn supported() -> Vec<Self> {
        let mut targets = Vec::new();
        for arch in Arch::ALL {
            for os in Os::ALL {
                let (vendor, abi) = match os {
                    Os::Windows => (Vendor::Pc, Abi::Msvc),
                    Os::Macos => (Vendor::Apple, Abi::None),
                    Os::Linux => (Vendor::Unknown, Abi::Gnu),
                    Os::None | Os::Unknown => (Vendor::Unknown, Abi::None),
                };
                let target = Self {
                    arch,
                    vendor,
                    os,
                    abi,
                };
                if Self::parse(&target.to_triple()).is_ok() {
                    targets.push(target);
                }
            }
        }
        targets
    }

    /// The four components of a triple, each recognized on its own.
    fn parse_components(s: &str) -> Result<Self, ParseError> {
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

    /// Renders the target in canonical `arch-vendor-os[-abi]` form, the
    /// triple LLVM and Clang take (the architecture by
    /// [`Arch::llvm_name`]: `x86` renders as `i686`).
    #[must_use]
    pub fn to_triple(&self) -> String {
        let arch = self.arch.llvm_name();
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

    /// The linking facts for this target, or `None` when `lilac build` cannot
    /// link for it (bare metal, WebAssembly, 32-bit targets).
    #[must_use]
    pub fn link_info(&self) -> Option<LinkInfo> {
        if !matches!(self.arch, Arch::X86_64 | Arch::Aarch64) {
            return None;
        }
        let info = match self.os {
            Os::Windows => LinkInfo {
                flavor: LinkFlavor::Coff,
                exe_suffix: ".exe",
                obj_suffix: ".obj",
                entry_symbol: ".lilac.native_entry",
                system_libs: &["kernel32.lib", "bcryptprimitives.lib"],
                dynamic_linker: None,
            },
            Os::Linux => LinkInfo {
                flavor: LinkFlavor::Elf,
                exe_suffix: "",
                obj_suffix: ".o",
                entry_symbol: ".lilac.native_entry",
                system_libs: &[],
                dynamic_linker: Some(if self.arch == Arch::X86_64 {
                    "/lib64/ld-linux-x86-64.so.2"
                } else {
                    "/lib/ld-linux-aarch64.so.1"
                }),
            },
            Os::Macos => LinkInfo {
                flavor: LinkFlavor::MachO,
                exe_suffix: "",
                obj_suffix: ".o",
                entry_symbol: ".lilac.native_entry",
                system_libs: &["libSystem.tbd"],
                dynamic_linker: None,
            },
            Os::None | Os::Unknown => return None,
        };
        Some(info)
    }

    /// The memory page size in bytes: 16 KiB on Apple arm64, 64 KiB on
    /// WebAssembly, 4 KiB elsewhere.
    #[must_use]
    pub fn page_size(&self) -> u64 {
        match (self.arch, self.os) {
            (Arch::Aarch64, Os::Macos) => 0x4000,
            (Arch::Wasm32 | Arch::Wasm64, _) => 0x1_0000,
            _ => 0x1000,
        }
    }

    /// Whether `@linux_syscall` has an instruction here: Linux on x86-64 or
    /// aarch64 (`E035` elsewhere).
    #[must_use]
    pub fn has_syscall(&self) -> bool {
        self.os == Os::Linux && matches!(self.arch, Arch::X86_64 | Arch::Aarch64)
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

    /// The size and alignment in bytes of a scalar in the target's C ABI:
    /// the one table every layout is built from.
    ///
    /// Sizes are the scalar's width. Alignment is natural (equal to the
    /// size) except for:
    /// - `uint64_t` and `double` on 32-bit x86 off Windows (i386 System V
    ///   and Darwin) and on 32-bit Arm on macOS (APCS): 4;
    /// - the 128-bit float (`__float128`, or a binary128 `long double`):
    ///   16 everywhere.
    ///
    /// `lilac-codegen-llvm` checks this table against LLVM's data layout and
    /// `lilac-layout` against Clang's C layouts, for every supported target.
    #[must_use]
    pub fn scalar_layout(&self, scalar: Scalar) -> (u64, u64) {
        let size = scalar.size(self);
        let align = match scalar {
            Scalar::Bit64 | Scalar::Float64 if self.aligns_8_byte_scalars_to_4() => 4,
            Scalar::Float128 => 16,
            _ => size,
        };
        (size, align)
    }

    /// Whether the C ABI aligns 8-byte integers and doubles to 4 bytes:
    /// 32-bit x86 except on Windows, and 32-bit Arm on macOS.
    fn aligns_8_byte_scalars_to_4(self) -> bool {
        match self.arch {
            Arch::X86 => self.os != Os::Windows,
            Arch::Arm => self.os == Os::Macos,
            _ => false,
        }
    }

    /// The largest object the target allows, in bytes: its `isize::MAX`
    /// (`2^31 − 1` or `2^63 − 1`), so every offset fits a signed
    /// pointer-width integer.
    #[must_use]
    pub fn max_object_size(&self) -> u64 {
        (1u64 << (self.bit_native_width() - 1)) - 1
    }
}

impl std::fmt::Display for Target {
    /// The triple, as [`Target::to_triple`] renders it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_triple())
    }
}

/// A scalar of the target's C ABI: what every layout is built from.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum Scalar {
    /// `uint8_t` (`@bit8`).
    Bit8,
    /// `uint16_t` (`@bit16`).
    Bit16,
    /// `uint32_t` (`@bit32`).
    Bit32,
    /// `uint64_t` (`@bit64`).
    Bit64,
    /// `_Float16` (`@fbit16`).
    Float16,
    /// `float` (`@fbit32`).
    Float32,
    /// `double` (`@fbit64`).
    Float64,
    /// `__float128`, or a binary128 `long double` (`@fbit128`).
    Float128,
    /// `T *` (`ref T`), and `uintptr_t` (`@bit_native`), which has the
    /// pointer's size and alignment on every target.
    Pointer,
}

impl Scalar {
    /// Every scalar, in declaration order.
    pub const ALL: [Self; 9] = [
        Self::Bit8,
        Self::Bit16,
        Self::Bit32,
        Self::Bit64,
        Self::Float16,
        Self::Float32,
        Self::Float64,
        Self::Float128,
        Self::Pointer,
    ];

    /// The integer scalar `bits` wide (8, 16, 32 or 64).
    #[must_use]
    pub fn bit(bits: u32) -> Option<Self> {
        match bits {
            8 => Some(Self::Bit8),
            16 => Some(Self::Bit16),
            32 => Some(Self::Bit32),
            64 => Some(Self::Bit64),
            _ => None,
        }
    }

    /// The float scalar `bits` wide (16, 32, 64 or 128).
    #[must_use]
    pub fn float(bits: u32) -> Option<Self> {
        match bits {
            16 => Some(Self::Float16),
            32 => Some(Self::Float32),
            64 => Some(Self::Float64),
            128 => Some(Self::Float128),
            _ => None,
        }
    }

    /// The size in bytes on `target`.
    #[must_use]
    pub fn size(self, target: &Target) -> u64 {
        match self {
            Self::Bit8 => 1,
            Self::Bit16 | Self::Float16 => 2,
            Self::Bit32 | Self::Float32 => 4,
            Self::Bit64 | Self::Float64 => 8,
            Self::Float128 => 16,
            Self::Pointer => u64::from(target.bit_native_width() / 8),
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

/// The object-file format a target links, which picks lld's flavor.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum LinkFlavor {
    /// PE/COFF (Windows): `lld -flavor link`.
    Coff,
    /// ELF (Linux): `lld -flavor gnu`.
    Elf,
    /// Mach-O (macOS): `lld -flavor darwin`.
    MachO,
}

impl LinkFlavor {
    /// The `-flavor` argument lld takes for this format.
    #[must_use]
    pub fn lld_flavor(self) -> &'static str {
        match self {
            Self::Coff => "link",
            Self::Elf => "gnu",
            Self::MachO => "darwin",
        }
    }
}

/// How to link an executable for a target: the single place for per-OS
/// linking facts.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct LinkInfo {
    /// The object format and lld flavor.
    pub flavor: LinkFlavor,
    /// The executable file suffix (`.exe` or empty).
    pub exe_suffix: &'static str,
    /// The object file suffix (`.obj` or `.o`).
    pub obj_suffix: &'static str,
    /// The native entry symbol (Mach-O adds its own `_` prefix).
    pub entry_symbol: &'static str,
    /// The system stubs a native link adds, found in the sysroot's
    /// `<triple>/` folder.
    pub system_libs: &'static [&'static str],
    /// The program loader an ELF executable names when it links a shared
    /// library (a static executable needs none). glibc's path.
    pub dynamic_linker: Option<&'static str>,
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
    /// Each component is known, but together they name no C ABI (LLVM or
    /// Clang has none): the reason, for the user.
    Unsupported(String),
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
            Self::Unsupported(why) => write!(f, "unsupported target: {why}"),
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
            "i686-unknown-linux-gnu",
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
    fn link_info_per_supported_triple() {
        let info = |t: &str| Target::parse(t).expect("valid").link_info();
        let windows = info("x86_64-pc-windows-msvc").expect("windows links");
        assert_eq!(windows.flavor, LinkFlavor::Coff);
        assert_eq!((windows.exe_suffix, windows.obj_suffix), (".exe", ".obj"));
        assert_eq!(
            windows.system_libs,
            ["kernel32.lib", "bcryptprimitives.lib"]
        );

        let linux = info("x86_64-unknown-linux-gnu").expect("linux links");
        assert_eq!(linux.flavor, LinkFlavor::Elf);
        assert_eq!((linux.exe_suffix, linux.obj_suffix), ("", ".o"));
        assert!(linux.system_libs.is_empty());
        assert_eq!(
            info("aarch64-unknown-linux-gnu").map(|i| i.flavor),
            Some(LinkFlavor::Elf)
        );
        let macos = info("aarch64-apple-darwin").expect("macos links");
        assert_eq!(macos.flavor, LinkFlavor::MachO);
        assert_eq!(macos.system_libs, ["libSystem.tbd"]);
        for info in [windows, linux, macos] {
            assert_eq!(info.entry_symbol, ".lilac.native_entry");
        }
        for unsupported in [
            "x86_64-unknown-none",
            "wasm32-unknown-unknown",
            "x86-unknown-linux-gnu",
        ] {
            assert_eq!(info(unsupported), None, "{unsupported}");
        }
        assert_eq!(LinkFlavor::Coff.lld_flavor(), "link");
        assert_eq!(LinkFlavor::Elf.lld_flavor(), "gnu");
        assert_eq!(LinkFlavor::MachO.lld_flavor(), "darwin");
    }

    #[test]
    fn x86_renders_as_llvm_names_it() {
        let t = Target::parse("x86-linux").expect("valid");
        assert_eq!(t.arch, Arch::X86);
        assert_eq!(t.arch.as_str(), "x86");
        assert_eq!(t.arch.builtin_name(), "X86");
        assert_eq!(t.to_triple(), "i686-unknown-linux");
        assert_eq!(t.to_string(), "i686-unknown-linux");
        assert_eq!(
            Target::parse("i686-pc-windows-msvc").expect("valid").arch,
            Arch::X86
        );
        for arch in Arch::ALL {
            if arch != Arch::X86 {
                assert_eq!(arch.llvm_name(), arch.as_str(), "{arch:?}");
            }
        }
    }

    #[test]
    fn webassembly_has_no_os_or_vendor() {
        for ok in [
            "wasm32-unknown-unknown",
            "wasm32",
            "wasm64-unknown-none",
            "wasm32-unknown-none-musl",
        ] {
            assert!(Target::parse(ok).is_ok(), "{ok}");
        }
        for refused in [
            "wasm32-linux",
            "wasm32-unknown-linux-gnu",
            "wasm64-pc-windows-msvc",
            "wasm32-apple-darwin",
            "wasm32-pc-none",
        ] {
            let error = Target::parse(refused).expect_err(refused);
            assert!(matches!(error, ParseError::Unsupported(_)), "{refused}");
            assert!(
                error.to_string().contains("no operating system or vendor"),
                "{error}"
            );
        }
    }

    #[test]
    fn supported_is_every_accepted_pair() {
        let supported = Target::supported();
        assert_eq!(supported.len(), 6 * Os::ALL.len() + 2 * 2);
        for target in &supported {
            assert_eq!(Target::parse(&target.to_triple()).as_ref(), Ok(target));
        }
        for arch in Arch::ALL {
            for os in Os::ALL {
                let accepted = Target::parse(&format!("{}-{}", arch.as_str(), os.as_str())).is_ok();
                let listed = supported.iter().any(|t| (t.arch, t.os) == (arch, os));
                assert_eq!(accepted, listed, "{arch:?} {os:?}");
            }
        }
    }

    #[test]
    fn scalar_layouts_follow_the_c_abi() {
        let layouts = |triple: &str| -> Vec<(u64, u64)> {
            let target = Target::parse(triple).expect("valid");
            Scalar::ALL
                .iter()
                .map(|scalar| target.scalar_layout(*scalar))
                .collect()
        };
        // Bit8 Bit16 Bit32 Bit64 Float16 Float32 Float64 Float128 Pointer
        let natural_64 = [
            (1, 1),
            (2, 2),
            (4, 4),
            (8, 8),
            (2, 2),
            (4, 4),
            (8, 8),
            (16, 16),
            (8, 8),
        ];
        let natural_32 = [
            (1, 1),
            (2, 2),
            (4, 4),
            (8, 8),
            (2, 2),
            (4, 4),
            (8, 8),
            (16, 16),
            (4, 4),
        ];
        let packed_32 = [
            (1, 1),
            (2, 2),
            (4, 4),
            (8, 4),
            (2, 2),
            (4, 4),
            (8, 4),
            (16, 16),
            (4, 4),
        ];
        for triple in [
            "x86_64-unknown-linux-gnu",
            "x86_64-pc-windows-msvc",
            "aarch64-apple-darwin",
            "riscv64-unknown-none",
            "wasm64-unknown-unknown",
        ] {
            assert_eq!(layouts(triple), natural_64, "{triple}");
        }
        for triple in [
            "x86-pc-windows-msvc",
            "x86-pc-windows-gnu",
            "arm-unknown-linux-gnu",
            "arm-pc-windows-msvc",
            "riscv32-unknown-none",
            "wasm32-unknown-unknown",
        ] {
            assert_eq!(layouts(triple), natural_32, "{triple}");
        }
        for triple in [
            "x86-unknown-linux-gnu",
            "x86-unknown-none",
            "x86-apple-darwin",
            "arm-apple-darwin",
        ] {
            assert_eq!(layouts(triple), packed_32, "{triple}");
        }
    }

    #[test]
    fn max_object_size_is_isize_max() {
        let max = |triple: &str| Target::parse(triple).expect("valid").max_object_size();
        assert_eq!(max("x86_64-unknown-linux-gnu"), (1 << 63) - 1);
        assert_eq!(max("x86-unknown-linux-gnu"), (1 << 31) - 1);
        assert_eq!(max("wasm32-unknown-unknown"), 0x7fff_ffff);
    }

    #[test]
    fn scalar_widths_round_trip() {
        for bits in [8, 16, 32, 64] {
            let scalar = Scalar::bit(bits).expect("integer width");
            assert_eq!(scalar.size(&Target::default()) * 8, u64::from(bits));
        }
        for bits in [16, 32, 64, 128] {
            let scalar = Scalar::float(bits).expect("float width");
            assert_eq!(scalar.size(&Target::default()) * 8, u64::from(bits));
        }
        assert_eq!(Scalar::bit(128), None);
        assert_eq!(Scalar::float(8), None);
    }

    #[test]
    fn host_round_trips() {
        assert_eq!(
            Target::parse(&Target::host().to_triple()).expect("host triple"),
            Target::host()
        );
    }
}
