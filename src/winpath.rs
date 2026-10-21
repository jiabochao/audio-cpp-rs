//! Paths across the C ABI on Windows.
//!
//! The library builds `std::filesystem::path` from the `char *` it is handed,
//! which MSVC decodes in the process's ANSI code page, while ggml opens the
//! same string as UTF-8. The two agree only for ASCII, or when the process
//! code page *is* UTF-8 — which the host executable opts into with
//! `<activeCodePage>UTF-8</activeCodePage>` in its manifest (Windows 10 1903
//! and later). Otherwise a path such as `C:\Users\张三\…` fails to load, or
//! loads a different file.
//!
//! Passing the path's 8.3 alias does not help: the loaders canonicalize
//! paths, which expands the alias back to the long name and fails the same
//! way. So such paths are refused up front, with an error that says what to do.

use std::path::Path;

use crate::error::{Error, Result};

const CP_UTF8: u32 = 65001;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetACP() -> u32;
}

/// The bytes to hand the library for `path`.
pub(crate) fn encode(path: &Path) -> Result<Vec<u8>> {
    // SAFETY: no arguments; reads process state.
    encode_with(path, unsafe { GetACP() })
}

fn encode_with(path: &Path, code_page: u32) -> Result<Vec<u8>> {
    let utf8 =
        path.to_str().ok_or_else(|| Error::invalid_argument(format!("path is not valid UTF-8: {}", path.display())))?;
    if utf8.is_ascii() || code_page == CP_UTF8 {
        return Ok(utf8.as_bytes().to_vec());
    }
    Err(Error::invalid_argument(format!(
        "{}: audio.cpp reads paths in the process code page ({code_page}), which cannot hold this one on Windows. \
         Declare <activeCodePage>UTF-8</activeCodePage> in the application manifest, or use an ASCII path",
        path.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_always_passes_and_non_ascii_needs_the_utf8_code_page() {
        assert_eq!(encode_with(Path::new(r"C:\models\vad"), 1252).unwrap(), br"C:\models\vad");
        let chinese = Path::new(r"C:\Users\张三\models");
        assert_eq!(encode_with(chinese, CP_UTF8).unwrap(), chinese.to_str().unwrap().as_bytes());
        let error = encode_with(chinese, 1252).unwrap_err();
        assert!(error.to_string().contains("activeCodePage"), "{error}");
    }
}
