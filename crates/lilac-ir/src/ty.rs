//! LIR value types.

use std::fmt;

use lilac_target::Scalar;

/// A LIR value type. It prints as its text spelling (`#bit32`,
/// `ref #Object`), which is also what [`LirTy::parse`] reads.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum LirTy {
    /// `#bitN`: an `N`-bit integer container (8, 16, 32 or 64).
    Bit(u16),
    /// `#bit_native`: the target's pointer-width integer.
    BitNative,
    /// `#fbitN`: an `N`-bit IEEE-754 float (16, 32, 64 or 128).
    FBit(u16),
    /// `ref T`: a pointer to a `T`.
    Ref(Box<LirTy>),
    /// `[N x T]`: `N` elements of `T` in a row.
    Array(Box<LirTy>, u64),
    /// Any other `#name`, stored without the `#`: a module layout
    /// (`#Object`), a builtin aggregate (`#byte_buffer`, `#format_args`), or
    /// `#unknown` (only valid behind a `ref`).
    Named(String),
}

impl LirTy {
    /// `#byte_buffer`.
    #[must_use]
    pub fn byte_buffer() -> Self {
        Self::Named("byte_buffer".to_owned())
    }

    /// `#format_args`: an f-string, the template it was built from and the
    /// addresses of its holes (see [`crate::LirInst::Format`]).
    #[must_use]
    pub fn format_args() -> Self {
        Self::Named("format_args".to_owned())
    }

    /// `#.lilac.format_part`: one part of an f-string template, a piece's
    /// text and the formatter of the hole after it
    /// ([`crate::format_template_layout`]).
    #[must_use]
    pub fn format_part() -> Self {
        Self::Named(".lilac.format_part".to_owned())
    }

    /// `#.lilac.format_line`: a template with no holes and one piece, its
    /// fields flattened (`holes`, the text's `len` and `data`, `format`);
    /// the panic handler builds one on its stack for a run-time line. It is
    /// laid out as [`crate::format_template_layout`]`(0)`.
    #[must_use]
    pub fn format_line() -> Self {
        Self::Named(".lilac.format_line".to_owned())
    }

    /// `#.lilac.frame`: one shadow-stack record (`prev`, `site`; see
    /// [`crate::trace`]). The leading `.` keeps it apart from user types.
    #[must_use]
    pub fn trace_frame() -> Self {
        Self::Named(".lilac.frame".to_owned())
    }

    /// `ref #unknown`, the opaque pointer.
    #[must_use]
    pub fn opaque_ref() -> Self {
        Self::Named("unknown".to_owned()).to_ref()
    }

    /// `ref #bit8`, the byte pointer.
    #[must_use]
    pub fn byte_ref() -> Self {
        Self::Bit(8).to_ref()
    }

    /// `ref self`.
    #[must_use]
    pub fn to_ref(self) -> Self {
        Self::Ref(Box::new(self))
    }

    /// Whether this is a `ref`.
    #[must_use]
    pub fn is_ref(&self) -> bool {
        matches!(self, Self::Ref(_))
    }

    /// The pointee of a `ref`, else `self`.
    #[must_use]
    pub fn strip_ref(&self) -> &Self {
        match self {
            Self::Ref(inner) => inner,
            _ => self,
        }
    }

    /// Whether this is an integer container (`#bitN`, `#bit_native`).
    #[must_use]
    pub fn is_int(&self) -> bool {
        matches!(self, Self::Bit(_) | Self::BitNative)
    }

    /// Whether this is a float container (`#fbitN`).
    #[must_use]
    pub fn is_float(&self) -> bool {
        matches!(self, Self::FBit(_))
    }

    /// The width in bits of a scalar (`ptr_bits` for `#bit_native` and
    /// pointers); `None` for aggregates.
    #[must_use]
    pub fn bits(&self, ptr_bits: u32) -> Option<u32> {
        match self {
            Self::Bit(bits) | Self::FBit(bits) => Some(u32::from(*bits)),
            Self::BitNative | Self::Ref(_) => Some(ptr_bits),
            Self::Named(_) | Self::Array(..) => None,
        }
    }

    /// The C ABI scalar a scalar type is laid out as (`#bit_native` as a
    /// pointer, `uintptr_t`); `None` for aggregates.
    #[must_use]
    pub fn scalar(&self) -> Option<Scalar> {
        match self {
            Self::Bit(bits) => Scalar::bit(u32::from(*bits)),
            Self::FBit(bits) => Scalar::float(u32::from(*bits)),
            Self::BitNative | Self::Ref(_) => Some(Scalar::Pointer),
            Self::Named(_) | Self::Array(..) => None,
        }
    }

    /// Whether a register may hold this type: a `#bit`, `#fbit`,
    /// `#bit_native` or `ref`. Aggregates (a named layout or an array) live
    /// in places.
    #[must_use]
    pub fn is_scalar(&self) -> bool {
        self.scalar().is_some()
    }

    /// The layout name of a `Named` type (without `#`).
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Named(name) => Some(name),
            _ => None,
        }
    }

    /// Parses a type spelling (`#bit32`, `ref #Object`, `[4 x #bit8]`).
    /// `None` when it does not start with `#`, `ref ` or `[`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        if let Some(inner) = text.strip_prefix("ref ") {
            return Self::parse(inner).map(Self::to_ref);
        }
        if let Some(inner) = text.strip_prefix('[') {
            let (len, elem) = inner.strip_suffix(']')?.split_once(" x ")?;
            let len = len.parse::<u64>().ok().filter(|n| *n.to_string() == *len)?;
            return Some(Self::Array(Box::new(Self::parse(elem)?), len));
        }
        let name = text.strip_prefix('#')?;
        let width = |digits: &str, allowed: &[u16]| {
            digits
                .parse::<u16>()
                .ok()
                .filter(|bits| allowed.contains(bits) && digits == bits.to_string())
        };
        if name == "bit_native" {
            return Some(Self::BitNative);
        }
        if let Some(bits) = name
            .strip_prefix("fbit")
            .and_then(|d| width(d, &[16, 32, 64, 128]))
        {
            return Some(Self::FBit(bits));
        }
        if let Some(bits) = name
            .strip_prefix("bit")
            .and_then(|d| width(d, &[8, 16, 32, 64]))
        {
            return Some(Self::Bit(bits));
        }
        Some(Self::Named(name.to_owned()))
    }
}

impl fmt::Display for LirTy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bit(bits) => write!(f, "#bit{bits}"),
            Self::BitNative => f.write_str("#bit_native"),
            Self::FBit(bits) => write!(f, "#fbit{bits}"),
            Self::Ref(inner) => write!(f, "ref {inner}"),
            Self::Array(elem, len) => write!(f, "[{len} x {elem}]"),
            Self::Named(name) => write!(f, "#{name}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LirTy;

    #[test]
    fn spellings_round_trip() {
        for text in [
            "#bit8",
            "#bit_native",
            "#fbit128",
            "ref #unknown",
            "ref ref #bit32",
            "#GenBox[m]",
            "[2 x #GenBox[I6GenBoxmE]]",
            "#byte_buffer",
            "[4 x #bit32]",
            "[2 x [3 x ref #bit8]]",
            "ref [16 x #Object]",
        ] {
            let ty = LirTy::parse(text).expect(text);
            assert_eq!(ty.to_string(), text);
        }
        assert_eq!(LirTy::parse("#bit32"), Some(LirTy::Bit(32)));
        assert_eq!(LirTy::parse("#bit7"), Some(LirTy::Named("bit7".to_owned())));
        assert_eq!(LirTy::parse("bit32"), None);
        assert_eq!(LirTy::parse("[04 x #bit8]"), None);
        assert_eq!(LirTy::parse("[4 #bit8]"), None);
    }
}
