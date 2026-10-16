//! Helpers shared by every wrapper: string conversion, owned handles, and
//! out-parameter constructors.

use std::ffi::{CStr, CString, c_char};
use std::ptr::NonNull;

use crate::error::{Error, ErrorKind, Result, check};
use crate::sys;

/// Converts a Rust string to a C string, rejecting interior NULs.
pub(crate) fn c_string(value: &str, what: &str) -> Result<CString> {
    CString::new(value).map_err(|_| Error::invalid_argument(format!("{what} contains a NUL byte")))
}

pub(crate) fn opt_c_string(value: Option<&str>, what: &str) -> Result<Option<CString>> {
    value.map(|v| c_string(v, what)).transpose()
}

pub(crate) fn opt_ptr(value: &Option<CString>) -> *const c_char {
    value.as_ref().map_or(std::ptr::null(), |v| v.as_ptr())
}

/// Copies a borrowed C string. NULL reads as empty; invalid UTF-8 is replaced
/// rather than reported, since a garbled character in a transcript is better
/// than losing the whole result.
///
/// # Safety
/// `ptr` must be NULL or a valid NUL-terminated string.
pub(crate) unsafe fn owned_string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned()
}

/// # Safety
/// `ptr` must be NULL or a valid NUL-terminated string with static lifetime.
pub(crate) unsafe fn static_str(ptr: *const c_char) -> &'static str {
    if ptr.is_null() {
        return "";
    }
    unsafe { CStr::from_ptr(ptr) }.to_str().unwrap_or("")
}

/// Constructors that return a bare pointer do not set `audiocpp_last_error`,
/// so reading it after one fails would report an unrelated, stale message.
pub(crate) fn allocation_failed(what: &str) -> Error {
    Error::new(ErrorKind::OutOfMemory, format!("failed to allocate {what}"))
}

/// Runs a constructor that writes an out handle, wrapping the result.
pub(crate) fn create<T, R>(
    f: impl FnOnce(*mut *mut T) -> sys::audiocpp_status,
    wrap: impl FnOnce(NonNull<T>) -> R,
) -> Result<R> {
    let mut out: *mut T = std::ptr::null_mut();
    check(f(&mut out))?;
    NonNull::new(out).map(wrap).ok_or_else(|| Error::new(ErrorKind::Runtime, "call succeeded but returned no handle"))
}

/// Declares an owning wrapper around a C handle with a matching free function.
///
/// Every handle is `Send` and `!Sync`: the C ABI lets each handle move between
/// threads but forbids using one handle from two threads at once.
macro_rules! handle {
    ($(#[$meta:meta])* $vis:vis $name:ident, $raw:ty, $free:path) => {
        $(#[$meta])*
        $vis struct $name {
            ptr: std::ptr::NonNull<$raw>,
        }

        impl $name {
            /// # Safety
            /// `ptr` must be a live handle whose ownership is transferred.
            #[allow(dead_code)]
            pub(crate) unsafe fn from_raw(ptr: std::ptr::NonNull<$raw>) -> Self {
                Self { ptr }
            }

            /// The underlying C handle, still owned by `self`. For calling
            /// `audio_cpp::sys` functions this crate does not wrap.
            pub fn as_ptr(&self) -> *mut $raw {
                self.ptr.as_ptr()
            }
        }

        impl Drop for $name {
            fn drop(&mut self) {
                // SAFETY: we own the handle and free it exactly once.
                unsafe { $free(self.ptr.as_ptr()) }
            }
        }

        // SAFETY: handles carry no thread affinity. NonNull already makes the
        // type !Sync, which is what forbids concurrent use of one handle.
        unsafe impl Send for $name {}
    };
}
pub(crate) use handle;
