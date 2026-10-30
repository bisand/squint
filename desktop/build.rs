//! Puts the icon into the Windows executable.
//!
//! Windows reads an application's icon from the binary's own resources. The
//! .ico beside it is the installer's business and reaches nothing else, so
//! without this Explorer, the taskbar and the window all fall back to the
//! generic one.

fn main() {
    println!("cargo::rerun-if-changed=../packaging/icons/squint.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut resource = winresource::WindowsResource::new();
    resource.set_icon("../packaging/icons/squint.ico");
    resource.compile().expect("squint.ico into the executable");
}
