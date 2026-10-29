//! Expand both format literals so Rust checks placeholders in both languages.
use proc_macro::TokenStream;
use quote::quote;
use syn::{
    parse::{Parse, ParseStream},
    LitStr,
};
struct Arguments {
    source: LitStr,
    rest: proc_macro2::TokenStream,
}
impl Parse for Arguments {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        Ok(Self {
            source: input.parse()?,
            rest: input.parse()?,
        })
    }
}
#[proc_macro]
pub fn format(input: TokenStream) -> TokenStream {
    let Arguments { source, rest } = syn::parse_macro_input!(input as Arguments);
    let mut catalogue: std::collections::BTreeMap<String, String> =
        serde_json::from_str(include_str!("../../uu-i18n/catalogue.json"))
            .expect("valid translation catalogue");
    // A separately built consumer can own its format templates. The main
    // application does not need to bundle extension-specific vocabulary.
    if let Some(directory)=std::env::var_os("CARGO_MANIFEST_DIR") {
        let path=std::path::PathBuf::from(directory).join("catalogue.json");
        if path.is_file() {
            let source=std::fs::read_to_string(path).expect("consumer translation catalogue");
            let extra:std::collections::BTreeMap<String,String>=serde_json::from_str(&source).expect("valid consumer translation catalogue");
            catalogue.extend(extra);
        }
    }
    let Some(english) = catalogue.get(&source.value()) else {
        return syn::Error::new(
            source.span(),
            "missing English translation for format template",
        )
        .to_compile_error()
        .into();
    };
    let english = LitStr::new(english, source.span());
    // Argument expressions are kept as provided. Only the format template is
    // selected; device names and other interpolated user content stay intact.
    quote! { if ::uu_i18n::is_english() { ::std::format!(#english #rest) } else { ::std::format!(#source #rest) } }.into()
}
