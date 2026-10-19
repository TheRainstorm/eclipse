//! Embed the application icon and product identity into Windows executables.
use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=../../assets/branding/eclipse.ico");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-env-changed=RC");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let icon = root
        .join("../../assets/branding/eclipse.ico")
        .canonicalize()
        .unwrap();
    let icon = icon
        .to_string_lossy()
        .replace('\\', "/")
        .replace('"', "\\\"");
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let numbers: Vec<_> = version
        .split('.')
        .take(3)
        .map(|v| v.parse::<u16>().unwrap())
        .collect();
    let resource = output.join("uu-client.rc");
    fs::write(
        &resource,
        format!(
            r#"
#pragma code_page(65001)
1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
FILEFLAGSMASK 0x3fL
FILEOS 0x40004L
FILETYPE 0x1L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904b0"
        BEGIN
            VALUE "CompanyName", "TheRainstorm\0"
            VALUE "FileDescription", "uu-client remote streaming client\0"
            VALUE "FileVersion", "{version}\0"
            VALUE "InternalName", "uu-client\0"
            VALUE "OriginalFilename", "uu-gui.exe\0"
            VALUE "ProductName", "uu-client\0"
            VALUE "ProductVersion", "{version}\0"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x0409, 1200
    END
END
"#,
            major = numbers[0],
            minor = numbers[1],
            patch = numbers[2]
        ),
    )
    .unwrap();
    let msvc = env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    let object = output.join(if msvc { "uu-client.res" } else { "uu-client.o" });
    let compiler = env::var_os("RC").unwrap_or_else(|| {
        if msvc {
            "rc.exe".into()
        } else if env::var("HOST").unwrap().contains("windows") {
            "windres".into()
        } else {
            "x86_64-w64-mingw32-windres".into()
        }
    });
    let mut command = Command::new(compiler);
    if msvc {
        command
            .arg("/nologo")
            .arg("/fo")
            .arg(&object)
            .arg(&resource);
    } else {
        command
            .arg("-i")
            .arg(&resource)
            .arg("-O")
            .arg("coff")
            .arg("-o")
            .arg(&object);
    }
    let result = command
        .status()
        .expect("Windows resource compiler is required (MinGW windres or Windows SDK rc.exe)");
    assert!(
        result.success(),
        "Windows application resource compilation failed"
    );
    println!("cargo:rustc-link-arg-bin=uu-gui={}", object.display());
}
