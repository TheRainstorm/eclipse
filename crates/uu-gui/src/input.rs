//! Forward physical transitions rather than synthesizing clicks on release.
use eframe::egui::{self, Event, PointerButton, Rect};
use serde_json::{json, Value};

#[derive(Default)]
pub struct RemotePointer {
    held: [bool; 3],
    scroll: egui::Vec2,
    last: Option<egui::Pos2>,
    motion: egui::Vec2,
}

fn button(button: PointerButton) -> Option<(usize, u32)> {
    match button {
        PointerButton::Primary => Some((0, 1)),
        PointerButton::Secondary => Some((1, 2)),
        PointerButton::Middle => Some((2, 16)), // Windows UU uses MK_MBUTTON (0x10), verified on the host.
        _ => None,
    }
}

impl RemotePointer {
    pub fn release_all(&mut self, screen: u32) -> Vec<Value> {
        self.last = None;
        self.scroll = egui::Vec2::ZERO;
        self.motion = egui::Vec2::ZERO;
        self.held
            .iter_mut()
            .zip([1, 2, 16])
            .filter_map(|(held, mask)| {
                std::mem::take(held)
                    .then(|| json!({"action":"mouse_release","button":mask,"screen_id":screen}))
            })
            .collect()
    }

    pub fn events(
        &mut self,
        events: &[Event],
        draw: Rect,
        pointer: Option<egui::Pos2>,
        screen: u32,
    ) -> Vec<Value> {
        self.events_with_mode(events,draw,pointer,screen,false)
    }
    pub fn events_relative(&mut self,events:&[Event],screen:u32)->Vec<Value> {
        self.events_with_mode(events,Rect::EVERYTHING,None,screen,true)
    }
    fn events_with_mode(&mut self,events:&[Event],draw:Rect,pointer:Option<egui::Pos2>,screen:u32,relative:bool)->Vec<Value> {
        let mut out = Vec::new();
        let move_to = |out: &mut Vec<Value>, pos: egui::Pos2| {
            if relative {return;}
            out.push(json!({"action":"mouse_move_absolute", "screen_id":screen,
                "abs_x":((pos.x-draw.left())/draw.width()).clamp(0.0,1.0),
                "abs_y":((pos.y-draw.top())/draw.height()).clamp(0.0,1.0)}));
        };
        for event in events {
            match event {
                Event::MouseMoved(delta) if relative && delta.is_finite() => {
                    self.motion+=*delta;
                    let x=self.motion.x.trunc().clamp(-32767.0,32767.0) as i32;
                    let y=self.motion.y.trunc().clamp(-32767.0,32767.0) as i32;
                    self.motion-=egui::vec2(x as f32,y as f32);
                    if x!=0 || y!=0 {out.push(json!({"action":"mouse_move_relative","delta_x":x,"delta_y":y,"mousetype":2,"screen_id":screen}));}
                }
                Event::PointerMoved(pos) if !relative && (draw.contains(*pos) || self.held.iter().any(|h| *h)) => {
                    if self.last != Some(*pos) {
                        move_to(&mut out, *pos);
                        self.last = Some(*pos);
                    }
                }
                Event::PointerButton {
                    pos,
                    button: raw,
                    pressed,
                    ..
                } => {
                    if let Some((index, mask)) = button(*raw) {
                        if (*pressed && (relative || draw.contains(*pos)) && !self.held[index])
                            || (!pressed && self.held[index])
                        {
                            move_to(&mut out, *pos);
                            self.last = Some(*pos);
                            self.held[index] = *pressed;
                            out.push(json!({"action":if *pressed {"mouse_press"} else {"mouse_release"},"button":mask,"screen_id":screen}));
                        }
                    }
                }
                Event::MouseWheel { unit, delta, .. }
                    if relative || pointer.is_some_and(|p| draw.contains(p)) =>
                {
                    let factor = match unit {
                        egui::MouseWheelUnit::Line => 120.0,
                        egui::MouseWheelUnit::Point => 2.4,
                        egui::MouseWheelUnit::Page => 360.0,
                    };
                    self.scroll += *delta * factor;
                    let (x, y) = (self.scroll.x.trunc() as i32, self.scroll.y.trunc() as i32);
                    self.scroll -= egui::vec2(x as f32, y as f32);
                    if x != 0 || y != 0 {
                        out.push(json!({"action":"mouse_scroll","delta_x":x,"delta_y":y,"screen_id":screen}));
                    }
                }
                _ => {}
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn transition(pos: egui::Pos2, pressed: bool) -> Event {
        Event::PointerButton {
            pos,
            button: PointerButton::Middle,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }
    #[test]
    fn relative_uses_raw_motion_once_keeps_fraction_and_never_repositions_on_button() {
        let mut pointer=RemotePointer::default();
        let events=[Event::PointerMoved(egui::pos2(500.0,200.0)),Event::MouseMoved(egui::vec2(1.25,-2.5)),transition(egui::pos2(1.0,2.0),true)];
        let out=pointer.events_relative(&events,42);
        assert_eq!(out.len(),2);assert_eq!(out[0],json!({"action":"mouse_move_relative","delta_x":1,"delta_y":-2,"mousetype":2,"screen_id":42}));
        assert_eq!(out[1]["action"],"mouse_press");
        let out=pointer.events_relative(&[Event::MouseMoved(egui::vec2(0.75,-0.5))],42);
        assert_eq!(out[0]["delta_x"],1);assert_eq!(out[0]["delta_y"],-1);
        assert_eq!(pointer.release_all(42)[0]["button"],16);
    }
    #[test]
    fn middle_hold_drag_and_release_outside_are_preserved() {
        let mut input = RemotePointer::default();
        let draw = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(100.0, 100.0));
        let first = input.events(&[transition(egui::pos2(40.0, 50.0), true)], draw, None, 1);
        assert_eq!(
            first[1],
            json!({"action":"mouse_press","button":16,"screen_id":1})
        );
        assert_eq!(first.len(), 2); // No premature release.
        let drag = input.events(
            &[Event::PointerMoved(egui::pos2(120.0, 60.0))],
            draw,
            None,
            1,
        );
        assert_eq!(drag[0]["abs_x"], 1.0);
        let release = input.events(&[transition(egui::pos2(120.0, 60.0), false)], draw, None, 1);
        assert_eq!(release[1]["action"], "mouse_release");
        assert_eq!(release[1]["button"], 16);
        assert!(input.release_all(1).is_empty());
    }
    #[test]
    fn focus_loss_releases_held_buttons_once_and_black_bars_ignore_presses() {
        let mut input = RemotePointer::default();
        let draw = Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(100.0, 100.0));
        assert!(input
            .events(&[transition(egui::Pos2::ZERO, true)], draw, None, 1)
            .is_empty());
        input.events(&[transition(egui::pos2(50.0, 50.0), true)], draw, None, 1);
        assert_eq!(input.release_all(1).len(), 1);
        assert!(input.release_all(1).is_empty());
    }
}
