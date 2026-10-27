fn main() {
    println!("cargo:rerun-if-changed=assets/windows.rc");
    println!("cargo:rerun-if-changed=assets/rumstsc.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var_os("CARGO_FEATURE_GUI").is_some()
    {
        embed_resource::compile_for("assets/windows.rc", ["rumstsc"], embed_resource::NONE)
            .manifest_required()
            .expect("Unable to embed the application icon");
    }
}
