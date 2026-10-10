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

/// Write the entries (idempotent). Returns joined output for error display.
pub fn install(exe: &std::path::Path) -> Result<(), String> {
    run_all(build_reg_args(exe))
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
}
