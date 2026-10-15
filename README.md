# audio-cpp-rs

Rust bindings for [audio.cpp](https://github.com/0xShug0/audio.cpp), a ggml-based
local audio inference framework with 100+ model families: TTS, voice cloning,
ASR, VAD, diarization, source separation, music generation and more.

| crate | |
|---|---|
| `audio-cpp-sys` | Raw FFI over the upstream C ABI (`include/audiocpp.h`, ABI 0.2), plus the build of `libaudiocpp` |
| `audio-cpp` | Safe wrapper: owned handles, `Result` errors, borrowed audio slices |

## Usage

```rust
use audio_cpp::{Backend, BackendConfig, Mode, ModelConfig, Registry, Request};

let registry = Registry::new()?;
let model = registry.load_model("models/kokoro-82m-q8_0.gguf", &ModelConfig::family("kokoro_tts"), None)?;
let mut session = model.create_session("tts", Mode::Offline, &BackendConfig::new(Backend::Cuda), None)?;

let mut request = Request::new()?;
request.set_text("Hello from Rust.", Some("en-us"))?.set_option("voice-id", "af_heart")?;

let result = session.run(&request)?;
if let Some(audio) = result.audio()? {
    // audio.samples: &[f32] interleaved, borrowed from `result`
}
```

Inputs and outputs:

- **Request**: text, audio (`&[f32]` interleaved PCM), a speaker reference, style
  controls, artifacts, and options (`set_option` maps to `--request-option`).
- **Result**: `audio()`, `text()`, `segments()`, `speaker_turns()`, `words()`,
  `named_audio()`, `artifacts()`. An output the task did not produce reads as
  `Ok(None)` or an empty `Vec`. `Err` means the library returned something
  malformed; failures are never silently skipped.
- **Streaming**: create the session with `Mode::Streaming`, then call
  `stream_start`, `stream_push` with each chunk, `stream_next_event`, and
  `stream_finish`.
- **Introspection**: `Model::options(scope)` lists the options a family accepts,
  `Model::supports(task, mode)` reports what it can do, and
  `audio_cpp::tasks()` lists the task names this build knows.

Handles are `Send` but not `Sync`: you can move one to another thread, but
never use one handle from two threads at once. Each handle keeps its parent
alive, so dropping the `Model` or `Registry` early is safe.

`Model::create_session` holds a process-wide lock while it runs. Models loaded
separately can share upstream weight caches, and creating Silero sessions
concurrently crashes with a segfault without the lock (a regression test
covers this). Inference in separate sessions still runs in parallel. Code that
calls the raw `audio_cpp::sys` API directly must serialize session creation
itself.

Inputs are checked before they reach C: interior NUL bytes, partial PCM
frames, non-positive sample rates or channel counts, non-finite style values,
invalid device or thread counts, and negative or overflowing stream offsets.

## Examples

```sh
cargo run --example vad                       # Silero VAD from tests/data, no model download needed
cargo run --example inspect                   # families compiled into the library
cargo run --example inspect -- <model> [family]
cargo run --release --example tts -- <model> <family> "text" out.wav voice-id=...
cargo run --release --example transcribe -- <model> <family> input.wav [language]
```

Models come from the [audio.cpp GGUF repository](https://huggingface.co/audio-cpp/audio.cpp-gguf).

## Building

Add it as a dependency (a git dependency works; no submodules involved):

```toml
audio-cpp = { git = "<this repo>", features = ["cuda"] }
```

`audio-cpp-sys` gets `libaudiocpp` in this order:

1. **`AUDIOCPP_LIB_DIR`**: if set, link the library in that directory and do
   nothing else.
2. **Prebuilt download**: fetch the upstream release archive
   `audio-<tag>-lib-<platform>-<backend>` that matches the target and the
   backend feature. The archive is checked against the sha256 digest GitHub
   publishes and against the ABI version in its `audiocpp.h`.
3. **Source build**: if no compatible archive exists, download the upstream
   source at the commit pinned in `audio-cpp-sys/build.rs` and build it with
   CMake. This needs a C++17 compiler and CMake, and always builds in Release
   mode, with model specs compiled in (`AUDIOCPP_DEPLOYMENT_BUILD=ON`). The
   downloaded source must contain the same `audiocpp.h` as
   `audio-cpp-sys/include/`. A tree given through `AUDIOCPP_SRC_DIR` only needs
   a compatible ABI.

> Upstream releases up to v0.9.1 ship no `-lib-` archives, so until the next
> release step 2 always falls through to a source build.

Prebuilt archives exist for:

| target | archives |
|---|---|
| `x86_64-unknown-linux-gnu` | cpu, vulkan, cuda (+ `-portable` cpu/vulkan) |
| `x86_64-pc-windows-msvc` | cpu, vulkan, cuda (+ `-portable` cpu/vulkan) |
| `aarch64-apple-darwin`, `x86_64-apple-darwin` | metal |

The `hip` feature, and any other target, always builds from source.

| feature | effect |
|---|---|
| `cuda`, `vulkan`, `metal`, `hip` | GPU backend; also selects the prebuilt archive |
| `openmp` (default) | source build only: `ENGINE_ENABLE_OPENMP` |
| `native` | source build only: ggml CPU kernels tuned for the host CPU. Faster, but the library runs only on CPUs like the build machine's. |

Environment variables:

| variable | |
|---|---|
| `AUDIOCPP_BUILD` | `auto` (default: prebuilt, then source), `prebuilt` (fail instead of building), `source` (never download a binary) |
| `AUDIOCPP_LIB_DIR` | Link an existing `libaudiocpp` and skip everything else |
| `AUDIOCPP_RELEASE` | Release tag to download; default is the latest release |
| `AUDIOCPP_CUDA_VERSION` | CUDA flavour of the archive, e.g. `12.8`; default is the oldest offered, which asks the least of the driver |
| `AUDIOCPP_PORTABLE` | `1` selects the `-portable` archive, for CPUs without AVX2 |
| `AUDIOCPP_GITHUB_API` | GitHub API base URL, for mirrors |
| `GITHUB_TOKEN` | Optional; raises the GitHub API rate limit |
| `AUDIOCPP_SRC_DIR` | Build from a local audio.cpp checkout instead of downloading the source |
| `AUDIOCPP_MODEL_SET` | `full` (default), `core`, or `custom`. Setting it implies a source build. `core` builds much faster but includes only the bundled VAD models. |
| `AUDIOCPP_MODELS` | Comma-separated model targets for `AUDIOCPP_MODEL_SET=custom` |
| `AUDIOCPP_CMAKE_ARGS` | Extra `-DKEY=VALUE` arguments for CMake; implies a source build |

With `cargo --offline`, nothing is downloaded: set `AUDIOCPP_LIB_DIR` or
`AUDIOCPP_SRC_DIR`.

`libaudiocpp` is always a shared library, because upstream builds the C ABI only
that way. `cargo run` and `cargo test` find it on their own. To deploy, ship
everything in its directory (a prebuilt archive bundles its runtime
dependencies there) next to your executable or on the loader path. Build
scripts of dependent crates can read that directory from `DEP_AUDIOCPP_LIB_DIR`.

## Updating audio.cpp

1. Set `SOURCE_COMMIT` in `audio-cpp-sys/build.rs` to the new commit.
2. Copy that commit's `include/audiocpp.h` to `audio-cpp-sys/include/`.
3. Run `scripts/gen-bindings.sh` (needs bindgen-cli and libclang).
4. Run `cargo test`.

At runtime, `Registry::new()` also refuses a library whose ABI major differs
from the bindings, or whose minor is older than the bindings.

## License

Apache-2.0, matching audio.cpp. Model weights keep their own licenses.
