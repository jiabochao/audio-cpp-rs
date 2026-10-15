//! End-to-end checks against Silero VAD, which ships inside the audio.cpp
//! submodule together with a 16 kHz speech sample, so no download is needed.
//! Mirrors upstream `tests/capi/path_test.c`.

use std::path::PathBuf;

use audio_cpp::{
    AbiVersion, ArtifactKind, BackendConfig, ErrorKind, Mode, ModelConfig, OptionScope, Registry, Request, Segment,
    StreamInputKind,
};

fn upstream(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("audio-cpp-sys/audio.cpp").join(path)
}

fn model_path() -> PathBuf {
    upstream("assets/framework/models/silero_vad")
}

fn load_sample() -> (Vec<f32>, u32, u32) {
    let mut reader = hound::WavReader::open(upstream("assets/resources/sample_16k.wav")).unwrap();
    let spec = reader.spec();
    assert_eq!(spec.bits_per_sample, 16);
    let samples = reader.samples::<i16>().map(|s| s.unwrap() as f32 / 32768.0).collect();
    (samples, spec.sample_rate, spec.channels as u32)
}

fn spans(segments: &[Segment]) -> Vec<(i64, i64)> {
    segments.iter().map(|s| (s.start_sample, s.end_sample)).collect()
}

#[test]
fn version_and_tasks() {
    assert!(audio_cpp::abi_version().is_compatible());
    assert_eq!(audio_cpp::abi_version().major, AbiVersion::BINDINGS.major);
    assert!(!audio_cpp::build_version().is_empty());

    let tasks = audio_cpp::tasks();
    for task in ["vad", "asr", "tts"] {
        assert!(tasks.contains(&task), "{task} missing from {tasks:?}");
    }
    assert_eq!(audio_cpp::task_from_spec_name("music"), Some("gen"));
    assert_eq!(audio_cpp::task_from_spec_name("no_such_task"), None);
}

#[test]
fn unknown_family_is_reported() {
    let registry = Registry::new().unwrap();
    let err = registry.load_model(model_path(), &ModelConfig::family("no_such_family"), None).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::UnsupportedFamily);
    assert!(!err.message().is_empty());
}

#[test]
fn interior_nul_is_rejected_before_ffi() {
    let mut request = Request::new();
    let err = request.set_option("a\0b", "c").unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidArgument);
    let err = request.set_audio(&[0.0; 3], 16_000, 2).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidArgument);
}

#[test]
fn offline_vad() {
    let (samples, rate, channels) = load_sample();
    let frames = samples.len() as i64 / channels as i64;

    let registry = Registry::new().unwrap();
    assert!(registry.families().iter().any(|f| f == "silero_vad"));

    let model = registry.load_model(model_path(), &ModelConfig::family("silero_vad"), None).unwrap();
    assert_eq!(model.family(), "silero_vad");
    assert!(model.supports("vad", Mode::Offline));
    assert!(!model.supports("tts", Mode::Offline));
    assert!(!model.supports("nonsense", Mode::Offline));
    for scope in [OptionScope::Request, OptionScope::Session, OptionScope::Load] {
        model.options(scope).unwrap();
    }

    let mut session = model.create_session("vad", Mode::Offline, &BackendConfig::default().threads(2), None).unwrap();
    assert!(!session.family().is_empty());

    // Handles keep their parents alive, so dropping these must be harmless.
    drop(model);
    drop(registry);

    let mut request = Request::new();
    request.set_audio(&samples, rate, channels).unwrap();

    let first = session.run(&request).unwrap();
    let segments = first.segments();
    assert!(!segments.is_empty(), "no speech found in a speech recording");
    let mut previous_end = -1;
    for segment in &segments {
        assert!(segment.start_sample >= 0 && segment.end_sample >= segment.start_sample);
        assert!(segment.end_sample <= frames);
        assert!(segment.start_sample >= previous_end);
        previous_end = segment.end_sample;
    }
    assert!(first.audio().is_none(), "a VAD result carries no audio");
    assert!(first.text().is_none());

    // Session reuse must be deterministic.
    let second = session.run(&request).unwrap();
    assert_eq!(spans(&segments), spans(&second.segments()));
}

#[test]
fn streaming_vad() {
    let (samples, rate, channels) = load_sample();

    let registry = Registry::new().unwrap();
    let model = registry.load_model(model_path(), &ModelConfig::family("silero_vad"), None).unwrap();
    if !model.supports("vad", Mode::Streaming) {
        eprintln!("silero_vad does not stream in this build; skipping");
        return;
    }
    let mut session = model.create_session("vad", Mode::Streaming, &BackendConfig::default().threads(2), None).unwrap();
    let policy = session.stream_policy().unwrap();
    assert_eq!(policy.input, StreamInputKind::AudioChunks);
    let chunk_frames = if policy.preferred_chunk_samples > 0 { policy.preferred_chunk_samples as usize } else { 512 };

    session.stream_start(None).unwrap();
    let mut events = 0;
    let mut activity = 0;
    for (i, chunk) in samples.chunks(chunk_frames * channels as usize).enumerate() {
        let start = (i * chunk_frames) as i64;
        // Silero wants exactly the preferred chunk size; zero-pad the tail.
        let mut chunk = chunk.to_vec();
        chunk.resize(chunk_frames * channels as usize, 0.0);
        if let Some(event) = session.stream_push(&chunk, rate, channels, start).unwrap() {
            events += 1;
            activity += event.voice_activity().len();
        }
        while let Some(event) = session.stream_next_event().unwrap() {
            events += 1;
            activity += event.voice_activity().len();
        }
    }
    let result = session.stream_finish().unwrap();
    assert!(events > 0);
    assert!(activity > 0 || !result.segments().is_empty(), "stream found no speech");

    // Offline-only calls on a streaming session and vice versa fail cleanly.
    let offline = model.create_session("vad", Mode::Offline, &BackendConfig::default(), None).unwrap();
    assert!(offline.stream_policy().is_err());
}

#[test]
fn request_setters_accept_valid_input() {
    let mut request = Request::new();
    request
        .set_text("hello", Some("en"))
        .unwrap()
        .set_text_language("en")
        .unwrap()
        .set_voice_id("voice")
        .unwrap()
        .set_style_language("en")
        .unwrap()
        .set_emotion("calm")
        .unwrap()
        .set_speaking_rate(1.0)
        .unwrap()
        .set_pitch_shift(0.0)
        .unwrap()
        .set_energy_scale(1.0)
        .unwrap()
        .set_style_tag("k", "v")
        .unwrap()
        .set_option("k", "v")
        .unwrap()
        .set_option_list("list", &["a", "b"])
        .unwrap()
        .set_option_list::<&str>("empty", &[])
        .unwrap();
    let index = request.add_artifact(ArtifactKind::SpeakerEmbedding, "spk", &[1, 2, 3]).unwrap();
    request.set_artifact_meta(index, "dim", "3").unwrap();
    assert_eq!(request.set_artifact_meta(index + 1, "k", "v").unwrap_err().kind(), ErrorKind::OutOfRange);
}
