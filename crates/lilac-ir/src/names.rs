//! The names LIR refers to things by. Each is its own type, so a register
//! cannot be passed where a place or a label is expected, and each displays
//! with its sigil (`&r`, `%p`, `$l`, `@f`).

use std::borrow::Borrow;
use std::fmt;

macro_rules! names {
    ($($(#[$doc:meta])* $name:ident => $sigil:literal;)*) => {$(
        $(#[$doc])*
        #[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Default)]
        pub struct $name(String);

        impl $name {
            /// The name, without its sigil.
            #[must_use]
            pub fn new(name: impl Into<String>) -> Self {
                Self(name.into())
            }

            /// The name, without its sigil.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!($sigil, "{}"), self.0)
            }
        }

        impl From<&str> for $name {
            fn from(name: &str) -> Self {
                Self::new(name)
            }
        }

        impl From<String> for $name {
            fn from(name: String) -> Self {
                Self(name)
            }
        }

        impl From<&String> for $name {
            fn from(name: &String) -> Self {
                Self::new(name.as_str())
            }
        }

        impl Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool {
                self.0 == other
            }
        }

        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self.0 == *other
            }
        }
    )*};
}

names! {
    /// An SSA value register (`&1`).
    Reg => "&";
    /// A mutable place (`%x`).
    PlaceName => "%";
    /// A block label (`$entry`).
    Label => "$";
    /// A function or data symbol (`@main`; data prints as `!c0`).
    Symbol => "@";
}
