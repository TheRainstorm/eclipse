//! Native resize gestures for our undecorated window.
use eframe::egui::{self, viewport::ResizeDirection as D, CursorIcon as C};

pub fn resize_border(ctx: &egui::Context, fullscreen: bool) -> bool {
    if fullscreen || ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
        return false;
    }
    let Some(p) = ctx.input(|i| i.pointer.hover_pos()) else { return false };
    let r = ctx.screen_rect();
    if !r.contains(p) { return false; }
    let edge = 6.0;
    let corner = 16.0;
    let left = p.x < r.left() + edge;
    let right = p.x > r.right() - edge;
    let top = p.y < r.top() + edge;
    let bottom = p.y > r.bottom() - edge;
    let direction = if (left && p.y < r.top() + corner) || (top && p.x < r.left() + corner) {
        Some((D::NorthWest, C::ResizeNwSe))
    } else if (right && p.y < r.top() + corner) || (top && p.x > r.right() - corner) {
        Some((D::NorthEast, C::ResizeNeSw))
    } else if (left && p.y > r.bottom() - corner) || (bottom && p.x < r.left() + corner) {
        Some((D::SouthWest, C::ResizeNeSw))
    } else if (right && p.y > r.bottom() - corner) || (bottom && p.x > r.right() - corner) {
        Some((D::SouthEast, C::ResizeNwSe))
    } else if left { Some((D::West, C::ResizeHorizontal)) }
    else if right { Some((D::East, C::ResizeHorizontal)) }
    else if top { Some((D::North, C::ResizeVertical)) }
    else if bottom { Some((D::South, C::ResizeVertical)) }
    else { None };
    if let Some((direction, cursor)) = direction {
        ctx.set_cursor_icon(cursor);
        if ctx.input(|i| i.pointer.primary_pressed()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(direction));
        }
        return true;
    }
    false
}
