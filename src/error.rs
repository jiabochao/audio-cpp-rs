use std::ffi::CStr;
use std::fmt;

use audio_cpp_sys as sys;

/// What went wrong. The first group mirrors the C ABI's status codes; the
/// rest are raised by this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    /// A bad argument, rejected either by this crate before the call or by
    /// the library.
    InvalidArgument,
    /// The registry has no loader for the requested family.
    UnsupportedFamily,
    /// The model files could not be loaded.
    LoadFailed,
    /// Inference or session setup failed inside the library.
    Runtime,
    OutOfMemory,
    OutOfRange,
    /// The requested output or feature does not exist.
    NotAvailable,
    /// The loaded `libaudiocpp` cannot serve these bindings.
    IncompatibleAbi,
    /// The caller cancelled the operation through a [`crate::CancelToken`].
    Cancelled,
    /// Waiting for a pooled session timed out.
    Timeout,
    /// A status code this version of the bindings does not know about.
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
            Self::IncompatibleAbi => "incompatible library ABI",
            Self::Cancelled => "cancelled",
            Self::Timeout => "timed out",
            Self::Other(_) => "unknown status",
        }
    }
}

/// An error from audio.cpp or from this crate: a kind plus a detail message.
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

    /// The detail message: the library's `audiocpp_last_error()` for native
    /// failures, possibly empty.
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
