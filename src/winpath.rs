//! Paths across the C ABI on Windows.
//!
//! The library builds `std::filesystem::path` from the `char *` it is handed,
//! which MSVC decodes in the process's ANSI code page, while ggml opens the
//! same string as UTF-8. The two agree only for ASCII, or when the process
//! code page *is* UTF-8 — which the host executable opts into with
//! `<activeCodePage>UTF-8</activeCodePage>` in its manifest (Windows 10 1903
//! and later). Otherwise a path such as `C:\Users\张三\…` fails to load, or
//! loads a different file. This module passes paths that work and refuses
//! the rest with an error that says what to do.

use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::Path;

use crate::error::{Error, Result};

const CP_UTF8: u32 = 65001;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetACP() -> u32;
    fn GetShortPathNameW(long_path: *const u16, short_path: *mut u16, length: u32) -> u32;
}

/// The bytes to hand the library for `path`.
pub(crate) fn encode(path: &Path) -> Result<Vec<u8>> {
    // SAFETY: no arguments; reads process state.
    encode_with(path, unsafe { GetACP() }, short_path)
}

fn encode_with(path: &Path, code_page: u32, short_path: impl Fn(&Path) -> Option<String>) -> Result<Vec<u8>> {
    let utf8 =
        path.to_str().ok_or_else(|| Error::invalid_argument(format!("path is not valid UTF-8: {}", path.display())))?;
    if utf8.is_ascii() || code_page == CP_UTF8 {
        return Ok(utf8.as_bytes().to_vec());
    }
    // The 8.3 alias of an existing path is ASCII when the volume keeps them,
    // as system drives do by default.
    if let Some(short) = short_path(path).filter(|s| s.is_ascii()) {
        return Ok(short.into_bytes());
    }
    Err(Error::invalid_argument(format!(
        "{}: audio.cpp reads paths in the process code page ({code_page}), which cannot spell this one. \
         Declare <activeCodePage>UTF-8</activeCodePage> in the application manifest, or use an ASCII path",
        path.display()
    )))
}

fn short_path(path: &Path) -> Option<String> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut buffer = vec![0u16; 260];
    loop {
        // SAFETY: `wide` is NUL-terminated; `buffer` holds `len` u16s.
        let written = unsafe { GetShortPathNameW(wide.as_ptr(), buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
        if written == 0 {
            return None;
        }
        if written < buffer.len() {
            buffer.truncate(written);
            return std::ffi::OsString::from_wide(&buffer).into_string().ok();
        }
        // Too small: `written` is the size needed, terminator included.
        buffer.resize(written, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CP_1252: u32 = 1252;

    #[test]
    fn ascii_and_utf8_code_page_pass_through() {
        let none = |_: &Path| None;
        assert_eq!(encode_with(Path::new(r"C:\models\vad"), CP_1252, none).unwrap(), br"C:\models\vad");
        let chinese = Path::new(r"C:\Users\张三\models");
        assert_eq!(encode_with(chinese, CP_UTF8, none).unwrap(), chinese.to_str().unwrap().as_bytes());
    }

    #[test]
    fn other_code_pages_use_the_short_alias_or_explain() {
        let chinese = Path::new(r"C:\Users\张三\models");
        let alias = |_: &Path| Some(r"C:\Users\5F20~1\models".to_string());
        assert_eq!(encode_with(chinese, CP_1252, alias).unwrap(), br"C:\Users\5F20~1\models");

        let error = encode_with(chinese, CP_1252, |_: &Path| None).unwrap_err();
        assert!(error.to_string().contains("activeCodePage"), "{error}");
        // An alias that is still not ASCII is no help.
        let error = encode_with(chinese, CP_1252, |p: &Path| Some(p.to_str().unwrap().into())).unwrap_err();
        assert!(error.to_string().contains("activeCodePage"), "{error}");
    }
}
