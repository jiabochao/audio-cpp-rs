use std::ffi::{c_char, c_int, c_void};
use std::marker::{PhantomData, PhantomPinned};
use std::ops::Deref;
use std::ptr;

use crate::error::{Error, ErrorKind, Result, check};
use crate::{handle, owned_string, sys};

/// Kinds of opaque payloads a family produces or consumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ArtifactKind {
    SpeakerEmbedding,
    StyleEmbedding,
    PromptEmbedding,
    AcousticTokens,
    Midi,
    TranscriptAlignment,
    DiarizationState,
    VadState,
    Custom,
    /// A kind this version of the bindings does not know about.
    Other(u32),
}

impl ArtifactKind {
    pub(crate) fn raw(self) -> sys::audiocpp_artifact_kind {
        match self {
            Self::SpeakerEmbedding => sys::AUDIOCPP_ARTIFACT_SPEAKER_EMBEDDING,
            Self::StyleEmbedding => sys::AUDIOCPP_ARTIFACT_STYLE_EMBEDDING,
            Self::PromptEmbedding => sys::AUDIOCPP_ARTIFACT_PROMPT_EMBEDDING,
            Self::AcousticTokens => sys::AUDIOCPP_ARTIFACT_ACOUSTIC_TOKENS,
            Self::Midi => sys::AUDIOCPP_ARTIFACT_MIDI,
            Self::TranscriptAlignment => sys::AUDIOCPP_ARTIFACT_TRANSCRIPT_ALIGNMENT,
            Self::DiarizationState => sys::AUDIOCPP_ARTIFACT_DIARIZATION_STATE,
            Self::VadState => sys::AUDIOCPP_ARTIFACT_VAD_STATE,
            Self::Custom => sys::AUDIOCPP_ARTIFACT_CUSTOM,
            Self::Other(raw) => raw as sys::audiocpp_artifact_kind,
        }
    }

    fn from_raw(raw: sys::audiocpp_artifact_kind) -> Self {
        match raw {
            sys::AUDIOCPP_ARTIFACT_SPEAKER_EMBEDDING => Self::SpeakerEmbedding,
            sys::AUDIOCPP_ARTIFACT_STYLE_EMBEDDING => Self::StyleEmbedding,
            sys::AUDIOCPP_ARTIFACT_PROMPT_EMBEDDING => Self::PromptEmbedding,
            sys::AUDIOCPP_ARTIFACT_ACOUSTIC_TOKENS => Self::AcousticTokens,
            sys::AUDIOCPP_ARTIFACT_MIDI => Self::Midi,
            sys::AUDIOCPP_ARTIFACT_TRANSCRIPT_ALIGNMENT => Self::TranscriptAlignment,
            sys::AUDIOCPP_ARTIFACT_DIARIZATION_STATE => Self::DiarizationState,
            sys::AUDIOCPP_ARTIFACT_VAD_STATE => Self::VadState,
            sys::AUDIOCPP_ARTIFACT_CUSTOM => Self::Custom,
            other => Self::Other(other as u32),
        }
    }
}

/// Borrowed interleaved float PCM.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioRef<'a> {
    /// `frames() * channels` interleaved samples.
    pub samples: &'a [f32],
    pub sample_rate: u32,
    pub channels: u32,
}

impl AudioRef<'_> {
    /// Samples per channel.
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1) as usize
    }

    pub fn duration_secs(&self) -> f64 {
        if self.sample_rate == 0 { 0.0 } else { self.frames() as f64 / self.sample_rate as f64 }
    }
}

fn invalid_output(what: &str) -> Error {
    Error::new(ErrorKind::Runtime, format!("library returned {what}"))
}

/// Builds an `AudioRef` from the C out parameters, validating the format.
///
/// # Safety
/// `samples` must be NULL with `frames == 0`, or point at `frames * channels`
/// floats living for `'a`.
unsafe fn audio_ref<'a>(
    samples: *const f32,
    frames: usize,
    sample_rate: c_int,
    channels: c_int,
) -> Result<AudioRef<'a>> {
    if sample_rate <= 0 || channels <= 0 {
        return Err(invalid_output(&format!("an invalid PCM format ({sample_rate} Hz, {channels} channels)")));
    }
    let len = frames
        .checked_mul(channels as usize)
        .filter(|&len| len <= isize::MAX as usize / size_of::<f32>())
        .ok_or_else(|| invalid_output("an oversized PCM buffer"))?;
    let samples = if len == 0 {
        &[][..]
    } else if samples.is_null() {
        return Err(invalid_output("a NULL PCM buffer"));
    } else {
        unsafe { std::slice::from_raw_parts(samples, len) }
    };
    Ok(AudioRef { samples, sample_rate: sample_rate as u32, channels: channels as u32 })
}

/// Maps NOT_AVAILABLE to `None` and any other failure to an error.
fn optional(status: sys::audiocpp_status) -> Result<bool> {
    if status == sys::AUDIOCPP_ERR_NOT_AVAILABLE {
        return Ok(false);
    }
    check(status)?;
    Ok(true)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Text {
    pub text: String,
    pub language: String,
}

/// A time span in input samples, e.g. a VAD speech segment or an ASR chunk.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub start_sample: i64,
    pub end_sample: i64,
    pub confidence: f32,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpeakerTurn {
    pub start_sample: i64,
    pub end_sample: i64,
    pub speaker_id: String,
    pub confidence: f32,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Word {
    pub word: String,
    pub start_sample: i64,
    pub end_sample: i64,
    pub confidence: f32,
}

/// One of several audio outputs (separated stems, per-speaker tracks, ...).
#[derive(Debug, Clone, PartialEq)]
pub struct NamedAudio<'a> {
    pub id: String,
    pub audio: AudioRef<'a>,
}

/// An artifact the task produced. The payload is borrowed from the result.
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactRef<'a> {
    pub kind: ArtifactKind,
    pub id: String,
    pub payload: &'a [u8],
    pub meta: Vec<(String, String)>,
}

/// The contents of a task result. Obtained by dereferencing a [`TaskResult`]
/// or from [`crate::Event::result`]; never owned directly.
///
/// Absent outputs read as `Ok(None)` or an empty `Vec`; errors are reserved
/// for malformed library output.
pub struct ResultView {
    _opaque: [u8; 0],
    _marker: PhantomData<(*mut u8, PhantomPinned)>,
}

impl ResultView {
    /// # Safety
    /// `ptr` must be a live result handle that outlives `'a`.
    pub(crate) unsafe fn from_ptr<'a>(ptr: *const sys::audiocpp_result) -> &'a Self {
        unsafe { &*ptr.cast::<Self>() }
    }

    fn raw(&self) -> *const sys::audiocpp_result {
        (self as *const Self).cast()
    }

    /// The main audio output, if the task produced any.
    pub fn audio(&self) -> Result<Option<AudioRef<'_>>> {
        let (mut samples, mut frames, mut rate, mut channels) = (ptr::null(), 0usize, 0, 0);
        // SAFETY: valid handle; the samples are borrowed for the life of &self.
        let status =
            unsafe { sys::audiocpp_result_audio(self.raw(), &mut samples, &mut frames, &mut rate, &mut channels) };
        if !optional(status)? {
            return Ok(None);
        }
        unsafe { audio_ref(samples, frames, rate, channels) }.map(Some)
    }

    /// The text output (transcript, partial text, ...), if any.
    pub fn text(&self) -> Result<Option<Text>> {
        let (mut text, mut language): (*const c_char, *const c_char) = (ptr::null(), ptr::null());
        // SAFETY: valid handle; strings copied immediately.
        let status = unsafe { sys::audiocpp_result_text(self.raw(), &mut text, &mut language) };
        if !optional(status)? {
            return Ok(None);
        }
        Ok(Some(unsafe { Text { text: owned_string(text), language: owned_string(language) } }))
    }

    pub fn segments(&self) -> Result<Vec<Segment>> {
        // SAFETY: valid handle; indices in range; strings copied.
        let count = unsafe { sys::audiocpp_result_segment_count(self.raw()) };
        (0..count)
            .map(|i| {
                let (mut start, mut end, mut confidence, mut text) = (0, 0, 0.0, ptr::null());
                check(unsafe {
                    sys::audiocpp_result_segment(self.raw(), i, &mut start, &mut end, &mut confidence, &mut text)
                })?;
                Ok(Segment { start_sample: start, end_sample: end, confidence, text: unsafe { owned_string(text) } })
            })
            .collect()
    }

    pub fn speaker_turns(&self) -> Result<Vec<SpeakerTurn>> {
        // SAFETY: as in `segments`.
        let count = unsafe { sys::audiocpp_result_speaker_turn_count(self.raw()) };
        (0..count)
            .map(|i| {
                let (mut start, mut end, mut speaker, mut confidence, mut text) = (0, 0, ptr::null(), 0.0, ptr::null());
                check(unsafe {
                    sys::audiocpp_result_speaker_turn(
                        self.raw(),
                        i,
                        &mut start,
                        &mut end,
                        &mut speaker,
                        &mut confidence,
                        &mut text,
                    )
                })?;
                Ok(SpeakerTurn {
                    start_sample: start,
                    end_sample: end,
                    speaker_id: unsafe { owned_string(speaker) },
                    confidence,
                    text: unsafe { owned_string(text) },
                })
            })
            .collect()
    }

    pub fn words(&self) -> Result<Vec<Word>> {
        // SAFETY: as in `segments`.
        let count = unsafe { sys::audiocpp_result_word_count(self.raw()) };
        (0..count)
            .map(|i| {
                let (mut word, mut start, mut end, mut confidence) = (ptr::null(), 0, 0, 0.0);
                check(unsafe {
                    sys::audiocpp_result_word(self.raw(), i, &mut word, &mut start, &mut end, &mut confidence)
                })?;
                Ok(Word { word: unsafe { owned_string(word) }, start_sample: start, end_sample: end, confidence })
            })
            .collect()
    }

    pub fn named_audio(&self) -> Result<Vec<NamedAudio<'_>>> {
        // SAFETY: valid handle; samples borrowed for the life of &self.
        let count = unsafe { sys::audiocpp_result_named_audio_count(self.raw()) };
        (0..count)
            .map(|i| {
                let (mut id, mut samples, mut frames, mut rate, mut channels) =
                    (ptr::null(), ptr::null(), 0usize, 0, 0);
                check(unsafe {
                    sys::audiocpp_result_named_audio(
                        self.raw(),
                        i,
                        &mut id,
                        &mut samples,
                        &mut frames,
                        &mut rate,
                        &mut channels,
                    )
                })?;
                Ok(NamedAudio {
                    id: unsafe { owned_string(id) },
                    audio: unsafe { audio_ref(samples, frames, rate, channels)? },
                })
            })
            .collect()
    }

    pub fn artifacts(&self) -> Result<Vec<ArtifactRef<'_>>> {
        // SAFETY: valid handle; payload borrowed for the life of &self.
        let count = unsafe { sys::audiocpp_result_artifact_count(self.raw()) };
        (0..count)
            .map(|i| {
                let (mut kind, mut id, mut payload, mut bytes) = (0, ptr::null(), ptr::null::<c_void>(), 0usize);
                check(unsafe {
                    sys::audiocpp_result_artifact(self.raw(), i, &mut kind, &mut id, &mut payload, &mut bytes)
                })?;
                let payload = if bytes == 0 {
                    &[][..]
                } else if payload.is_null() || bytes > isize::MAX as usize {
                    return Err(invalid_output("a NULL or oversized artifact payload"));
                } else {
                    unsafe { std::slice::from_raw_parts(payload.cast::<u8>(), bytes) }
                };
                Ok(ArtifactRef {
                    kind: ArtifactKind::from_raw(kind),
                    id: unsafe { owned_string(id) },
                    payload,
                    meta: self.artifact_meta(i)?,
                })
            })
            .collect()
    }

    fn artifact_meta(&self, index: usize) -> Result<Vec<(String, String)>> {
        // SAFETY: valid handle; indices in range; strings copied.
        let count = unsafe { sys::audiocpp_result_artifact_meta_count(self.raw(), index) };
        (0..count)
            .map(|j| {
                let (mut key, mut value) = (ptr::null(), ptr::null());
                check(unsafe { sys::audiocpp_result_artifact_meta(self.raw(), index, j, &mut key, &mut value) })?;
                Ok(unsafe { (owned_string(key), owned_string(value)) })
            })
            .collect()
    }
}

impl std::fmt::Debug for ResultView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = |n: Result<usize>| n.map_or_else(|e| e.to_string(), |n| n.to_string());
        f.debug_struct("ResultView")
            .field("audio", &self.audio().map(|a| a.map(|a| (a.frames(), a.sample_rate, a.channels))))
            .field("text", &self.text())
            .field("segments", &count(self.segments().map(|v| v.len())))
            .field("speaker_turns", &count(self.speaker_turns().map(|v| v.len())))
            .field("words", &count(self.words().map(|v| v.len())))
            .field("named_audio", &count(self.named_audio().map(|v| v.len())))
            .field("artifacts", &count(self.artifacts().map(|v| v.len())))
            .finish()
    }
}

handle!(
    /// An owned task result. Dereferences to [`ResultView`] for its contents.
    TaskResult,
    sys::audiocpp_result,
    sys::audiocpp_result_free
);

impl Deref for TaskResult {
    type Target = ResultView;

    fn deref(&self) -> &ResultView {
        // SAFETY: the handle lives as long as `self`.
        unsafe { ResultView::from_ptr(self.as_ptr()) }
    }
}

impl std::fmt::Debug for TaskResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        (**self).fmt(f)
    }
}
