//! Models under non-ASCII paths, such as a Windows profile named `张三`.
//!
//! Unix passes path bytes through. On Windows the library can only open such
//! a path when the process runs under the UTF-8 code page; otherwise the
//! bindings refuse it with an error naming the fix (see `src/winpath.rs`).
//! Windows CI runs it twice: under code page 1252, which checks that error,
//! and linked with `tests/utf8.manifest`, which checks that the load works.

use std::path::{Path, PathBuf};

use audio_cpp::{ModelConfig, Registry};

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
    }
}

#[cfg(windows)]
fn utf8_code_page() -> bool {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetACP() -> u32;
    }
    // SAFETY: no arguments; reads process state.
    unsafe { GetACP() == 65001 }
}

#[cfg(not(windows))]
fn utf8_code_page() -> bool {
    true
}

#[test]
fn loads_a_model_from_a_non_ascii_directory() {
    let root = std::env::temp_dir().join(format!("audio-cpp 模型 Ünïcode {}", std::process::id()));
    let model: PathBuf = root.join("语音检测").join("silero_vad");
    copy_dir(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/silero_vad"), &model);

    // Set by the CI step that links the UTF-8 manifest, so a manifest that
    // failed to take effect cannot pass as the refusal case.
    if std::env::var_os("AUDIOCPP_TEST_EXPECT_UTF8").is_some() {
        assert!(utf8_code_page(), "the UTF-8 manifest did not take effect");
    }
    let registry = Registry::new().unwrap();
    let loaded = registry.load_model(&model, &ModelConfig::family("silero_vad"), None);
    std::fs::remove_dir_all(&root).ok();
    if utf8_code_page() {
        assert_eq!(loaded.unwrap().info().unwrap().family, "silero_vad");
    } else {
        let error = loaded.expect_err("a non-ASCII path must be refused outside the UTF-8 code page");
        assert_eq!(error.kind(), audio_cpp::ErrorKind::InvalidArgument);
        assert!(error.to_string().contains("activeCodePage"), "{error}");
    }
}
