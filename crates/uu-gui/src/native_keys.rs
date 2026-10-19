//! Convert UI semantics into standard native HID/normalized pointer commands.
use eframe::egui;
use serde_json::Value;
use uu_core::native_input::{InputCommand as I, KeyCode, MouseButton};

pub fn hid_for_egui(key: egui::Key) -> Option<u16> {
    let raw = format!("{key:?}");
    let code = if raw.len() == 1 && raw.as_bytes()[0].is_ascii_uppercase() {
        format!("Key{raw}")
    } else if let Some(digit) = raw
        .strip_prefix("Num")
        .filter(|d| d.len() == 1 && d.as_bytes()[0].is_ascii_digit())
    {
        format!("Digit{digit}")
    } else {
        match raw.as_str() {
            "Equals" | "Plus" => "Equal",
            "Backtick" => "Backquote",
            "OpenBracket" => "BracketLeft",
            "CloseBracket" => "BracketRight",
            "Colon" => "Semicolon",
            "Questionmark" => "Slash",
            "Pipe" => "Backslash",
            other => other,
        }
        .to_owned()
    };
    KeyCode::from_w3c_code(&code).map(|key| key.0)
}
#[cfg(windows)]
fn hid_for_vk(vk: u32) -> Option<u16> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{MapVirtualKeyW, MAPVK_VK_TO_VSC_EX};
    let scan = unsafe { MapVirtualKeyW(vk, MAPVK_VK_TO_VSC_EX) };
    KeyCode::from_windows_scan(u16::try_from(scan).ok()?).map(|key| key.0)
}
#[cfg(not(windows))]
fn hid_for_vk(vk: u32) -> Option<u16> {
    // Egui supplies physical ordinary keys separately. Modifier state is
    // delivered as aggregate flags, using the existing left-side UI identities.
    Some(match vk {
        0x11 | 0xa2 => 0xe0,
        0x10 | 0xa0 => 0xe1,
        0x12 | 0xa4 => 0xe2,
        0x5b => 0xe3,
        0xa3 => 0xe4,
        0xa1 => 0xe5,
        0xa5 => 0xe6,
        0x5c => 0xe7,
        _ => return None,
    })
}
pub fn command(value: &Value) -> Option<I> {
    let action = value["action"].as_str()?;
    let delta = |name: &str| {
        Some(
            value[name]
                .as_i64()?
                .clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16,
        )
    };
    Some(match action {
        "mouse_move_absolute" => {
            let axis = |name: &str| {
                let value = value[name].as_f64()?;
                value
                    .is_finite()
                    .then(|| (value.clamp(0.0, 1.0) * 65535.0).round() as u16)
            };
            I::Absolute {
                x: axis("abs_x")?,
                y: axis("abs_y")?,
            }
        }
        "mouse_move_relative" => I::Relative {
            dx: delta("delta_x")?,
            dy: delta("delta_y")?,
        },
        "mouse_scroll" => I::Wheel {
            dx: delta("delta_x")?,
            dy: delta("delta_y")?,
        },
        "mouse_press" | "mouse_release" => I::Button {
            button: match value["button"].as_u64()? {
                1 => MouseButton::Left,
                2 => MouseButton::Right,
                16 => MouseButton::Middle,
                32 => MouseButton::Back,
                64 => MouseButton::Forward,
                _ => return None,
            },
            pressed: action == "mouse_press",
        },
        "kbd_press" | "kbd_release" => I::Key {
            usage: hid_for_vk(u32::try_from(value["key"].as_u64()?).ok()?)?,
            pressed: action == "kbd_press",
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn physical_keys_preserve_punctuation_numbers_functions_and_navigation() {
        for (key, usage) in [
            (egui::Key::A, 4),
            (egui::Key::Num1, 0x1e),
            (egui::Key::Semicolon, 0x33),
            (egui::Key::Colon, 0x33),
            (egui::Key::Questionmark, 0x38),
            (egui::Key::OpenBracket, 0x2f),
            (egui::Key::F12, 0x45),
            (egui::Key::ArrowRight, 0x4f),
        ] {
            assert_eq!(hid_for_egui(key), Some(usage));
        }
    }
    #[test]
    fn semantic_middle_button_is_normalized_without_reusing_a_vendor_wire_message() {
        let value = serde_json::json!({"action":"mouse_press","button":16,"screen_id":42});
        assert!(matches!(
            command(&value),
            Some(I::Button {
                button: MouseButton::Middle,
                pressed: true
            })
        ));
        let value = serde_json::json!({"action":"mouse_move_absolute","abs_x":0.5,"abs_y":1.0});
        assert!(matches!(
            command(&value),
            Some(I::Absolute { x: 32768, y: 65535 })
        ));
    }
}
