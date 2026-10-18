//! Resolve an actual Bash runtime, never Windows' WSL launcher or cmd.exe.
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};
/// Resolved Bash runtime. Public because the prerequisite report (and any
/// embedder) must be able to ask what the Bash tool would actually use.
pub struct BashRuntime {
    pub executable: PathBuf,
    pub path: Option<OsString>,
}
impl BashRuntime {
    pub fn discover() -> Result<Self, String> {
        if let Some(value) = std::env::var_os("FEATHER_BASH") {
            let executable = PathBuf::from(value);
            if !executable.is_file() {
                return Err("FEATHER_BASH must point to an existing Bash executable".into());
            }
            #[cfg(windows)]
            if !is_msys_bash(&executable) {
                return Err(
                    "FEATHER_BASH must be Git Bash/MSYS2, not cmd.exe or a WSL launcher".into(),
                );
            }
            return Ok(Self::from_executable(executable));
        }
        #[cfg(windows)]
        {
            let mut candidates = Vec::new();
            // Explorer-started GUIs often have Git's cmd directory but not Bash
            // on PATH. Prefer runtimes alongside an installed Git distribution.
            for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
                candidates.push(dir.join("bash.exe"));
                if dir.join("git.exe").is_file() {
                    if let Some(root) = dir.parent() {
                        candidates.push(root.join("bin/bash.exe"));
                        candidates.push(root.join("usr/bin/bash.exe"));
                        if let Some(root) = root.parent() {
                            candidates.push(root.join("usr/bin/bash.exe"));
                        }
                    }
                }
            }
            for var in [
                "ProgramW6432",
                "ProgramFiles",
                "ProgramFiles(x86)",
                "LOCALAPPDATA",
            ] {
                if let Some(dir) = std::env::var_os(var) {
                    let dir = PathBuf::from(dir);
                    for name in [
                        "Git/bin/bash.exe",
                        "Programs/Git/bin/bash.exe",
                        "Git/usr/bin/bash.exe",
                    ] {
                        candidates.push(dir.join(name));
                    }
                }
            }
            for path in candidates {
                if is_msys_bash(&path) {
                    return Ok(Self::from_executable(path));
                }
            }
            Err("bash: Git Bash/MSYS2 was not found. Install Git for Windows or set FEATHER_BASH to its bash.exe; cmd.exe and the WSL bash launcher are not Bash substitutes.".into())
        }
        #[cfg(not(windows))]
        {
            for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .chain([PathBuf::from("/bin"), PathBuf::from("/usr/bin")])
            {
                let path = dir.join("bash");
                if path.is_file() {
                    return Ok(Self::from_executable(path));
                }
            }
            Err("bash: Bash not found; install Bash or set FEATHER_BASH".into())
        }
    }
    fn from_executable(executable: PathBuf) -> Self {
        #[cfg(windows)]
        let path = msys_root(&executable).and_then(|root| {
            let mut paths = vec![
                root.join("usr/bin"),
                root.join("mingw64/bin"),
                root.join("bin"),
            ];
            // Preserve native programs (cargo/python/etc.) and their ordering.
            paths.extend(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            ));
            std::env::join_paths(paths).ok()
        });
        #[cfg(not(windows))]
        let path = None;
        Self { executable, path }
    }
}
#[cfg(windows)]
fn msys_root(path: &Path) -> Option<PathBuf> {
    let dir = path.parent()?;
    if dir.join("msys-2.0.dll").is_file() {
        if dir.file_name()?.eq_ignore_ascii_case("bin")
            && dir.parent()?.file_name()?.eq_ignore_ascii_case("usr")
        {
            return dir.parent()?.parent().map(Path::to_path_buf);
        }
    }
    let root = dir.parent()?;
    root.join("usr/bin/msys-2.0.dll")
        .is_file()
        .then(|| root.to_path_buf())
}
#[cfg(windows)]
fn is_msys_bash(path: &Path) -> bool {
    path.is_file() && msys_root(path).is_some()
}

/// Bash/MSYS emits UTF-8; Windows-native tools may still emit the OEM page.
/// Decode before text reaches the transcript/UI: fonts cannot repair U+FFFD.
pub(crate) fn decode_output(bytes: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_owned();
    }
    #[cfg(windows)]
    if let Some(text) = decode_windows(bytes, 0) {
        return text;
    }
    String::from_utf8_lossy(bytes).into_owned()
}
#[cfg(windows)]
fn decode_windows(bytes: &[u8], code_page: u32) -> Option<String> {
    #[link(name = "kernel32")]
    extern "system" {
        fn MultiByteToWideChar(
            cp: u32,
            flags: u32,
            input: *const u8,
            len: i32,
            output: *mut u16,
            capacity: i32,
        ) -> i32;
        fn GetOEMCP() -> u32;
    }
    let cp = if code_page == 0 {
        unsafe { GetOEMCP() }
    } else {
        code_page
    };
    let len = i32::try_from(bytes.len()).ok()?;
    let needed =
        unsafe { MultiByteToWideChar(cp, 8, bytes.as_ptr(), len, std::ptr::null_mut(), 0) };
    if needed <= 0 {
        return None;
    }
    let mut out = vec![0; needed as usize];
    let written =
        unsafe { MultiByteToWideChar(cp, 8, bytes.as_ptr(), len, out.as_mut_ptr(), needed) };
    (written > 0).then(|| String::from_utf16_lossy(&out[..written as usize]))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_utf8_is_not_redecoded() {
        assert_eq!(decode_output("中文工具输出\n".as_bytes()), "中文工具输出\n");
    }
    #[cfg(windows)]
    #[test]
    fn native_chinese_cp936_is_decoded_without_replacement_glyphs() {
        assert_eq!(
            decode_windows(&[0xd6, 0xd0, 0xce, 0xc4], 936).as_deref(),
            Some("中文")
        );
    }
    #[cfg(windows)]
    #[test]
    fn rejects_wsl_launcher_and_resolves_real_git_bash() {
        assert!(!is_msys_bash(Path::new("C:/Windows/System32/bash.exe")));
        let runtime = BashRuntime::discover().unwrap();
        assert!(is_msys_bash(&runtime.executable));
        assert!(runtime.path.is_some());
    }
}
