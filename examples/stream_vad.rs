//! Streaming voice activity detection: feeds a WAV file chunk by chunk, as a
//! microphone would, and prints speech start/end events as they happen.
//!
//! cargo run --example stream_vad -- [model_dir] [input.wav]   (defaults to tests/data)

use std::path::PathBuf;

use audio_cpp::{AudioBuffer, AudioRef, BackendConfig, ModelConfig, Registry, Task, VoiceActivityKind};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let mut args = std::env::args().skip(1);
    let model = args.next().map_or_else(|| data.join("silero_vad"), PathBuf::from);
    let input = args.next().map_or_else(|| data.join("sample_16k.wav"), PathBuf::from);

    let audio = AudioBuffer::read_wav(&input)?.into_mono();
    let registry = Registry::new()?;
    let model = registry.load_model(&model, &ModelConfig::family("silero_vad"), None)?;
    let mut stream = model.streaming_session(Task::Vad, &BackendConfig::default(), None)?;
    let chunk = stream.policy()?.chunk_frames(audio.sample_rate).unwrap_or(512);

    stream.start(None)?;
    let seconds = |sample: i64| sample as f64 / audio.sample_rate as f64;
    for piece in audio.samples.chunks(chunk) {
        let mut piece = piece.to_vec();
        piece.resize(chunk, 0.0);
        let mut events: Vec<_> = stream.push(AudioRef::mono(&piece, audio.sample_rate)?)?.into_iter().collect();
        events.extend(stream.drain_events()?);
        for event in events {
            for activity in event.voice_activity()? {
                match activity.kind {
                    VoiceActivityKind::SpeechStart => println!("{:7.2}s speech starts", seconds(activity.sample)),
                    VoiceActivityKind::SpeechEnd => println!("{:7.2}s speech ends", seconds(activity.sample)),
                    _ => {}
                }
            }
        }
    }
    let result = stream.finish()?;
    println!("{} speech segments", result.segments()?.len());
    Ok(())
}
