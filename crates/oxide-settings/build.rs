fn main() {
    println!("cargo:rerun-if-changed=../../resources/app.manifest");
    println!("cargo:rerun-if-changed=../../resources/oxide.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let mut resource = winres::WindowsResource::new();
    resource
        .set_icon("../../resources/oxide.ico")
        .set_manifest_file("../../resources/app.manifest")
        .set("ProductName", "Oxide Monitor Settings")
        .set("FileDescription", "Oxide Monitor settings window")
        .set("CompanyName", "Oxide Monitor contributors")
        .set("LegalCopyright", "MIT OR Apache-2.0");
    resource
        .compile()
        .expect("failed to compile Windows resources (icon/manifest)");
}
