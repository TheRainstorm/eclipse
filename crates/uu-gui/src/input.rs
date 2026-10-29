//! Forward physical transitions rather than synthesizing clicks on release.
use eframe::egui::{self, Event, PointerButton, Rect};
use uu_core::native_input::{InputCommand as I,MouseButton};

#[derive(Default)]
pub struct RemotePointer {
    held: [bool; 5],
    scroll: egui::Vec2,
    last: Option<egui::Pos2>,
    motion: egui::Vec2,
}

fn button(button: PointerButton) -> (usize, MouseButton) {
    match button {
        PointerButton::Primary => (0, MouseButton::Left),
        PointerButton::Secondary => (1, MouseButton::Right),
        PointerButton::Middle => (2, MouseButton::Middle),
        PointerButton::Extra1 => (3, MouseButton::Back),
        PointerButton::Extra2 => (4, MouseButton::Forward),
    }
}

impl RemotePointer {
    pub fn release_all(&mut self, _screen: u32) -> Vec<I> {
        self.last = None;
        self.scroll = egui::Vec2::ZERO;
        self.motion = egui::Vec2::ZERO;
        self.held
            .iter_mut()
            .zip([MouseButton::Left,MouseButton::Right,MouseButton::Middle,MouseButton::Back,MouseButton::Forward])
            .filter_map(|(held, button)| {
                std::mem::take(held)
                    .then_some(I::Button{button,pressed:false})
            })
            .collect()
    }

    pub fn events(
        &mut self,
        events: &[Event],
        draw: Rect,
        pointer: Option<egui::Pos2>,
        screen: u32,
    ) -> Vec<I> {
        self.events_with_mode(events,draw,pointer,screen,false)
    }
    pub fn events_relative(&mut self,events:&[Event],screen:u32)->Vec<I> {
        self.events_with_mode(events,Rect::EVERYTHING,None,screen,true)
    }
    fn events_with_mode(&mut self,events:&[Event],draw:Rect,pointer:Option<egui::Pos2>,_screen:u32,relative:bool)->Vec<I> {
        let mut out = Vec::new();
        let move_to = |out: &mut Vec<I>, pos: egui::Pos2| {
            if relative {return;}
            out.push(I::Absolute{x:(((pos.x-draw.left())/draw.width()).clamp(0.0,1.0)*65535.).round() as u16,y:(((pos.y-draw.top())/draw.height()).clamp(0.0,1.0)*65535.).round() as u16});
        };
        for event in events {
            match event {
                Event::MouseMoved(delta) if relative && delta.is_finite() => {
                    self.motion+=*delta;
                    let x=self.motion.x.trunc().clamp(-32767.0,32767.0) as i32;
                    let y=self.motion.y.trunc().clamp(-32767.0,32767.0) as i32;
                    self.motion-=egui::vec2(x as f32,y as f32);
                    if x!=0 || y!=0 {out.push(I::Relative{dx:x as i16,dy:y as i16});}
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
                    {let (index, button) = button(*raw);
                        if (*pressed && (relative || draw.contains(*pos)) && !self.held[index])
                            || (!pressed && self.held[index])
                        {
                            move_to(&mut out, *pos);
                            self.last = Some(*pos);
                            self.held[index] = *pressed;
                            out.push(I::Button{button,pressed:*pressed});
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
                        out.push(I::Wheel{dx:x.clamp(i16::MIN as i32,i16::MAX as i32) as i16,dy:y.clamp(i16::MIN as i32,i16::MAX as i32) as i16});
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
        assert_eq!(out.len(),2);assert_eq!(out[0],I::Relative{dx:1,dy:-2});
        assert_eq!(out[1],I::Button{button:MouseButton::Middle,pressed:true});
        let out=pointer.events_relative(&[Event::MouseMoved(egui::vec2(0.75,-0.5))],42);
        assert_eq!(out[0],I::Relative{dx:1,dy:-1});
        assert_eq!(pointer.release_all(42)[0],I::Button{button:MouseButton::Middle,pressed:false});
    }
    #[test]
    fn middle_hold_drag_and_release_outside_are_preserved() {
        let mut input = RemotePointer::default();
        let draw = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(100.0, 100.0));
        let first = input.events(&[transition(egui::pos2(40.0, 50.0), true)], draw, None, 1);
        assert_eq!(
            first[1],
            I::Button{button:MouseButton::Middle,pressed:true}
        );
        assert_eq!(first.len(), 2); // No premature release.
        let drag = input.events(
            &[Event::PointerMoved(egui::pos2(120.0, 60.0))],
            draw,
            None,
            1,
        );
        assert!(matches!(drag[0],I::Absolute{x:65535,..}));
        let release = input.events(&[transition(egui::pos2(120.0, 60.0), false)], draw, None, 1);
        assert_eq!(release[1],I::Button{button:MouseButton::Middle,pressed:false});
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
