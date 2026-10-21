use std::ffi::{CString, c_char, c_int};
use std::path::Path;
use std::ptr::{self, NonNull};
use std::sync::{Mutex, PoisonError};

use crate::error::{Error, ErrorKind, Result, check};
use crate::ffi::{allocation_failed, c_string, create, handle, opt_c_string, opt_ptr, owned_string};
use crate::session::{OfflineSession, StreamingSession};
use crate::task::{BackendConfig, Mode, Task};
use crate::{AbiVersion, abi_version, sys};

handle!(
    /// A `string -> string` option map, passed to model loading
    /// (`--load-option`) and session creation (`--session-option`).
    pub Options,
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

fn options_ptr(options: Option<&Options>) -> *const sys::audiocpp_options {
    options.map_or(ptr::null(), |o| o.as_ptr())
}

pub(crate) fn path_c_string(path: &Path) -> Result<CString> {
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    };
    #[cfg(windows)]
    let bytes = crate::winpath::encode(path)?;
    #[cfg(not(any(unix, windows)))]
    let bytes = path
        .to_str()
        .ok_or_else(|| Error::invalid_argument(format!("path is not valid UTF-8: {}", path.display())))?
        .as_bytes()
        .to_vec();
    CString::new(bytes).map_err(|_| Error::invalid_argument("path contains a NUL byte"))
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

handle!(
    /// The set of model loaders compiled into the library. Entry point for
    /// loading models.
    pub Registry,
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
                ErrorKind::IncompatibleAbi,
                format!("libaudiocpp ABI {version} cannot serve bindings built for {}", AbiVersion::BINDINGS),
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
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

    pub fn config_id(mut self, id: impl Into<String>) -> Self {
        self.config_id = Some(id.into());
        self
    }

    pub fn weight_id(mut self, id: impl Into<String>) -> Self {
        self.weight_id = Some(id.into());
        self
    }

    pub fn model_spec_override(mut self, spec: impl Into<String>) -> Self {
        self.model_spec_override = Some(spec.into());
        self
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
/// discovery metadata for building UIs; the library does not enforce them.
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

/// Everything a UI needs to describe a loaded model, gathered in one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInfo {
    pub family: String,
    pub description: String,
    /// Tasks the model serves offline.
    pub offline_tasks: Vec<Task>,
    /// Tasks the model serves in streaming mode.
    pub streaming_tasks: Vec<Task>,
    pub languages: Vec<String>,
    pub timestamps: bool,
    pub speaker_reference: bool,
    pub style_condition: bool,
    pub load_options: Vec<OptionInfo>,
    pub session_options: Vec<OptionInfo>,
    pub request_options: Vec<OptionInfo>,
}

handle!(
    /// A loaded model. Cheap: it holds the parsed config and a mapping of the
    /// weight files. Each [`OfflineSession`] / [`StreamingSession`] created
    /// from it uploads its own copy of the weights to the compute device, so
    /// reuse sessions rather than creating one per request.
    pub Model,
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

    /// Whether the model serves `task` in `mode`.
    pub fn supports(&self, task: &Task, mode: Mode) -> bool {
        let Ok(task) = task.c_string() else { return false };
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

    /// Family, capabilities, languages and declared options in one value.
    pub fn info(&self) -> Result<ModelInfo> {
        let tasks = Task::all();
        let serving = |mode| tasks.iter().filter(|t| self.supports(t, mode)).cloned().collect();
        Ok(ModelInfo {
            family: self.family(),
            description: self.description(),
            offline_tasks: serving(Mode::Offline),
            streaming_tasks: serving(Mode::Streaming),
            languages: self.languages()?,
            timestamps: self.supports_timestamps(),
            speaker_reference: self.supports_speaker_reference(),
            style_condition: self.supports_style_condition(),
            load_options: self.options(OptionScope::Load)?,
            session_options: self.options(OptionScope::Session)?,
            request_options: self.options(OptionScope::Request)?,
        })
    }

    pub(crate) fn create_session(
        &self,
        task: &Task,
        mode: Mode,
        backend: &BackendConfig,
        options: Option<&Options>,
    ) -> Result<NonNull<sys::audiocpp_session>> {
        let task = task.c_string()?;
        let raw = backend.raw()?;
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
            |p| p,
        )
    }

    /// Creates an offline session for `task`. `options` corresponds to
    /// `--session-option`.
    pub fn offline_session(
        &self,
        task: impl Into<Task>,
        backend: &BackendConfig,
        options: Option<&Options>,
    ) -> Result<OfflineSession> {
        let ptr = self.create_session(&task.into(), Mode::Offline, backend, options)?;
        // SAFETY: a fresh session handle whose ownership we take.
        Ok(unsafe { OfflineSession::from_raw(ptr) })
    }

    /// Creates a streaming session for `task`.
    pub fn streaming_session(
        &self,
        task: impl Into<Task>,
        backend: &BackendConfig,
        options: Option<&Options>,
    ) -> Result<StreamingSession> {
        let ptr = self.create_session(&task.into(), Mode::Streaming, backend, options)?;
        // SAFETY: as above.
        Ok(unsafe { StreamingSession::from_raw(ptr) })
    }
}

impl std::fmt::Debug for Options {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options").finish_non_exhaustive()
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
