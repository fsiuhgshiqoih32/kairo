fn main() {
    // The installer shows Kairo's version, kept in one place: the main Cargo.toml.
    println!("cargo:rerun-if-changed=../Cargo.toml");
    let manifest = std::fs::read_to_string("../Cargo.toml").expect("../Cargo.toml");
    let version = manifest
        .lines()
        .find_map(|l| l.strip_prefix("version = \"")?.strip_suffix('"'))
        .expect("a version in ../Cargo.toml");
    println!("cargo:rustc-env=KAIRO_VERSION={version}");

    println!("cargo:rerun-if-changed=../assets/icon.ico");
    println!("cargo:rerun-if-changed=setup.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../assets/icon.ico");
        res.set("ProductName", "Kairo");
        res.set("FileDescription", "Kairo Setup");
        res.set("ProductVersion", version);
        // Runs as the user: without this, Windows sees "setup" in the name and asks for admin.
        res.set_manifest_file("setup.manifest");
        if let Err(e) = res.compile() {
            panic!("could not embed the icon and manifest: {e}");
        }
    }
}
