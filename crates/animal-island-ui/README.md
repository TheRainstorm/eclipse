# Animal Island UI for egui

An independent Rust/egui implementation of the core controls used by Eclipse. The production dependency is **egui 0.29**; there are no transport, configuration, application, or localization dependencies. Copy this crate to another workspace or depend on it by path.

The visual language follows [guokaigdg/animal-island-ui](https://github.com/guokaigdg/animal-island-ui), reference revision `02fa9da20534d303a4757563102229847e15b3c6`: cream paper, warm brown text, mint accents, pill controls, primary button depth, yellow input focus, flat switch handles and rounded cards. This is a native port of selected components, not the full React API.

## Included

- `Button`: primary, secondary, ghost and danger, compact sizing, hover/press feedback and disabled state.
- `Input`: native text editing, selection, clipboard, password and IME support, with island focus styling.
- `Select`: native egui dropdown interaction and popup ownership.
- `Switch`: animated on/off control, keyboard activation and disabled semantics.
- `Card`, `Tag`, `Ribbon`: containers, status labels and decorative section headings.
- `tokens`: semantic light/night colors, geometry and standalone `Style` generation.
- `fonts`: optional bundled rounded Latin font with host CJK fallbacks preserved.

```rust,no_run
use animal_island_ui::{egui, Button, ButtonKind, Input, Switch};
fn settings(ui: &mut egui::Ui, name: &mut String, enabled: &mut bool) {
    ui.add(Input::new(name).hint_text("Profile name"));
    ui.add(Switch::new(enabled, "Enable streaming"));
    if ui.add(Button::new("Save", ButtonKind::Primary)).clicked() {
        // The application owns persistence.
    }
}
```

Call `animal_island_ui::apply(ctx)` before rendering a standalone application. It registers light/night styles while preserving the current theme preference. For a host that already installs fonts, pass its complete `FontDefinitions` to `fonts::install(ctx, definitions)` first. The named island font uses the host's proportional fonts as fallbacks and leaves its original proportional family unchanged. Do not call `set_fonts` on every frame.

Run `cargo run -p animal-island-ui --example gallery` for an interactive component gallery. Controls use egui's native interaction and focus model; visual animation requests frames only while changing. Business actions and data remain outside this crate.

The upstream design specifies a light theme. The warm night palette is an extension for native desktop use. Muted text and small status colors are deepened for readability. Windows, scrolling, accessibility and IME follow egui rather than browser behavior; charts, complex modals, uploads, decorative scenes and the complete React library are outside this port's scope.

## License and provenance

The upstream MIT notice is retained in [LICENSE](LICENSE). The Rust port is also MIT. No Nintendo/game artwork, web runtime, React dependency or remote font request is included.

[Island Rounded SemiBold](assets/README.md) is a static weight-600 derivative of Nunito, renamed and distributed under the [SIL Open Font License](assets/OFL.txt). Chinese text uses host-supplied fallback fonts.
