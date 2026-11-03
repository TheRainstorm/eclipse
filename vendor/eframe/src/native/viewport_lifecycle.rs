//! Repaint/command placeholders are not declarations of native windows.
use egui::{ViewportClass, ViewportId};

/// egui creates a default Root-class entry when a late callback addresses an
/// unknown viewport. Only ROOT may have that class; a child is declared by
/// show_viewport_deferred/immediate. Keep its normal parent-pass GC unchanged.
pub(super) fn declared(id: ViewportId, class: ViewportClass) -> bool {
    id == ViewportId::ROOT || class != ViewportClass::Root
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn late_wake_does_not_declare_a_native_window() {
        let ctx = egui::Context::default();
        ctx.set_embed_viewports(false);
        let live = ViewportId::from_hash_of("live");
        let retired = ViewportId::from_hash_of("retired");
        let raw = |id| egui::RawInput {
            viewport_id: id,
            viewports: [
                (ViewportId::ROOT, Default::default()),
                (id, Default::default()),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let _ = ctx.run(raw(ViewportId::ROOT), |ctx| {
            ctx.show_viewport_deferred(live, Default::default(), |_, _| {})
        });
        // This is the precise in-flight race: a producer read the old id before
        // transfer, then publishes after the parent stopped declaring it.
        ctx.request_repaint_of(retired);
        let output = ctx.run(raw(live), |_| {});
        assert!(
            output.viewport_output.contains_key(&retired),
            "exercise egui's real late-wake placeholder"
        );
        assert!(!declared(retired, output.viewport_output[&retired].class));
        assert!(declared(live, output.viewport_output[&live].class));
        assert!(declared(
            ViewportId::ROOT,
            output.viewport_output[&ViewportId::ROOT].class
        ));
    }
    #[test]
    fn both_declared_child_classes_are_valid() {
        let id = ViewportId::from_hash_of("child");
        assert!(declared(id, ViewportClass::Deferred));
        assert!(declared(id, ViewportClass::Immediate));
        assert!(!declared(id, ViewportClass::Root));
    }
}
