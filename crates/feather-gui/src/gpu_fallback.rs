//! Out-of-process CPU rescue after native-GPU initialization fails. winit only
//! permits one EventLoop per process; never try software_window::run after an
//! eframe failure has already constructed its EventLoop. Keep restart token/home
//! inherited so the child consumes the untouched session/draft handoff exactly once.
use std::path::Path;
pub fn launch_cpu(root:&Path)->std::io::Result<()> {
    let mut command=std::process::Command::new(std::env::current_exe()?);
    command.arg(root).env("FEATHER_GUI_RENDERER","cpu");
    // CPU never initializes a GPU, so there is no retry/fallback process loop.
    #[cfg(windows)]{use std::os::windows::process::CommandExt;command.creation_flags(0x08000000);}
    command.spawn()?;Ok(())
}
