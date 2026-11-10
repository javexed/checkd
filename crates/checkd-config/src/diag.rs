//! Errors and warnings found while loading a configuration.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

/// A place in a configuration file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loc {
    pub file: Arc<PathBuf>,
    pub line: u32,
}

impl fmt::Display for Loc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.file.display(), self.line)
    }
}

/// One error or warning.
#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub message: String,
    pub loc: Option<Loc>,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.loc {
            Some(loc) => write!(f, "{loc}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

/// Everything found so far. Loading goes on after an error where it can, so
/// that one run reports as many faults as possible.
#[derive(Default, Debug)]
pub struct Diagnostics {
    pub errors: Vec<Diagnostic>,
    pub warnings: Vec<Diagnostic>,
}

impl Diagnostics {
    pub fn error(&mut self, loc: Option<&Loc>, message: impl Into<String>) {
        self.errors.push(Diagnostic {
            message: message.into(),
            loc: loc.cloned(),
        });
    }

    pub fn warn(&mut self, loc: Option<&Loc>, message: impl Into<String>) {
        self.warnings.push(Diagnostic {
            message: message.into(),
            loc: loc.cloned(),
        });
    }

    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }
}
