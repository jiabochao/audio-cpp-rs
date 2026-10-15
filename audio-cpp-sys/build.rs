//! Provides `libaudiocpp`, the audio.cpp C ABI shared library, in this order:
//!
//! 1. `AUDIOCPP_LIB_DIR` set: link the library in that directory, e.g. the
//!    `libs/` directory of an unpacked `audio-<tag>-lib-*` release archive.
//! 2. Download the prebuilt release archive matching the target and the
//!    enabled backend feature, verified against GitHub's sha256 digest and
//!    checked for ABI compatibility with `include/audiocpp.h`.
//! 3. Otherwise build from source with CMake: `AUDIOCPP_SRC_DIR` if set, else
//!    the pinned upstream commit [`SOURCE_COMMIT`], downloaded as a tarball.
//!
//! Environment:
//!
//! | variable | |
//! |---|---|
//! | `AUDIOCPP_BUILD` | `auto` (default: 2 then 3), `prebuilt` (2 only), `source` (3 only) |
//! | `AUDIOCPP_RELEASE` | release tag to download; default the latest release |
//! | `AUDIOCPP_CUDA_VERSION` | CUDA flavour of the prebuilt archive, e.g. `12.8`; default the lowest offered |
//! | `AUDIOCPP_PORTABLE` | `1` selects the `-portable` (no AVX2) CPU/Vulkan archive |
//! | `AUDIOCPP_SRC_DIR` | local audio.cpp checkout for source builds |
//! | `AUDIOCPP_MODEL_SET`, `AUDIOCPP_MODELS`, `AUDIOCPP_CMAKE_ARGS` | source build knobs; setting any implies a source build |
//! | `GITHUB_TOKEN` | optional, raises the GitHub API rate limit |
//! | `AUDIOCPP_GITHUB_API` | GitHub API base URL, default `https://api.github.com` (for mirrors) |
//!
//! The library is always linked dynamically. Its directory is exported to
//! dependent build scripts as `DEP_AUDIOCPP_LIB_DIR`.

use std::env;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

const UPSTREAM: &str = "0xShug0/audio.cpp";
/// Upstream commit the bindings were generated from; used for source builds.
const SOURCE_COMMIT: &str = "6796068942c4ae6f55299371983b3d05ef8389ee";

type BoxError = Box<dyn std::error::Error>;

fn main() {
    for var in [
        "AUDIOCPP_LIB_DIR",
        "AUDIOCPP_BUILD",
        "AUDIOCPP_RELEASE",
        "AUDIOCPP_CUDA_VERSION",
        "AUDIOCPP_PORTABLE",
        "AUDIOCPP_SRC_DIR",
        "AUDIOCPP_MODEL_SET",
        "AUDIOCPP_MODELS",
        "AUDIOCPP_CMAKE_ARGS",
        "AUDIOCPP_GITHUB_API",
        "DOCS_RS",
    ] {
        println!("cargo:rerun-if-env-changed={var}");
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=include/audiocpp.h");

    // docs.rs has neither network nor time to build audio.cpp, and
    // documentation needs only the declarations.
    if env::var_os("DOCS_RS").is_some() {
        return;
    }

    let lib_dir = match env::var_os("AUDIOCPP_LIB_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => locate_or_build(),
    };

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=dylib=audiocpp");
    println!("cargo:lib_dir={}", lib_dir.display());
}

fn locate_or_build() -> PathBuf {
    let mode = env::var("AUDIOCPP_BUILD").unwrap_or_else(|_| "auto".into());
    let wants_source_knobs =
        ["AUDIOCPP_MODEL_SET", "AUDIOCPP_MODELS", "AUDIOCPP_CMAKE_ARGS"].iter().any(|v| env::var_os(v).is_some());
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

fn out_dir() -> PathBuf {
    PathBuf::from(env::var("OUT_DIR").unwrap())
}

fn feature(name: &str) -> bool {
    env::var_os(format!("CARGO_FEATURE_{}", name.to_uppercase())).is_some()
}

fn offline() -> bool {
    env::var("CARGO_NET_OFFLINE").is_ok_and(|v| v == "true")
}

// ---------------------------------------------------------------------------
// Prebuilt release archives
// ---------------------------------------------------------------------------

fn download_prebuilt() -> Result<PathBuf, BoxError> {
    if offline() {
        return Err("cargo is offline".into());
    }
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
    let portable = env::var("AUDIOCPP_PORTABLE").is_ok_and(|v| v == "1" || v == "true");

    let release = github_release()?;
    let tag = release["tag_name"].as_str().ok_or("release has no tag_name")?;
    let assets = release["assets"].as_array().ok_or("release has no assets")?;
    let ext = if target_os == "windows" { "zip" } else { "tar.gz" };
    let asset_named = |name: &str| assets.iter().find(|a| a["name"].as_str() == Some(name));

    let (flavour, cudart) = if backend == "cuda" {
        let version = cuda_version(assets, tag, platform, ext)?;
        (format!("cuda{version}"), Some(format!("audio-{tag}-cudart-{platform}-cuda{version}.{ext}")))
    } else if portable && backend != "metal" {
        (format!("{backend}-portable"), None)
    } else {
        (backend.to_string(), None)
    };
    let name = format!("audio-{tag}-lib-{platform}-{flavour}.{ext}");
    let asset = asset_named(&name).ok_or_else(|| format!("release {tag} has no {name}"))?;

    let dest = out_dir().join("prebuilt").join(name.trim_end_matches(&format!(".{ext}")));
    // The marker records what was unpacked, so a re-uploaded asset is fetched again.
    let marker = dest.join(".complete");
    let stamp = format!("{name} {}", asset["digest"].as_str().unwrap_or("-"));
    if fs::read_to_string(&marker).ok().as_deref() != Some(stamp.as_str()) {
        if dest.exists() {
            fs::remove_dir_all(&dest)?;
        }
        fs::create_dir_all(&dest)?;
        fetch_asset(asset, &dest)?;
        if let Some(cudart) = cudart {
            let asset = asset_named(&cudart).ok_or_else(|| format!("release {tag} has no {cudart}"))?;
            fetch_asset(asset, &dest.join("libs"))?;
        }
        check_abi(&dest.join("include").join("audiocpp.h"))?;
        fs::write(&marker, &stamp)?;
    }
    let libs = dest.join("libs");
    if !has_library(&libs) {
        return Err(format!("{name} contains no libaudiocpp under libs/").into());
    }
    Ok(libs)
}

fn cuda_version(assets: &[serde_json::Value], tag: &str, platform: &str, ext: &str) -> Result<String, BoxError> {
    let prefix = format!("audio-{tag}-lib-{platform}-cuda");
    let suffix = format!(".{ext}");
    let mut offered: Vec<String> = assets
        .iter()
        .filter_map(|a| a["name"].as_str()?.strip_prefix(&prefix)?.strip_suffix(&suffix).map(str::to_string))
        .collect();
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

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(30 * 60))).build().into()
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

fn github_release() -> Result<serde_json::Value, BoxError> {
    let url = match env::var("AUDIOCPP_RELEASE") {
        Ok(tag) => format!("{}/repos/{UPSTREAM}/releases/tags/{tag}", github_api()),
        Err(_) => format!("{}/repos/{UPSTREAM}/releases/latest", github_api()),
    };
    let body = get(&url)?.body_mut().read_to_string()?;
    Ok(serde_json::from_str(&body)?)
}

/// Downloads a release asset, verifies its sha256 digest when GitHub
/// publishes one, and unpacks it into `dest`.
fn fetch_asset(asset: &serde_json::Value, dest: &Path) -> Result<(), BoxError> {
    use sha2::{Digest, Sha256};

    let name = asset["name"].as_str().ok_or("asset has no name")?;
    let url = asset["browser_download_url"].as_str().ok_or("asset has no download url")?;
    let archive = out_dir().join(name);

    let mut response = get(url)?;
    let mut reader = response.body_mut().as_reader();
    let mut file = fs::File::create(&archive)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        io::Write::write_all(&mut file, &buffer[..n])?;
    }
    drop(file);

    let actual: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    match asset["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")) {
        Some(expected) if expected.eq_ignore_ascii_case(&actual) => {}
        Some(expected) => {
            fs::remove_file(&archive).ok();
            return Err(format!("{name}: sha256 {actual} does not match published {expected}").into());
        }
        None => println!("cargo:warning={name}: release publishes no digest; not verified"),
    }

    fs::create_dir_all(dest)?;
    let file = fs::File::open(&archive)?;
    if name.ends_with(".zip") {
        zip::ZipArchive::new(file)?.extract(dest)?;
    } else {
        tar::Archive::new(flate2::read::GzDecoder::new(file)).unpack(dest)?;
    }
    fs::remove_file(&archive).ok();
    Ok(())
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
    let ours = abi_version(&PathBuf::from(env::var("CARGO_MANIFEST_DIR")?).join("include/audiocpp.h"))?;
    let theirs = abi_version(header)?;
    if theirs.0 != ours.0 || theirs.1 < ours.1 {
        return Err(format!(
            "prebuilt ABI {}.{} is incompatible with bindings for {}.{}",
            theirs.0, theirs.1, ours.0, ours.1
        )
        .into());
    }
    Ok(())
}

fn has_library(dir: &Path) -> bool {
    ["libaudiocpp.so", "libaudiocpp.dylib", "audiocpp.lib", "libaudiocpp.dll.a"]
        .iter()
        .any(|name| dir.join(name).exists())
}

// ---------------------------------------------------------------------------
// Source build
// ---------------------------------------------------------------------------

fn source_dir() -> PathBuf {
    if let Some(dir) = env::var_os("AUDIOCPP_SRC_DIR") {
        return PathBuf::from(dir);
    }
    let root = out_dir().join("source");
    let source = root.join(format!("audio.cpp-{SOURCE_COMMIT}"));
    if source.join(".complete").exists() {
        return source;
    }
    if offline() {
        panic!("cargo is offline and no audio.cpp source is available; set AUDIOCPP_SRC_DIR or AUDIOCPP_LIB_DIR");
    }
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    fs::create_dir_all(&root).unwrap();
    let url = format!("https://github.com/{UPSTREAM}/archive/{SOURCE_COMMIT}.tar.gz");
    let result: Result<(), BoxError> = (|| {
        let mut response = get(&url)?;
        tar::Archive::new(flate2::read::GzDecoder::new(response.body_mut().as_reader())).unpack(&root)?;
        Ok(())
    })();
    if let Err(e) = result {
        panic!("failed to download audio.cpp source from {url}: {e}");
    }
    fs::write(source.join(".complete"), SOURCE_COMMIT).unwrap();
    source
}

fn build_from_source() -> PathBuf {
    let source = source_dir();
    if !source.join("CMakeLists.txt").exists() {
        panic!("no audio.cpp CMakeLists.txt under {}", source.display());
    }
    if env::var_os("AUDIOCPP_SRC_DIR").is_some() {
        for path in ["CMakeLists.txt", "include", "src", "external/ggml", "model_specs"] {
            println!("cargo:rerun-if-changed={}", source.join(path).display());
        }
    }

    let on_off = |enabled: bool| if enabled { "ON" } else { "OFF" };
    let mut config = cmake::Config::new(&source);
    config
        // An unoptimised inference engine is unusable, whatever the Rust profile.
        .profile("Release")
        .build_target("audiocpp")
        .define("AUDIOCPP_BUILD_C_API", "ON")
        .define("ENGINE_ENABLE_CUDA", on_off(feature("cuda")))
        .define("ENGINE_ENABLE_HIP", on_off(feature("hip")))
        .define("ENGINE_ENABLE_VULKAN", on_off(feature("vulkan")))
        .define("ENGINE_ENABLE_METAL", on_off(feature("metal")))
        .define("ENGINE_ENABLE_OPENMP", on_off(feature("openmp")))
        .define("ENGINE_ENABLE_NATIVE_CPU", on_off(feature("native")))
        .define("ENGINE_BUILD_EXAMPLES", "OFF")
        .define("ENGINE_BUILD_TESTS", "OFF");

    if let Ok(set) = env::var("AUDIOCPP_MODEL_SET") {
        config.define("AUDIOCPP_MODEL_SET", set);
    }
    if let Ok(models) = env::var("AUDIOCPP_MODELS") {
        config.define("AUDIOCPP_MODELS", models);
    }
    if let Ok(args) = env::var("AUDIOCPP_CMAKE_ARGS") {
        for arg in args.split_whitespace() {
            let define = arg.strip_prefix("-D").unwrap_or(arg);
            match define.split_once('=') {
                Some((key, value)) => {
                    // Drop a `:TYPE` suffix; cmake::Config::define adds none.
                    let key = key.split_once(':').map_or(key, |(k, _)| k);
                    config.define(key, value);
                }
                None => panic!("AUDIOCPP_CMAKE_ARGS entry is not -DKEY=VALUE: {arg}"),
            }
        }
    }

    let out = config.build();
    // Upstream sends both libraries and runtimes to <build>/bin. On Windows the
    // import library also lands there, possibly under a per-configuration
    // subdirectory for multi-config generators.
    let bin = out.join("build").join("bin");
    [bin.join("Release"), bin.clone()]
        .into_iter()
        .find(|dir| has_library(dir))
        .unwrap_or_else(|| panic!("libaudiocpp was not found under {}", bin.display()))
}
