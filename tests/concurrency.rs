//! Regression test for concurrent session construction.
//!
//! Upstream models loaded separately can share a cached weight source, and
//! Silero session construction unmaps it after uploading. Without the
//! crate's session-creation lock, this test crashed with SIGSEGV in 39 of 40
//! runs.

use std::path::PathBuf;
use std::sync::{Arc, Barrier};

use audio_cpp::{BackendConfig, Mode, ModelConfig, Registry, Request};

const THREADS: usize = 8;
const SESSIONS_PER_THREAD: usize = 16;

#[test]
fn concurrent_session_creation_across_models() {
    let data = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let mut wav = hound::WavReader::open(data.join("sample_16k.wav")).unwrap();
    let samples: Arc<Vec<f32>> = Arc::new(wav.samples::<i16>().map(|s| s.unwrap() as f32 / 32768.0).collect());
    let barrier = Arc::new(Barrier::new(THREADS));

    let workers: Vec<_> = (0..THREADS)
        .map(|_| {
            let (data, samples, barrier) = (data.clone(), samples.clone(), barrier.clone());
            std::thread::spawn(move || {
                let registry = Registry::new().unwrap();
                let model =
                    registry.load_model(data.join("silero_vad"), &ModelConfig::family("silero_vad"), None).unwrap();
                // Hold every model loaded before any session is created, so the
                // shared weight source is live when constructions overlap.
                barrier.wait();
                for i in 0..SESSIONS_PER_THREAD {
                    let mode = if i % 2 == 0 { Mode::Offline } else { Mode::Streaming };
                    let mut session =
                        model.create_session("vad", mode, &BackendConfig::default().threads(1), None).unwrap();
                    if mode == Mode::Offline {
                        let mut request = Request::new().unwrap();
                        request.set_audio(&samples, 16_000, 1).unwrap();
                        assert!(!session.run(&request).unwrap().segments().unwrap().is_empty());
                    }
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
}
