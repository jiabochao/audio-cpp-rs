//! Text to speech.
//!
//! cargo run --release --example tts -- <model> <family> "<text>" [out.wav] [key=value ...]
//!
//! e.g. cargo run --release --example tts -- models/kokoro-82m-q8_0.gguf kokoro_tts \
//!          "Hello from Rust." hello.wav voice-id=af_heart language=en-us
//!
//! Trailing key=value pairs become request options; run the `inspect` example
//! to see what a model accepts.

use audio_cpp::{Backend, BackendConfig, Mode, ModelConfig, Registry, Request};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [model_path, family, text, rest @ ..] = args.as_slice() else {
        eprintln!("usage: tts <model> <family> <text> [out.wav] [key=value ...]");
        std::process::exit(2);
    };
    let (out, options) = match rest.split_first() {
        Some((first, options)) if !first.contains('=') => (first.as_str(), options),
        _ => ("out.wav", rest),
    };

    let registry = Registry::new()?;
    let model = registry.load_model(model_path, &ModelConfig::family(family.as_str()), None)?;
    let mut session = model.create_session("tts", Mode::Offline, &BackendConfig::new(Backend::Best), None)?;

    let mut request = Request::new()?;
    request.set_text(text, None)?;
    for option in options {
        let (key, value) = option.split_once('=').ok_or("options must be key=value")?;
        request.set_option(key, value)?;
    }

    let started = std::time::Instant::now();
    let result = session.run(&request)?;
    let audio = result.audio()?.ok_or("model produced no audio")?;
    println!(
        "{:.2}s of audio at {} Hz in {:.2}s",
        audio.duration_secs(),
        audio.sample_rate,
        started.elapsed().as_secs_f64()
    );

    let spec = hound::WavSpec {
        channels: audio.channels as u16,
        sample_rate: audio.sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(out, spec)?;
    for &sample in audio.samples {
        writer.write_sample(sample)?;
    }
    writer.finalize()?;
    println!("wrote {out}");
    Ok(())
}
