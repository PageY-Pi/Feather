fn main() {
    println!("cargo:rerun-if-changed=assets/feather.ico");
    // Embed the feather icon into the Windows executable so the taskbar,
    // Explorer, and the right-click menu entry all show the brand mark.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/feather.ico");
        if let Err(e) = res.compile() {
            eprintln!("winresource: {e}");
        }
    }
}
