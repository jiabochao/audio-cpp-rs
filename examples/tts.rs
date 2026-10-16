//! Text to speech with any TTS family.
//!
//! cargo run --release --example tts -- <model> <family> "<text>" [out.wav] [key=value ...]
//!
//! Trailing key=value pairs become request options, except `voice=<wav>` and
//! `voice_text=<transcript>`, which set the reference voice for cloning:
//!
//!   cargo run --release --example tts -- models/Qwen3-TTS-12Hz-0.6B-Base-GGUF qwen3_tts \
//!       "Hello from Rust." out.wav voice=ref.wav voice_text="Transcript of ref.wav" seed=7
//!
//! Run the `inspect` example to see which options a model accepts.

use audio_cpp::{AudioBuffer, Backend, BackendConfig, ModelConfig, Registry, Request, Task};

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
    let mut session = model.offline_session(Task::Tts, &BackendConfig::new(Backend::Best), None)?;

    let mut request = Request::with_text(text)?;
    let mut voice = None;
    for option in options {
        let (key, value) = option.split_once('=').ok_or("options must be key=value")?;
        match key {
            "voice" => voice = Some(AudioBuffer::read_wav(value)?),
            "voice_text" => {
                request.set_option("reference_text", value)?;
            }
            _ => {
                request.set_option(key, value)?;
            }
        }
    }
    if let Some(voice) = &voice {
        request.set_voice_audio(voice.as_ref())?;
    }

    let started = std::time::Instant::now();
    let result = session.run(&request)?;
    let audio = result.audio()?.ok_or("model produced no audio")?;
    println!(
        "{:.2}s of audio at {} Hz in {:.2}s",
        audio.duration().as_secs_f64(),
        audio.sample_rate,
        started.elapsed().as_secs_f64()
    );
    audio.to_buffer().write_wav(out)?;
    println!("wrote {out}");
    Ok(())
}
