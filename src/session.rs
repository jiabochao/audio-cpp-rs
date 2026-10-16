use std::ops::Deref;
use std::ptr::{self, NonNull};

use crate::audio::AudioRef;
use crate::error::{Error, Result, check};
use crate::ffi::{create, handle, owned_string};
use crate::request::Request;
use crate::result::{ResultView, TaskResult};
use crate::sys;

handle!(
    /// A reusable offline session, created by [`crate::Model::offline_session`].
    ///
    /// Creating a session uploads the model's weights to the compute device,
    /// so keep sessions and run many requests through each; see
    /// [`crate::SessionPool`].
    pub OfflineSession,
    sys::audiocpp_session,
    sys::audiocpp_session_free
);

impl OfflineSession {
    pub fn family(&self) -> String {
        // SAFETY: valid handle; string copied.
        unsafe { owned_string(sys::audiocpp_session_family(self.as_ptr())) }
    }

    /// Forces allocation early with a representative request. Optional:
    /// [`Self::run`] always prepares with the request it runs.
    pub fn prepare(&mut self, request: &Request) -> Result<()> {
        // SAFETY: valid handles.
        check(unsafe { sys::audiocpp_session_prepare(self.as_ptr(), request.as_ptr()) })
    }

    /// Runs one request to completion. Blocks; run it off any async executor.
    pub fn run(&mut self, request: &Request) -> Result<TaskResult> {
        // SAFETY: valid handles; the result handle is owned by the caller.
        create(
            |out| unsafe { sys::audiocpp_session_run(self.as_ptr(), request.as_ptr(), out) },
            |p| unsafe { TaskResult::from_raw(p) },
        )
    }
}

/// How a streaming session takes input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamInputKind {
    /// The request alone drives the stream (TTS, generation).
    None,
    /// Audio is pushed chunk by chunk ([`StreamingSession::push`]).
    AudioChunks,
    Other(u32),
}

/// How a streaming session delivers output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamOutputKind {
    /// Only [`StreamingSession::finish`] yields output.
    FinalResult,
    /// Events arrive from [`StreamingSession::push`] and
    /// [`StreamingSession::next_event`].
    PullEvents,
    Other(u32),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StreamPolicy {
    pub input: StreamInputKind,
    pub output: StreamOutputKind,
    /// The chunk size the family wants, in frames; push exactly this many
    /// where possible. Zero or negative when the family states no preference.
    pub preferred_chunk_samples: i64,
    pub preferred_chunk_seconds: f64,
}

impl StreamPolicy {
    /// The preferred chunk size in frames at `sample_rate`, if the family
    /// states one.
    pub fn chunk_frames(&self, sample_rate: u32) -> Option<usize> {
        if self.preferred_chunk_samples > 0 {
            return usize::try_from(self.preferred_chunk_samples).ok();
        }
        (self.preferred_chunk_seconds > 0.0)
            .then(|| (self.preferred_chunk_seconds * sample_rate as f64).round() as usize)
    }
}

handle!(
    /// A pull-based streaming session, created by
    /// [`crate::Model::streaming_session`].
    ///
    /// Call [`Self::start`], then [`Self::push`] audio (for audio-input
    /// families) and drain [`Self::next_event`], then [`Self::finish`]. Calls
    /// out of that order fail with [`crate::ErrorKind::InvalidArgument`]
    /// instead of reaching the library in an undefined state.
    pub(crate) StreamingSessionHandle,
    sys::audiocpp_session,
    sys::audiocpp_session_free
);

pub struct StreamingSession {
    handle: StreamingSessionHandle,
    active: bool,
    /// Frames pushed since `start`, the default offset for the next push.
    position: i64,
}

impl StreamingSession {
    /// # Safety
    /// `ptr` must be a live streaming session whose ownership is transferred.
    pub(crate) unsafe fn from_raw(ptr: NonNull<sys::audiocpp_session>) -> Self {
        Self { handle: unsafe { StreamingSessionHandle::from_raw(ptr) }, active: false, position: 0 }
    }

    fn ptr(&self) -> *mut sys::audiocpp_session {
        self.handle.as_ptr()
    }

    /// The underlying C handle, still owned by `self`.
    pub fn as_ptr(&self) -> *mut sys::audiocpp_session {
        self.ptr()
    }

    pub fn family(&self) -> String {
        // SAFETY: valid handle; string copied.
        unsafe { owned_string(sys::audiocpp_session_family(self.ptr())) }
    }

    pub fn policy(&self) -> Result<StreamPolicy> {
        let (mut input, mut output, mut samples, mut seconds) = (0, 0, 0i64, 0f64);
        // SAFETY: valid handle and out pointers.
        check(unsafe { sys::audiocpp_stream_policy(self.ptr(), &mut input, &mut output, &mut samples, &mut seconds) })?;
        Ok(StreamPolicy {
            input: match input {
                sys::AUDIOCPP_STREAM_INPUT_NONE => StreamInputKind::None,
                sys::AUDIOCPP_STREAM_INPUT_AUDIO_CHUNKS => StreamInputKind::AudioChunks,
                other => StreamInputKind::Other(other as u32),
            },
            output: match output {
                sys::AUDIOCPP_STREAM_OUTPUT_FINAL_RESULT => StreamOutputKind::FinalResult,
                sys::AUDIOCPP_STREAM_OUTPUT_PULL_EVENTS => StreamOutputKind::PullEvents,
                other => StreamOutputKind::Other(other as u32),
            },
            preferred_chunk_samples: samples,
            preferred_chunk_seconds: seconds,
        })
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Starts a stream, discarding any stream already in flight.
    ///
    /// For text-driven families (TTS) the library may generate the whole
    /// utterance inside this call; see the crate docs on streaming.
    pub fn start(&mut self, request: Option<&Request>) -> Result<()> {
        self.active = false;
        self.position = 0;
        let request = request.map_or(ptr::null(), |r| r.as_ptr().cast_const());
        // SAFETY: valid handle; the request is only read during the call.
        check(unsafe { sys::audiocpp_stream_start(self.ptr(), request) })?;
        self.active = true;
        Ok(())
    }

    fn require_active(&self) -> Result<()> {
        if self.active { Ok(()) } else { Err(Error::invalid_argument("no stream is active; call start first")) }
    }

    /// Feeds the next chunk, placed right after everything pushed so far, and
    /// returns the event it produced.
    pub fn push(&mut self, audio: AudioRef<'_>) -> Result<Option<Event>> {
        self.push_at(audio, self.position)
    }

    /// Feeds a chunk starting at `start_sample` frames into the stream.
    pub fn push_at(&mut self, audio: AudioRef<'_>, start_sample: i64) -> Result<Option<Event>> {
        self.require_active()?;
        let (frames, rate, channels) = audio.ffi_layout()?;
        let end = i64::try_from(frames).ok().and_then(|n| start_sample.checked_add(n));
        let end = match end {
            Some(end) if start_sample >= 0 => end,
            _ => return Err(Error::invalid_argument(format!("invalid stream offset {start_sample}"))),
        };
        let mut event: *mut sys::audiocpp_event = ptr::null_mut();
        // SAFETY: the slice covers frames * channels floats and is copied.
        check(unsafe {
            sys::audiocpp_stream_push(
                self.ptr(),
                audio.samples.as_ptr(),
                frames,
                rate,
                channels,
                start_sample,
                &mut event,
            )
        })?;
        self.position = self.position.max(end);
        Ok(NonNull::new(event).map(|p| unsafe { Event::from_raw(p) }))
    }

    /// Takes one event the family queued; `None` when the queue is empty,
    /// which does not mean the stream has ended.
    pub fn next_event(&mut self) -> Result<Option<Event>> {
        self.require_active()?;
        let mut event: *mut sys::audiocpp_event = ptr::null_mut();
        // SAFETY: valid handle; the event, if any, is owned by the caller.
        check(unsafe { sys::audiocpp_stream_next_event(self.ptr(), &mut event) })?;
        Ok(NonNull::new(event).map(|p| unsafe { Event::from_raw(p) }))
    }

    /// Takes every event queued right now.
    pub fn drain_events(&mut self) -> Result<Vec<Event>> {
        let mut events = Vec::new();
        while let Some(event) = self.next_event()? {
            events.push(event);
        }
        Ok(events)
    }

    /// Ends the stream and returns the final result. The stream is over even
    /// if this fails; call [`Self::start`] again to reuse the session.
    pub fn finish(&mut self) -> Result<TaskResult> {
        self.require_active()?;
        self.active = false;
        // SAFETY: valid handle.
        create(|out| unsafe { sys::audiocpp_stream_finish(self.ptr(), out) }, |p| unsafe { TaskResult::from_raw(p) })
    }

    /// Discards the stream in flight, if any.
    pub fn reset(&mut self) -> Result<()> {
        self.active = false;
        self.position = 0;
        // SAFETY: valid handle.
        check(unsafe { sys::audiocpp_stream_reset(self.ptr()) })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VoiceActivityKind {
    SpeechStart,
    SpeechEnd,
    SpeechSegment,
    Other(u32),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VoiceActivity {
    pub kind: VoiceActivityKind,
    /// Frame offset in the stream.
    pub sample: i64,
    pub probability: f32,
}

handle!(
    /// A streaming event. Owns its data, so it outlives the session. Its
    /// outputs (partial text, audio chunks, segments) are read through
    /// [`ResultView`], which it dereferences to.
    pub Event,
    sys::audiocpp_event,
    sys::audiocpp_event_free
);

impl Event {
    pub fn is_final(&self) -> bool {
        // SAFETY: valid handle.
        unsafe { sys::audiocpp_event_is_final(self.as_ptr()) != 0 }
    }

    pub fn result(&self) -> &ResultView {
        // SAFETY: the view is owned by the event and lives as long as `self`.
        unsafe { ResultView::from_ptr(sys::audiocpp_event_as_result(self.as_ptr())) }
    }

    pub fn voice_activity(&self) -> Result<Vec<VoiceActivity>> {
        // SAFETY: valid handle; indices in range.
        let count = unsafe { sys::audiocpp_event_voice_activity_count(self.as_ptr()) };
        (0..count)
            .map(|i| {
                let (mut kind, mut sample, mut probability) = (0, 0i64, 0f32);
                check(unsafe {
                    sys::audiocpp_event_voice_activity(self.as_ptr(), i, &mut kind, &mut sample, &mut probability)
                })?;
                Ok(VoiceActivity {
                    kind: match kind {
                        sys::AUDIOCPP_VOICE_ACTIVITY_SPEECH_START => VoiceActivityKind::SpeechStart,
                        sys::AUDIOCPP_VOICE_ACTIVITY_SPEECH_END => VoiceActivityKind::SpeechEnd,
                        sys::AUDIOCPP_VOICE_ACTIVITY_SPEECH_SEGMENT => VoiceActivityKind::SpeechSegment,
                        other => VoiceActivityKind::Other(other as u32),
                    },
                    sample,
                    probability,
                })
            })
            .collect()
    }
}

impl Deref for Event {
    type Target = ResultView;

    fn deref(&self) -> &ResultView {
        self.result()
    }
}

impl std::fmt::Debug for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Event")
            .field("is_final", &self.is_final())
            .field("voice_activity", &self.voice_activity())
            .field("result", self.result())
            .finish()
    }
}

impl std::fmt::Debug for OfflineSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OfflineSession").field("family", &self.family()).finish()
    }
}

impl std::fmt::Debug for StreamingSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamingSession")
            .field("family", &self.family())
            .field("active", &self.active)
            .field("position", &self.position)
            .finish()
    }
}
