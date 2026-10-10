//! The registry of diagnostic codes: each is listed once, with its page
//! under `docs/errors/`, so a misspelled code or a missing page does not
//! compile.

/// A diagnostic code and its documentation page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Code {
    id: &'static str,
    doc: &'static str,
}

impl Code {
    /// The code as written in diagnostics (`E010`).
    #[must_use]
    pub const fn id(self) -> &'static str {
        self.id
    }

    /// Its page under `docs/errors/` (Markdown).
    #[must_use]
    pub const fn doc(self) -> &'static str {
        self.doc
    }
}

macro_rules! codes {
    ($($code:ident),* $(,)?) => {
        $(
            #[doc = concat!("`", stringify!($code), "` (`docs/errors/", stringify!($code), ".md`).")]
            pub const $code: Code = Code {
                id: stringify!($code),
                doc: include_str!(concat!("../../../docs/errors/", stringify!($code), ".md")),
            };
        )*

        /// Every registered code, in order.
        pub const ALL: &[Code] = &[$($code),*];
    };
}

codes!(
    E000, E001, E010, E020, E021, E022, E023, E024, E025, E026, E028, E029, E031, E032, E033, E034,
    E035, E036, E037, E042, E043, E044, E051, E052, E053, E054, E060, E061, E070,
);

/// The registered code spelled `id` (`E010`, or `e010`).
#[must_use]
pub fn lookup(id: &str) -> Option<Code> {
    ALL.iter()
        .copied()
        .find(|code| code.id().eq_ignore_ascii_case(id))
}

/// A warning code for the emitters' tests; the compiler emits no warnings.
#[cfg(test)]
pub(crate) const TEST_WARNING: Code = Code {
    id: "W001",
    doc: "",
};

#[cfg(test)]
mod tests {
    use super::ALL;

    #[test]
    fn lookup_ignores_case() {
        assert_eq!(super::lookup("e010"), Some(super::E010));
        assert_eq!(super::lookup("E999"), None);
    }

    #[test]
    fn every_page_starts_with_its_code() {
        for code in ALL {
            let heading = format!("# {}: ", code.id());
            assert!(
                code.doc().starts_with(&heading),
                "docs/errors/{}.md must start with `{heading}`",
                code.id()
            );
        }
    }

    #[test]
    fn codes_are_listed_once_in_order() {
        assert!(ALL.windows(2).all(|pair| pair[0].id() < pair[1].id()));
    }
}
