//! Timeline bar: click/drag to move the preview position, drag the two handles to trim.

use eframe::egui::{pos2, vec2, Color32, CursorIcon, Rect, Sense, Stroke, Ui};

/// Returns true when the preview position changed (so the caller should grab a new frame).
pub fn show(ui: &mut Ui, duration: f64, pos: &mut f64, start: &mut f64, end: &mut f64, kfs: &[f64]) -> bool {
    let mut pos_changed = false;
    let dur = duration.max(0.001);
    let gap = 0.05f64.min(dur / 2.0);

    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
    let track = Rect::from_min_max(pos2(rect.left() + 10.0, rect.top() + 11.0), pos2(rect.right() - 10.0, rect.bottom() - 11.0));
    let tw = track.width().max(1.0);
    let t2x = |t: f64| track.left() + (t / dur) as f32 * tw;
    let x2t = |x: f32| (((x - track.left()) / tw) as f64 * dur).clamp(0.0, dur);
    let id = ui.id().with("timeline");

    // Whole bar: click or drag to move the playhead.
    let bar = ui.interact(rect, id.with("bar"), Sense::click_and_drag());
    if bar.clicked() || bar.dragged() {
        if let Some(p) = bar.interact_pointer_pos() {
            *pos = x2t(p.x);
            pos_changed = true;
        }
    }

    // Handles are created after the bar so they sit on top of it.
    let hw = 8.0;
    let s_rect = Rect::from_center_size(pos2(t2x(*start), track.center().y), vec2(hw * 2.0, rect.height()));
    let e_rect = Rect::from_center_size(pos2(t2x(*end), track.center().y), vec2(hw * 2.0, rect.height()));
    let s_resp = ui.interact(s_rect, id.with("start"), Sense::drag());
    let e_resp = ui.interact(e_rect, id.with("end"), Sense::drag());
    if s_resp.dragged() {
        if let Some(p) = s_resp.interact_pointer_pos() {
            *start = x2t(p.x).min(*end - gap).max(0.0);
            *pos = *start;
            pos_changed = true;
        }
    }
    if e_resp.dragged() {
        if let Some(p) = e_resp.interact_pointer_pos() {
            *end = x2t(p.x).max(*start + gap).min(dur);
            *pos = (*end - 0.05).max(0.0);
            pos_changed = true;
        }
    }
    if s_resp.hovered() || e_resp.hovered() || s_resp.dragged() || e_resp.dragged() {
        ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
    }

    // Paint.
    let painter = ui.painter_at(rect);
    painter.rect_filled(track, 3.0, Color32::from_gray(38));
    let (sx, ex) = (t2x(*start), t2x(*end));
    let sel = Rect::from_min_max(pos2(sx, track.top()), pos2(ex, track.bottom()));
    painter.rect_filled(sel, 2.0, Color32::from_rgb(52, 92, 150));

    let mut last_x = f32::MIN;
    for &k in kfs {
        let x = t2x(k);
        if x - last_x >= 4.0 {
            painter.line_segment(
                [pos2(x, track.bottom() - 5.0), pos2(x, track.bottom())],
                Stroke::new(1.0_f32, Color32::from_white_alpha(70)),
            );
            last_x = x;
        }
    }

    for (x, active) in [(sx, s_resp.hovered() || s_resp.dragged()), (ex, e_resp.hovered() || e_resp.dragged())] {
        let h = Rect::from_center_size(pos2(x, track.center().y), vec2(8.0, rect.height() - 4.0));
        let c = if active { Color32::from_rgb(255, 255, 255) } else { Color32::from_rgb(215, 215, 220) };
        painter.rect_filled(h, 3.0, c);
        painter.line_segment(
            [pos2(x, h.top() + 8.0), pos2(x, h.bottom() - 8.0)],
            Stroke::new(1.5_f32, Color32::from_gray(90)),
        );
    }

    let px = t2x(*pos);
    painter.line_segment([pos2(px, rect.top() + 2.0), pos2(px, rect.bottom() - 2.0)], Stroke::new(2.0_f32, Color32::from_rgb(255, 160, 40)));
    painter.circle_filled(pos2(px, rect.top() + 4.0), 4.0, Color32::from_rgb(255, 160, 40));

    pos_changed
}
