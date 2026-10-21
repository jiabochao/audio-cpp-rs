//! Safe Rust bindings for [audio.cpp](https://github.com/0xShug0/audio.cpp),
//! a ggml-based local audio inference framework: TTS, voice cloning and
//! design, ASR, VAD, diarization, separation, music generation and more,
//! on CPU, CUDA, HIP, Vulkan and Metal.
//!
//! ```text
//! Registry ─load_model─▶ Model ─offline_session───▶ OfflineSession ─run(&Request)─▶ TaskResult
//!                              └streaming_session─▶ StreamingSession ─start/push/next_event/finish
//! ```
//!
//! ```no_run
//! use audio_cpp::{Backend, BackendConfig, ModelConfig, Registry, Request, Task};
//!
//! # fn main() -> audio_cpp::Result<()> {
//! let registry = Registry::new()?;
//! let model = registry.load_model("models/Kokoro-82M-GGUF", &ModelConfig::family("kokoro_tts"), None)?;
//! let mut session = model.offline_session(Task::Tts, &BackendConfig::new(Backend::Best), None)?;
//!
//! let mut request = Request::with_text("Hello from audio.cpp.")?;
//! request.set_option("voice_id", "af_heart")?;
//!
//! let result = session.run(&request)?;
//! if let Some(audio) = result.audio()? {
//!     println!("{:?} of audio at {} Hz", audio.duration(), audio.sample_rate);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Resources
//!
//! A [`Model`] is cheap: parsed config plus a mapping of the weight files.
//! Every session uploads its own copy of the weights to the compute device,
//! so create few sessions and reuse them. [`SessionPool`] does that for an
//! application: it keeps sessions warm, bounds how many are resident, and
//! evicts the least recently used.
//!
//! Free every session before the process exits ([`SessionPool::shutdown`]
//! for pooled ones): one that outlives `exit()` is freed after the GPU
//! backend's static teardown, which ggml's Metal backend aborts on.
//!
//! # Threading
//!
//! Handles are [`Send`] but not [`Sync`]: a session can move to a worker
//! thread, but one handle is never used from two threads at once. Separate
//! sessions run in parallel. Session creation is serialized process-wide
//! because concurrent creation crashes upstream.
//!
//! ```compile_fail
//! fn share<T: Sync>(_: &T) {}
//! fn check(session: &audio_cpp::OfflineSession) { share(session) }
//! ```
//! ```compile_fail
//! fn share<T: Sync>(_: &T) {}
//! fn check(model: &audio_cpp::Model) { share(model) }
//! ```
//! ```compile_fail
//! // Audio borrowed from a result cannot outlive it.
//! fn leak(result: audio_cpp::TaskResult) -> &'static [f32] {
//!     result.audio().unwrap().unwrap().samples
//! }
//! ```
//!
//! # Streaming
//!
//! Audio-input families (ASR, VAD, diarization) stream incrementally:
//! [`StreamingSession::push`] returns events as audio arrives. Text-driven
//! families (TTS) currently generate the whole utterance inside
//! [`StreamingSession::start`], because the C ABI carries no callback for
//! their incremental audio; split long text and run it piece by piece to get
//! progress and cancellation points.
//!
//! # Features
//!
//! - `catalog` (default): the embedded upstream model catalog, [`catalog`].
//! - `wav` (default): [`AudioBuffer`] WAV reading and writing.
//! - `decode`: [`AudioBuffer::decode_file`] for MP3, AAC/M4A, FLAC, Ogg Vorbis,
//!   AIFF, ALAC and CAF.
//! - `resample`: [`AudioBuffer::resample`].
//! - `cuda`, `hip`, `vulkan`, `metal`: GPU backends for the native library.
//! - `openmp` (default), `native`: native build options; see `audio-cpp-sys`.

// C enums are `c_uint` on most targets but `c_int` on MSVC, so casts that
// look redundant here are not redundant everywhere.
#![allow(clippy::unnecessary_cast)]
#![deny(unsafe_op_in_unsafe_fn)]

mod audio;
#[cfg(feature = "catalog")]
pub mod catalog;
mod error;
mod ffi;
mod model;
mod pool;
mod request;
mod result;
mod session;
mod task;
#[cfg(windows)]
mod winpath;

pub use audio_cpp_sys as sys;

pub use audio::{AudioBuffer, AudioRef};
pub use error::{Error, ErrorKind, Result};
pub use model::{Model, ModelConfig, ModelInfo, OptionInfo, OptionScope, Options, Registry};
pub use pool::{CancelToken, ModelConfigKey, PoolConfig, PoolStats, PooledSession, SessionPool, SessionSpec};
pub use request::Request;
pub use result::{ArtifactKind, ArtifactRef, NamedAudio, ResultView, Segment, SpeakerTurn, TaskResult, Text, Word};
pub use session::{
    Event, OfflineSession, StreamInputKind, StreamOutputKind, StreamPolicy, StreamingSession, VoiceActivity,
    VoiceActivityKind,
};
pub use task::{Backend, BackendConfig, Mode, Task};

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
    unsafe { ffi::static_str(sys::audiocpp_build_version()) }
}
