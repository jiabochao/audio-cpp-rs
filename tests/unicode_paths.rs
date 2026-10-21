//! Models under non-ASCII paths, such as a Windows profile named `张三`.
//!
//! On Windows this runs under the runner's ANSI code page (1252), where the
//! library cannot read such a path itself, so it exercises the bindings'
//! fallback to the 8.3 alias (see `src/winpath.rs`) — the situation of an
//! application that does not select UTF-8 in its manifest.

use std::path::{Path, PathBuf};

use audio_cpp::{ModelConfig, Registry};

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
    }
}

#[test]
fn loads_a_model_from_a_non_ascii_directory() {
    let root = std::env::temp_dir().join(format!("audio-cpp 模型 Ünïcode {}", std::process::id()));
    let model: PathBuf = root.join("语音检测").join("silero_vad");
    copy_dir(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/silero_vad"), &model);

    let registry = Registry::new().unwrap();
    let loaded = registry.load_model(&model, &ModelConfig::family("silero_vad"), None);
    std::fs::remove_dir_all(&root).ok();
    let loaded = loaded.unwrap();
    assert_eq!(loaded.info().unwrap().family, "silero_vad");
}
