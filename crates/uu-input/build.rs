fn main() {
    println!("cargo:rerun-if-changed=src/gamepad/uhid.c");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref()==Ok("linux") {
        cc::Build::new().file("src/gamepad/uhid.c").warnings(true).compile("eclipse_uhid_adapter");
    }
}
