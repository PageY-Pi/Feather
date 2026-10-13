//! Cross-platform workspace path normalization.
//!
//! Feather receives paths from model/tool callers as well as native APIs. On
//! Windows a Git-Bash path such as `/c/Users/Page/file.rs` is rooted from the
//! point of view of `std::path::Path`, but it is not an NT drive path; using it
//! directly makes Windows resolve it as `C:\c\Users\Page\file.rs`. Normalize
//! that spelling before canonicalization and comparisons.

use std::path::{Path, PathBuf};

/// Convert a Git-Bash drive path to a native Windows drive path.
///
/// Native absolute, verbatim, UNC and relative paths are returned unchanged.
/// On Unix this is intentionally a no-op: `/c/...` is a valid Unix path and
/// must not be interpreted as a Windows path.
pub fn normalize_input(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let raw = path.to_string_lossy();
        let bytes = raw.as_bytes();
        if bytes.len() >= 3
            && bytes[0] == b'/'
            && bytes[1].is_ascii_alphabetic()
            && bytes[2] == b'/'
        {
            let mut native = String::with_capacity(raw.len() + 1);
            native.push((bytes[1] as char).to_ascii_uppercase());
            native.push(':');
            native.push('\\');
            native.push_str(&raw[3..].replace('/', "\\"));
            return PathBuf::from(native);
        }
    }
    path.to_path_buf()
}

/// Return a stable path key for comparisons.
///
/// Windows drive letters are case-insensitive. Verbatim drive and UNC
/// prefixes are equivalent to their non-verbatim forms, so they are removed
/// only for comparison; the returned path is never used for filesystem I/O.
pub fn comparison_key(path: &Path) -> PathBuf {
    let path = normalize_input(path);
    #[cfg(windows)]
    {
        let raw = path.to_string_lossy().replace('/', "\\");
        let plain = raw.strip_prefix(r"\\?\").unwrap_or(&raw);
        let plain = if let Some(unc) = plain.strip_prefix(r"UNC\") {
            format!(r"\\{unc}")
        } else {
            plain.to_string()
        };
        return PathBuf::from(plain.to_ascii_lowercase());
    }
    #[cfg(not(windows))]
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_paths_are_unchanged() {
        let path = Path::new("/c/Users/Page/file.rs");
        if cfg!(windows) {
            assert_eq!(
                normalize_input(path),
                PathBuf::from(r"C:\Users\Page\file.rs")
            );
        } else {
            assert_eq!(normalize_input(path), path);
            assert_eq!(comparison_key(path), path);
        }
    }

    #[cfg(windows)]
    #[test]
    fn native_and_relative_paths_are_not_rewritten() {
        assert_eq!(
            normalize_input(Path::new(r"C:\Users\Page\file.rs")),
            PathBuf::from(r"C:\Users\Page\file.rs")
        );
        assert_eq!(
            normalize_input(Path::new(r".\file.rs")),
            PathBuf::from(r".\file.rs")
        );
    }

    #[cfg(windows)]
    #[test]
    fn verbatim_and_unc_keys_compare_equal() {
        assert_eq!(
            comparison_key(Path::new(r"\\?\C:\Users\Page\file.rs")),
            comparison_key(Path::new(r"c:\users\page\file.rs")),
        );
        assert_eq!(
            comparison_key(Path::new(r"\\?\UNC\server\share\file.rs")),
            comparison_key(Path::new(r"\\SERVER\SHARE\file.rs")),
        );
    }
}
