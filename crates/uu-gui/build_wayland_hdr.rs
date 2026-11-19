//! Build the native presenter into the VAAPI Linux client. Protocol bindings
//! come from the bundled XML so the distribution's protocol version is immaterial.
use std::{env, path::PathBuf, process::Command};

pub fn compile() {
    let src = PathBuf::from("native/wayland_hdr");
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let mut build = cc::Build::new();
    build.include(&output).include(&src);
    for file in ["presenter.c", "compose.c"] {
        build.file(src.join(file));
    }
    for (xml, output_name) in [
        ("color-management-v1", "color-management-v1"),
        ("linux-dmabuf-v1", "linux-dmabuf"),
        ("presentation-time", "presentation-time"),
        ("viewporter", "viewporter"),
    ] {
        let xml = src.join("protocols").join(format!("{xml}.xml"));
        for (command, extension) in [("client-header", "h"), ("private-code", "c")] {
            let generated = output.join(format!("{output_name}.{extension}"));
            let status = Command::new("wayland-scanner")
                .arg(command)
                .arg(&xml)
                .arg(&generated)
                .status()
                .expect("wayland-scanner is required to build the Linux HDR presenter");
            assert!(
                status.success(),
                "failed to generate {}",
                generated.display()
            );
            if extension == "c" {
                build.file(generated);
            }
        }
    }
    let libraries = ["libdrm", "epoxy", "gbm", "wayland-client"];
    for name in libraries {
        let library = pkg_config::Config::new()
            .cargo_metadata(false)
            .probe(name)
            .unwrap_or_else(|err| panic!("missing {name} development files: {err}"));
        build.includes(library.include_paths);
    }
    build.compile("eclipse_wayland_hdr");
    for name in ["epoxy", "gbm", "wayland-client"] {
        pkg_config::probe_library(name).expect("native display library disappeared");
    }
    println!("cargo:rerun-if-changed={}", src.display());
}
