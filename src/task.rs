use std::convert::Infallible;
use std::ffi::{CStr, CString};
use std::fmt;
use std::str::FromStr;

use crate::error::Result;
use crate::ffi::{c_string, static_str};
use crate::sys;

/// What a session does. Maps onto the C ABI's task tokens (`--task`).
///
/// Unknown tokens round-trip through [`Task::Other`], so a task added upstream
/// works before this enum learns its name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Task {
    /// `vad`: voice activity detection.
    Vad,
    /// `asr`: speech recognition.
    Asr,
    /// `diar`: speaker diarization.
    Diarization,
    /// `sep`: source separation.
    Separation,
    /// `gen`: music, sound effects and other audio generation.
    Generation,
    /// `tts`: text to speech.
    Tts,
    /// `clon`: voice cloning from reference audio.
    VoiceCloning,
    /// `vc`: voice conversion.
    VoiceConversion,
    /// `s2s`: speech to speech.
    SpeechToSpeech,
    /// `align`: forced alignment.
    Alignment,
    /// `vdes`: voice design from a description.
    VoiceDesign,
    /// `spk`: speaker embedding / verification.
    Speaker,
    /// `svc`: singing voice conversion.
    SingingVoiceConversion,
    /// `midi`: audio to MIDI / symbolic events.
    Midi,
    /// `turn`: conversational turn detection.
    TurnDetection,
    /// `wake`: wake-word detection.
    WakeWord,
    /// `cls`: audio classification.
    Classification,
    /// Any other token the library accepts.
    Other(String),
}

const KNOWN: [(&str, Task); 17] = [
    ("vad", Task::Vad),
    ("asr", Task::Asr),
    ("diar", Task::Diarization),
    ("sep", Task::Separation),
    ("gen", Task::Generation),
    ("tts", Task::Tts),
    ("clon", Task::VoiceCloning),
    ("vc", Task::VoiceConversion),
    ("s2s", Task::SpeechToSpeech),
    ("align", Task::Alignment),
    ("vdes", Task::VoiceDesign),
    ("spk", Task::Speaker),
    ("svc", Task::SingingVoiceConversion),
    ("midi", Task::Midi),
    ("turn", Task::TurnDetection),
    ("wake", Task::WakeWord),
    ("cls", Task::Classification),
];

impl Task {
    /// The canonical C ABI token, e.g. `"tts"`.
    pub fn as_str(&self) -> &str {
        match self {
            Task::Other(token) => token,
            known => KNOWN.iter().find(|(_, task)| task == known).map_or("", |(token, _)| token),
        }
    }

    /// Every task this build of the library accepts.
    pub fn all() -> Vec<Task> {
        // SAFETY: no preconditions; names are static or NULL.
        let count = unsafe { sys::audiocpp_task_count() };
        (0..count)
            .filter_map(|i| {
                let name = unsafe { sys::audiocpp_task_name(i) };
                (!name.is_null()).then(|| Task::from(unsafe { static_str(name) }))
            })
            .collect()
    }

    /// Maps a model-spec task name to its task (`"music"` → [`Task::Generation`]).
    /// `None` when this build cannot serve that kind of task.
    pub fn from_spec_name(spec_task: &str) -> Option<Task> {
        let spec_task = CString::new(spec_task).ok()?;
        // SAFETY: valid C string; the result is static or NULL.
        let name = unsafe { sys::audiocpp_task_from_spec_name(spec_task.as_ptr()) };
        (!name.is_null()).then(|| Task::from(unsafe { static_str(name) }))
    }

    pub(crate) fn c_string(&self) -> Result<CString> {
        c_string(self.as_str(), "task")
    }
}

impl From<&str> for Task {
    fn from(token: &str) -> Self {
        KNOWN
            .iter()
            .find(|(known, _)| *known == token)
            .map_or_else(|| Task::Other(token.to_string()), |(_, task)| task.clone())
    }
}

impl FromStr for Task {
    type Err = Infallible;

    fn from_str(token: &str) -> Result<Self, Infallible> {
        Ok(Task::from(token))
    }
}

impl fmt::Display for Task {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Offline (whole request at once) or streaming execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Mode {
    #[default]
    Offline,
    Streaming,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Offline => "offline",
            Mode::Streaming => "streaming",
        }
    }

    pub(crate) fn as_c_str(self) -> &'static CStr {
        match self {
            Mode::Offline => c"offline",
            Mode::Streaming => c"streaming",
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Compute backend for a session (`--backend`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Backend {
    #[default]
    Cpu,
    Cuda,
    /// AMD GPUs through HIP / ROCm.
    Hip,
    Vulkan,
    Metal,
    /// The best backend compiled into the library.
    Best,
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Cpu => "cpu",
            Backend::Cuda => "cuda",
            Backend::Hip => "hip",
            Backend::Vulkan => "vulkan",
            Backend::Metal => "metal",
            Backend::Best => "best",
        }
    }

    pub(crate) fn as_c_str(self) -> &'static CStr {
        match self {
            Backend::Cpu => c"cpu",
            Backend::Cuda => c"cuda",
            Backend::Hip => c"hip",
            Backend::Vulkan => c"vulkan",
            Backend::Metal => c"metal",
            Backend::Best => c"best",
        }
    }
}

impl FromStr for Backend {
    type Err = crate::Error;

    fn from_str(name: &str) -> Result<Self> {
        Ok(match name.trim().to_ascii_lowercase().as_str() {
            "cpu" => Backend::Cpu,
            "cuda" => Backend::Cuda,
            "hip" | "rocm" => Backend::Hip,
            "vulkan" => Backend::Vulkan,
            "metal" => Backend::Metal,
            "best" | "auto" => Backend::Best,
            other => return Err(crate::Error::invalid_argument(format!("unknown backend {other:?}"))),
        })
    }
}

impl fmt::Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Backend, device index and CPU thread count for a session (`--backend`,
/// `--device`, `--threads`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BackendConfig {
    pub backend: Backend,
    /// GPU index for multi-GPU machines; ignored on CPU.
    pub device: u32,
    /// CPU threads for host-side work. Must be at least 1.
    pub threads: u32,
}

impl BackendConfig {
    /// `backend` on device 0, with one thread per available CPU.
    pub fn new(backend: Backend) -> Self {
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
        Self { backend, device: 0, threads }
    }

    pub fn device(mut self, device: u32) -> Self {
        self.device = device;
        self
    }

    pub fn threads(mut self, threads: u32) -> Self {
        self.threads = threads;
        self
    }

    pub(crate) fn raw(&self) -> Result<sys::audiocpp_backend_config> {
        let device =
            i32::try_from(self.device).map_err(|_| crate::Error::invalid_argument("device index out of range"))?;
        let threads = i32::try_from(self.threads)
            .ok()
            .filter(|&t| t > 0)
            .ok_or_else(|| crate::Error::invalid_argument(format!("threads must be in 1..={}", i32::MAX)))?;
        Ok(sys::audiocpp_backend_config { backend: self.backend.as_c_str().as_ptr(), device, threads })
    }
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self::new(Backend::Cpu)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_tokens_round_trip() {
        for (token, task) in KNOWN {
            assert_eq!(Task::from(token), task);
            assert_eq!(task.as_str(), token);
        }
        assert_eq!(Task::from("future"), Task::Other("future".into()));
        assert_eq!(Task::Other("future".into()).as_str(), "future");
    }

    #[test]
    fn backend_parses_aliases() {
        assert_eq!("ROCm".parse::<Backend>().unwrap(), Backend::Hip);
        assert_eq!("auto".parse::<Backend>().unwrap(), Backend::Best);
        assert!("tpu".parse::<Backend>().is_err());
    }

    #[test]
    fn backend_config_rejects_zero_threads() {
        assert!(BackendConfig::default().threads(0).raw().is_err());
        assert!(BackendConfig::default().device(u32::MAX).raw().is_err());
        assert!(BackendConfig::default().threads(2).raw().is_ok());
    }
}
