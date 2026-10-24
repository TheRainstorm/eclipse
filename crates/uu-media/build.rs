fn main() {
    println!("cargo:rerun-if-changed=src/vpl/native.c");
    println!("cargo:rerun-if-changed=../../vendor/libvpl-api/include");
    let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    println!("cargo:rerun-if-changed=src/amf/native.cpp");
    println!("cargo:rerun-if-changed=../../vendor/amf-api/include");
    println!("cargo:rerun-if-changed=src/d3d11_color.cpp");
    println!("cargo:rerun-if-changed=src/hdr_present.cpp");
    println!("cargo:rerun-if-changed=src/d3d11_hdr_display.h");
    if target=="windows" && (std::env::var_os("CARGO_FEATURE_MF_ENCODE").is_some() || std::env::var_os("CARGO_FEATURE_D3D11VA").is_some()) {
        cc::Build::new().cpp(true).file("src/d3d11_color.cpp").file("src/hdr_present.cpp").flag_if_supported("-std=c++17").warnings(true).compile("eclipse_hdr_color");
        println!("cargo:rustc-link-lib=d3dcompiler");
        println!("cargo:rustc-link-lib=dcomp");
        println!("cargo:rustc-link-lib=user32");
        println!("cargo:rustc-link-lib=ole32");
        println!("cargo:rustc-link-lib=d3d11");
    }
    if target=="windows" && std::env::var_os("CARGO_FEATURE_MF_ENCODE").is_some() {
        cc::Build::new().cpp(true).file("src/amf/native.cpp")
            .include("../../vendor/amf-api/include").flag_if_supported("-std=c++17")
            .warnings(true).compile("eclipse_amf_adapter");
        println!("cargo:rustc-link-lib=d3d11");
        println!("cargo:rustc-link-lib=dxgi");
    }
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
