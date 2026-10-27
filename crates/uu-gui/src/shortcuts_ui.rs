//! Shortcut recording edits the settings draft; active routing changes on Apply.
use eframe::egui;
use uu_core::hotkeys::{Action, Bindings, Chord, Modifiers};
#[derive(Clone, Copy)]
struct Recording {
    action: Action,
    index: Option<usize>,
    seen: u64,
}
fn id(ctx: &egui::Context) -> egui::Id {
    egui::Id::new(("client-shortcut-recorder", ctx.viewport_id()))
}
#[derive(Clone, Copy)]
struct Captured {
    key: egui::Key,
    down: bool,
    released_at: u64,
}
fn capture_id(ctx: &egui::Context) -> egui::Id {
    id(ctx).with("owned-key")
}
fn listening(ctx: &egui::Context) -> bool {
    let record_id = id(ctx);
    ctx.data(|d| d.get_temp::<Recording>(record_id))
        .is_some_and(|r| ctx.cumulative_pass_nr().saturating_sub(r.seen) <= 1)
}
/// Own the complete recorded transition, including repeated layout passes.
pub fn recording(ctx: &egui::Context) -> bool {
    let key_id = capture_id(ctx);
    listening(ctx)
        || ctx
            .data(|d| d.get_temp::<Captured>(key_id))
            .is_some_and(|k| k.down || ctx.cumulative_pass_nr().saturating_sub(k.released_at) <= 1)
}
pub fn consume_key(ctx: &egui::Context, key: egui::Key, pressed: bool) -> bool {
    let key_id = capture_id(ctx);
    let pass = ctx.cumulative_pass_nr();
    let captured = ctx.data(|d| d.get_temp::<Captured>(key_id));
    if let Some(mut captured) =
        captured.filter(|k| k.key == key && (k.down || pass.saturating_sub(k.released_at) <= 1))
    {
        if !pressed && captured.down {
            captured.down = false;
            captured.released_at = pass;
            ctx.data_mut(|d| d.insert_temp(key_id, captured));
        }
        return true;
    }
    false
}
pub fn clear(ctx: &egui::Context) {
    let record_id = id(ctx);
    ctx.data_mut(|d| d.remove::<Recording>(record_id));
}
pub fn cancel(ctx: &egui::Context) {
    clear(ctx);
    let key_id = capture_id(ctx);
    ctx.data_mut(|d| d.remove::<Captured>(key_id));
}
pub fn editor(ui: &mut egui::Ui, bindings: &mut Bindings) {
    let ctx = ui.ctx().clone();
    let record_id = id(&ctx);
    let pass = ctx.cumulative_pass_nr();
    let active = ctx.data(|d| d.get_temp::<Recording>(record_id));
    if active.is_some() && !listening(&ctx) {
        clear(&ctx);
    }
    if let Some(mut active) = active.filter(|_| listening(&ctx)) {
        active.seen = pass;
        ctx.data_mut(|d| d.insert_temp(record_id, active));
        ui.label(uu_i18n::format!("为“{}”按下快捷键；Esc 取消", active.action.label()));
        if ui.small_button(uu_i18n::text("取消录入")).clicked() {
            clear(&ctx);
        }
        let event = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Key {
                    key,
                    pressed: true,
                    repeat: false,
                    modifiers,
                    ..
                } => Some((*key, *modifiers)),
                _ => None,
            })
        });
        if let Some((key, m)) = event {
            let key_id = capture_id(&ctx);
            // A quick tap can contain both transitions in one input batch.
            let down = ctx.input(|i| {
                i.events
                    .iter()
                    .rev()
                    .find_map(|e| match e {
                        egui::Event::Key {
                            key: k, pressed, ..
                        } if *k == key => Some(*pressed),
                        _ => None,
                    })
                    .unwrap_or(true)
            });
            ctx.data_mut(|d| {
                d.insert_temp(
                    key_id,
                    Captured {
                        key,
                        down,
                        released_at: pass,
                    },
                )
            });
            if key == egui::Key::Escape {
                clear(&ctx);
            } else if let Some(key) = crate::vk_for(key) {
                let chord = Chord {
                    key,
                    modifiers: Modifiers {
                        ctrl: m.ctrl,
                        alt: m.alt,
                        shift: m.shift,
                        meta: m.mac_cmd,
                    },
                };
                if chord.valid() {
                    let mut chords = bindings.chords(active.action).to_vec();
                    if let Some(index) = active.index {
                        if let Some(old) = chords.get_mut(index) {
                            *old = chord;
                        }
                    } else if chords.len() < 3 {
                        chords.push(chord);
                    }
                    bindings.set(active.action, chords);
                    clear(&ctx);
                }
            }
        }
    }
    let issues = bindings.issues();
    if issues.is_empty() {
        ui.colored_label(egui::Color32::from_rgb(55, 155, 110), uu_i18n::text("Eclipse 内部无冲突"));
    } else {
        for issue in &issues {
            ui.colored_label(egui::Color32::from_rgb(210, 70, 70), &issue.message);
        }
    }
    egui::Grid::new(ui.id().with("bindings"))
        .num_columns(2)
        .spacing([16., 9.])
        .show(ui, |ui| {
            for action in Action::ALL {
                let conflict = issues.iter().any(|e| e.actions.contains(&action));
                ui.label(egui::RichText::new(action.label()).color(if conflict {
                    egui::Color32::from_rgb(210, 70, 70)
                } else {
                    ui.visuals().text_color()
                }));
                ui.horizontal_wrapped(|ui| {
                    let mut chords = bindings.chords(action).to_vec();
                    let mut changed = false;
                    let mut remove = None;
                    for (index, chord) in chords.iter().enumerate() {
                        if ui
                            .button(chord.label())
                            .on_hover_text(uu_i18n::text("点击重新录入"))
                            .clicked()
                        {
                            ctx.data_mut(|d| {
                                d.insert_temp(
                                    record_id,
                                    Recording {
                                        action,
                                        index: Some(index),
                                        seen: pass,
                                    },
                                )
                            });
                        }
                        if ui.small_button("×").on_hover_text(uu_i18n::text("移除此绑定")).clicked() {
                            remove = Some(index);
                        }
                        if let Some(warning) = chord.warning() {
                            ui.label(
                                egui::RichText::new("⚠")
                                    .color(egui::Color32::from_rgb(195, 135, 55)),
                            )
                            .on_hover_text(warning);
                        }
                    }
                    if chords.is_empty() {
                        ui.weak(uu_i18n::text("已禁用"));
                    }
                    if chords.len() < 3 && ui.small_button("＋").on_hover_text(uu_i18n::text("增加绑定")).clicked()
                    {
                        ctx.data_mut(|d| {
                            d.insert_temp(
                                record_id,
                                Recording {
                                    action,
                                    index: None,
                                    seen: pass,
                                },
                            )
                        });
                    }
                    if let Some(index) = remove {
                        chords.remove(index);
                        changed = true;
                    }
                    if changed {
                        bindings.set(action, chords);
                    }
                    if ui.small_button(uu_i18n::text("默认")).clicked() {
                        bindings.reset(action);
                    }
                });
                ui.end_row();
            }
        });
    ui.small(uu_i18n::text("仅检查本软件内的冲突；系统与其它应用的快捷键可能仍会抢占。右 Alt / AltGr 保留给远端文本输入。"));
    ui.small(uu_i18n::text("嵌套模式把其它客户端命令交给远端，只有“嵌套快捷键透传”的绑定始终在本机切换；此项须保留一个绑定。"));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_quick_recorded_tap_does_not_hold_keyboard_ownership() {
        let ctx = egui::Context::default();
        let mut b = Bindings::default();
        let rid = id(&ctx);
        let seen = ctx.cumulative_pass_nr();
        ctx.data_mut(|d| {
            d.insert_temp(
                rid,
                Recording {
                    action: Action::Stats,
                    index: Some(0),
                    seen,
                },
            )
        });
        let key = egui::Key::Q;
        let modifiers = egui::Modifiers::CTRL;
        let input = egui::RawInput {
            events: [true, false]
                .into_iter()
                .map(|pressed| egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers,
                })
                .collect(),
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| editor(ui, &mut b));
        });
        for _ in 0..3 {
            let _ = ctx.run(Default::default(), |_| {});
        }
        assert!(!recording(&ctx));
        assert_eq!(b.chords(Action::Stats)[0].key, 0x51);
    }
    #[test]
    fn recording_owns_press_repeat_release_and_multipass_without_applying_the_draft() {
        let ctx = egui::Context::default();
        let mut b = Bindings::default();
        let record_id = id(&ctx);
        let seen = ctx.cumulative_pass_nr();
        ctx.data_mut(|d| {
            d.insert_temp(
                record_id,
                Recording {
                    action: Action::Stats,
                    index: Some(0),
                    seen,
                },
            )
        });
        let m = egui::Modifiers {
            ctrl: true,
            alt: true,
            ..Default::default()
        };
        let key = egui::Key::Q;
        let input = egui::RawInput {
            events: vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: m,
            }],
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            assert!(
                recording(ctx),
                "both the sizing and final UI pass suppress the exit command"
            );
            egui::CentralPanel::default().show(ctx, |ui| editor(ui, &mut b));
            assert!(
                recording(ctx),
                "recorded key belongs to the editor until key-up"
            );
        });
        assert_eq!(b.chords(Action::Stats), &[Chord::ctrl_alt(0x51)]);
        assert!(!listening(&ctx));
        assert!(consume_key(&ctx, key, true));
        let _ = ctx.run(Default::default(), |ctx| {
            assert!(consume_key(ctx, key, false));
            assert!(
                consume_key(ctx, key, false),
                "a second pass cannot forward an unowned release"
            );
        });
        for _ in 0..3 {
            let _ = ctx.run(Default::default(), |_| {});
        }
        assert!(!recording(&ctx));
        assert!(!consume_key(&ctx, key, true));
        let seen = ctx.cumulative_pass_nr();
        ctx.data_mut(|d| {
            d.insert_temp(
                record_id,
                Recording {
                    action: Action::Menu,
                    index: Some(0),
                    seen,
                },
            )
        });
        for _ in 0..3 {
            let _ = ctx.run(Default::default(), |_| {});
        }
        assert!(
            !recording(&ctx),
            "an editor that left cannot hold keyboard ownership"
        );
        cancel(&ctx);
    }
}
