//! Voice activity detection with the Silero VAD model bundled in audio.cpp.
//!
//! cargo run --example vad -- [model_dir] [input.wav]

use std::path::PathBuf;

use audio_cpp::{BackendConfig, Mode, ModelConfig, Registry, Request};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let upstream = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("audio-cpp-sys/audio.cpp");
    let mut args = std::env::args().skip(1);
    let model = args.next().map_or_else(|| upstream.join("assets/framework/models/silero_vad"), PathBuf::from);
    let input = args.next().map_or_else(|| upstream.join("assets/resources/sample_16k.wav"), PathBuf::from);

    let (samples, rate, channels) = read_wav(&input)?;
    let registry = Registry::new()?;
    let model = registry.load_model(&model, &ModelConfig::family("silero_vad"), None)?;
    let mut session = model.create_session("vad", Mode::Offline, &BackendConfig::default(), None)?;

    let mut request = Request::new();
    request.set_audio(&samples, rate, channels)?;
    let result = session.run(&request)?;

    for segment in result.segments() {
        println!(
            "speech {:7.2}s - {:7.2}s",
            segment.start_sample as f64 / rate as f64,
            segment.end_sample as f64 / rate as f64
        );
    }
    Ok(())
}

fn read_wav(path: &std::path::Path) -> Result<(Vec<f32>, u32, u32), hound::Error> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let samples = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader.samples::<i32>().map(|s| s.map(|s| s as f32 / scale)).collect::<Result<_, _>>()?
        }
    };
    Ok((samples, spec.sample_rate, spec.channels as u32))
}
