//! Provides `libaudiocpp`, the audio.cpp C ABI shared library.
//!
//! Resolution order:
//!
//! 1. `AUDIOCPP_LIB_DIR` set: link the library in that directory, e.g. the
//!    `libs/` directory of an unpacked `audio-<tag>-lib-*` release archive.
//! 2. A prebuilt release archive matching the target and the backend feature,
//!    verified against GitHub's sha256 digest and the ABI of its header.
//! 3. A CMake build of the pinned upstream commit [`SOURCE_COMMIT`] (or of
//!    `AUDIOCPP_SRC_DIR`).
//!
//! Downloads and native builds live in a persistent cache keyed by everything
//! that affects them, so `cargo clean`, other profiles and other checkouts
//! reuse them: a full source build takes the better part of an hour. The
//! libraries are then copied into `OUT_DIR/lib`, which is where `cargo run`
//! and `cargo test` look for them at run time.
//!
//! Environment:
//!
//! | variable | |
//! |---|---|
//! | `AUDIOCPP_BUILD` | `auto` (default: 2 then 3), `prebuilt` (2 only), `source` (3 only) |
//! | `AUDIOCPP_LIB_DIR` | link an existing library, skipping everything else |
//! | `AUDIOCPP_CACHE_DIR` | cache location; `off` keeps everything in `OUT_DIR` |
//! | `AUDIOCPP_OFFLINE` | `1` forbids network access (also `CARGO_NET_OFFLINE=true`) |
//! | `AUDIOCPP_RELEASE` | release tag to download, or `latest`; default [`PREBUILT_RELEASE`] |
//! | `AUDIOCPP_CUDA_VERSION` | CUDA flavour of the archive, e.g. `12.8`; default the oldest |
//! | `AUDIOCPP_PORTABLE` | `0` selects the CPU/Vulkan archives tuned to upstream's build machine instead of the `-portable` ones |
//! | `AUDIOCPP_GITHUB_API` | GitHub API base URL, for mirrors |
//! | `GITHUB_TOKEN` | raises the GitHub API rate limit |
//! | `AUDIOCPP_SRC_DIR` | build from a local audio.cpp tree (not cached) |
//! | `AUDIOCPP_MODEL_SET`, `AUDIOCPP_MODELS`, `AUDIOCPP_CMAKE_ARGS` | source build knobs; setting any implies a source build |
//!
//! Dependent build scripts get `DEP_AUDIOCPP_LIB_DIR` (the libraries to ship
//! next to an executable) and `DEP_AUDIOCPP_INCLUDE`.

use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

#[path = "build/archive.rs"]
mod archive;

const UPSTREAM: &str = "0xShug0/audio.cpp";
/// Upstream commit the bindings were generated from; used for source builds.
const SOURCE_COMMIT: &str = "52118521b338aaa376ad58350ec772342993485b";
/// Release whose prebuilt archives match [`SOURCE_COMMIT`]. `AUDIOCPP_RELEASE`
/// overrides it; `latest` follows upstream's newest release instead.
const PREBUILT_RELEASE: &str = "v0.9.2";
/// Library and CUDA runtime archives of [`PREBUILT_RELEASE`] with their
/// sha256, written by `scripts/pin-release.py`. Building against the pinned
/// release needs no GitHub API call — anonymous calls are rate limited per IP,
/// which CI runners and offices share — and checks archives against digests
/// recorded here, not ones fetched alongside them.
const PREBUILT_ASSETS: &[(&str, &str)] = &[
    // BEGIN PREBUILT_ASSETS (generated)
    (
        "audio-v0.9.2-cudart-ubuntu-x64-cuda12.8.tar.gz",
        "2a946a0e9f1278c9318bd4cc8cb2c8915ec966a8cb014795bcdf3fc7153da973",
    ),
    (
        "audio-v0.9.2-cudart-ubuntu-x64-cuda13.3.tar.gz",
        "7619300961d7a721f07de655cf2f5e5107c673a15ddacb0204ea9663f9f4253e",
    ),
    (
        "audio-v0.9.2-cudart-windows-x64-cuda12.4.zip",
        "50e278d213daa38e782a10b501b764c5c0738381898dacee7243101b890eeb0d",
    ),
    (
        "audio-v0.9.2-cudart-windows-x64-cuda13.3.zip",
        "f3bb616e43f4b8aec4be7f2c2b323aec381a1993bac382401a011c121c126e59",
    ),
    ("audio-v0.9.2-lib-macos-arm64-metal.tar.gz", "781939ba177f379b3bb6b3c9dfd056478d620ee63f783ef2b8119ec25d478571"),
    ("audio-v0.9.2-lib-macos-x64-metal.tar.gz", "b931f2cbdad2de12e8f1fc3be7aa42c5c548d42cfe3a77878fe0ccdb308756a2"),
    (
        "audio-v0.9.2-lib-ubuntu-x64-cpu-portable.tar.gz",
        "1769a744dddf452347b2a732b17c3e470d2daad0fb1b5051d91fd7dd93cd119d",
    ),
    ("audio-v0.9.2-lib-ubuntu-x64-cpu.tar.gz", "9caf953d5fa02eb9fb7690b5238ea5535b82d0b53cae3b31882f3e2260ef0d1c"),
    ("audio-v0.9.2-lib-ubuntu-x64-cuda12.8.tar.gz", "d59dc4c612bfe0dc4f9b33161db30f33bf9afa9de6e7f73d37f9b212aa25e0c0"),
    ("audio-v0.9.2-lib-ubuntu-x64-cuda13.3.tar.gz", "840ddf53bac3f3581817401c8d6a67557048a6f6a2f2319ee48be78db8af1737"),
    (
        "audio-v0.9.2-lib-ubuntu-x64-vulkan-portable.tar.gz",
        "e2b020f319abd5451fde775cb581cb9b5291c08c5d91433c525521e777b6c57d",
    ),
    ("audio-v0.9.2-lib-ubuntu-x64-vulkan.tar.gz", "f08f93b39a4f002263e58883db0feea980975087b1a5ce4355671302f4528ba3"),
    (
        "audio-v0.9.2-lib-windows-x64-cpu-portable.zip",
        "85bdbbe5f5c9b85cee1d59ba4e4d90db8902d07b9a16606b87d4cfb69d77a4f4",
    ),
    ("audio-v0.9.2-lib-windows-x64-cpu.zip", "c3839623692ec11df8c42f702faf5b03db68de2c09aa33d85e1a86f9038ab52f"),
    ("audio-v0.9.2-lib-windows-x64-cuda12.4.zip", "88b3bd81cda3b49799e227ff7cb73d25568730be74bcc34705a53ef4537d9145"),
    ("audio-v0.9.2-lib-windows-x64-cuda13.3.zip", "e85a6e4b7084140019776fbcf7e9827e69df5745b8a4af471b4fbc9a52e917d2"),
    (
        "audio-v0.9.2-lib-windows-x64-vulkan-portable.zip",
        "285f7e95e053807bd52cd888baa2d8a9fd1f9845305e2f17716987ffca472988",
    ),
    ("audio-v0.9.2-lib-windows-x64-vulkan.zip", "01c080d1455d4e10df4fab46f99d1a7b9119ea703db323c5691d08ffbad77afa"),
    // END PREBUILT_ASSETS
];
/// Bumped when the layout of cached builds changes.
const CACHE_LAYOUT: u32 = 1;

type BoxError = Box<dyn std::error::Error>;

fn main() {
    for var in [
        "AUDIOCPP_LIB_DIR",
        "AUDIOCPP_BUILD",
        "AUDIOCPP_CACHE_DIR",
        "AUDIOCPP_OFFLINE",
        "AUDIOCPP_RELEASE",
        "AUDIOCPP_CUDA_VERSION",
        "AUDIOCPP_PORTABLE",
        "AUDIOCPP_GITHUB_API",
        "AUDIOCPP_SRC_DIR",
        "AUDIOCPP_MODEL_SET",
        "AUDIOCPP_MODELS",
        "AUDIOCPP_CMAKE_ARGS",
        "CARGO_NET_OFFLINE",
        "CMAKE_GENERATOR",
        "CMAKE_TOOLCHAIN_FILE",
        "CC",
        "CXX",
        "CFLAGS",
        "CXXFLAGS",
        "VULKAN_SDK",
        "CUDA_PATH",
        "DOCS_RS",
    ] {
        println!("cargo:rerun-if-env-changed={var}");
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=include/audiocpp.h");
    println!("cargo:rerun-if-changed=cmake/short-paths.cmake");
    println!("cargo:include={}", manifest_dir().join("include").display());

    // docs.rs has neither network nor time to build audio.cpp, and
    // documentation needs only the declarations.
    if env::var_os("DOCS_RS").is_some() {
        return;
    }

    let lib_dir = match env::var_os("AUDIOCPP_LIB_DIR") {
        Some(dir) => {
            let dir = PathBuf::from(dir);
            if !has_library(&dir) {
                panic!("AUDIOCPP_LIB_DIR={} contains no libaudiocpp", dir.display());
            }
            println!("cargo:rerun-if-changed={}", dir.display());
            dir
        }
        None => {
            let found = locate_or_build();
            stage_runtime(&found).unwrap_or_else(|e| panic!("cannot stage libaudiocpp into OUT_DIR: {e}"))
        }
    };

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=dylib=audiocpp");
    println!("cargo:lib_dir={}", lib_dir.display());
}

fn locate_or_build() -> PathBuf {
    let mode = env::var("AUDIOCPP_BUILD").unwrap_or_else(|_| "auto".into());
    let wants_source_knobs = ["AUDIOCPP_MODEL_SET", "AUDIOCPP_MODELS", "AUDIOCPP_CMAKE_ARGS", "AUDIOCPP_SRC_DIR"]
        .iter()
        .any(|v| env::var_os(v).is_some());
    match mode.as_str() {
        "source" => build_from_source(),
        "prebuilt" => download_prebuilt().unwrap_or_else(|e| panic!("prebuilt libaudiocpp unavailable: {e}")),
        "auto" if wants_source_knobs => build_from_source(),
        "auto" => match download_prebuilt() {
            Ok(dir) => dir,
            Err(e) => {
                println!("cargo:warning=prebuilt libaudiocpp unavailable ({e}); building from source");
                build_from_source()
            }
        },
        other => panic!("AUDIOCPP_BUILD must be auto, prebuilt or source, not {other:?}"),
    }
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
}

fn out_dir() -> PathBuf {
    PathBuf::from(env::var("OUT_DIR").unwrap())
}

fn feature(name: &str) -> bool {
    env::var_os(format!("CARGO_FEATURE_{}", name.to_uppercase())).is_some()
}

fn truthy(var: &str) -> bool {
    env::var(var).is_ok_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
}

/// `cargo --offline` is not reliably visible to build scripts, hence the
/// crate's own switch alongside Cargo's.
fn offline() -> bool {
    truthy("AUDIOCPP_OFFLINE") || truthy("CARGO_NET_OFFLINE")
}

// ---------------------------------------------------------------------------
// Persistent cache
// ---------------------------------------------------------------------------

/// The cache root, or `None` to keep everything inside `OUT_DIR`.
fn cache_root() -> Option<PathBuf> {
    if let Some(dir) = env::var_os("AUDIOCPP_CACHE_DIR") {
        let off = dir.is_empty() || dir.to_str().is_some_and(|d| d.eq_ignore_ascii_case("off"));
        return (!off).then(|| PathBuf::from(dir));
    }
    // The build script runs on the host, so these are the host's conventions.
    let base = if cfg!(windows) {
        env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Caches"))
    } else {
        env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
    };
    base.map(|base| base.join("audio-cpp-rs"))
}

fn cache_dir(kind: &str) -> PathBuf {
    let dir = cache_root().unwrap_or_else(|| out_dir().join("cache")).join(kind);
    fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("cannot create {}: {e}", dir.display()));
    dir
}

/// Holds an exclusive lock on `<dir>.lock` while alive, so concurrent builds
/// (two checkouts, `cargo build` beside rust-analyzer) do not write the same
/// cache entry at once.
struct CacheLock(fs::File);

impl CacheLock {
    fn acquire(dir: &Path) -> io::Result<Self> {
        // Appended rather than `with_extension`: entry names contain dots
        // (`audio.cpp-<commit>`, `audio-v0.9.2-lib-…`).
        let mut name = dir.file_name().unwrap_or_default().to_os_string();
        name.push(".lock");
        let path = dir.with_file_name(name);
        let file = fs::OpenOptions::new().create(true).truncate(false).write(true).open(&path)?;
        if fs4::fs_std::FileExt::try_lock_exclusive(&file).is_err() {
            println!("cargo:warning=waiting for another build to finish {}", dir.display());
            fs4::fs_std::FileExt::lock_exclusive(&file)?;
        }
        Ok(Self(file))
    }
}

impl Drop for CacheLock {
    fn drop(&mut self) {
        let _ = fs4::fs_std::FileExt::unlock(&self.0);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Copies the shared libraries in `from` into `OUT_DIR/lib`, the directory
/// Cargo adds to the loader path for `cargo run` / `cargo test`, and returns it.
/// Unix symlinks (libaudiocpp.so -> .so.0 -> .so.0.1.0) are recreated.
fn stage_runtime(from: &Path) -> io::Result<PathBuf> {
    let to = out_dir().join("lib");
    if to.exists() {
        fs::remove_dir_all(&to)?;
    }
    fs::create_dir_all(&to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(text) = name.to_str() else { continue };
        let is_library =
            [".so", ".dylib", ".dll", ".lib", ".dll.a"].iter().any(|ext| text.ends_with(ext)) || text.contains(".so.");
        if !is_library {
            continue;
        }
        let file_type = entry.file_type()?;
        let target = to.join(&name);
        #[cfg(unix)]
        if file_type.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(entry.path())?, &target)?;
            continue;
        }
        if file_type.is_file() || file_type.is_symlink() {
            // A hard link is instant and costs no space; fall back to copying
            // across file systems.
            if fs::hard_link(entry.path(), &target).is_err() {
                fs::copy(entry.path(), &target)?;
            }
        }
    }
    if !has_library(&to) {
        return Err(io::Error::other(format!("no libaudiocpp in {}", from.display())));
    }
    Ok(to)
}

fn has_library(dir: &Path) -> bool {
    ["libaudiocpp.so", "libaudiocpp.dylib", "audiocpp.lib", "libaudiocpp.dll.a"]
        .iter()
        .any(|name| dir.join(name).exists())
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(60 * 60))).build().into()
}

fn github_api() -> String {
    env::var("AUDIOCPP_GITHUB_API").unwrap_or_else(|_| "https://api.github.com".into()).trim_end_matches('/').into()
}

fn get(url: &str) -> Result<ureq::http::Response<ureq::Body>, BoxError> {
    let mut request = agent().get(url).header("User-Agent", "audio-cpp-sys-build");
    if url.starts_with(&github_api()) {
        request = request.header("Accept", "application/vnd.github+json");
        if let Ok(token) = env::var("GITHUB_TOKEN") {
            request = request.header("Authorization", &format!("Bearer {token}"));
        }
    }
    Ok(request.call()?)
}

/// Streams `url` into `path`, returning the body's sha256. Writes to a
/// `.part` file first so an interrupted download is never mistaken for a
/// complete one.
fn download(url: &str, path: &Path) -> Result<String, BoxError> {
    let partial = path.with_extension("part");
    let mut response = get(url)?;
    let mut reader = response.body_mut().as_reader();
    let mut file = io::BufWriter::new(fs::File::create(&partial)?);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        file.write_all(&buffer[..n])?;
    }
    file.flush()?;
    drop(file);
    fs::rename(&partial, path)?;
    Ok(hex(&hasher.finalize()))
}

fn unpack(archive: &Path, dest: &Path) -> Result<(), BoxError> {
    if archive.extension().is_some_and(|e| e == "zip") {
        archive::unzip(&fs::read(archive)?, dest)
    } else {
        tar::Archive::new(flate2::read::GzDecoder::new(fs::File::open(archive)?)).unpack(dest)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ABI
// ---------------------------------------------------------------------------

fn bindings_header() -> PathBuf {
    manifest_dir().join("include/audiocpp.h")
}

fn abi_version(header: &Path) -> Result<(u32, u32), BoxError> {
    let text = fs::read_to_string(header).map_err(|e| format!("{}: {e}", header.display()))?;
    let define = |key: &str| -> Result<u32, BoxError> {
        text.lines()
            .find_map(|line| line.trim().strip_prefix("#define ")?.trim().strip_prefix(key)?.trim().parse().ok())
            .ok_or_else(|| format!("{} defines no {key}", header.display()).into())
    };
    Ok((define("AUDIOCPP_ABI_VERSION_MAJOR")?, define("AUDIOCPP_ABI_VERSION_MINOR")?))
}

/// The library must have the same major ABI and at least the minor the
/// bindings declare, or symbols the bindings reference may be missing.
fn check_abi(header: &Path) -> Result<(), BoxError> {
    let ours = abi_version(&bindings_header())?;
    let theirs = abi_version(header)?;
    if theirs.0 != ours.0 || theirs.1 < ours.1 {
        return Err(
            format!("ABI {}.{} is incompatible with bindings for {}.{}", theirs.0, theirs.1, ours.0, ours.1).into()
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Prebuilt release archives
// ---------------------------------------------------------------------------

fn download_prebuilt() -> Result<PathBuf, BoxError> {
    let target_os = env::var("CARGO_CFG_TARGET_OS")?;
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH")?;
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let platform = match (target_os.as_str(), target_arch.as_str(), target_env.as_str()) {
        ("linux", "x86_64", "gnu") => "ubuntu-x64",
        ("macos", "aarch64", _) => "macos-arm64",
        ("macos", "x86_64", _) => "macos-x64",
        ("windows", "x86_64", "msvc") => "windows-x64",
        _ => return Err(format!("no prebuilt archive for {target_arch}-{target_os}-{target_env}").into()),
    };
    let is_mac = target_os == "macos";

    let gpu: Vec<&str> = ["cuda", "hip", "vulkan", "metal"].into_iter().filter(|f| feature(f)).collect();
    let backend = match (gpu.as_slice(), is_mac) {
        ([], true) | (["metal"], true) => "metal",
        ([], false) => "cpu",
        (["cuda"], false) => "cuda",
        (["vulkan"], false) => "vulkan",
        _ => return Err(format!("no prebuilt archive for features {gpu:?} on {platform}").into()),
    };
    // Upstream's non-portable CPU and Vulkan archives are compiled for the
    // CPU of the machine that built them (ENGINE_ENABLE_NATIVE_CPU defaults
    // on), and crash with an illegal instruction on older CPUs — seen on
    // GitHub's own Windows runners. The portable ones dispatch at run time
    // (Linux) or target baseline x86-64 (Windows), so they are the default;
    // AUDIOCPP_PORTABLE=0 opts into the tuned ones for a known machine.
    let portable = env::var("AUDIOCPP_PORTABLE").map_or(true, |_| truthy("AUDIOCPP_PORTABLE"));
    let ext = if target_os == "windows" { "zip" } else { "tar.gz" };

    // A pinned tag that is already cached needs no network at all.
    let cache = cache_dir("prebuilt");
    let wanted = release_tag();
    if wanted != "latest" {
        if let Some(dir) = cached_prebuilt(&cache, &wanted, platform, backend, portable) {
            return Ok(dir);
        }
    }
    if offline() {
        return Err("offline, and no matching prebuilt archive is cached".into());
    }

    let (tag, assets) = release_assets(&wanted)?;
    let tag = tag.as_str();
    let asset_named = |name: &str| assets.iter().find(|a| a.name == name);

    let (flavour, cudart) = if backend == "cuda" {
        let version = cuda_version(&assets, tag, platform, ext)?;
        (format!("cuda{version}"), Some(format!("audio-{tag}-cudart-{platform}-cuda{version}.{ext}")))
    } else if portable && backend != "metal" {
        (format!("{backend}-portable"), None)
    } else {
        (backend.to_string(), None)
    };
    let name = format!("audio-{tag}-lib-{platform}-{flavour}.{ext}");
    let asset = asset_named(&name).ok_or_else(|| format!("release {tag} has no {name}"))?;
    let digest = asset.digest.clone();

    // Keyed by digest, so a re-uploaded asset is a different entry.
    let stem = name.trim_end_matches(&format!(".{ext}")).to_string();
    let dir = cache.join(format!("{stem}-{}", digest.as_deref().map_or("unverified", |d| &d[..12])));
    let marker = dir.join(".complete");
    let _lock = CacheLock::acquire(&dir)?;
    if !marker.exists() {
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
        }
        fs::create_dir_all(&dir)?;
        fetch_asset(asset, &dir)?;
        if let Some(cudart) = cudart {
            let asset = asset_named(&cudart).ok_or_else(|| format!("release {tag} has no {cudart}"))?;
            fetch_asset(asset, &dir.join("libs"))?;
        }
        check_abi(&dir.join("include/audiocpp.h"))?;
        if !has_library(&dir.join("libs")) {
            return Err(format!("{name} contains no libaudiocpp under libs/").into());
        }
        fs::write(&marker, format!("{tag}\n{name}\n"))?;
    }
    Ok(dir.join("libs"))
}

fn cached_prebuilt(cache: &Path, tag: &str, platform: &str, backend: &str, portable: bool) -> Option<PathBuf> {
    let flavour = if portable && backend != "metal" { format!("{backend}-portable") } else { backend.to_string() };
    let prefix = if backend == "cuda" {
        let version = env::var("AUDIOCPP_CUDA_VERSION").ok()?;
        format!("audio-{tag}-lib-{platform}-cuda{version}-")
    } else {
        format!("audio-{tag}-lib-{platform}-{flavour}-")
    };
    fs::read_dir(cache)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(&prefix))
                && p.join(".complete").exists()
        })
        .map(|p| p.join("libs"))
}

/// A downloadable archive of a release.
struct Asset {
    name: String,
    url: String,
    /// Lowercase hex sha256, when known.
    digest: Option<String>,
}

/// The archives of release `wanted`: from [`PREBUILT_ASSETS`] for the pinned
/// release, otherwise from the GitHub API.
fn release_assets(wanted: &str) -> Result<(String, Vec<Asset>), BoxError> {
    if wanted == PREBUILT_RELEASE && !PREBUILT_ASSETS.is_empty() {
        let assets = PREBUILT_ASSETS
            .iter()
            .map(|(name, digest)| Asset {
                name: name.to_string(),
                url: format!("https://github.com/{UPSTREAM}/releases/download/{wanted}/{name}"),
                digest: Some(digest.to_string()),
            })
            .collect();
        return Ok((wanted.to_string(), assets));
    }
    let release = github_release()?;
    let tag = release["tag_name"].as_str().ok_or("release has no tag_name")?.to_string();
    let assets = release["assets"]
        .as_array()
        .ok_or("release has no assets")?
        .iter()
        .filter_map(|a| {
            Some(Asset {
                name: a["name"].as_str()?.to_string(),
                url: a["browser_download_url"].as_str()?.to_string(),
                digest: a["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).map(str::to_ascii_lowercase),
            })
        })
        .collect();
    Ok((tag, assets))
}

fn cuda_version(assets: &[Asset], tag: &str, platform: &str, ext: &str) -> Result<String, BoxError> {
    let prefix = format!("audio-{tag}-lib-{platform}-cuda");
    let suffix = format!(".{ext}");
    let mut offered: Vec<String> =
        assets.iter().filter_map(|a| a.name.strip_prefix(&prefix)?.strip_suffix(&suffix).map(str::to_string)).collect();
    if offered.is_empty() {
        return Err(format!("release {tag} has no CUDA archive for {platform}").into());
    }
    if let Ok(wanted) = env::var("AUDIOCPP_CUDA_VERSION") {
        return offered
            .into_iter()
            .find(|v| *v == wanted)
            .ok_or_else(|| format!("release {tag} has no CUDA {wanted} archive for {platform}").into());
    }
    // The oldest toolkit asks the least of the installed driver.
    offered.sort_by_key(|v| v.split('.').map(|p| p.parse::<u32>().unwrap_or(0)).collect::<Vec<_>>());
    Ok(offered.remove(0))
}

fn release_tag() -> String {
    env::var("AUDIOCPP_RELEASE").ok().filter(|t| !t.trim().is_empty()).unwrap_or_else(|| PREBUILT_RELEASE.into())
}

fn github_release() -> Result<serde_json::Value, BoxError> {
    let url = match release_tag().as_str() {
        "latest" => format!("{}/repos/{UPSTREAM}/releases/latest", github_api()),
        tag => format!("{}/repos/{UPSTREAM}/releases/tags/{tag}", github_api()),
    };
    let body = get(&url)?.body_mut().read_to_string()?;
    Ok(serde_json::from_str(&body)?)
}

/// Downloads a release asset, verifies its sha256 when known, and unpacks it
/// into `dest`.
fn fetch_asset(asset: &Asset, dest: &Path) -> Result<(), BoxError> {
    fs::create_dir_all(dest)?;
    let archive = dest.join(&asset.name);
    let actual = download(&asset.url, &archive)?;
    match asset.digest.as_deref() {
        Some(expected) if expected == actual => {}
        Some(expected) => {
            fs::remove_file(&archive).ok();
            return Err(format!("{}: sha256 {actual} does not match {expected}", asset.name).into());
        }
        None => println!("cargo:warning={}: release publishes no digest; not verified", asset.name),
    }
    unpack(&archive, dest)?;
    fs::remove_file(&archive).ok();
    Ok(())
}

// ---------------------------------------------------------------------------
// Source build
// ---------------------------------------------------------------------------

fn source_dir() -> PathBuf {
    if let Some(dir) = env::var_os("AUDIOCPP_SRC_DIR") {
        let dir = PathBuf::from(dir);
        // A user-supplied tree may be any revision; it only has to be compatible.
        if let Err(e) = check_abi(&dir.join("include/audiocpp.h")) {
            panic!("AUDIOCPP_SRC_DIR {}: {e}", dir.display());
        }
        return dir;
    }
    let cache = cache_dir("source");
    let source = cache.join(format!("audio.cpp-{SOURCE_COMMIT}"));
    let marker = source.join(".complete");
    if marker.exists() {
        return source;
    }
    let _lock = CacheLock::acquire(&source).unwrap_or_else(|e| panic!("cannot lock {}: {e}", source.display()));
    if marker.exists() {
        return source;
    }
    if offline() {
        panic!("offline, and the audio.cpp source is not cached; set AUDIOCPP_SRC_DIR or AUDIOCPP_LIB_DIR");
    }
    // Unpack into a staging directory and rename, so an interrupted download
    // never leaves a tree that looks complete.
    let staging = cache.join(format!(".staging-{SOURCE_COMMIT}"));
    if staging.exists() {
        fs::remove_dir_all(&staging).unwrap();
    }
    if source.exists() {
        fs::remove_dir_all(&source).unwrap();
    }
    fs::create_dir_all(&staging).unwrap();
    let url = format!("https://github.com/{UPSTREAM}/archive/{SOURCE_COMMIT}.tar.gz");
    let result: Result<(), BoxError> = (|| {
        let mut response = get(&url)?;
        tar::Archive::new(flate2::read::GzDecoder::new(response.body_mut().as_reader())).unpack(&staging)?;
        Ok(())
    })();
    if let Err(e) = result {
        fs::remove_dir_all(&staging).ok();
        panic!("failed to download audio.cpp source from {url}: {e}");
    }
    let unpacked = staging.join(format!("audio.cpp-{SOURCE_COMMIT}"));
    // The pinned commit must carry exactly the header the bindings were
    // generated from; anything else means the pin and the snapshot drifted.
    let normalize = |path: &Path| fs::read_to_string(path).map(|text| text.replace("\r\n", "\n"));
    if normalize(&unpacked.join("include/audiocpp.h")).ok() != normalize(&bindings_header()).ok() {
        fs::remove_dir_all(&staging).ok();
        panic!(
            "audiocpp.h at {SOURCE_COMMIT} differs from include/audiocpp.h; update the pin or regenerate the bindings"
        );
    }
    fs::rename(&unpacked, &source).unwrap();
    fs::remove_dir_all(&staging).ok();
    fs::write(&marker, SOURCE_COMMIT).unwrap();
    source
}

/// Everything that changes the native build's output, hashed into the cache key.
fn build_key(defines: &[(String, String)]) -> String {
    let mut hasher = Sha256::new();
    let mut add = |label: &str, value: &str| {
        hasher.update(label.as_bytes());
        hasher.update([0]);
        hasher.update(value.as_bytes());
        hasher.update([0]);
    };
    add("layout", &CACHE_LAYOUT.to_string());
    add("commit", SOURCE_COMMIT);
    add("target", &env::var("TARGET").unwrap_or_default());
    for (key, value) in defines {
        add(key, value);
    }
    for var in ["CC", "CXX", "CFLAGS", "CXXFLAGS", "CMAKE_GENERATOR", "CMAKE_TOOLCHAIN_FILE", "VULKAN_SDK", "CUDA_PATH"]
    {
        add(var, &env::var(var).unwrap_or_default());
    }
    hex(&hasher.finalize())[..16].to_string()
}

fn cmake_defines() -> Vec<(String, String)> {
    let on_off = |enabled: bool| if enabled { "ON" } else { "OFF" }.to_string();
    let macos = env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "macos");
    let mut defines = vec![
        ("AUDIOCPP_BUILD_C_API".into(), "ON".into()),
        // Compile model specs into the library, so it does not look for a
        // model_specs directory next to wherever it is deployed.
        ("AUDIOCPP_DEPLOYMENT_BUILD".into(), "ON".into()),
        ("ENGINE_ENABLE_CUDA".into(), on_off(feature("cuda"))),
        ("ENGINE_ENABLE_HIP".into(), on_off(feature("hip"))),
        ("ENGINE_ENABLE_VULKAN".into(), on_off(feature("vulkan"))),
        // Metal is how audio.cpp runs on a Mac, and what the prebuilt macOS
        // archives carry, so it is on there whether or not the feature is.
        ("ENGINE_ENABLE_METAL".into(), on_off(feature("metal") || macos)),
        // Apple's compilers ship no OpenMP runtime; upstream's macOS builds
        // go without it too.
        ("ENGINE_ENABLE_OPENMP".into(), on_off(feature("openmp") && !macos)),
        ("ENGINE_ENABLE_NATIVE_CPU".into(), on_off(feature("native"))),
        ("ENGINE_BUILD_EXAMPLES".into(), "OFF".into()),
        ("ENGINE_BUILD_TESTS".into(), "OFF".into()),
    ];
    if let Ok(set) = env::var("AUDIOCPP_MODEL_SET") {
        defines.push(("AUDIOCPP_MODEL_SET".into(), set));
    }
    if let Ok(models) = env::var("AUDIOCPP_MODELS") {
        defines.push(("AUDIOCPP_MODELS".into(), models));
    }
    if let Ok(args) = env::var("AUDIOCPP_CMAKE_ARGS") {
        for arg in args.split_whitespace() {
            let define = arg.strip_prefix("-D").unwrap_or(arg);
            match define.split_once('=') {
                // Drop a `:TYPE` suffix; cmake::Config::define adds none.
                Some((key, value)) => defines.push((key.split_once(':').map_or(key, |(k, _)| k).into(), value.into())),
                None => panic!("AUDIOCPP_CMAKE_ARGS entry is not -DKEY=VALUE: {arg}"),
            }
        }
    }
    defines
}

/// Refuses cross builds CMake cannot do on its own: different operating
/// systems need a toolchain file. Apple's compilers build either macOS
/// architecture, which the cmake crate selects.
fn check_cross_build() {
    let (host, target) = (env::var("HOST").unwrap_or_default(), env::var("TARGET").unwrap_or_default());
    if host == target || env::var_os("CMAKE_TOOLCHAIN_FILE").is_some() {
        return;
    }
    let apple = |triple: &str| triple.ends_with("apple-darwin");
    if apple(&host) && apple(&target) {
        return;
    }
    panic!(
        "building audio.cpp from source for {target} on {host} needs CMAKE_TOOLCHAIN_FILE, \
         or point AUDIOCPP_LIB_DIR at a library built for {target}"
    );
}

/// CMake refuses a build tree configured for another source directory.
fn discard_stale_cmake_cache(build_root: &Path, source: &Path) {
    let build = build_root.join("build");
    let Ok(cache) = fs::read_to_string(build.join("CMakeCache.txt")) else { return };
    let configured = cache.lines().find_map(|line| line.strip_prefix("CMAKE_HOME_DIRECTORY:INTERNAL="));
    let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    };
    if configured.is_some_and(|dir| !same(Path::new(dir), source)) {
        fs::remove_dir_all(&build).ok();
    }
}

/// Upstream sends libraries and runtimes to `<build>/bin`, possibly under a
/// per-configuration directory for multi-config generators (Visual Studio).
fn built_lib_dir(build_root: &Path) -> Option<PathBuf> {
    let bin = build_root.join("build").join("bin");
    [bin.join("Release"), bin].into_iter().find(|dir| has_library(dir))
}

fn build_from_source() -> PathBuf {
    check_cross_build();
    let source = source_dir();
    if !source.join("CMakeLists.txt").exists() {
        panic!("no audio.cpp CMakeLists.txt under {}", source.display());
    }
    let defines = cmake_defines();

    // A local tree changes under us, so its builds are not cached across
    // checkouts; the pinned commit's are.
    let local = env::var_os("AUDIOCPP_SRC_DIR").is_some();
    let build_root = if local {
        for path in ["CMakeLists.txt", "include", "src", "external/ggml", "model_specs"] {
            println!("cargo:rerun-if-changed={}", source.join(path).display());
        }
        out_dir().join("native")
    } else {
        cache_dir("native").join(build_key(&defines))
    };
    let _lock = (!local).then(|| CacheLock::acquire(&build_root).unwrap_or_else(|e| panic!("cannot lock cache: {e}")));
    let marker = build_root.join(".complete");
    if !local && marker.exists() {
        if let Some(dir) = built_lib_dir(&build_root) {
            return dir;
        }
    }
    fs::create_dir_all(&build_root).unwrap();
    discard_stale_cmake_cache(&build_root, &source);

    let mut config = cmake::Config::new(&source);
    config
        .out_dir(&build_root)
        // An unoptimised inference engine is unusable, whatever the Rust profile.
        .profile("Release")
        .build_target("audiocpp")
        .define("CMAKE_PROJECT_AudioCpp_INCLUDE", manifest_dir().join("cmake/short-paths.cmake"));
    for (key, value) in &defines {
        config.define(key, value);
    }
    if env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|e| e == "msvc") {
        // Upstream sources contain non-ASCII literals; MSVC otherwise decodes
        // them with the machine's ANSI code page.
        config.cflag("/utf-8").cxxflag("/utf-8");
    }
    config.build();

    let dir = built_lib_dir(&build_root)
        .unwrap_or_else(|| panic!("libaudiocpp was not found under {}", build_root.join("build/bin").display()));
    if !local {
        fs::write(&marker, SOURCE_COMMIT).unwrap();
    }
    dir
}
