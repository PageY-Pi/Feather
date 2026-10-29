//! Explorer right-click integration: "Open Feather here" on directory
//! backgrounds, folders, and the desktop — like "Open in Terminal".
//!
//! Entries live under HKCU (no admin required):
//! - `Software\Classes\Directory\Background\shell\FeatherGUI`  (folder background)
//! - `Software\Classes\Directory\shell\FeatherGUI`             (right-clicked folder)
//! - `Software\Classes\DesktopBackground\Shell\FeatherGUI`     (desktop)
//!
//! The command passes the directory to the GUI (`feather-gui.exe "%V"`);
//! feather also accepts a positional path so `feather-gui.exe some/dir` works
//! from any shell.

/// Registry root we write to (current user — no elevation needed).
pub const REG_ROOT: &str = r"HKCU\Software\Classes";
pub const MENU_KEY: &str = "FeatherGUI";
pub const MENU_LABEL: &str = "Open Feather here";

/// The three shell locations that get the entry: (key suffix, arg the
/// explorer substitutes with the directory).
pub const LOCATIONS: &[(&str, &str)] = &[
    ("Directory\\Background\\shell", "%V"),
    ("Directory\\shell", "%1"),
    ("DesktopBackground\\Shell", "%V"),
];

/// Pure builder for the `reg add` argument sets (tested; install()/uninstall()
/// execute these with `std::process::Command`).
pub fn build_reg_args(exe: &std::path::Path) -> Vec<Vec<String>> {
    // registry values want backslash paths
    let exe = exe.to_string_lossy().replace('/', "\\");
    let mut out = Vec::new();
    for (suffix, dir_arg) in LOCATIONS {
        let key = format!("{REG_ROOT}\\{suffix}\\{MENU_KEY}");
        // display name
        out.push(vec![
            "add".into(),
            key.clone(),
            "/ve".into(),
            "/t".into(),
            "REG_SZ".into(),
            "/d".into(),
            MENU_LABEL.into(),
            "/f".into(),
        ]);
        // brand icon from the exe's embedded resource
        out.push(vec![
            "add".into(),
            key.clone(),
            "/v".into(),
            "Icon".into(),
            "/t".into(),
            "REG_SZ".into(),
            "/d".into(),
            exe.clone(),
            "/f".into(),
        ]);
        // launch command: pass the directory as argv[1]
        let command = format!("\"{exe}\" \"{dir_arg}\"");
        out.push(vec![
            "add".into(),
            format!("{key}\\command"),
            "/ve".into(),
            "/t".into(),
            "REG_SZ".into(),
            "/d".into(),
            command,
            "/f".into(),
        ]);
    }
    out
}

/// Pure builder for the removal argument sets.
pub fn build_unreg_args() -> Vec<Vec<String>> {
    LOCATIONS
        .iter()
        .map(|(suffix, _)| {
            vec![
                "delete".to_string(),
                format!("{REG_ROOT}\\{suffix}\\{MENU_KEY}"),
                "/f".into(),
            ]
        })
        .collect()
}

/// Location under the user's temp directory — where release-verification runs
/// unpack a throwaway copy of the executables.
pub fn is_transient(exe: &std::path::Path) -> bool {
    let path = exe
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
    if path.contains("\\appdata\\local\\temp\\") {
        return true;
    }
    ["TEMP", "TMP"].iter().any(|var| {
        std::env::var(var)
            .ok()
            .map(|value| value.replace('/', "\\").to_ascii_lowercase())
            .filter(|value| value.len() > 3)
            .is_some_and(|temp| path.starts_with(&(temp + "\\")))
    })
}

/// Write the entries (idempotent). Returns joined output for error display.
///
/// A throwaway copy (a verification run unpacked under `%TEMP%`) refuses by
/// default: registering it would repoint the user's real shell menu at a
/// directory that is deleted when verification ends, so every later
/// right-click would fail. `FEATHER_ALLOW_TRANSIENT_CONTEXT_MENU=1` is the
/// explicit opt-in for tests that really want to exercise the registry.
pub fn install(exe: &std::path::Path) -> Result<(), String> {
    if is_transient(exe)
        && std::env::var("FEATHER_ALLOW_TRANSIENT_CONTEXT_MENU")
            .ok()
            .as_deref()
            != Some("1")
    {
        return Err(format!(
            "refusing to register \"{MENU_LABEL}\" from a temporary copy: {}\n\
             Run it from the installed feather-gui.exe instead. If this really is \
             intended, set FEATHER_ALLOW_TRANSIENT_CONTEXT_MENU=1.",
            exe.display()
        ));
    }
    run_all(build_reg_args(exe))
}

/// The executable inside a registered command line:
/// `"C:\apps\feather-gui.exe" "%V"` -> `C:\apps\feather-gui.exe`.
pub fn exe_from_command(command: &str) -> Option<std::path::PathBuf> {
    let command = command.trim();
    let exe = match command.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or_default(),
        None => command.split_whitespace().next().unwrap_or_default(),
    };
    (!exe.is_empty()).then(|| std::path::PathBuf::from(exe))
}

/// Read the command line a menu location currently runs.
fn registered_command(key: &str) -> Option<String> {
    let out = feather_core::process::command("reg")
        .args(["query", key, "/ve"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|line| {
            let value = line.split_once("REG_SZ")?.1.trim().to_string();
            (!value.is_empty()).then_some(value)
        })
}

/// Heal an update that moved or removed the executables: when a registered
/// menu entry points at a file that no longer exists and we are running from a
/// stable location, rewrite the entries to this executable. Returns the stale
/// path that was replaced, if any. A live registration is never touched, so a
/// menu deliberately pointed at another build stays where the user put it.
pub fn repair_if_stale(exe: &std::path::Path) -> Option<std::path::PathBuf> {
    if is_transient(exe) {
        return None;
    }
    let stale = LOCATIONS.iter().find_map(|(suffix, _)| {
        let key = format!("{REG_ROOT}\\{suffix}\\{MENU_KEY}\\command");
        let command = registered_command(&key)?;
        let target = exe_from_command(&command)?;
        (!target.exists()).then_some(target)
    })?;
    install(exe).ok()?;
    Some(stale)
}

/// Remove the entries (missing keys are fine — reg delete /f ignores them
/// only when they exist; we tolerate non-zero exits).
pub fn uninstall() -> Result<(), String> {
    for args in build_unreg_args() {
        let _ = feather_core::process::command("reg").args(&args).output();
    }
    Ok(())
}

fn run_all(arg_sets: Vec<Vec<String>>) -> Result<(), String> {
    for args in arg_sets {
        let out = feather_core::process::command("reg")
            .args(&args)
            .output()
            .map_err(|e| format!("reg: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "reg {} failed: {} {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ));
        }
    }
    Ok(())
}

#[allow(dead_code)]
/// Is the menu currently registered for the running exe? (Best effort: any
/// of the command keys present counts.)
pub fn installed() -> bool {
    for (suffix, _) in LOCATIONS {
        let key = format!("{REG_ROOT}\\{suffix}\\{MENU_KEY}\\command");
        if feather_core::process::command("reg")
            .args(["query", &key])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reg_args_cover_all_locations() {
        let args = build_reg_args(std::path::Path::new("C:/apps/feather-gui.exe"));
        // 3 locations x (label + icon + command)
        assert_eq!(args.len(), 9);
        let flat: Vec<String> = args.iter().flatten().cloned().collect();
        assert!(flat.iter().any(|s| s == MENU_LABEL));
        // the command passes the directory via %V for backgrounds/desktop
        let cmds: Vec<&String> = flat
            .iter()
            .filter(|s| s.contains("feather-gui.exe\" \""))
            .collect();
        assert_eq!(cmds.len(), 3);
        assert!(cmds.iter().any(|c| c.contains("%V")));
        assert!(cmds.iter().any(|c| c.contains("%1")));
        // quoted exe path survives
        assert!(flat.iter().any(|s| s.contains("C:\\apps\\feather-gui.exe")));
    }

    #[test]
    fn unreg_args_delete_all_roots() {
        let args = build_unreg_args();
        assert_eq!(args.len(), 3);
        for a in &args {
            assert_eq!(a[0], "delete");
            assert!(a[1].starts_with("HKCU\\Software\\Classes\\"));
            assert!(a[1].ends_with(MENU_KEY));
        }
    }

    #[test]
    fn command_path_is_extracted_from_quoted_and_bare_lines() {
        assert_eq!(
            exe_from_command("\"C:\\apps\\feather-gui.exe\" \"%V\""),
            Some(std::path::PathBuf::from("C:\\apps\\feather-gui.exe"))
        );
        assert_eq!(
            exe_from_command("C:\\apps\\feather-gui.exe %1"),
            Some(std::path::PathBuf::from("C:\\apps\\feather-gui.exe"))
        );
        assert_eq!(exe_from_command("   "), None);
        assert_eq!(exe_from_command("\"\""), None);
    }

    #[test]
    fn temp_copies_are_transient_but_installed_paths_are_not() {
        // %TEMP% and %LOCALAPPDATA%\Temp copies are throwaway verify builds
        assert!(is_transient(std::path::Path::new(
            "C:/Users/Page/AppData/Local/Temp/feather-114-verify/feather-gui.exe"
        )));
        // stable install locations are accepted
        assert!(!is_transient(std::path::Path::new(
            "C:/Users/Page/AppData/Local/Programs/Feather/feather-gui.exe"
        )));
        assert!(!is_transient(std::path::Path::new(
            "C:/Users/Page/Desktop/feather/target/release/feather-gui.exe"
        )));
    }

    #[test]
    fn transient_install_is_refused_without_the_opt_in() {
        let temp = std::env::temp_dir().join("feather-transient-refusal-test.exe");
        // Only exercise the guard: no registry write happens on the Err path.
        if std::env::var("FEATHER_ALLOW_TRANSIENT_CONTEXT_MENU").is_ok() {
            return; // the opt-in is exported; the guard is intentionally open
        }
        let error = install(&temp).expect_err("a %TEMP% copy must not touch the shell menu");
        assert!(error.contains("temporary copy"), "{error}");
        assert!(
            error.contains("FEATHER_ALLOW_TRANSIENT_CONTEXT_MENU"),
            "{error}"
        );
    }
}
