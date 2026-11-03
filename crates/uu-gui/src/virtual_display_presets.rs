//! Named creation specifications belong to application preferences. Selecting a
//! preset creates a new display; editing presets never mutates a live display.
use eframe::egui;
use serde::{Deserialize, Serialize};
use uu_core::virtual_display::Spec;
use uuid::Uuid;

const MAX_PRESETS: usize = 32;
const NAME_LIMIT: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preset {
    pub id: Uuid,
    pub name: String,
    pub spec: Spec,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Presets {
    pub items: Vec<Preset>,
    pub default: Uuid,
}
impl Default for Presets {
    fn default() -> Self {
        Self {
            items: [
                (1, "1080p 60 Hz", 1920, 1080),
                (2, "1440p 60 Hz", 2560, 1440),
                (3, "4K 60 Hz", 3840, 2160),
            ]
            .into_iter()
            .map(|(id, name, width, height)| Preset {
                id: Uuid::from_u128(id),
                name: name.into(),
                spec: Spec {
                    width,
                    height,
                    refresh_millihz: 60_000,
                    hdr: false,
                },
            })
            .collect(),
            default: Uuid::from_u128(1),
        }
    }
}
fn valid_spec(spec: Spec) -> bool {
    // Matches the remote virtual-display Spec contract. The driver makes the
    // final mode-support decision when the new display is created.
    (320..=16_384).contains(&spec.width)
        && (200..=16_384).contains(&spec.height)
        && (1_000..=1_000_000).contains(&spec.refresh_millihz)
}
impl Presets {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.items.is_empty() && self.items.len() <= MAX_PRESETS,
            "{}",
            uu_i18n::text("虚拟显示器规格需要保留 1–32 项")
        );
        let mut ids = std::collections::HashSet::new();
        let mut names = std::collections::HashSet::new();
        for preset in &self.items {
            anyhow::ensure!(
                !preset.id.is_nil() && ids.insert(preset.id),
                "{}",
                uu_i18n::text("虚拟显示器规格标识无效或重复")
            );
            anyhow::ensure!(
                !preset.name.trim().is_empty()
                    && preset.name.chars().count() <= NAME_LIMIT
                    && !preset.name.chars().any(char::is_control),
                "{}",
                uu_i18n::text("虚拟显示器规格名称须为 1–64 个可见字符")
            );
            anyhow::ensure!(
                names.insert(preset.name.trim().to_lowercase()),
                "{}",
                uu_i18n::text("虚拟显示器规格名称不能重复")
            );
            anyhow::ensure!(
                valid_spec(preset.spec),
                "{}",
                uu_i18n::text("虚拟显示器规格的分辨率或刷新率超出支持范围")
            );
        }
        anyhow::ensure!(
            ids.contains(&self.default),
            "{}",
            uu_i18n::text("请选择一个有效的默认虚拟显示器规格")
        );
        Ok(())
    }
    pub fn default_spec(&self) -> Spec {
        self.items
            .iter()
            .find(|p| p.id == self.default && valid_spec(p.spec))
            .or_else(|| self.items.iter().find(|p| valid_spec(p.spec)))
            .map(|p| p.spec)
            .unwrap_or_default()
    }
    fn remove(&mut self, id: Uuid) {
        if self.items.len() <= 1 {
            return;
        }
        self.items.retain(|p| p.id != id);
        if self.default == id {
            self.default = self.items[0].id;
        }
    }
    fn add(&mut self) {
        if self.items.len() >= MAX_PRESETS {
            return;
        }
        let spec = self.default_spec();
        let base = uu_i18n::text("自定义规格");
        let name = (1..)
            .map(|number| format!("{base} {number}"))
            .find(|name| {
                !self
                    .items
                    .iter()
                    .any(|p| p.name.trim().eq_ignore_ascii_case(name))
            })
            .unwrap();
        self.items.push(Preset {
            id: Uuid::new_v4(),
            name,
            spec,
        });
    }
}
fn specification(spec: Spec) -> String {
    let hz = if spec.refresh_millihz % 1000 == 0 {
        (spec.refresh_millihz / 1000).to_string()
    } else {
        format!("{:.3}", spec.refresh_millihz as f64 / 1000.)
            .trim_end_matches('0')
            .to_owned()
    };
    format!(
        "{} × {} · {hz} Hz{}",
        spec.width,
        spec.height,
        if spec.hdr { " · HDR" } else { "" }
    )
}
/// Entries for the existing '+' context menu. The caller owns dispatching the
/// selected specification through the same connection-creation action.
pub fn create_menu(ui: &mut egui::Ui, presets: &Presets) -> Option<Spec> {
    ui.weak(uu_i18n::text("按规格新建虚拟显示器"));
    let mut selected = None;
    for preset in &presets.items {
        let label = format!(
            "{}{}\n{}",
            preset.name,
            if preset.id == presets.default {
                format!(" · {}", uu_i18n::text("默认"))
            } else {
                String::new()
            },
            specification(preset.spec)
        );
        if ui
            .add_enabled(valid_spec(preset.spec), egui::Button::new(label))
            .clicked()
        {
            selected = Some(preset.spec);
            ui.close_menu();
        }
    }
    selected
}

pub fn editor(ui: &mut egui::Ui, presets: &mut Presets) {
    ui.label(uu_i18n::text(
        "左键 + 使用默认规格；右键 + 可选择其它规格。修改规格只影响之后新建的虚拟显示器。",
    ));
    let mut remove = None;
    let removable = presets.items.len() > 1;
    for preset in &mut presets.items {
        ui.push_id(preset.id, |ui| {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.radio_value(&mut presets.default, preset.id, uu_i18n::text("默认"));
                    ui.label(uu_i18n::text("规格名称"));
                    ui.add(
                        egui::TextEdit::singleline(&mut preset.name)
                            .desired_width(160.)
                            .char_limit(NAME_LIMIT),
                    );
                    if ui
                        .add_enabled(removable, egui::Button::new(uu_i18n::text("删除")))
                        .clicked()
                    {
                        remove = Some(preset.id);
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label(uu_i18n::text("分辨率"));
                    ui.add(egui::DragValue::new(&mut preset.spec.width).range(320..=16_384));
                    ui.label("×");
                    ui.add(egui::DragValue::new(&mut preset.spec.height).range(200..=16_384));
                    ui.label(uu_i18n::text("刷新率"));
                    let mut hz = preset.spec.refresh_millihz as f64 / 1000.;
                    if ui
                        .add(
                            egui::DragValue::new(&mut hz)
                                .range(1.0..=1000.0)
                                .speed(0.1)
                                .max_decimals(3)
                                .suffix(" Hz"),
                        )
                        .changed()
                    {
                        preset.spec.refresh_millihz = (hz * 1000.).round() as u32;
                    }
                    ui.checkbox(&mut preset.spec.hdr, "HDR");
                });
            });
        });
    }
    if let Some(id) = remove {
        presets.remove(id);
    }
    if ui
        .add_enabled(
            presets.items.len() < MAX_PRESETS,
            egui::Button::new(uu_i18n::text("添加规格")),
        )
        .clicked()
    {
        presets.add();
    }
    ui.weak(uu_i18n::text(
        "规格在创建时交给远端驱动验证；HDR 还需要远端驱动、显示与串流编码支持。",
    ));
    if let Err(error) = presets.validate() {
        ui.colored_label(super::theme::DANGER, error.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_selection_names_and_fractional_refresh_round_trip() {
        let mut presets = Presets::default();
        presets.default = presets.items[1].id;
        presets.items[1].spec = Spec {
            width: 3440,
            height: 1440,
            refresh_millihz: 59_940,
            hdr: true,
        };
        presets.validate().unwrap();
        assert_eq!(
            specification(presets.default_spec()),
            "3440 × 1440 · 59.94 Hz · HDR"
        );
        let value: Presets =
            serde_json::from_slice(&serde_json::to_vec(&presets).unwrap()).unwrap();
        assert_eq!(value, presets);
    }
    #[test]
    fn deleting_default_selects_a_remaining_spec_and_never_removes_the_last_one() {
        let mut presets = Presets::default();
        presets.remove(presets.default);
        presets.validate().unwrap();
        assert_eq!(presets.default, presets.items[0].id);
        presets.remove(presets.default);
        presets.remove(presets.default);
        assert_eq!(presets.items.len(), 1);
        presets.validate().unwrap();
        presets.add();
        presets.add();
        presets.validate().unwrap();
        assert_ne!(presets.items[1].id, presets.items[2].id);
        assert_ne!(presets.items[1].name, presets.items[2].name);
    }
    #[test]
    fn invalid_persistent_presets_are_rejected_instead_of_silently_repaired() {
        for mutate in [
            |p: &mut Presets| p.default = Uuid::nil(),
            |p: &mut Presets| p.items[0].spec.width = 0,
            |p: &mut Presets| p.items[0].spec.refresh_millihz = 999,
            |p: &mut Presets| p.items[0].name = "\n".into(),
            |p: &mut Presets| p.items[0].name = p.items[1].name.clone(),
            |p: &mut Presets| p.items[0].id = p.items[1].id,
        ] {
            let mut presets = Presets::default();
            mutate(&mut presets);
            assert!(presets.validate().is_err());
        }
    }
}
