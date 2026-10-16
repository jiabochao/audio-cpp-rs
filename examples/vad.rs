//! Voice activity detection with Silero VAD.
//!
//! cargo run --example vad -- [model_dir] [input.wav]   (defaults to tests/data)

use std::path::PathBuf;

use audio_cpp::{AudioBuffer, BackendConfig, ModelConfig, Registry, Request, Task};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let mut args = std::env::args().skip(1);
    let model = args.next().map_or_else(|| data.join("silero_vad"), PathBuf::from);
    let input = args.next().map_or_else(|| data.join("sample_16k.wav"), PathBuf::from);

    let audio = AudioBuffer::read_wav(&input)?;
    let registry = Registry::new()?;
    let model = registry.load_model(&model, &ModelConfig::family("silero_vad"), None)?;
    let mut session = model.offline_session(Task::Vad, &BackendConfig::default(), None)?;

    let result = session.run(&Request::with_audio(audio.as_ref())?)?;
    let seconds = |sample: i64| sample as f64 / audio.sample_rate as f64;
    for segment in result.segments()? {
        println!("speech {:7.2}s - {:7.2}s", seconds(segment.start_sample), seconds(segment.end_sample));
    }
    Ok(())
}
