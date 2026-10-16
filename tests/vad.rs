//! End-to-end checks against Silero VAD. The model and a 16 kHz speech sample
//! are copied from upstream into `tests/data`, so no download is needed.
//! Mirrors upstream `tests/capi/path_test.c`.

use std::path::PathBuf;

use audio_cpp::{
    AbiVersion, ArtifactKind, AudioBuffer, AudioRef, BackendConfig, ErrorKind, Mode, ModelConfig, OptionScope,
    Registry, Request, Segment, StreamInputKind, Task,
};

fn data(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data").join(path)
}

fn model_path() -> PathBuf {
    data("silero_vad")
}

fn load_sample() -> AudioBuffer {
    AudioBuffer::read_wav(data("sample_16k.wav")).unwrap()
}

fn spans(segments: &[Segment]) -> Vec<(i64, i64)> {
    segments.iter().map(|s| (s.start_sample, s.end_sample)).collect()
}

fn silero(registry: &Registry) -> audio_cpp::Model {
    registry.load_model(model_path(), &ModelConfig::family("silero_vad"), None).unwrap()
}

#[test]
fn version_and_tasks() {
    assert!(audio_cpp::abi_version().is_compatible());
    assert_eq!(audio_cpp::abi_version().major, AbiVersion::BINDINGS.major);
    assert!(!audio_cpp::build_version().is_empty());

    let tasks = Task::all();
    for task in [Task::Vad, Task::Asr, Task::Tts] {
        assert!(tasks.contains(&task), "{task} missing from {tasks:?}");
    }
    assert_eq!(Task::from_spec_name("music"), Some(Task::Generation));
    assert_eq!(Task::from_spec_name("no_such_task"), None);
}

#[test]
fn unknown_family_is_reported() {
    let registry = Registry::new().unwrap();
    let err = registry.load_model(model_path(), &ModelConfig::family("no_such_family"), None).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::UnsupportedFamily);
    assert!(!err.message().is_empty());
}

#[test]
fn invalid_inputs_are_rejected_before_ffi() {
    let mut request = Request::new().unwrap();
    assert_eq!(request.set_option("a\0b", "c").unwrap_err().kind(), ErrorKind::InvalidArgument);
    for (rate, channels, len) in [(16_000, 2, 3), (0, 1, 4), (16_000, 0, 4), (u32::MAX, 1, 4)] {
        let samples = vec![0.0; len];
        let audio = AudioRef { samples: &samples, sample_rate: rate, channels };
        assert_eq!(request.set_audio(audio).unwrap_err().kind(), ErrorKind::InvalidArgument, "{rate}/{channels}/{len}");
    }
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert_eq!(request.set_speaking_rate(value).unwrap_err().kind(), ErrorKind::InvalidArgument);
        assert_eq!(request.set_pitch_shift(value).unwrap_err().kind(), ErrorKind::InvalidArgument);
        assert_eq!(request.set_energy_scale(value).unwrap_err().kind(), ErrorKind::InvalidArgument);
    }

    let registry = Registry::new().unwrap();
    let model = silero(&registry);
    let err = model.offline_session(Task::Vad, &BackendConfig::default().threads(0), None).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidArgument);

    let mut stream = model.streaming_session(Task::Vad, &BackendConfig::default(), None).unwrap();
    let chunk = [0.0f32; 512];
    let audio = AudioRef::mono(&chunk, 16_000).unwrap();
    // Out-of-order calls are refused before they reach the library.
    assert_eq!(stream.push(audio).unwrap_err().kind(), ErrorKind::InvalidArgument);
    assert_eq!(stream.finish().unwrap_err().kind(), ErrorKind::InvalidArgument);
    stream.start(None).unwrap();
    for offset in [-1, i64::MAX] {
        assert_eq!(stream.push_at(audio, offset).unwrap_err().kind(), ErrorKind::InvalidArgument, "offset {offset}");
    }
}

#[test]
fn offline_vad() {
    let sample = load_sample();
    let frames = sample.frames() as i64;

    let registry = Registry::new().unwrap();
    assert!(registry.families().unwrap().iter().any(|f| f == "silero_vad"));

    let model = silero(&registry);
    assert_eq!(model.family(), "silero_vad");
    assert!(model.supports(&Task::Vad, Mode::Offline));
    assert!(!model.supports(&Task::Tts, Mode::Offline));
    assert!(!model.supports(&Task::Other("nonsense".into()), Mode::Offline));
    let info = model.info().unwrap();
    assert_eq!(info.family, "silero_vad");
    assert!(info.offline_tasks.contains(&Task::Vad));
    for scope in [OptionScope::Request, OptionScope::Session, OptionScope::Load] {
        model.options(scope).unwrap();
    }

    let mut session = model.offline_session(Task::Vad, &BackendConfig::default().threads(2), None).unwrap();
    assert!(!session.family().is_empty());

    // Handles keep their parents alive, so dropping these must be harmless.
    drop(model);
    drop(registry);

    let request = Request::with_audio(sample.as_ref()).unwrap();
    let first = session.run(&request).unwrap();
    let segments = first.segments().unwrap();
    assert!(!segments.is_empty(), "no speech found in a speech recording");
    let mut previous_end = -1;
    for segment in &segments {
        assert!(segment.start_sample >= 0 && segment.end_sample >= segment.start_sample);
        assert!(segment.end_sample <= frames);
        assert!(segment.start_sample >= previous_end);
        previous_end = segment.end_sample;
    }
    assert!(first.audio().unwrap().is_none(), "a VAD result carries no audio");
    assert!(first.text().unwrap().is_none());

    // Session reuse must be deterministic, and results outlive the session.
    let second = session.run(&request).unwrap();
    drop(session);
    assert_eq!(spans(&segments), spans(&second.segments().unwrap()));
}

#[test]
fn streaming_vad() {
    let sample = load_sample();

    let registry = Registry::new().unwrap();
    let model = silero(&registry);
    assert!(model.supports(&Task::Vad, Mode::Streaming));
    let mut session = model.streaming_session(Task::Vad, &BackendConfig::default().threads(2), None).unwrap();
    let policy = session.policy().unwrap();
    assert_eq!(policy.input, StreamInputKind::AudioChunks);
    let chunk_frames = policy.chunk_frames(sample.sample_rate).unwrap_or(512);

    let run = |session: &mut audio_cpp::StreamingSession| {
        session.start(None).unwrap();
        let mut events = Vec::new();
        for chunk in sample.samples.chunks(chunk_frames) {
            // Silero wants exactly the preferred chunk size; zero-pad the tail.
            let mut chunk = chunk.to_vec();
            chunk.resize(chunk_frames, 0.0);
            events.extend(session.push(AudioRef::mono(&chunk, sample.sample_rate).unwrap()).unwrap());
            events.extend(session.drain_events().unwrap());
        }
        let result = session.finish().unwrap();
        assert!(!session.is_active());
        (events, result)
    };

    let (events, result) = run(&mut session);
    let activity: usize = events.iter().map(|e| e.voice_activity().unwrap().len()).sum();
    assert!(!events.is_empty());
    assert!(activity > 0 || !result.segments().unwrap().is_empty(), "stream found no speech");

    // Events own their data and outlive both the stream and the session.
    let (again, _) = run(&mut session);
    drop(session);
    assert_eq!(events.len(), again.len());
}

#[test]
fn request_setters_accept_valid_input() {
    let mut request = Request::new().unwrap();
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
