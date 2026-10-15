//! Safe Rust bindings for [audio.cpp](https://github.com/0xShug0/audio.cpp),
//! a ggml-based local audio inference framework (TTS, voice cloning, ASR, VAD,
//! diarization, source separation, music generation, ...).
//!
//! The API mirrors the upstream C ABI (`include/audiocpp.h`):
//!
//! ```text
//! Registry ──load_model──▶ Model ──create_session──▶ Session ──run(&Request)──▶ TaskResult
//! ```
//!
//! Every handle keeps its parent alive internally, so a [`Session`] stays
//! usable after its [`Model`] and [`Registry`] are dropped.
//!
//! ```no_run
//! use audio_cpp::{Backend, BackendConfig, Mode, ModelConfig, Registry, Request};
//!
//! # fn main() -> audio_cpp::Result<()> {
//! let registry = Registry::new()?;
//! let model = registry.load_model(
//!     "models/kokoro-82m-q8_0.gguf",
//!     &ModelConfig::family("kokoro_tts"),
//!     None,
//! )?;
//! let mut session = model.create_session(
//!     "tts",
//!     Mode::Offline,
//!     &BackendConfig::new(Backend::Cpu),
//!     None,
//! )?;
//!
//! let mut request = Request::new()?;
//! request
//!     .set_text("Hello from audio.cpp.", Some("en-us"))?
//!     .set_option("voice-id", "af_heart")?;
//!
//! let result = session.run(&request)?;
//! if let Some(audio) = result.audio()? {
//!     println!("{} frames at {} Hz", audio.frames(), audio.sample_rate);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Threading
//!
//! Handles are [`Send`] but not [`Sync`]: each may move between threads but
//! must not be used from two threads at once. Separate handles may run
//! concurrently.

// C enums are `c_uint` on most targets but `c_int` on MSVC, so the casts
// that look redundant here are not redundant everywhere.
#![allow(clippy::unnecessary_cast)]

mod error;
mod model;
mod request;
mod result;
mod session;

use std::ffi::{CStr, CString, c_char};

pub use audio_cpp_sys as sys;

pub use error::{Error, ErrorKind, Result};
pub use model::{Backend, BackendConfig, Mode, Model, ModelConfig, OptionInfo, OptionScope, Options, Registry};
pub use request::Request;
pub use result::{
    ArtifactKind, ArtifactRef, AudioRef, NamedAudio, ResultView, Segment, SpeakerTurn, TaskResult, Text, Word,
};
pub use session::{Event, Session, StreamInputKind, StreamOutputKind, StreamPolicy, VoiceActivity, VoiceActivityKind};

/// A packed ABI version, as reported by `audiocpp_abi_version()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AbiVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl AbiVersion {
    /// The version of the header these bindings were generated from.
    pub const BINDINGS: AbiVersion = AbiVersion::unpack(sys::AUDIOCPP_ABI_VERSION);

    const fn unpack(packed: u32) -> Self {
        Self { major: (packed >> 16) & 0xff, minor: (packed >> 8) & 0xff, patch: packed & 0xff }
    }

    /// Whether a library reporting `self` can serve these bindings: same major,
    /// and at least the minor the bindings were generated against.
    pub fn is_compatible(self) -> bool {
        self.major == Self::BINDINGS.major && self.minor >= Self::BINDINGS.minor
    }
}

impl std::fmt::Display for AbiVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The ABI version of the loaded library.
pub fn abi_version() -> AbiVersion {
    // SAFETY: no preconditions.
    AbiVersion::unpack(unsafe { sys::audiocpp_abi_version() })
}

/// audio.cpp's own build version string, e.g. `"0.9.1"`.
pub fn build_version() -> &'static str {
    // SAFETY: static lifetime per the header.
    unsafe { static_str(sys::audiocpp_build_version()) }
}

/// The canonical task tokens this build accepts (`"tts"`, `"asr"`, `"vad"`, ...).
pub fn tasks() -> Vec<&'static str> {
    // SAFETY: no preconditions; names are static.
    let count = unsafe { sys::audiocpp_task_count() };
    (0..count)
        .filter_map(|i| {
            let name = unsafe { sys::audiocpp_task_name(i) };
            (!name.is_null()).then(|| unsafe { static_str(name) })
        })
        .collect()
}

/// Maps a model-spec task name to the canonical token (`"music"` → `"gen"`).
/// `None` when the name is no task this build can serve.
pub fn task_from_spec_name(spec_task: &str) -> Option<&'static str> {
    let spec_task = CString::new(spec_task).ok()?;
    // SAFETY: valid C string; result is static or NULL.
    let name = unsafe { sys::audiocpp_task_from_spec_name(spec_task.as_ptr()) };
    (!name.is_null()).then(|| unsafe { static_str(name) })
}

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

/// Copies a borrowed C string. NULL reads as empty.
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
/// `ptr` must be a valid NUL-terminated string with static lifetime.
unsafe fn static_str(ptr: *const c_char) -> &'static str {
    if ptr.is_null() {
        return "";
    }
    unsafe { CStr::from_ptr(ptr) }.to_str().unwrap_or("")
}

/// Declares an owning wrapper around a C handle with a matching free function.
macro_rules! handle {
    ($(#[$meta:meta])* $name:ident, $raw:ty, $free:path) => {
        $(#[$meta])*
        pub struct $name {
            ptr: std::ptr::NonNull<$raw>,
        }

        impl $name {
            /// # Safety
            /// `ptr` must be a live handle whose ownership is transferred.
            #[allow(dead_code)]
            pub(crate) unsafe fn from_raw(ptr: std::ptr::NonNull<$raw>) -> Self {
                Self { ptr }
            }

            /// The underlying C handle. Still owned by `self`.
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

        // SAFETY: handles carry no thread affinity; the C ABI only forbids
        // concurrent use of one handle, which `!Sync` already prevents.
        unsafe impl Send for $name {}
    };
}
pub(crate) use handle;

/// Runs a constructor that writes an out handle, wrapping the result.
pub(crate) fn create<T, R>(
    f: impl FnOnce(*mut *mut T) -> sys::audiocpp_status,
    wrap: impl FnOnce(std::ptr::NonNull<T>) -> R,
) -> Result<R> {
    let mut out: *mut T = std::ptr::null_mut();
    error::check(f(&mut out))?;
    std::ptr::NonNull::new(out)
        .map(wrap)
        .ok_or_else(|| Error::new(ErrorKind::Runtime, "call succeeded but returned no handle"))
}
