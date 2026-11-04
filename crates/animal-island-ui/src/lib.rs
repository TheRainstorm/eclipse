//! Native egui interpretation of Animal Island UI's core visual language.
//!
//! This crate has no dependency on the application, transport, storage or i18n.
//! See README.md for the supported component scope and upstream provenance.
pub use egui;
pub mod button;
pub mod containers;
pub mod fonts;
pub mod forms;
pub mod tokens;
pub use button::{Button, ButtonKind};
pub use containers::{Card, Ribbon, Tag};
pub use forms::{Input, Select, Switch};
pub use tokens::{Tokens, DARK, LIGHT};

/// Register both light/night styles without changing the host's mode choice.
pub fn apply(ctx: &egui::Context) {
    fonts::ensure(ctx);
    for theme in [egui::Theme::Light, egui::Theme::Dark] {
        ctx.set_style_of(theme, tokens::style(theme));
    }
}
