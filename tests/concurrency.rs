//! Concurrency: session creation across models, and the session pool.
//!
//! Upstream models loaded separately can share a cached weight source, and
//! Silero session construction unmaps it after uploading. Without the
//! crate's session-creation lock, the first test crashed with SIGSEGV in 39
//! of 40 runs.

use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use std::time::Duration;

use audio_cpp::{
    AudioBuffer, BackendConfig, CancelToken, ErrorKind, ModelConfig, PoolConfig, Registry, Request, SessionPool,
    SessionSpec, Task,
};

const THREADS: usize = 8;
const SESSIONS_PER_THREAD: usize = 16;

fn data(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data").join(path)
}

fn sample() -> Arc<AudioBuffer> {
    Arc::new(AudioBuffer::read_wav(data("sample_16k.wav")).unwrap())
}

#[test]
fn concurrent_session_creation_across_models() {
    let samples = sample();
    let barrier = Arc::new(Barrier::new(THREADS));

    let workers: Vec<_> = (0..THREADS)
        .map(|_| {
            let (samples, barrier) = (samples.clone(), barrier.clone());
            std::thread::spawn(move || {
                let registry = Registry::new().unwrap();
                let model = registry.load_model(data("silero_vad"), &ModelConfig::family("silero_vad"), None).unwrap();
                // Hold every model loaded before any session is created, so the
                // shared weight source is live when constructions overlap.
                barrier.wait();
                let backend = BackendConfig::default().threads(1);
                for i in 0..SESSIONS_PER_THREAD {
                    if i % 2 == 0 {
                        let mut session = model.offline_session(Task::Vad, &backend, None).unwrap();
                        let request = Request::with_audio(samples.as_ref().as_ref()).unwrap();
                        assert!(!session.run(&request).unwrap().segments().unwrap().is_empty());
                    } else {
                        model.streaming_session(Task::Vad, &backend, None).unwrap();
                    }
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
}

fn spec(threads: u32) -> SessionSpec {
    // Thread count is part of the spec, so it makes distinct but cheap specs.
    SessionSpec::new(
        data("silero_vad"),
        &ModelConfig::family("silero_vad"),
        Task::Vad,
        &BackendConfig::default().threads(threads),
    )
}

#[test]
fn pool_reuses_sessions_and_bounds_residency() {
    let pool = SessionPool::new(PoolConfig { max_sessions: 2, idle_timeout: None });
    let audio = sample();
    let request = Request::with_audio(audio.as_ref().as_ref()).unwrap();

    let first = pool.checkout(&spec(1)).unwrap();
    let first_ptr = first.as_ptr();
    drop(first);
    let mut again = pool.checkout(&spec(1)).unwrap();
    assert_eq!(again.as_ptr(), first_ptr, "an idle session with the same spec is reused");
    assert!(!again.run(&request).unwrap().segments().unwrap().is_empty());

    // A second spec fits; a third must evict the least recently used idle one.
    let other = pool.checkout(&spec(2)).unwrap();
    drop(again);
    drop(other);
    assert_eq!(pool.stats().idle.len(), 2);
    let third = pool.checkout(&spec(3)).unwrap();
    let stats = pool.stats();
    assert_eq!(stats.idle.len() + stats.in_use.len(), 2);
    assert!(!stats.idle.contains(&spec(1)), "spec(1) was least recently used");
    drop(third);

    pool.evict_idle();
    assert!(pool.stats().idle.is_empty());
}

#[test]
fn pool_waits_times_out_and_cancels_when_full() {
    let pool = SessionPool::new(PoolConfig { max_sessions: 1, idle_timeout: None });
    let held = pool.checkout(&spec(1)).unwrap();

    let err = pool.checkout_timeout(&spec(2), Duration::from_millis(100)).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Timeout);

    let cancel = CancelToken::new();
    let waiter = {
        let (pool, cancel) = (pool.clone(), cancel.clone());
        std::thread::spawn(move || pool.checkout_cancellable(&spec(2), &cancel).map(|_| ()))
    };
    std::thread::sleep(Duration::from_millis(100));
    cancel.cancel();
    assert_eq!(waiter.join().unwrap().unwrap_err().kind(), ErrorKind::Cancelled);

    // Returning the held session wakes a blocked checkout, which evicts it.
    let waiter = {
        let pool = pool.clone();
        std::thread::spawn(move || pool.checkout(&spec(2)).map(|s| s.spec().clone()))
    };
    std::thread::sleep(Duration::from_millis(100));
    drop(held);
    assert_eq!(waiter.join().unwrap().unwrap(), spec(2));
}

#[test]
fn pool_runs_sessions_in_parallel_threads() {
    let pool = SessionPool::new(PoolConfig { max_sessions: 4, idle_timeout: None });
    let audio = sample();
    let workers: Vec<_> = (0..8)
        .map(|i| {
            let (pool, audio) = (pool.clone(), audio.clone());
            std::thread::spawn(move || {
                let mut session = pool.checkout(&spec(1 + i % 2)).unwrap();
                let request = Request::with_audio(audio.as_ref().as_ref()).unwrap();
                session.run(&request).unwrap().segments().unwrap().len()
            })
        })
        .collect();
    let counts: Vec<usize> = workers.into_iter().map(|w| w.join().unwrap()).collect();
    assert!(counts.iter().all(|&c| c == counts[0] && c > 0), "{counts:?}");
    assert!(pool.stats().idle.len() <= 4);
}

#[test]
fn pool_shrinks_when_the_limit_drops_and_expires_idle_sessions() {
    let pool = SessionPool::new(PoolConfig { max_sessions: 3, idle_timeout: None });
    for t in 1..=3 {
        drop(pool.checkout(&spec(t)).unwrap());
    }
    assert_eq!(pool.stats().idle.len(), 3);
    pool.set_config(PoolConfig { max_sessions: 1, idle_timeout: Some(Duration::from_millis(50)) });
    assert_eq!(pool.stats().idle.len(), 1);
    std::thread::sleep(Duration::from_millis(80));
    // Expiry is applied on the next checkout.
    drop(pool.checkout(&spec(9)).unwrap());
    assert_eq!(pool.stats().idle, vec![spec(9)]);
}

#[test]
fn pool_shutdown_frees_sessions_and_refuses_checkouts() {
    let pool = SessionPool::new(PoolConfig { max_sessions: 2, idle_timeout: None });
    drop(pool.checkout(&spec(1)).unwrap());
    let busy = pool.checkout(&spec(2)).unwrap();

    // A session still in use holds the shutdown back until it returns.
    assert!(!pool.shutdown(Duration::from_millis(50)));
    assert!(pool.is_closed());
    assert!(pool.stats().idle.is_empty());
    let waiter = std::thread::spawn({
        let pool = pool.clone();
        move || pool.shutdown(Duration::from_secs(10))
    });
    std::thread::sleep(Duration::from_millis(50));
    drop(busy);
    assert!(waiter.join().unwrap());

    let stats = pool.stats();
    assert!(stats.idle.is_empty() && stats.in_use.is_empty() && stats.creating == 0, "{stats:?}");
    assert_eq!(pool.checkout(&spec(1)).unwrap_err().kind(), ErrorKind::Closed);
}
