use std::ops::Deref;
use std::ptr::{self, NonNull};

use crate::error::{Result, check};
use crate::request::{Request, frames, positive_i32};
use crate::result::{ResultView, TaskResult};
use crate::{create, handle, owned_string, sys};

handle!(
    /// A task session created by [`crate::Model::create_session`]. Reusing one
    /// session across requests keeps graphs and caches warm.
    Session,
    sys::audiocpp_session,
    sys::audiocpp_session_free
);

/// How a streaming session takes input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamInputKind {
    None,
    AudioChunks,
}

/// How a streaming session delivers output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamOutputKind {
    /// Only [`Session::stream_finish`] yields output.
    FinalResult,
    /// Events arrive from [`Session::stream_push`] / [`Session::stream_next_event`].
    PullEvents,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StreamPolicy {
    pub input: StreamInputKind,
    pub output: StreamOutputKind,
    /// The chunk size the family wants; push this many frames per call where
    /// possible. Zero or negative when the family states no preference.
    pub preferred_chunk_samples: i64,
    pub preferred_chunk_seconds: f64,
}

impl Session {
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

    /// Runs one offline request.
    pub fn run(&mut self, request: &Request) -> Result<TaskResult> {
        // SAFETY: valid handles; the result handle is owned by the caller.
        create(
            |out| unsafe { sys::audiocpp_session_run(self.as_ptr(), request.as_ptr(), out) },
            |p| unsafe { TaskResult::from_raw(p) },
        )
    }

    /// Streaming policy. Requires a session created with [`crate::Mode::Streaming`].
    pub fn stream_policy(&self) -> Result<StreamPolicy> {
        let (mut input, mut output, mut samples, mut seconds) = (0, 0, 0i64, 0f64);
        // SAFETY: valid handle and out pointers.
        check(unsafe {
            sys::audiocpp_stream_policy(self.as_ptr(), &mut input, &mut output, &mut samples, &mut seconds)
        })?;
        Ok(StreamPolicy {
            input: if input == sys::AUDIOCPP_STREAM_INPUT_AUDIO_CHUNKS {
                StreamInputKind::AudioChunks
            } else {
                StreamInputKind::None
            },
            output: if output == sys::AUDIOCPP_STREAM_OUTPUT_PULL_EVENTS {
                StreamOutputKind::PullEvents
            } else {
                StreamOutputKind::FinalResult
            },
            preferred_chunk_samples: samples,
            preferred_chunk_seconds: seconds,
        })
    }

    /// Starts (or restarts) a stream. Any stream already in flight is reset.
    pub fn stream_start(&mut self, request: Option<&Request>) -> Result<()> {
        let request = request.map_or(ptr::null(), |r| r.as_ptr().cast_const());
        // SAFETY: valid handle; the request is only read during the call.
        check(unsafe { sys::audiocpp_stream_start(self.as_ptr(), request) })
    }

    /// Feeds one chunk of interleaved audio starting at `start_sample` (in
    /// frames since the stream began) and returns the event it produced.
    pub fn stream_push(
        &mut self,
        samples: &[f32],
        sample_rate: u32,
        channels: u32,
        start_sample: i64,
    ) -> Result<Option<Event>> {
        let frames = frames(samples, channels)?;
        let (rate, ch) = (positive_i32(sample_rate, "sample_rate")?, positive_i32(channels, "channels")?);
        let mut event: *mut sys::audiocpp_event = ptr::null_mut();
        // SAFETY: the slice covers frames * channels floats and is copied.
        check(unsafe {
            sys::audiocpp_stream_push(self.as_ptr(), samples.as_ptr(), frames, rate, ch, start_sample, &mut event)
        })?;
        Ok(NonNull::new(event).map(|p| unsafe { Event::from_raw(p) }))
    }

    /// Drains one event the family queued itself; `None` when the queue is empty.
    pub fn stream_next_event(&mut self) -> Result<Option<Event>> {
        let mut event: *mut sys::audiocpp_event = ptr::null_mut();
        // SAFETY: valid handle; the event, if any, is owned by the caller.
        check(unsafe { sys::audiocpp_stream_next_event(self.as_ptr(), &mut event) })?;
        Ok(NonNull::new(event).map(|p| unsafe { Event::from_raw(p) }))
    }

    /// Ends the stream and returns the final result.
    pub fn stream_finish(&mut self) -> Result<TaskResult> {
        // SAFETY: valid handle.
        create(|out| unsafe { sys::audiocpp_stream_finish(self.as_ptr(), out) }, |p| unsafe { TaskResult::from_raw(p) })
    }

    /// Discards the stream in flight.
    pub fn stream_reset(&mut self) -> Result<()> {
        // SAFETY: valid handle.
        check(unsafe { sys::audiocpp_stream_reset(self.as_ptr()) })
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
    pub sample: i64,
    pub probability: f32,
}

handle!(
    /// A streaming event. Its outputs (partial text, audio chunks, segments)
    /// are read through [`ResultView`], which it dereferences to.
    Event,
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

    pub fn voice_activity(&self) -> Vec<VoiceActivity> {
        // SAFETY: valid handle; indices in range.
        let count = unsafe { sys::audiocpp_event_voice_activity_count(self.as_ptr()) };
        (0..count)
            .filter_map(|i| {
                let (mut kind, mut sample, mut probability) = (0, 0i64, 0f32);
                let status = unsafe {
                    sys::audiocpp_event_voice_activity(self.as_ptr(), i, &mut kind, &mut sample, &mut probability)
                };
                (status == sys::AUDIOCPP_OK).then_some(VoiceActivity {
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

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session").field("family", &self.family()).finish()
    }
}
