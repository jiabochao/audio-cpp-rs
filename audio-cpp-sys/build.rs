//! Locates or builds `libaudiocpp`, the audio.cpp C ABI shared library.
//!
//! Two modes:
//!
//! * `AUDIOCPP_LIB_DIR=/path/to/libs` links against a prebuilt library, such as
//!   the `libs/` directory of an `audio-<tag>-lib-<platform>-<backend>` release
//!   archive. Nothing is compiled.
//! * Otherwise the vendored `audio.cpp` submodule is built with CMake
//!   (`-DAUDIOCPP_BUILD_C_API=ON`). Backends follow the crate features.
//!
//! Environment knobs for the source build:
//!
//! * `AUDIOCPP_MODEL_SET`  `full` (default), `core`, or `custom`
//! * `AUDIOCPP_MODELS`     model list for `AUDIOCPP_MODEL_SET=custom`
//! * `AUDIOCPP_CMAKE_ARGS` extra `-D...` arguments, whitespace separated
//!
//! The library is always linked dynamically: upstream only builds the C ABI as
//! a shared library. `cargo run` / `cargo test` find it automatically because
//! it lives inside the target directory; for deployment ship it next to the
//! executable. Its directory is exported to dependent build scripts as
//! `DEP_AUDIOCPP_LIB_DIR`.

use std::env;
use std::path::{Path, PathBuf};

fn main() {
    for var in ["AUDIOCPP_LIB_DIR", "AUDIOCPP_MODEL_SET", "AUDIOCPP_MODELS", "AUDIOCPP_CMAKE_ARGS", "DOCS_RS"] {
        println!("cargo:rerun-if-env-changed={var}");
    }

    // docs.rs has neither the time nor the toolchains to build audio.cpp, and
    // documentation needs only the declarations.
    if env::var_os("DOCS_RS").is_some() {
        return;
    }

    let lib_dir = match env::var_os("AUDIOCPP_LIB_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => build_from_source(),
    };

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=dylib=audiocpp");
    println!("cargo:lib_dir={}", lib_dir.display());
}

fn build_from_source() -> PathBuf {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let source = manifest_dir.join("audio.cpp");
    if !source.join("CMakeLists.txt").exists() {
        panic!(
            "audio.cpp sources not found at {}. Run `git submodule update --init` \
             or set AUDIOCPP_LIB_DIR to a prebuilt libaudiocpp.",
            source.display()
        );
    }
    for path in ["CMakeLists.txt", "include", "src", "external/ggml", "model_specs"] {
        println!("cargo:rerun-if-changed={}", source.join(path).display());
    }

    let on_off = |enabled: bool| if enabled { "ON" } else { "OFF" };
    let feature = |name: &str| env::var_os(format!("CARGO_FEATURE_{}", name.to_uppercase())).is_some();

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
    // import library also lands there (ARCHIVE_OUTPUT_DIRECTORY), possibly under
    // a per-configuration subdirectory for multi-config generators.
    let bin = out.join("build").join("bin");
    let candidates = [bin.join("Release"), bin.clone()];
    candidates
        .into_iter()
        .find(|dir| has_library(dir))
        .unwrap_or_else(|| panic!("libaudiocpp was not found under {}", bin.display()))
}

fn has_library(dir: &Path) -> bool {
    ["libaudiocpp.so", "libaudiocpp.dylib", "audiocpp.lib", "libaudiocpp.dll.a"]
        .iter()
        .any(|name| dir.join(name).exists())
}
