use std::ffi::{CString, c_char, c_int};
use std::path::Path;
use std::ptr::{self, NonNull};
use std::sync::{Mutex, PoisonError};

use crate::error::{Error, ErrorKind, Result, check};
use crate::session::Session;
use crate::{AbiVersion, abi_version, c_string, create, handle, opt_c_string, opt_ptr, owned_string, sys};

handle!(
    /// A `string -> string` option map, passed to model loading
    /// (`--load-option`) and session creation (`--session-option`).
    Options,
    sys::audiocpp_options,
    sys::audiocpp_options_free
);

impl Options {
    pub fn new() -> Result<Self> {
        // SAFETY: no preconditions.
        let ptr = unsafe { sys::audiocpp_options_create() };
        NonNull::new(ptr).map(|ptr| Self { ptr }).ok_or_else(|| allocation_failed("options"))
    }

    /// Builds an option map from key/value pairs.
    pub fn from_pairs<K: AsRef<str>, V: AsRef<str>>(pairs: impl IntoIterator<Item = (K, V)>) -> Result<Self> {
        let mut options = Self::new()?;
        for (key, value) in pairs {
            options.set(key.as_ref(), value.as_ref())?;
        }
        Ok(options)
    }

    pub fn set(&mut self, key: &str, value: &str) -> Result<&mut Self> {
        let key = c_string(key, "option key")?;
        let value = c_string(value, "option value")?;
        // SAFETY: valid handle and C strings; both are copied.
        check(unsafe { sys::audiocpp_options_set(self.as_ptr(), key.as_ptr(), value.as_ptr()) })?;
        Ok(self)
    }
}

/// Constructors that return a bare pointer do not set `audiocpp_last_error`,
/// so reading it after one fails would report an unrelated, stale message.
pub(crate) fn allocation_failed(what: &str) -> Error {
    Error::new(ErrorKind::OutOfMemory, format!("failed to allocate {what}"))
}

/// Serializes `audiocpp_session_create` across every model and registry.
///
/// Separately loaded models can share cached weight sources upstream. Silero
/// session construction uploads from that source and then unmaps it, so a
/// concurrent construction elsewhere reads freed memory and crashes
/// (reproduced: 8 threads each loading a model and creating sessions crash
/// almost every run). Running sessions do not touch the shared source, so only
/// construction is serialized.
static SESSION_CREATION: Mutex<()> = Mutex::new(());

fn options_ptr(options: Option<&Options>) -> *const sys::audiocpp_options {
    options.map_or(ptr::null(), |o| o.as_ptr())
}

pub(crate) fn path_c_string(path: &Path) -> Result<CString> {
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    };
    #[cfg(not(unix))]
    let bytes = path
        .to_str()
        .ok_or_else(|| Error::invalid_argument(format!("path is not valid UTF-8: {}", path.display())))?
        .as_bytes()
        .to_vec();
    CString::new(bytes).map_err(|_| Error::invalid_argument("path contains a NUL byte"))
}

handle!(
    /// The set of model loaders compiled into the library. Entry point for
    /// loading models.
    Registry,
    sys::audiocpp_registry,
    sys::audiocpp_registry_free
);

impl Registry {
    /// Creates a registry with the default loader set.
    pub fn new() -> Result<Self> {
        Self::create(None)
    }

    /// Creates a registry from a loader config file.
    pub fn with_config(config_path: impl AsRef<Path>) -> Result<Self> {
        Self::create(Some(config_path.as_ref()))
    }

    fn create(config_path: Option<&Path>) -> Result<Self> {
        let version = abi_version();
        if !version.is_compatible() {
            return Err(Error::new(
                ErrorKind::NotAvailable,
                format!("libaudiocpp ABI {version} is incompatible with bindings built for {}", AbiVersion::BINDINGS),
            ));
        }
        let config_path = config_path.map(path_c_string).transpose()?;
        let config_ptr = config_path.as_ref().map_or(ptr::null(), |p| p.as_ptr());
        // SAFETY: config_ptr is NULL or a valid C string; out is written on success.
        create(|out| unsafe { sys::audiocpp_registry_create(config_ptr, out) }, |p| unsafe { Self::from_raw(p) })
    }

    /// Names of the model families this build can load.
    pub fn families(&self) -> Result<Vec<String>> {
        // SAFETY: valid handle.
        let count = unsafe { sys::audiocpp_registry_family_count(self.as_ptr()) };
        (0..count)
            .map(|i| {
                let mut name: *const c_char = ptr::null();
                // SAFETY: index in range; the string is copied immediately.
                check(unsafe { sys::audiocpp_registry_family(self.as_ptr(), i, &mut name) })?;
                Ok(unsafe { owned_string(name) })
            })
            .collect()
    }

    /// Loads a model from a file or package directory. `options` corresponds
    /// to the CLI's `--load-option`.
    pub fn load_model(&self, path: impl AsRef<Path>, config: &ModelConfig, options: Option<&Options>) -> Result<Model> {
        let path = path_c_string(path.as_ref())?;
        let family = opt_c_string(config.family.as_deref(), "family")?;
        let config_id = opt_c_string(config.config_id.as_deref(), "config_id")?;
        let weight_id = opt_c_string(config.weight_id.as_deref(), "weight_id")?;
        let spec = opt_c_string(config.model_spec_override.as_deref(), "model_spec_override")?;
        let raw = sys::audiocpp_model_config {
            family_hint: opt_ptr(&family),
            config_id: opt_ptr(&config_id),
            weight_id: opt_ptr(&weight_id),
            model_spec_override: opt_ptr(&spec),
        };
        // SAFETY: every pointer outlives the call; the library copies what it keeps.
        create(
            |out| unsafe { sys::audiocpp_model_load(self.as_ptr(), path.as_ptr(), &raw, options_ptr(options), out) },
            |p| unsafe { Model::from_raw(p) },
        )
    }
}

/// Model selection. Every field is optional and maps onto the CLI flag of the
/// same name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelConfig {
    /// `--family`; `None` identifies the model from its path.
    pub family: Option<String>,
    /// `--config`, for packages that ship several configs.
    pub config_id: Option<String>,
    /// `--weight`, for packages that ship several weight sets.
    pub weight_id: Option<String>,
    /// `--model-spec-override`.
    pub model_spec_override: Option<String>,
}

impl ModelConfig {
    /// A config that names only the family.
    pub fn family(family: impl Into<String>) -> Self {
        Self { family: Some(family.into()), ..Self::default() }
    }
}

/// Offline (whole request at once) or streaming (pushed chunks) execution.
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

    pub(crate) fn as_c_str(self) -> &'static std::ffi::CStr {
        match self {
            Mode::Offline => c"offline",
            Mode::Streaming => c"streaming",
        }
    }
}

/// Compute backend for a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Backend {
    #[default]
    Cpu,
    Cuda,
    Hip,
    Vulkan,
    Metal,
    /// Let audio.cpp pick the best available backend.
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

    fn as_c_str(self) -> &'static std::ffi::CStr {
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

/// Backend, device index and thread count for a session (`--backend`,
/// `--device`, `--threads`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BackendConfig {
    pub backend: Backend,
    pub device: i32,
    pub threads: i32,
}

impl BackendConfig {
    /// `backend` on device 0, with one thread per available CPU.
    pub fn new(backend: Backend) -> Self {
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get() as i32);
        Self { backend, device: 0, threads }
    }

    pub fn device(mut self, device: i32) -> Self {
        self.device = device;
        self
    }

    pub fn threads(mut self, threads: i32) -> Self {
        self.threads = threads;
        self
    }
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self::new(Backend::Cpu)
    }
}

/// Which option map an [`OptionInfo`] belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OptionScope {
    /// Per-request options (`--request-option`, [`crate::Request::set_option`]).
    Request,
    /// Session options (`--session-option`).
    Session,
    /// Load options (`--load-option`).
    Load,
}

impl OptionScope {
    fn raw(self) -> sys::audiocpp_option_scope {
        match self {
            OptionScope::Request => sys::AUDIOCPP_OPTION_SCOPE_REQUEST,
            OptionScope::Session => sys::AUDIOCPP_OPTION_SCOPE_SESSION,
            OptionScope::Load => sys::AUDIOCPP_OPTION_SCOPE_LOAD,
        }
    }
}

/// An option a model declares. Absent strings are empty. Bounds are
/// discovery metadata only; they are not enforced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionInfo {
    pub name: String,
    pub value_name: String,
    pub description: String,
    pub default_value: String,
    pub min_value: String,
    pub max_value: String,
    pub required: bool,
}

handle!(
    /// A loaded model. Create [`Session`]s from it to run tasks.
    Model,
    sys::audiocpp_model,
    sys::audiocpp_model_free
);

impl Model {
    pub fn family(&self) -> String {
        // SAFETY: valid handle; borrowed string copied immediately.
        unsafe { owned_string(sys::audiocpp_model_family(self.as_ptr())) }
    }

    pub fn description(&self) -> String {
        // SAFETY: as above.
        unsafe { owned_string(sys::audiocpp_model_description(self.as_ptr())) }
    }

    /// Whether the model serves `task` (one of [`crate::tasks`]) in `mode`.
    /// An unrecognised task reads as `false`.
    pub fn supports(&self, task: &str, mode: Mode) -> bool {
        let Ok(task) = CString::new(task) else { return false };
        // SAFETY: valid handle and C strings.
        unsafe { sys::audiocpp_model_supports(self.as_ptr(), task.as_ptr(), mode.as_c_str().as_ptr()) != 0 }
    }

    pub fn supports_timestamps(&self) -> bool {
        // SAFETY: valid handle.
        unsafe { sys::audiocpp_model_supports_timestamps(self.as_ptr()) != 0 }
    }

    pub fn supports_speaker_reference(&self) -> bool {
        // SAFETY: valid handle.
        unsafe { sys::audiocpp_model_supports_speaker_reference(self.as_ptr()) != 0 }
    }

    pub fn supports_style_condition(&self) -> bool {
        // SAFETY: valid handle.
        unsafe { sys::audiocpp_model_supports_style_condition(self.as_ptr()) != 0 }
    }

    pub fn languages(&self) -> Result<Vec<String>> {
        // SAFETY: valid handle; indices in range; strings copied.
        let count = unsafe { sys::audiocpp_model_language_count(self.as_ptr()) };
        (0..count)
            .map(|i| {
                let mut language: *const c_char = ptr::null();
                check(unsafe { sys::audiocpp_model_language(self.as_ptr(), i, &mut language) })?;
                Ok(unsafe { owned_string(language) })
            })
            .collect()
    }

    /// The options this model declares in `scope`.
    pub fn options(&self, scope: OptionScope) -> Result<Vec<OptionInfo>> {
        // SAFETY: valid handle.
        let count = unsafe { sys::audiocpp_model_option_count(self.as_ptr(), scope.raw()) };
        (0..count)
            .map(|i| {
                let mut s: [*const c_char; 6] = [ptr::null(); 6];
                let mut required: c_int = 0;
                // SAFETY: valid handle and out pointers; strings copied below.
                check(unsafe {
                    sys::audiocpp_model_option(
                        self.as_ptr(),
                        scope.raw(),
                        i,
                        &mut s[0],
                        &mut s[1],
                        &mut s[2],
                        &mut s[3],
                        &mut s[4],
                        &mut s[5],
                        &mut required,
                    )
                })?;
                let [name, value_name, description, default_value, min_value, max_value] =
                    s.map(|p| unsafe { owned_string(p) });
                Ok(OptionInfo {
                    name,
                    value_name,
                    description,
                    default_value,
                    min_value,
                    max_value,
                    required: required != 0,
                })
            })
            .collect()
    }

    /// Creates a session for `task` (one of [`crate::tasks`], e.g. `"tts"`,
    /// `"asr"`, `"vad"`). `options` corresponds to `--session-option`.
    pub fn create_session(
        &self,
        task: &str,
        mode: Mode,
        backend: &BackendConfig,
        options: Option<&Options>,
    ) -> Result<Session> {
        if backend.device < 0 || backend.threads <= 0 {
            return Err(Error::invalid_argument("device must be non-negative and threads positive"));
        }
        let task = c_string(task, "task")?;
        let raw = sys::audiocpp_backend_config {
            backend: backend.backend.as_c_str().as_ptr(),
            device: backend.device,
            threads: backend.threads,
        };
        create(
            |out| {
                // The lock guards native state, not Rust data, and the C ABI
                // contains exceptions, so a poisoned lock is safe to reuse. The
                // error detail is thread-local, so checking it after the lock is
                // released cannot pick up another thread's failure.
                let _guard = SESSION_CREATION.lock().unwrap_or_else(PoisonError::into_inner);
                // SAFETY: all pointers outlive the call.
                unsafe {
                    sys::audiocpp_session_create(
                        self.as_ptr(),
                        task.as_ptr(),
                        mode.as_c_str().as_ptr(),
                        &raw,
                        options_ptr(options),
                        out,
                    )
                }
            },
            |p| unsafe { Session::from_raw(p) },
        )
    }
}

impl std::fmt::Debug for Options {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options").field("ptr", &self.ptr).finish()
    }
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry").field("families", &self.families().map(|f| f.len())).finish()
    }
}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Model").field("family", &self.family()).finish()
    }
}
