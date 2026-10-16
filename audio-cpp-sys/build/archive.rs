//! ZIP extraction for the build script, kept in its own file so it can also
//! be compiled as a test target (`cargo test -p audio-cpp-sys --test archive`).

use std::fs;
use std::io::Read;
use std::path::Path;

type BoxError = Box<dyn std::error::Error>;

/// Extracts a ZIP archive: stored and deflated entries, no zip64, no
/// encryption — what upstream's Windows release archives are. Written out
/// here rather than taken from the `zip` crate, which yanks every older
/// release whenever it publishes, breaking the build of anyone downstream.
pub fn unzip(bytes: &[u8], dest: &Path) -> Result<(), BoxError> {
    let u16_at = |at: usize| -> Result<u16, BoxError> {
        bytes.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).ok_or_else(|| "truncated zip".into())
    };
    let u32_at = |at: usize| -> Result<u32, BoxError> {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .ok_or_else(|| "truncated zip".into())
    };
    // The end-of-central-directory record sits in the last 64 KiB + 22 bytes.
    let search_from = bytes.len().saturating_sub(22 + 0xFFFF);
    let eocd = (search_from..bytes.len().saturating_sub(21))
        .rev()
        .find(|&at| bytes[at..at + 4] == [0x50, 0x4b, 0x05, 0x06])
        .ok_or("not a zip archive")?;
    let entries = u16_at(eocd + 10)? as usize;
    let mut at = u32_at(eocd + 16)? as usize;
    if entries == 0xFFFF || at == 0xFFFF_FFFF {
        return Err("zip64 archives are not supported".into());
    }
    for _ in 0..entries {
        if u32_at(at)? != 0x0201_4b50 {
            return Err("corrupt zip central directory".into());
        }
        let flags = u16_at(at + 8)?;
        let method = u16_at(at + 10)?;
        let compressed = u32_at(at + 20)? as usize;
        let size = u32_at(at + 24)? as usize;
        let (name_len, extra_len, comment_len) =
            (u16_at(at + 28)? as usize, u16_at(at + 30)? as usize, u16_at(at + 32)? as usize);
        let local = u32_at(at + 42)? as usize;
        let name =
            String::from_utf8_lossy(bytes.get(at + 46..at + 46 + name_len).ok_or("truncated zip")?).replace('\\', "/");
        at += 46 + name_len + extra_len + comment_len;

        if flags & 1 != 0 {
            return Err(format!("{name}: encrypted zip entries are not supported").into());
        }
        // Never write outside `dest`: no absolute paths, no `..`.
        let relative = Path::new(&name);
        if name.starts_with('/') || !relative.components().all(|c| matches!(c, std::path::Component::Normal(_))) {
            return Err(format!("unsafe path in zip: {name}").into());
        }
        let target = dest.join(relative);
        if name.ends_with('/') {
            fs::create_dir_all(&target)?;
            continue;
        }
        if u32_at(local)? != 0x0403_4b50 {
            return Err(format!("{name}: corrupt local header").into());
        }
        let data_at = local + 30 + u16_at(local + 26)? as usize + u16_at(local + 28)? as usize;
        let data = bytes.get(data_at..data_at + compressed).ok_or("truncated zip")?;
        let contents = match method {
            0 => data.to_vec(),
            8 => {
                let mut out = Vec::with_capacity(size);
                flate2::read::DeflateDecoder::new(data).read_to_end(&mut out)?;
                out
            }
            other => return Err(format!("{name}: unsupported zip compression method {other}").into()),
        };
        if contents.len() != size {
            return Err(format!("{name}: size mismatch after decompression").into());
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&target, contents)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("audio-cpp-sys-unzip-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fixture(name: &str) -> Vec<u8> {
        fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)).unwrap()
    }

    #[test]
    fn extracts_stored_and_deflated_entries_with_directories() {
        let dest = scratch("ok");
        unzip(&fixture("mixed.zip"), &dest).unwrap();
        assert_eq!(fs::read_to_string(dest.join("libs/stored.txt")).unwrap(), "stored, not compressed\n");
        assert_eq!(fs::read(dest.join("libs/deflated.bin")).unwrap(), vec![7u8; 100_000]);
        assert_eq!(
            fs::read_to_string(dest.join("include/audiocpp.h")).unwrap(),
            "#define AUDIOCPP_ABI_VERSION_MAJOR 0\n"
        );
        fs::remove_dir_all(dest).ok();
    }

    #[test]
    fn refuses_paths_that_escape_the_destination() {
        let dest = scratch("evil");
        let error = unzip(&fixture("traversal.zip"), &dest).unwrap_err();
        assert!(error.to_string().contains("unsafe path"), "{error}");
        assert!(!dest.parent().unwrap().join("escaped.txt").exists());
        fs::remove_dir_all(dest).ok();
    }

    #[test]
    fn rejects_garbage_and_truncation() {
        let dest = scratch("bad");
        assert!(unzip(b"definitely not a zip", &dest).is_err());
        let whole = fixture("mixed.zip");
        assert!(unzip(&whole[..whole.len() / 2], &dest).is_err());
        fs::remove_dir_all(dest).ok();
    }

    /// The real thing, when available: `AUDIOCPP_TEST_ZIP=path/to/audio-*-lib-windows-*.zip`.
    #[test]
    fn extracts_a_release_archive() {
        let Some(path) = std::env::var_os("AUDIOCPP_TEST_ZIP") else { return };
        let dest = scratch("release");
        unzip(&fs::read(path).unwrap(), &dest).unwrap();
        assert!(dest.join("libs/audiocpp.dll").is_file());
        assert!(dest.join("libs/audiocpp.lib").is_file());
        assert!(dest.join("include/audiocpp.h").is_file());
        fs::remove_dir_all(dest).ok();
    }
}
