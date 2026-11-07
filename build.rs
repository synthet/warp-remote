//! Embeds the icon and manifest in the Windows binaries. The manifest is required: without Common
//! Controls v6, the tray app fails to load with an entry-point error.

fn main() {
    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=assets/app.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("assets/app.rc", embed_resource::NONE)
            .manifest_optional()
            .expect("compiling the Windows icon resource");
    }
}
