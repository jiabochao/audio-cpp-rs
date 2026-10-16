use std::ffi::c_char;
use std::ptr::NonNull;

use crate::audio::AudioRef;
use crate::error::{Error, Result, check};
use crate::ffi::{allocation_failed, c_string, handle, opt_c_string, opt_ptr};
use crate::result::ArtifactKind;
use crate::sys;

handle!(
    /// The inputs for one task run: text, audio, a speaker reference, style
    /// conditioning, artifacts and free-form options.
    ///
    /// Setters copy their arguments and return `&mut Self` for chaining.
    pub Request,
    sys::audiocpp_request,
    sys::audiocpp_request_free
);

fn finite(value: f32, what: &str) -> Result<f32> {
    if value.is_finite() { Ok(value) } else { Err(Error::invalid_argument(format!("{what} must be finite"))) }
}

impl Request {
    pub fn new() -> Result<Self> {
        // SAFETY: no preconditions.
        let ptr = unsafe { sys::audiocpp_request_create() };
        NonNull::new(ptr).map(|ptr| Self { ptr }).ok_or_else(|| allocation_failed("request"))
    }

    /// A request carrying only `text`, the usual start for TTS.
    pub fn with_text(text: &str) -> Result<Self> {
        let mut request = Self::new()?;
        request.set_text(text, None)?;
        Ok(request)
    }

    /// A request carrying only input audio, the usual start for ASR or VAD.
    pub fn with_audio(audio: AudioRef<'_>) -> Result<Self> {
        let mut request = Self::new()?;
        request.set_audio(audio)?;
        Ok(request)
    }

    /// Sets the text. A non-empty `language` is also written to the
    /// `"language"` request option, as the CLI's `--language` does. Use
    /// [`Self::set_text_language`] to set the transcript language alone.
    pub fn set_text(&mut self, text: &str, language: Option<&str>) -> Result<&mut Self> {
        let text = c_string(text, "text")?;
        let language = opt_c_string(language, "language")?;
        // SAFETY: valid handle and strings; copied.
        check(unsafe { sys::audiocpp_request_set_text(self.as_ptr(), text.as_ptr(), opt_ptr(&language)) })?;
        Ok(self)
    }

    /// Sets the transcript language without touching the `"language"` option.
    pub fn set_text_language(&mut self, language: &str) -> Result<&mut Self> {
        let language = c_string(language, "language")?;
        // SAFETY: as above.
        check(unsafe { sys::audiocpp_request_set_text_language(self.as_ptr(), language.as_ptr()) })?;
        Ok(self)
    }

    /// Sets the input audio (ASR, VAD, separation, conversion, ...). Copied.
    pub fn set_audio(&mut self, audio: AudioRef<'_>) -> Result<&mut Self> {
        let (frames, rate, channels) = audio.ffi_layout()?;
        // SAFETY: the slice covers frames * channels floats; copied by the library.
        check(unsafe {
            sys::audiocpp_request_set_audio(self.as_ptr(), audio.samples.as_ptr(), frames, rate, channels)
        })?;
        Ok(self)
    }

    /// Sets the speaker reference audio, for cloning and conversion. Copied.
    pub fn set_voice_audio(&mut self, audio: AudioRef<'_>) -> Result<&mut Self> {
        let (frames, rate, channels) = audio.ffi_layout()?;
        // SAFETY: as above.
        check(unsafe {
            sys::audiocpp_request_set_voice_audio(self.as_ptr(), audio.samples.as_ptr(), frames, rate, channels)
        })?;
        Ok(self)
    }

    /// Selects a cached voice by id.
    pub fn set_voice_id(&mut self, voice_id: &str) -> Result<&mut Self> {
        let voice_id = c_string(voice_id, "voice_id")?;
        // SAFETY: valid handle and string; copied.
        check(unsafe { sys::audiocpp_request_set_voice_id(self.as_ptr(), voice_id.as_ptr()) })?;
        Ok(self)
    }

    /// `--style-language`.
    pub fn set_style_language(&mut self, language: &str) -> Result<&mut Self> {
        let language = c_string(language, "style language")?;
        // SAFETY: as above.
        check(unsafe { sys::audiocpp_request_set_style_language(self.as_ptr(), language.as_ptr()) })?;
        Ok(self)
    }

    /// `--emotion`.
    pub fn set_emotion(&mut self, emotion: &str) -> Result<&mut Self> {
        let emotion = c_string(emotion, "emotion")?;
        // SAFETY: as above.
        check(unsafe { sys::audiocpp_request_set_emotion(self.as_ptr(), emotion.as_ptr()) })?;
        Ok(self)
    }

    /// `--speaking-rate`.
    pub fn set_speaking_rate(&mut self, rate: f32) -> Result<&mut Self> {
        // SAFETY: valid handle.
        let rate = finite(rate, "speaking rate")?;
        check(unsafe { sys::audiocpp_request_set_speaking_rate(self.as_ptr(), rate) })?;
        Ok(self)
    }

    /// `--pitch-shift`.
    pub fn set_pitch_shift(&mut self, shift: f32) -> Result<&mut Self> {
        // SAFETY: valid handle.
        let shift = finite(shift, "pitch shift")?;
        check(unsafe { sys::audiocpp_request_set_pitch_shift(self.as_ptr(), shift) })?;
        Ok(self)
    }

    /// `--energy-scale`.
    pub fn set_energy_scale(&mut self, scale: f32) -> Result<&mut Self> {
        // SAFETY: valid handle.
        let scale = finite(scale, "energy scale")?;
        check(unsafe { sys::audiocpp_request_set_energy_scale(self.as_ptr(), scale) })?;
        Ok(self)
    }

    /// `--style-tag key=value`.
    pub fn set_style_tag(&mut self, key: &str, value: &str) -> Result<&mut Self> {
        let key = c_string(key, "style tag key")?;
        let value = c_string(value, "style tag value")?;
        // SAFETY: valid handle and strings; copied.
        check(unsafe { sys::audiocpp_request_set_style_tag(self.as_ptr(), key.as_ptr(), value.as_ptr()) })?;
        Ok(self)
    }

    /// Adds an input artifact (e.g. a speaker embedding read from an earlier
    /// result) and returns its index, for [`Self::set_artifact_meta`].
    pub fn add_artifact(&mut self, kind: ArtifactKind, id: &str, payload: &[u8]) -> Result<usize> {
        let id = c_string(id, "artifact id")?;
        let mut index = 0usize;
        // SAFETY: valid handle; payload covers payload.len() bytes and is copied.
        check(unsafe {
            sys::audiocpp_request_add_artifact(
                self.as_ptr(),
                kind.raw(),
                id.as_ptr(),
                payload.as_ptr().cast(),
                payload.len(),
                &mut index,
            )
        })?;
        Ok(index)
    }

    pub fn set_artifact_meta(&mut self, index: usize, key: &str, value: &str) -> Result<&mut Self> {
        let key = c_string(key, "artifact meta key")?;
        let value = c_string(value, "artifact meta value")?;
        // SAFETY: valid handle and strings; copied.
        check(unsafe { sys::audiocpp_request_set_artifact_meta(self.as_ptr(), index, key.as_ptr(), value.as_ptr()) })?;
        Ok(self)
    }

    /// `--request-option key=value`. See [`crate::Model::options`] for what
    /// a model accepts.
    pub fn set_option(&mut self, key: &str, value: &str) -> Result<&mut Self> {
        let key = c_string(key, "option key")?;
        let value = c_string(value, "option value")?;
        // SAFETY: valid handle and strings; copied.
        check(unsafe { sys::audiocpp_request_set_option(self.as_ptr(), key.as_ptr(), value.as_ptr()) })?;
        Ok(self)
    }

    /// Sets a list-valued option, replacing any earlier list under `key`.
    /// List options live apart from single-valued ones.
    pub fn set_option_list<S: AsRef<str>>(&mut self, key: &str, values: &[S]) -> Result<&mut Self> {
        let key = c_string(key, "option key")?;
        let values = values.iter().map(|v| c_string(v.as_ref(), "option value")).collect::<Result<Vec<_>>>()?;
        let ptrs: Vec<*const c_char> = values.iter().map(|v| v.as_ptr()).collect();
        // SAFETY: ptrs holds values.len() valid strings; all copied.
        check(unsafe {
            sys::audiocpp_request_set_option_array(self.as_ptr(), key.as_ptr(), ptrs.as_ptr(), ptrs.len())
        })?;
        Ok(self)
    }
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request").field("ptr", &self.ptr).finish()
    }
}
