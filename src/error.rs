use std::ffi::CStr;
use std::fmt;

use audio_cpp_sys as sys;

/// Non-OK status codes returned by the C ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    InvalidArgument,
    UnsupportedFamily,
    LoadFailed,
    Runtime,
    OutOfMemory,
    OutOfRange,
    NotAvailable,
    /// A status this version of the bindings does not know about.
    Other(u32),
}

impl ErrorKind {
    fn from_status(status: sys::audiocpp_status) -> Self {
        match status {
            sys::AUDIOCPP_ERR_INVALID_ARGUMENT => Self::InvalidArgument,
            sys::AUDIOCPP_ERR_UNSUPPORTED_FAMILY => Self::UnsupportedFamily,
            sys::AUDIOCPP_ERR_LOAD_FAILED => Self::LoadFailed,
            sys::AUDIOCPP_ERR_RUNTIME => Self::Runtime,
            sys::AUDIOCPP_ERR_OUT_OF_MEMORY => Self::OutOfMemory,
            sys::AUDIOCPP_ERR_OUT_OF_RANGE => Self::OutOfRange,
            sys::AUDIOCPP_ERR_NOT_AVAILABLE => Self::NotAvailable,
            other => Self::Other(other as u32),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::InvalidArgument => "invalid argument",
            Self::UnsupportedFamily => "unsupported family",
            Self::LoadFailed => "load failed",
            Self::Runtime => "runtime error",
            Self::OutOfMemory => "out of memory",
            Self::OutOfRange => "out of range",
            Self::NotAvailable => "not available",
            Self::Other(_) => "unknown status",
        }
    }
}

/// An error from audio.cpp: the status code plus the detail message the
/// library recorded for the failing call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    kind: ErrorKind,
    message: String,
}

impl Error {
    pub(crate) fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }

    pub(crate) fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidArgument, message)
    }

    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// The library's detail message (`audiocpp_last_error()`), possibly empty.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.message.is_empty() {
            write!(f, "audio.cpp: {}", self.kind.as_str())
        } else {
            write!(f, "audio.cpp: {}: {}", self.kind.as_str(), self.message)
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Converts a status into a `Result`, capturing `audiocpp_last_error()`.
///
/// Must be called on the same thread, immediately after the call that
/// returned `status`: the error detail is thread-local and the next call may
/// clear it.
pub(crate) fn check(status: sys::audiocpp_status) -> Result<()> {
    if status == sys::AUDIOCPP_OK {
        return Ok(());
    }
    // SAFETY: never NULL, borrowed until this thread's next call; copied here.
    let message = unsafe { CStr::from_ptr(sys::audiocpp_last_error()) }.to_string_lossy().into_owned();
    Err(Error::new(ErrorKind::from_status(status), message))
}
