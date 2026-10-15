//! Speech recognition.
//!
//! cargo run --release --example transcribe -- <model> <family> <input.wav> [language]

use audio_cpp::{Backend, BackendConfig, Mode, ModelConfig, Registry, Request};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [model_path, family, input, rest @ ..] = args.as_slice() else {
        eprintln!("usage: transcribe <model> <family> <input.wav> [language]");
        std::process::exit(2);
    };

    let mut reader = hound::WavReader::open(input)?;
    let spec = reader.spec();
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader.samples::<i32>().map(|s| s.map(|s| s as f32 / scale)).collect::<Result<_, _>>()?
        }
    };

    let registry = Registry::new()?;
    let model = registry.load_model(model_path, &ModelConfig::family(family.as_str()), None)?;
    let mut session = model.create_session("asr", Mode::Offline, &BackendConfig::new(Backend::Best), None)?;

    let mut request = Request::new()?;
    request.set_audio(&samples, spec.sample_rate, spec.channels as u32)?;
    if let Some(language) = rest.first() {
        request.set_option("language", language)?;
    }

    let result = session.run(&request)?;
    match result.text()? {
        Some(text) => println!("{}", text.text),
        None => eprintln!("model produced no text"),
    }
    for segment in result.segments()? {
        let to_secs = |s: i64| s as f64 / spec.sample_rate as f64;
        println!("[{:7.2} - {:7.2}] {}", to_secs(segment.start_sample), to_secs(segment.end_sample), segment.text);
    }
    Ok(())
}
