fn main() {
    println!("cargo:rerun-if-changed=catalogue.json");
    let entries: std::collections::BTreeMap<String, String> =
        serde_json::from_str(include_str!("catalogue.json")).expect("valid translation catalogue");
    let mut code =
        String::from("pub fn english(source: &str) -> Option<&'static str> { match source {\n");
    for (source, translated) in entries {
        code.push_str(&format!("{source:?} => Some({translated:?}),\n"));
    }
    code.push_str("_ => None, } }\n");
    std::fs::write(
        std::path::Path::new(&std::env::var_os("OUT_DIR").unwrap()).join("catalogue.rs"),
        code,
    )
    .unwrap();
}
