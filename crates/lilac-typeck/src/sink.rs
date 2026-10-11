//! Where the checker's diagnostics go ([`Sink`]).

use lilac_error::{Diagnostic, Diags, ErrorGuaranteed, Level};

use crate::Requirement;

/// A diagnostic held while an instance body is checked, with what the
/// checker knows about it.
#[derive(Clone, Debug)]
pub(crate) struct Pending {
    pub diagnostic: Diagnostic,
    /// The duck-typing requirement it reports; a generic instance folds
    /// these into one `E042`.
    pub requirement: Option<Requirement>,
    /// Whether it is about a type containing a generic parameter, which a
    /// template check cannot decide, so it drops it.
    pub dependent: bool,
}

/// The checker's diagnostic sink.
///
/// Outside instance bodies a diagnostic goes straight to the session's
/// [`Diags`]. While a body is checked they are held ([`Sink::hold`]), and
/// once the body is done the checker decides what to report
/// (`Checker::settle`): each as it is, folded into `E042`, or dropped.
pub(crate) struct Sink<'a> {
    diags: &'a mut Diags,
    /// The diagnostics of the body being checked, when one is.
    held: Option<Vec<Pending>>,
}

impl<'a> Sink<'a> {
    pub(crate) fn new(diags: &'a mut Diags) -> Self {
        Self { diags, held: None }
    }

    /// Reports `diagnostic`, or holds it while a body is checked.
    pub(crate) fn emit(&mut self, diagnostic: Diagnostic) -> ErrorGuaranteed {
        self.push(Pending {
            diagnostic,
            requirement: None,
            dependent: false,
        })
    }

    /// Reports `pending`'s diagnostic, or holds it while a body is checked.
    pub(crate) fn push(&mut self, pending: Pending) -> ErrorGuaranteed {
        match &mut self.held {
            Some(held) => {
                let error = ErrorGuaranteed::held(&pending.diagnostic);
                held.push(pending);
                error
            }
            None => self.diags.emit(pending.diagnostic),
        }
    }

    /// How many errors were reported (held ones not included).
    pub(crate) fn error_count(&self) -> usize {
        self.diags.error_count()
    }

    /// Whether an error is held.
    pub(crate) fn holds_error(&self) -> bool {
        self.held.as_deref().is_some_and(has_error)
    }

    /// The token for poison passed along after an error, reported or held.
    /// Debug builds check there is one.
    pub(crate) fn already_reported(&self) -> ErrorGuaranteed {
        let held = self.held.iter().flatten();
        match held
            .into_iter()
            .find(|pending| pending.diagnostic.level == Level::Error)
        {
            Some(pending) => ErrorGuaranteed::held(&pending.diagnostic),
            None => self.diags.already_reported(),
        }
    }

    /// Starts holding diagnostics. Returns what was held so far, for the
    /// matching [`Sink::release`] to restore.
    pub(crate) fn hold(&mut self) -> Option<Vec<Pending>> {
        self.held.replace(Vec::new())
    }

    /// Ends a [`Sink::hold`]: returns the diagnostics held since, and holds
    /// `outer` again.
    pub(crate) fn release(&mut self, outer: Option<Vec<Pending>>) -> Vec<Pending> {
        std::mem::replace(&mut self.held, outer).unwrap_or_default()
    }
}

/// Whether any of `held` is an error.
pub(crate) fn has_error(held: &[Pending]) -> bool {
    held.iter()
        .any(|pending| pending.diagnostic.level == Level::Error)
}
