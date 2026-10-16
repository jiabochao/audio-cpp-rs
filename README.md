# audio-cpp-rs

**English** | [简体中文](README.zh-CN.md)

Rust bindings for [audio.cpp](https://github.com/0xShug0/audio.cpp): local
inference for 120+ audio model families — TTS, voice cloning and design, ASR,
VAD, diarization, source separation, music generation and more — on CPU,
CUDA, HIP/ROCm, Vulkan and Metal.

| crate | |
|---|---|
| `audio-cpp-sys` | Raw FFI over upstream's C ABI (`audiocpp.h`, ABI 0.2), plus fetching or building `libaudiocpp` |
| `audio-cpp` | Safe API, a session pool for applications, and the model catalog |

```toml
[dependencies]
audio-cpp = { git = "https://github.com/jiabochao/audio-cpp-rs", features = ["metal"] }
```

## Quick start

```rust
use audio_cpp::{AudioBuffer, Backend, BackendConfig, ModelConfig, Registry, Request, Task};

let registry = Registry::new()?;
let model = registry.load_model("models/Qwen3-TTS-12Hz-0.6B-Base-GGUF", &ModelConfig::family("qwen3_tts"), None)?;
let mut session = model.offline_session(Task::Tts, &BackendConfig::new(Backend::Best), None)?;

let reference = AudioBuffer::read_wav("reference.wav")?;
let mut request = Request::with_text("Hello from Rust.")?;
request
    .set_voice_audio(reference.as_ref())?
    .set_option("reference_text", "Transcript of reference.wav")?
    .set_option("seed", "7")?;

let result = session.run(&request)?;
if let Some(audio) = result.audio()? {
    audio.to_buffer().write_wav("out.wav")?;
}
```

Run any family the same way: pick the [`Task`](src/task.rs), set inputs on a
`Request`, and read the outputs that apply — `audio()`, `text()`,
`segments()`, `speaker_turns()`, `words()`, `named_audio()` (separated stems),
`artifacts()` (embeddings, MIDI).

## What you get

**Sessions, typed.** `OfflineSession::run` for whole requests;
`StreamingSession` for chunked input (`start` → `push` → `next_event` →
`finish`). The streaming type tracks its own state, so calls out of order fail
with an error instead of reaching the library, and it counts input offsets
for you.

**A session pool for applications.** Creating a session uploads the model's
weights to the device — seconds and gigabytes for large models — while
loading a `Model` is cheap. `SessionPool` keeps sessions warm per
`SessionSpec`, bounds how many are resident, evicts the least recently used
one, frees *before* it creates so memory peaks at the limit, and can wait
with a timeout or a `CancelToken`.

```rust
let pool = SessionPool::new(PoolConfig { max_sessions: 2, idle_timeout: Some(Duration::from_secs(600)) });
let spec = SessionSpec::new(model_dir, &ModelConfig::family("qwen3_tts"), Task::Tts, &BackendConfig::new(Backend::Metal))
    .session_option("qwen3_tts.voice_prompt_cache_slots", "8");
let mut session = pool.checkout(&spec)?;   // reused while warm; returned on drop
let result = session.run(&request)?;
```

**The model catalog.** Upstream's 120+ model specs are embedded:
families, tasks, languages, typed options for building UIs, and packages with
their files and Hugging Face / ModelScope URLs. No network, no native call.

```rust
use audio_cpp::catalog::{Catalog, Mirror};

let catalog = Catalog::builtin();
for family in catalog.serving(&Task::VoiceCloning) { println!("{}", family.display_name); }
let family = catalog.family("qwen3_tts").unwrap();
let package = family.recommended_package().unwrap();
for file in package.files()? {
    let url = Mirror::model_scope().url(family.download(package).unwrap(), &file);
}
```

**Introspection at run time.** `Model::info()` reports a loaded model's
tasks per mode, languages and every load/session/request option with
defaults and bounds.

**Audio helpers.** `AudioRef` (borrowed PCM, used for inputs and outputs
alike) and `AudioBuffer` (owned): WAV read/write (`wav`, default), decoding of
MP3/AAC/FLAC/Ogg/AIFF (`decode`), resampling (`resample`), mono downmix,
concatenation with gaps.

## Safety and robustness

- Every handle frees itself; borrowed audio and payloads cannot outlive the
  result they came from (checked at compile time).
- Handles are `Send` but not `Sync`, matching the C ABI's threading contract.
- **Session creation is serialized process-wide.** Upstream crashes when
  sessions are created concurrently, even from separately loaded models
  (reproduced: 39 of 40 runs). `tests/concurrency.rs` guards this.
- Inputs are validated before they reach C: interior NULs, partial PCM frames,
  zero sample rates or channels, non-finite style values, zero threads,
  negative or overflowing stream offsets.
- Malformed library output (NULL buffers, impossible PCM formats) is an
  `Err`, never undefined behaviour; nothing is silently skipped.
- `Registry::new` refuses a library with an incompatible ABI, and the build
  checks a prebuilt archive's header before linking it.

## Streaming

Audio-input families (ASR, VAD, diarization) stream incrementally. Text-driven
families (TTS) currently generate the whole utterance inside `start`: the C
ABI has no callback for their incremental audio, so their events never reach
the caller. For progress and cancellation, split long text and synthesize it
piece by piece.

## Building

`audio-cpp-sys` provides `libaudiocpp` in this order:

1. **`AUDIOCPP_LIB_DIR`**: link the library in that directory.
2. **Prebuilt download**: the upstream release archive
   `audio-<tag>-lib-<platform>-<backend>` for the target and backend feature,
   checked against GitHub's sha256 digest and its header's ABI version.
3. **Source build**: the upstream source at the commit pinned in
   `audio-cpp-sys/build.rs`, built with CMake (needs a C++17 compiler and
   CMake). The downloaded header must match the bindings exactly.

The prebuilt release is pinned to the one matching the source commit
(currently **v0.9.2**, upstream's first release with library archives), so a
build is reproducible and, once cached, needs no network. Set
`AUDIOCPP_RELEASE=latest` to follow upstream instead.

Downloads and native builds live in a **persistent cache** keyed by
everything that affects them (commit, target, features, model set, compiler
flags): `~/.cache/audio-cpp-rs`, `~/Library/Caches/audio-cpp-rs` or
`%LOCALAPPDATA%\audio-cpp-rs`. `cargo clean`, other profiles and other
checkouts reuse them — a full source build takes the better part of an hour,
a cache hit takes seconds. Concurrent builds coordinate with file locks.

| feature | |
|---|---|
| `cuda`, `vulkan`, `metal`, `hip` | GPU backend; also selects the prebuilt archive |
| `openmp` (default) | source builds: OpenMP |
| `native` | source builds: CPU kernels for the build machine only (faster, not portable) |
| `catalog` (default), `wav` (default), `resample` | see above |
| `decode` | `AudioBuffer::decode_file`: MP3, AAC/M4A, FLAC, Ogg Vorbis, AIFF, ALAC, CAF |

| variable | |
|---|---|
| `AUDIOCPP_BUILD` | `auto` (default), `prebuilt` (fail instead of building), `source` (never download a binary) |
| `AUDIOCPP_LIB_DIR` | link an existing library, skip everything else |
| `AUDIOCPP_CACHE_DIR` | cache location; `off` keeps everything in `OUT_DIR` |
| `AUDIOCPP_OFFLINE` | `1` forbids network access (`cargo --offline` is not visible to build scripts) |
| `AUDIOCPP_RELEASE` | release tag to download, or `latest`; default the pinned release (v0.9.2) |
| `AUDIOCPP_CUDA_VERSION` | CUDA flavour, e.g. `12.8`; default the oldest, which asks least of the driver |
| `AUDIOCPP_PORTABLE` | `1` selects the `-portable` archive (no AVX2) |
| `AUDIOCPP_GITHUB_API`, `GITHUB_TOKEN` | API mirror; higher rate limit |
| `AUDIOCPP_SRC_DIR` | build a local audio.cpp tree (ABI-checked, not cached) |
| `AUDIOCPP_MODEL_SET` | `full` (default), `core` (VAD only, minutes), `custom`; implies a source build |
| `AUDIOCPP_MODELS` | model targets for `custom`, e.g. `qwen3_tts,qwen3_asr` |
| `AUDIOCPP_CMAKE_ARGS` | extra `-DKEY=VALUE`s |

Source builds always use Release, compile model specs into the library
(`AUDIOCPP_DEPLOYMENT_BUILD`), use `/utf-8` on MSVC, shorten ExternalProject
paths for Windows' MAX_PATH, rebuild cleanly when the source directory
changes, and refuse cross-OS builds without `CMAKE_TOOLCHAIN_FILE`.

### Shipping an application

`libaudiocpp` is a shared library (upstream builds the C ABI only that way).
`cargo run` and `cargo test` find it automatically. To ship, copy everything
in `DEP_AUDIOCPP_LIB_DIR` next to the executable:

- **Windows**: next to the `.exe`.
- **macOS**: into `Contents/Frameworks` (Tauri: `bundle.macOS.frameworks`),
  with an `@executable_path/../Frameworks` rpath.
- **Linux**: next to the binary with an `$ORIGIN` rpath, or on the loader path.

## Development

```sh
cargo test --features resample,decode     # downloads the pinned prebuilt library once, then cached
cargo run --example vad                                  # bundled Silero VAD, no download
cargo run --example catalog -- qwen3_tts                 # packages and URLs
cargo run --example inspect -- <model> [family]          # a model's tasks and options
cargo run --release --example tts -- <model> <family> "text" out.wav [key=value ...]
cargo run --release --example transcribe -- <model> <family> input.wav [language]
```

Updating audio.cpp: `scripts/update-upstream.sh [<tag>|<branch>|<commit>]` fetches
it, copies its header, regenerates bindings and catalog, and moves the pin (a
tag also becomes the pinned prebuilt release); it stops on an ABI major change. The `upstream-sync` workflow runs it
daily and opens a pull request (give it a PAT if CI should run on that PR;
pull requests opened with `GITHUB_TOKEN` do not trigger workflows).

## License

Apache-2.0, matching audio.cpp. Model weights keep their own licenses; see
upstream's `docs/model_licenses.md`.
