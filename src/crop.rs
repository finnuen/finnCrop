//! Crop rectangle math. Pure geometry, no GUI types, so it can be unit tested.
//! All values are in VIDEO pixels unless a function says otherwise.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CropRect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl CropRect {
    pub fn full(w: f32, h: f32) -> Self {
        Self { x0: 0.0, y0: 0.0, x1: w, y1: h }
    }

    pub fn w(&self) -> f32 {
        self.x1 - self.x0
    }

    pub fn h(&self) -> f32 {
        self.y1 - self.y0
    }

    /// True when the box covers the whole frame (within 1 px), i.e. "not cropped".
    pub fn is_full(&self, fw: f32, fh: f32) -> bool {
        self.x0 <= 1.0 && self.y0 <= 1.0 && self.x1 >= fw - 1.0 && self.y1 >= fh - 1.0
    }

    /// Integer crop for FFmpeg: (w, h, x, y). Even numbers, always inside the frame.
    pub fn to_pixels(&self, fw: u32, fh: u32) -> (u32, u32, u32, u32) {
        let x = (self.x0.max(0.0).round() as u32).min(fw.saturating_sub(2)) & !1;
        let y = (self.y0.max(0.0).round() as u32).min(fh.saturating_sub(2)) & !1;
        let x1 = (self.x1.max(0.0).round() as u32).min(fw);
        let y1 = (self.y1.max(0.0).round() as u32).min(fh);
        let w = (x1.saturating_sub(x) & !1).min((fw - x) & !1).max(2);
        let h = (y1.saturating_sub(y) & !1).min((fh - y) & !1).max(2);
        (w, h, x, y)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aspect {
    Free,
    Square,
    R16x9,
    R9x16,
    R4x3,
    R3x4,
}

impl Aspect {
    pub const ALL: [Aspect; 6] = [
        Aspect::Free,
        Aspect::Square,
        Aspect::R16x9,
        Aspect::R9x16,
        Aspect::R4x3,
        Aspect::R3x4,
    ];

    pub fn ratio(self) -> Option<f32> {
        match self {
            Aspect::Free => None,
            Aspect::Square => Some(1.0),
            Aspect::R16x9 => Some(16.0 / 9.0),
            Aspect::R9x16 => Some(9.0 / 16.0),
            Aspect::R4x3 => Some(4.0 / 3.0),
            Aspect::R3x4 => Some(3.0 / 4.0),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Aspect::Free => "Free",
            Aspect::Square => "1:1",
            Aspect::R16x9 => "16:9",
            Aspect::R9x16 => "9:16",
            Aspect::R4x3 => "4:3",
            Aspect::R3x4 => "3:4",
        }
    }
}

/// Largest centered rectangle with the given width/height ratio inside the frame.
pub fn fit_aspect(fw: f32, fh: f32, ratio: f32) -> CropRect {
    let (w, h) = if fw / fh > ratio { (fh * ratio, fh) } else { (fw, fw / ratio) };
    let x0 = (fw - w) / 2.0;
    let y0 = (fh - h) / 2.0;
    CropRect { x0, y0, x1: x0 + w, y1: y0 + h }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    Move,
    N,
    S,
    E,
    W,
    NE,
    NW,
    SE,
    SW,
}

fn sides(h: Handle) -> (bool, bool, bool, bool) {
    // (left edge moves, right edge moves, top edge moves, bottom edge moves)
    match h {
        Handle::Move => (false, false, false, false),
        Handle::N => (false, false, true, false),
        Handle::S => (false, false, false, true),
        Handle::E => (false, true, false, false),
        Handle::W => (true, false, false, false),
        Handle::NE => (false, true, true, false),
        Handle::NW => (true, false, true, false),
        Handle::SE => (false, true, false, true),
        Handle::SW => (true, false, false, true),
    }
}

/// Which handle (if any) is under the point. `r` and the point must share one coordinate space
/// (the app passes SCREEN coordinates here).
pub fn hit_test(r: &CropRect, px: f32, py: f32, tol: f32) -> Option<Handle> {
    let tol = tol.min(0.35 * r.w().min(r.h())).max(3.0);
    if px < r.x0 - tol || px > r.x1 + tol || py < r.y0 - tol || py > r.y1 + tol {
        return None;
    }
    let l = (px - r.x0).abs() <= tol;
    let rt = (px - r.x1).abs() <= tol;
    let t = (py - r.y0).abs() <= tol;
    let b = (py - r.y1).abs() <= tol;
    match (l, rt, t, b) {
        (true, _, true, _) => Some(Handle::NW),
        (_, true, true, _) => Some(Handle::NE),
        (true, _, _, true) => Some(Handle::SW),
        (_, true, _, true) => Some(Handle::SE),
        (true, _, _, _) => Some(Handle::W),
        (_, true, _, _) => Some(Handle::E),
        (_, _, true, _) => Some(Handle::N),
        (_, _, _, true) => Some(Handle::S),
        _ => {
            if px > r.x0 && px < r.x1 && py > r.y0 && py < r.y1 {
                Some(Handle::Move)
            } else {
                None
            }
        }
    }
}

fn clampf(v: f32, lo: f32, hi: f32) -> f32 {
    if hi < lo {
        lo
    } else {
        v.max(lo).min(hi)
    }
}

/// Apply a drag of (dx, dy) video pixels to the rectangle, keeping it inside the frame.
/// `ratio` = Some(w/h) locks the aspect ratio.
pub fn apply_drag(
    r: &mut CropRect,
    handle: Handle,
    dx: f32,
    dy: f32,
    fw: f32,
    fh: f32,
    min: f32,
    ratio: Option<f32>,
) {
    let min = min.min(fw).min(fh);
    if handle == Handle::Move {
        let (w, h) = (r.w(), r.h());
        let nx = clampf(r.x0 + dx, 0.0, fw - w);
        let ny = clampf(r.y0 + dy, 0.0, fh - h);
        *r = CropRect { x0: nx, y0: ny, x1: nx + w, y1: ny + h };
        return;
    }
    let (l, rt, t, b) = sides(handle);

    let Some(ra) = ratio else {
        if l {
            r.x0 = clampf(r.x0 + dx, 0.0, r.x1 - min);
        }
        if rt {
            r.x1 = clampf(r.x1 + dx, r.x0 + min, fw);
        }
        if t {
            r.y0 = clampf(r.y0 + dy, 0.0, r.y1 - min);
        }
        if b {
            r.y1 = clampf(r.y1 + dy, r.y0 + min, fh);
        }
        return;
    };

    let min_w = (min * ra).max(min);
    let min_h = (min / ra).max(min);
    let corner = (l || rt) && (t || b);

    if corner {
        let sx = if rt { 1.0 } else { -1.0 };
        let sy = if b { 1.0 } else { -1.0 };
        let ax = if rt { r.x0 } else { r.x1 };
        let ay = if b { r.y0 } else { r.y1 };
        let cx = if rt { r.x1 } else { r.x0 } + dx;
        let cy = if b { r.y1 } else { r.y0 } + dy;
        let wc = ((cx - ax) * sx).max(((cy - ay) * sy) * ra);
        let avail_x = if rt { fw - ax } else { ax };
        let avail_y = if b { fh - ay } else { ay };
        let max_w = avail_x.min(avail_y * ra);
        let w = clampf(wc, min_w.min(max_w), max_w);
        let h = w / ra;
        let (nx0, nx1) = if rt { (ax, ax + w) } else { (ax - w, ax) };
        let (ny0, ny1) = if b { (ay, ay + h) } else { (ay - h, ay) };
        *r = CropRect { x0: nx0, y0: ny0, x1: nx1, y1: ny1 };
    } else if l || rt {
        let sx = if rt { 1.0 } else { -1.0 };
        let ax = if rt { r.x0 } else { r.x1 };
        let cy = (r.y0 + r.y1) / 2.0;
        let max_h = 2.0 * cy.min(fh - cy);
        let avail_x = if rt { fw - ax } else { ax };
        let max_w = avail_x.min(max_h * ra);
        let w = clampf(r.w() + dx * sx, min_w.min(max_w), max_w);
        let h = w / ra;
        let (nx0, nx1) = if rt { (ax, ax + w) } else { (ax - w, ax) };
        *r = CropRect { x0: nx0, y0: cy - h / 2.0, x1: nx1, y1: cy + h / 2.0 };
    } else {
        let sy = if b { 1.0 } else { -1.0 };
        let ay = if b { r.y0 } else { r.y1 };
        let cx = (r.x0 + r.x1) / 2.0;
        let max_w_c = 2.0 * cx.min(fw - cx);
        let avail_y = if b { fh - ay } else { ay };
        let max_h = avail_y.min(max_w_c / ra);
        let h = clampf(r.h() + dy * sy, min_h.min(max_h), max_h);
        let w = h * ra;
        let (ny0, ny1) = if b { (ay, ay + h) } else { (ay - h, ay) };
        *r = CropRect { x0: cx - w / 2.0, y0: ny0, x1: cx + w / 2.0, y1: ny1 };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inside(r: &CropRect, fw: f32, fh: f32) -> bool {
        r.x0 >= -0.01 && r.y0 >= -0.01 && r.x1 <= fw + 0.01 && r.y1 <= fh + 0.01 && r.w() > 0.0 && r.h() > 0.0
    }

    #[test]
    fn fit_aspect_is_centered_and_correct() {
        let r = fit_aspect(1920.0, 1080.0, 1.0);
        assert!((r.w() - 1080.0).abs() < 0.01 && (r.h() - 1080.0).abs() < 0.01);
        assert!((r.x0 - 420.0).abs() < 0.01);
        let r = fit_aspect(1920.0, 1080.0, 9.0 / 16.0);
        assert!((r.w() / r.h() - 9.0 / 16.0).abs() < 0.001);
        assert!(inside(&r, 1920.0, 1080.0));
    }

    #[test]
    fn full_detection_and_even_pixels() {
        let f = CropRect::full(1081.0, 721.0);
        assert!(f.is_full(1081.0, 721.0));
        let r = CropRect { x0: 11.0, y0: 5.0, x1: 500.0, y1: 301.0 };
        let (w, h, x, y) = r.to_pixels(1081, 721);
        assert!(w % 2 == 0 && h % 2 == 0 && x % 2 == 0 && y % 2 == 0);
        assert!(x + w <= 1081 && y + h <= 721);
        let (w, h, x, y) = CropRect::full(1081.0, 721.0).to_pixels(1081, 721);
        assert!(x + w <= 1081 && y + h <= 721 && w == 1080 && h == 720);
    }

    #[test]
    fn move_stays_inside() {
        let mut r = CropRect { x0: 100.0, y0: 100.0, x1: 300.0, y1: 250.0 };
        apply_drag(&mut r, Handle::Move, 5000.0, -5000.0, 1920.0, 1080.0, 16.0, None);
        assert!(inside(&r, 1920.0, 1080.0));
        assert!((r.w() - 200.0).abs() < 0.01 && (r.h() - 150.0).abs() < 0.01);
    }

    #[test]
    fn free_resize_respects_min_and_bounds() {
        let mut r = CropRect::full(640.0, 360.0);
        apply_drag(&mut r, Handle::SE, -10000.0, -10000.0, 640.0, 360.0, 16.0, None);
        assert!(r.w() >= 16.0 - 0.01 && r.h() >= 16.0 - 0.01);
        apply_drag(&mut r, Handle::SE, 10000.0, 10000.0, 640.0, 360.0, 16.0, None);
        assert!(inside(&r, 640.0, 360.0));
    }

    #[test]
    fn locked_ratio_is_kept_for_all_handles() {
        for ra in [1.0f32, 16.0 / 9.0, 9.0 / 16.0, 4.0 / 3.0] {
            for h in [Handle::N, Handle::S, Handle::E, Handle::W, Handle::NE, Handle::NW, Handle::SE, Handle::SW] {
                for (dx, dy) in [(-80.0, -50.0), (120.0, 90.0), (-9999.0, 9999.0), (9999.0, -9999.0)] {
                    let mut r = fit_aspect(1920.0, 1080.0, ra);
                    // shrink first so there is room to grow
                    apply_drag(&mut r, Handle::SE, -300.0, -300.0, 1920.0, 1080.0, 16.0, Some(ra));
                    apply_drag(&mut r, h, dx, dy, 1920.0, 1080.0, 16.0, Some(ra));
                    assert!(inside(&r, 1920.0, 1080.0), "{ra} {h:?} {dx} {dy} -> {r:?}");
                    assert!((r.w() / r.h() - ra).abs() < 0.01, "{ra} {h:?} -> {r:?}");
                }
            }
        }
    }

    #[test]
    fn hit_test_basics() {
        let r = CropRect { x0: 100.0, y0: 100.0, x1: 300.0, y1: 200.0 };
        assert_eq!(hit_test(&r, 100.0, 100.0, 10.0), Some(Handle::NW));
        assert_eq!(hit_test(&r, 300.0, 200.0, 10.0), Some(Handle::SE));
        assert_eq!(hit_test(&r, 200.0, 101.0, 10.0), Some(Handle::N));
        assert_eq!(hit_test(&r, 299.0, 150.0, 10.0), Some(Handle::E));
        assert_eq!(hit_test(&r, 200.0, 150.0, 10.0), Some(Handle::Move));
        assert_eq!(hit_test(&r, 500.0, 500.0, 10.0), None);
    }
}
