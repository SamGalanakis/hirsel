//! How a [`NativeStop`] reads and what converts into one.

use super::NativeStop;
use std::{fmt, io};

impl fmt::Display for NativeStop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Check(s) => write!(f, "check: {s}"),
            Self::Instances(s) => write!(f, "instances: {s}"),
            Self::Entry(s) => write!(f, "entry: {s}"),
            Self::ForeignRecording { reason, .. } => write!(f, "refused: {reason}"),
            Self::Archive(error) => error.fmt(f),
            Self::Emit {
                definition,
                message,
            } => write!(f, "emit {definition}: {message}"),
            Self::Link(s) => write!(f, "link: {s}"),
            Self::Module(s) => write!(f, "module: unsupported: {s}"),
            Self::Coverage(mismatch) => mismatch.fmt(f),
            Self::Input(e) => write!(f, "input: {e}"),
            Self::Output { error, .. } => f.write_str(&crate::executable::output_stop(error)),
        }
    }
}
impl std::error::Error for NativeStop {}
impl From<io::Error> for NativeStop {
    fn from(e: io::Error) -> Self {
        Self::Input(e)
    }
}
impl From<crate::code_admission::AdmissionError> for NativeStop {
    fn from(error: crate::code_admission::AdmissionError) -> Self {
        Self::Archive(error)
    }
}
