# audio-cpp-rs

[English](README.md) | **简体中文**

[audio.cpp](https://github.com/0xShug0/audio.cpp) 的 Rust 绑定。可在 CPU、CUDA、HIP/ROCm、Vulkan、Metal 上本地运行 120 多个音频模型家族，覆盖语音合成（TTS）、音色克隆与设计、语音识别（ASR）、语音活动检测（VAD）、说话人分离、音源分离、音乐生成等任务。

| crate | 说明 |
|---|---|
| `audio-cpp-sys` | 上游 C ABI（`audiocpp.h`，ABI 0.2）的原始 FFI，负责获取或构建 `libaudiocpp` |
| `audio-cpp` | 安全 API、面向应用的 Session 池、模型目录 |

```toml
[dependencies]
audio-cpp = { git = "https://github.com/jiabochao/audio-cpp-rs", features = ["metal"] }
```

## 快速上手

```rust
use audio_cpp::{AudioBuffer, Backend, BackendConfig, ModelConfig, Registry, Request, Task};

let registry = Registry::new()?;
let model = registry.load_model("models/Qwen3-TTS-12Hz-0.6B-Base-GGUF", &ModelConfig::family("qwen3_tts"), None)?;
let mut session = model.offline_session(Task::Tts, &BackendConfig::new(Backend::Best), None)?;

let reference = AudioBuffer::read_wav("reference.wav")?;
let mut request = Request::with_text("你好，这是 Rust。")?;
request
    .set_voice_audio(reference.as_ref())?
    .set_option("reference_text", "reference.wav 的文字内容")?
    .set_option("seed", "7")?;

let result = session.run(&request)?;
if let Some(audio) = result.audio()? {
    audio.to_buffer().write_wav("out.wav")?;
}
```

所有模型家族的用法都一样：选一个 `Task`，在 `Request` 上设置输入，再从结果里读取对应的输出。可读的输出有 `audio()`、`text()`、`segments()`、`speaker_turns()`、`words()`、`named_audio()`（分离出的各个音轨）、`artifacts()`（说话人嵌入、MIDI 等）。

## 功能

**类型化的 Session。** `OfflineSession::run` 一次处理完整请求；`StreamingSession` 处理分块输入，调用顺序是 `start` → `push` → `next_event` → `finish`。流式 Session 自己跟踪状态，调用顺序不对会直接返回错误，不会把错误状态带进原生库；输入的采样偏移也由它自动累计。

**面向应用的 Session 池。** 加载 `Model` 很便宜，但每创建一个 Session 都要把整份权重上传到计算设备，大模型要花几秒钟、占几个 GB。`SessionPool` 的做法：

- 按 `SessionSpec` 复用常驻的 Session；
- 限制常驻数量，满了就按 LRU 驱逐空闲的；
- 先释放再创建，内存峰值不超过上限；
- 需要等待时，支持超时和 `CancelToken` 取消。

```rust
let pool = SessionPool::new(PoolConfig { max_sessions: 2, idle_timeout: Some(Duration::from_secs(600)) });
let spec = SessionSpec::new(model_dir, &ModelConfig::family("qwen3_tts"), Task::Tts, &BackendConfig::new(Backend::Metal))
    .session_option("qwen3_tts.voice_prompt_cache_slots", "8");
let mut session = pool.checkout(&spec)?;   // 常驻时直接复用；drop 时归还到池里
let result = session.run(&request)?;
```

**模型目录。** 内嵌了上游 120 多个模型的 spec：家族、任务、语言、可直接用来生成界面控件的类型化参数，以及每个下载包的文件清单和 Hugging Face / ModelScope 下载地址。查询目录不需要联网，也不调用原生库。

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

**运行时查询能力。** `Model::info()` 返回已加载模型在离线和流式模式下各支持哪些任务、支持哪些语言，以及全部 load / session / request 参数的默认值和取值范围。

**音频工具。** `AudioRef` 是借用的 PCM，输入和输出共用这一个类型；`AudioBuffer` 是拥有所有权的版本。提供的功能：WAV 读写（`wav` 特性，默认开启）、MP3/AAC/FLAC/Ogg/AIFF 解码（`decode` 特性）、高质量重采样（`resample` 特性）、下混为单声道、带间隔的拼接。

## 安全与健壮性

- 每个句柄都会自动释放。借用的音频和数据不能活得比它所属的结果长，这一点在编译期检查。
- 句柄是 `Send` 但不是 `Sync`，与 C ABI 的线程约定一致。
- **Session 的创建在整个进程内串行执行。** 上游在并发创建 Session 时会崩溃，即使这些 Session 来自分别加载的模型（实测 40 次崩溃 39 次）。`tests/concurrency.rs` 是对应的回归测试。
- 输入在进入 C 之前就做校验：字符串里的 NUL 字节、不完整的 PCM 帧、采样率或声道数为 0、非有限的风格参数、线程数为 0、为负或会溢出的流式偏移。
- 原生库返回的异常数据（空指针缓冲区、不合理的 PCM 格式）一律返回 `Err`，不会造成未定义行为，也不会被悄悄跳过。
- `Registry::new` 会拒绝 ABI 不兼容的库；构建时也会先检查预编译包里头文件的 ABI，再链接。

## 关于流式

以音频为输入的家族（ASR、VAD、说话人分离）支持真正的增量流式。以文本驱动的家族（TTS）目前会在 `start` 内部一次生成完整段语音：C ABI 没有传递增量音频的回调，这些事件到不了调用方。如果需要进度和取消，请把长文本切分后逐段合成。

## 构建

`audio-cpp-sys` 按下面的顺序获取 `libaudiocpp`：

1. **`AUDIOCPP_LIB_DIR`**：如果设置了，直接链接该目录里的库。
2. **下载预编译包**：按目标平台和后端特性下载上游 release 中的 `audio-<tag>-lib-<platform>-<backend>`，用 GitHub 公布的 sha256 校验，并检查包内头文件的 ABI 版本。
3. **源码构建**：下载 `audio-cpp-sys/build.rs` 中固定 commit 的上游源码，用 CMake 构建（需要 C++17 编译器和 CMake）。下载的头文件必须与绑定完全一致。

预编译包固定使用与源码 commit 对应的 release（目前是 **v0.9.2**，这是上游第一个带 lib 包的版本）。这样构建结果可以复现，而且缓存之后不需要联网。固定版本的包名和 sha256 记录在 `build.rs` 里（由 `scripts/pin-release.py` 生成）。所以下载时完全不调用 GitHub API，不受 CI 机器和办公网络共用的匿名限额影响；校验用的 digest 也是固定的，不会随下载一起被替换。如果想跟随上游最新版，设置 `AUDIOCPP_RELEASE=latest`。

下载内容和编译产物存放在**持久缓存**里，缓存键包含所有会影响产物的因素：commit、目标平台、特性、模型集、编译器参数。缓存位置：

- Linux：`~/.cache/audio-cpp-rs`
- macOS：`~/Library/Caches/audio-cpp-rs`
- Windows：`%LOCALAPPDATA%\audio-cpp-rs`

`cargo clean` 之后、切换 profile、甚至换一个 checkout，都能直接复用缓存。完整的源码构建要接近一小时，命中缓存只要几秒。多个构建同时进行时，靠文件锁互相协调。

| 特性 | 说明 |
|---|---|
| `cuda`、`vulkan`、`metal`、`hip` | 启用 GPU 后端，同时决定下载哪个预编译包。macOS 上始终启用 Metal |
| `openmp`（默认） | 源码构建时启用 OpenMP（macOS 除外，苹果的编译器不带 OpenMP） |
| `native` | 源码构建时按本机 CPU 指令集编译。更快，但产物只能在同类 CPU 上运行 |
| `catalog`（默认）、`wav`（默认）、`resample` | 见上文 |
| `decode` | `AudioBuffer::decode_file`：解码 MP3、AAC/M4A、FLAC、Ogg Vorbis、AIFF、ALAC、CAF |

| 环境变量 | 说明 |
|---|---|
| `AUDIOCPP_BUILD` | `auto`（默认）；`prebuilt`：拿不到预编译包就报错；`source`：只做源码构建 |
| `AUDIOCPP_LIB_DIR` | 链接已有的库，跳过其他所有步骤 |
| `AUDIOCPP_CACHE_DIR` | 缓存位置；设为 `off` 时全部放在 `OUT_DIR` 里 |
| `AUDIOCPP_OFFLINE` | 设为 `1` 时禁止联网。`cargo --offline` 不一定会传递给构建脚本，所以需要这个开关 |
| `AUDIOCPP_RELEASE` | 要下载的 release tag，或设为 `latest`；默认使用固定的版本（v0.9.2） |
| `AUDIOCPP_CUDA_VERSION` | CUDA 版本，例如 `12.8`；默认取最旧的版本，对显卡驱动要求最低 |
| `AUDIOCPP_PORTABLE` | 默认开启：使用 `-portable` 的 CPU/Vulkan 包，任何 x86-64 CPU 都能运行。设为 `0` 时改用按上游构建机 CPU 调优的包：CPU 推理更快，但在缺少相应指令集的 CPU 上会因非法指令而崩溃 |
| `AUDIOCPP_GITHUB_API`、`GITHUB_TOKEN` | API 镜像地址；提高 API 调用限额 |
| `AUDIOCPP_SRC_DIR` | 从本地 audio.cpp 源码构建（会检查 ABI，不进入缓存） |
| `AUDIOCPP_MODEL_SET` | `full`（默认）、`core`（只含 VAD，几分钟编完）、`custom`；设置后强制源码构建 |
| `AUDIOCPP_MODELS` | `custom` 模式下要编译的模型，例如 `qwen3_tts,qwen3_asr` |
| `AUDIOCPP_CMAKE_ARGS` | 额外传给 CMake 的 `-DKEY=VALUE` 参数 |

源码构建还会自动处理这些细节：

- 始终以 Release 模式编译；
- 把模型 spec 编译进库（`AUDIOCPP_DEPLOYMENT_BUILD`）；
- 在 MSVC 下启用 `/utf-8`；
- 缩短 ExternalProject 的路径，避开 Windows 的 MAX_PATH 限制；
- 源码目录变了会干净地重新构建；
- 没有 `CMAKE_TOOLCHAIN_FILE` 时，拒绝跨操作系统交叉编译。

### 发布应用

`libaudiocpp` 是动态库，因为上游只把 C ABI 构建成动态库。`cargo run` 和 `cargo test` 会自动找到它。发布时，需要把 `DEP_AUDIOCPP_LIB_DIR` 目录下的所有文件放到可执行文件旁边：

- **Windows**：放在 `.exe` 同目录；
- **macOS**：放进 `Contents/Frameworks`（Tauri 用 `bundle.macOS.frameworks` 配置），并设置 `@executable_path/../Frameworks` 的 rpath；
- **Linux**：放在可执行文件旁并设置 `$ORIGIN` 的 rpath，或放在系统动态库搜索路径里。

## 开发

```sh
cargo test --features resample,decode     # 首次下载固定版本的预编译库，之后命中缓存
cargo run --example vad                                  # 使用仓库自带的 Silero VAD，无需下载
cargo run --example catalog -- qwen3_tts                 # 查看下载包和地址
cargo run --example inspect -- <model> [family]          # 查看模型支持的任务和参数
cargo run --release --example tts -- <model> <family> "文本" out.wav [key=value ...]
cargo run --release --example transcribe -- <model> <family> input.wav [language]
```

升级 audio.cpp：运行 `scripts/update-upstream.sh [<tag>|<branch>|<commit>]`。它会拉取指定版本、复制头文件、重新生成绑定和模型目录，并更新固定的 commit（传入 tag 时，同时更新固定的预编译版本）；如果 ABI 主版本变了就停下来，等人工处理。`upstream-sync` 工作流每天自动运行它并开 PR。如果希望这个 PR 自动跑 CI，需要给工作流配置 PAT，因为用 `GITHUB_TOKEN` 创建的 PR 不会触发其他工作流。

## 许可证

Apache-2.0，与 audio.cpp 一致。模型权重各有自己的许可证，详见上游的 `docs/model_licenses.md`。
