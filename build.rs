//! Build script: embed the Windows application icon into the executable so the
//! .exe shows the daftie logo in Explorer, the taskbar and the title bar.
fn main() {
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=assets/icon.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        if let Err(e) = res.compile() {
            // Don't fail the whole build if the resource compiler is missing;
            // the icon is cosmetic.
            println!("cargo:warning=could not embed icon: {e}");
        }
    }
}
