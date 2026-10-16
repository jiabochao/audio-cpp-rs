//! Round trips through real downloaded models: synthesize speech, then
//! transcribe it back. Skipped unless `AUDIOCPP_TEST_MODELS` names a directory
//! holding the packages below (fetch them with `cargo run --example catalog`):
//!
//! ```text
//! $AUDIOCPP_TEST_MODELS/qwen3-tts-0.6b-base/qwen3-tts-12hz-0.6b-base-q8_0.gguf
//! $AUDIOCPP_TEST_MODELS/moonshine-tiny/moonshine-streaming-tiny-q8_0.gguf
//! ```

use std::path::PathBuf;

use audio_cpp::{
    AudioBuffer, BackendConfig, ModelConfig, PoolConfig, Registry, Request, SessionPool, SessionSpec, Task,
};

fn models() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("AUDIOCPP_TEST_MODELS")?);
    let ready = dir.join("qwen3-tts-0.6b-base").is_dir() && dir.join("moonshine-tiny").is_dir();
    if !ready {
        eprintln!("AUDIOCPP_TEST_MODELS={} lacks the test packages; skipping", dir.display());
    }
    ready.then_some(dir)
}

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

#[test]
fn cloned_speech_transcribes_back_to_its_text() {
    let Some(models) = models() else { return };
    let text = "Hello from Rust. The quick brown fox jumps over the lazy dog.";
    let reference =
        AudioBuffer::read_wav(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/sample_16k.wav")).unwrap();

    let pool = SessionPool::new(PoolConfig { max_sessions: 2, idle_timeout: None });
    let backend = BackendConfig::default();
    let tts =
        SessionSpec::new(models.join("qwen3-tts-0.6b-base"), &ModelConfig::family("qwen3_tts"), Task::Tts, &backend);
    let asr =
        SessionSpec::new(models.join("moonshine-tiny"), &ModelConfig::family("moonshine_asr"), Task::Asr, &backend);

    let mut request = Request::with_text(text).unwrap();
    request
        .set_voice_audio(reference.as_ref())
        .unwrap()
        .set_option("x_vector_only_mode", "true")
        .unwrap()
        .set_option("seed", "7")
        .unwrap();
    let speech = pool.checkout(&tts).unwrap().run(&request).unwrap().audio().unwrap().unwrap().to_buffer();
    assert_eq!(speech.sample_rate, 24_000);
    assert!(speech.duration().as_secs_f32() > 1.0, "{:?}", speech.duration());

    let transcript = pool.checkout(&asr).unwrap().run(&Request::with_audio(speech.as_ref()).unwrap()).unwrap();
    let heard = transcript.text().unwrap().unwrap().text;
    let (expected, heard_words) = (words(text), words(&heard));
    let matched = expected.iter().filter(|w| heard_words.contains(w)).count();
    // Sampling can drop or blur a word; most of the sentence must survive.
    assert!(matched * 10 >= expected.len() * 8, "expected {text:?}, heard {heard:?}");

    // Same seed, same session: identical audio.
    let again = pool.checkout(&tts).unwrap().run(&request).unwrap().audio().unwrap().unwrap().to_buffer();
    assert_eq!(again.samples, speech.samples, "a fixed seed must reproduce the take");
}

#[test]
fn model_info_describes_tts_options() {
    let Some(models) = models() else { return };
    let registry = Registry::new().unwrap();
    let model =
        registry.load_model(models.join("qwen3-tts-0.6b-base"), &ModelConfig::family("qwen3_tts"), None).unwrap();
    let info = model.info().unwrap();
    assert!(info.offline_tasks.contains(&Task::Tts));
    assert!(info.offline_tasks.contains(&Task::VoiceCloning));
    for option in ["temperature", "seed", "reference_text", "x_vector_only_mode"] {
        assert!(info.request_options.iter().any(|o| o.name == option), "{option} missing");
    }
}
