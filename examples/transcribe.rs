//! Speech recognition with any ASR family.
//!
//! cargo run --release --example transcribe -- <model> <family> <input.wav> [language]

use audio_cpp::{AudioBuffer, Backend, BackendConfig, ModelConfig, Registry, Request, Task};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [model_path, family, input, rest @ ..] = args.as_slice() else {
        eprintln!("usage: transcribe <model> <family> <input.wav> [language]");
        std::process::exit(2);
    };

    let audio = AudioBuffer::read_wav(input)?;
    let registry = Registry::new()?;
    let model = registry.load_model(model_path, &ModelConfig::family(family.as_str()), None)?;
    let mut session = model.offline_session(Task::Asr, &BackendConfig::new(Backend::Best), None)?;

    let mut request = Request::with_audio(audio.as_ref())?;
    if let Some(language) = rest.first() {
        request.set_option("language", language)?;
    }

    let result = session.run(&request)?;
    match result.text()? {
        Some(text) => println!("{}", text.text),
        None => eprintln!("model produced no text"),
    }
    let seconds = |sample: i64| sample as f64 / audio.sample_rate as f64;
    for segment in result.segments()? {
        println!("[{:7.2} - {:7.2}] {}", seconds(segment.start_sample), seconds(segment.end_sample), segment.text);
    }
    Ok(())
}
