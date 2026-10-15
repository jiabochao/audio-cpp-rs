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

let mut request = Request::new();
request.set_text("Hello from Rust.", Some("en-us"))?.set_option("voice-id", "af_heart")?;

let result = session.run(&request)?;
let audio = result.audio().unwrap(); // &[f32] interleaved, borrowed from `result`
```

Inputs and outputs:

- **Request**: text, audio (`&[f32]` interleaved PCM), a speaker reference, style
  controls, artifacts, and options (`set_option` maps to `--request-option`).
- **Result**: `audio()`, `text()`, `segments()`, `speaker_turns()`, `words()`,
  `named_audio()`, `artifacts()`.
- **Streaming**: create the session with `Mode::Streaming`, then call
  `stream_start`, `stream_push` with each chunk, `stream_next_event`, and
  `stream_finish`.
- **Introspection**: `Model::options(scope)` lists the options a family accepts,
  `Model::supports(task, mode)` reports what it can do, and
  `audio_cpp::tasks()` lists the task names this build knows.

Handles are `Send` but not `Sync`. Each handle keeps its parent alive, so
dropping the `Model` or `Registry` early is safe.

## Examples

```sh
cargo run --example vad                       # bundled Silero VAD, no download needed
cargo run --example inspect                   # families compiled into the library
cargo run --example inspect -- <model> [family]
cargo run --release --example tts -- <model> <family> "text" out.wav voice-id=...
cargo run --release --example transcribe -- <model> <family> input.wav [language]
```

Models come from the [audio.cpp GGUF repository](https://huggingface.co/audio-cpp/audio.cpp-gguf).

## Building

Clone with the submodule:

```sh
git clone --recursive <this repo>
# or, in an existing checkout:
git submodule update --init
```

By default, `audio-cpp-sys` builds `libaudiocpp` from the vendored source with
CMake. This needs a C++17 compiler and CMake. The build always runs in Release
mode.

| feature | effect |
|---|---|
| `openmp` (default) | `ENGINE_ENABLE_OPENMP` |
| `native` (default) | ggml CPU kernels tuned for the host CPU. Turn it off for portable binaries. |
| `cuda`, `hip`, `vulkan`, `metal` | GPU backends |

Environment variables:

| variable | |
|---|---|
| `AUDIOCPP_LIB_DIR` | Link a prebuilt `libaudiocpp`, such as the `libs/` directory of an upstream `audio-<tag>-lib-*` release, and skip the CMake build |
| `AUDIOCPP_MODEL_SET` | `full` (default), `core`, or `custom`. `core` builds much faster but includes only the bundled VAD models. |
| `AUDIOCPP_MODELS` | Comma-separated model targets for `AUDIOCPP_MODEL_SET=custom` |
| `AUDIOCPP_CMAKE_ARGS` | Extra `-DKEY=VALUE` arguments for CMake |

`libaudiocpp` is always a shared library, because upstream builds the C ABI only
that way. `cargo run` and `cargo test` find it on their own. To deploy, ship
`libaudiocpp.so`, `.dylib` or `.dll` next to your executable or on the loader
path. Build scripts of dependent crates can read its directory from
`DEP_AUDIOCPP_LIB_DIR`.

## Updating audio.cpp

```sh
git -C audio-cpp-sys/audio.cpp fetch && git -C audio-cpp-sys/audio.cpp checkout <rev>
scripts/gen-bindings.sh   # needs bindgen-cli and libclang
cargo test
```

At runtime, `Registry::new()` refuses a library whose ABI major differs from the
bindings, or whose minor is older than the bindings.

## License

Apache-2.0, matching audio.cpp. Model weights keep their own licenses.
