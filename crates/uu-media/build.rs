fn main() {
    println!("cargo:rerun-if-changed=src/vpl/native.c");
    println!("cargo:rerun-if-changed=../../vendor/libvpl-api/include");
    let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if std::env::var_os("CARGO_FEATURE_INTEL_VPL").is_none()
        || !matches!(target.as_str(), "linux" | "windows")
    {
        return;
    }
    cc::Build::new()
        .file("src/vpl/native.c")
        .include("../../vendor/libvpl-api/include")
        .define("MFX_DEPRECATED_OFF", None)
        .warnings(true)
        .compile("eclipse_vpl_adapter");
    if target == "linux" {
        println!("cargo:rustc-link-lib=dl");
    }
    if target == "windows" {
        println!("cargo:rustc-link-lib=winmm");
    }
}
