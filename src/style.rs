use nih_plug::prelude::*;
use nih_plug_egui::egui::{
    self, epaint::PathStroke, Align2, Color32, ColorImage, CornerRadius, FontId, Pos2, Rect,
    Response, Sense, Shape, Stroke, StrokeKind, TextureHandle, TextureOptions, Ui, Vec2,
};
use std::f32::consts::PI;

pub const PINK: Color32 = Color32::from_rgb(255, 94, 170);
pub const PINK_DIM: Color32 = Color32::from_rgb(150, 50, 100);
pub const CYAN: Color32 = Color32::from_rgb(70, 225, 235);
pub const AMBER: Color32 = Color32::from_rgb(255, 196, 80);
pub const TEXT: Color32 = Color32::from_rgb(200, 200, 206);
pub const TEXT_DIM: Color32 = Color32::from_rgb(120, 120, 128);
pub const SCREEN_BG: Color32 = Color32::from_rgb(13, 13, 15);

pub fn alpha(c: Color32, a: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (a.clamp(0.0, 1.0) * 255.0) as u8)
}

pub fn spaced(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

pub fn metal_texture(ctx: &egui::Context) -> TextureHandle {
    let (w, h) = (512usize, 512usize);
    let mut seed = 0x1234_5678u32;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed as f32 / u32::MAX as f32) - 0.5
    };
    // Horizontal brushing: per-row streak value plus fine per-pixel grain.
    let rows: Vec<f32> = (0..h).map(|_| rnd() * 7.0).collect();
    let mut pixels = Vec::with_capacity(w * h);
    for y in 0..h {
        let mut streak = 0.0f32;
        for _x in 0..w {
            streak = streak * 0.97 + rnd() * 1.6;
            let v = (33.0 + rows[y] + streak + rnd() * 2.0).clamp(0.0, 255.0) as u8;
            pixels.push(Color32::from_rgb(v, v, v.saturating_add(2)));
        }
    }
    let img = ColorImage { size: [w, h], pixels };
    ctx.load_texture("metal", img, TextureOptions::LINEAR_REPEAT)
}

pub fn logo_texture(ctx: &egui::Context) -> Option<TextureHandle> {
    let bytes: &[u8] = include_bytes!("../assets/logo.png");
    let decoder = png::Decoder::new(bytes);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    let img = ColorImage::from_rgba_unmultiplied(
        [info.width as usize, info.height as usize],
        &buf[..info.buffer_size()],
    );
    Some(ctx.load_texture("logo", img, TextureOptions::LINEAR))
}

pub fn draw_background(ui: &Ui, rect: Rect, metal: &TextureHandle) {
    let p = ui.painter();
    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(rect.width() / 512.0, rect.height() / 512.0));
    p.image(metal.id(), rect, uv, Color32::WHITE);
    // Vignette edges
    for i in 0..10 {
        let r = rect.shrink(i as f32 * 2.0);
        p.rect_stroke(r, 0.0, Stroke::new(2.0_f32, alpha(Color32::BLACK, 0.25 - i as f32 * 0.025)), StrokeKind::Inside);
    }
    for c in [
        rect.left_top() + Vec2::new(14.0, 14.0),
        rect.right_top() + Vec2::new(-14.0, 14.0),
        rect.left_bottom() + Vec2::new(14.0, -14.0),
        rect.right_bottom() + Vec2::new(-14.0, -14.0),
    ] {
        screw(ui, c);
    }
}

pub fn screw(ui: &Ui, c: Pos2) {
    let p = ui.painter();
    p.circle_filled(c + Vec2::new(0.6, 1.0), 6.0, alpha(Color32::BLACK, 0.6));
    p.circle_filled(c, 6.0, Color32::from_rgb(78, 78, 84));
    p.circle_filled(c - Vec2::new(1.0, 1.0), 4.5, Color32::from_rgb(98, 98, 104));
    p.circle_stroke(c, 6.0, Stroke::new(1.0_f32, Color32::from_rgb(40, 40, 44)));
    let d = Vec2::new(3.2, 3.2);
    let s = Stroke::new(1.4_f32, Color32::from_rgb(38, 38, 42));
    p.line_segment([c - d, c + d], s);
    p.line_segment([c + Vec2::new(-d.x, d.y), c + Vec2::new(d.x, -d.y)], s);
}

pub fn glow_rect(ui: &Ui, rect: Rect, radius: f32, color: Color32, strength: f32) {
    let p = ui.painter();
    for i in (1..=6).rev() {
        let e = i as f32 * 2.0;
        p.rect_filled(rect.expand(e), radius + e, alpha(color, strength * 0.05 * (7 - i) as f32 / 6.0));
    }
}

pub fn panel(ui: &Ui, rect: Rect, radius: f32) {
    let p = ui.painter();
    p.rect_filled(rect.translate(Vec2::new(0.0, 2.0)), radius, alpha(Color32::BLACK, 0.55));
    p.rect_filled(rect, radius, Color32::from_rgb(22, 22, 25));
    p.rect_stroke(rect, radius, Stroke::new(1.0_f32, Color32::from_rgb(58, 58, 64)), StrokeKind::Inside);
}

pub fn screen(ui: &Ui, rect: Rect) {
    glow_rect(ui, rect, 10.0, PINK, 0.55);
    let p = ui.painter();
    p.rect_filled(rect.expand(4.0), 13.0, Color32::from_rgb(52, 52, 58));
    p.rect_filled(rect, 10.0, SCREEN_BG);
    p.rect_stroke(rect, 10.0, Stroke::new(1.2_f32, alpha(PINK, 0.55)), StrokeKind::Inside);
}

/// Pill-shaped toggle button. Returns the response; caller handles `clicked()`.
pub fn pill(ui: &mut Ui, rect: Rect, text: &str, active: bool) -> Response {
    let resp = ui.interact(rect, ui.id().with(("pill", rect.min.x as i32, rect.min.y as i32)), Sense::click());
    let r = rect.height() / 2.0;
    let hovered = resp.hovered();
    if active {
        glow_rect(ui, rect, r, PINK, 0.9);
    }
    let p = ui.painter();
    p.rect_filled(rect.translate(Vec2::new(0.0, 1.5)), r, alpha(Color32::BLACK, 0.6));
    if active {
        p.rect_filled(rect, r, PINK);
        p.rect_filled(
            Rect::from_min_max(rect.min + Vec2::new(4.0, 2.0), Pos2::new(rect.max.x - 4.0, rect.center().y)),
            r,
            alpha(Color32::WHITE, 0.18),
        );
    } else {
        p.rect_filled(rect, r, if hovered { Color32::from_rgb(40, 40, 46) } else { Color32::from_rgb(28, 28, 32) });
        p.rect_stroke(rect, r, Stroke::new(1.0_f32, Color32::from_rgb(72, 72, 80)), StrokeKind::Inside);
    }
    let color = if active { Color32::from_rgb(30, 10, 22) } else if hovered { Color32::WHITE } else { TEXT };
    p.text(rect.center(), Align2::CENTER_CENTER, text, FontId::proportional(12.0), color);
    resp
}

pub fn label(ui: &Ui, pos: Pos2, align: Align2, text: &str, size: f32, color: Color32) -> Rect {
    ui.painter().text(pos, align, text, FontId::proportional(size), color)
}

pub fn value_pill(ui: &Ui, center: Pos2, text: &str, color: Color32) {
    let p = ui.painter();
    let galley = p.layout_no_wrap(text.to_owned(), FontId::monospace(11.0), color);
    let size = galley.size() + Vec2::new(14.0, 6.0);
    let rect = Rect::from_center_size(center, size);
    p.rect_filled(rect, size.y / 2.0, Color32::from_rgba_unmultiplied(10, 10, 12, 230));
    p.rect_stroke(rect, size.y / 2.0, Stroke::new(1.0_f32, alpha(color, 0.6)), StrokeKind::Inside);
    p.galley(rect.min + Vec2::new(7.0, 3.0), galley, color);
}

/// Tooltip-style box with several lines, kept inside `bounds`.
pub fn info_box(ui: &Ui, anchor: Pos2, bounds: Rect, lines: &[(String, Color32)]) {
    let p = ui.painter();
    let galleys: Vec<_> = lines
        .iter()
        .map(|(t, c)| p.layout_no_wrap(t.clone(), FontId::monospace(11.5), *c))
        .collect();
    let w = galleys.iter().map(|g| g.size().x).fold(0.0, f32::max) + 16.0;
    let h = galleys.iter().map(|g| g.size().y + 2.0).sum::<f32>() + 10.0;
    let mut min = anchor + Vec2::new(16.0, 16.0);
    if min.x + w > bounds.right() - 4.0 {
        min.x = anchor.x - w - 16.0;
    }
    if min.y + h > bounds.bottom() - 4.0 {
        min.y = anchor.y - h - 16.0;
    }
    let rect = Rect::from_min_size(min, Vec2::new(w, h));
    p.rect_filled(rect, CornerRadius::same(8), Color32::from_rgba_unmultiplied(12, 12, 14, 235));
    p.rect_stroke(rect, CornerRadius::same(8), Stroke::new(1.0_f32, alpha(PINK, 0.7)), StrokeKind::Inside);
    let mut y = rect.top() + 5.0;
    for g in galleys {
        let gh = g.size().y;
        p.galley(Pos2::new(rect.left() + 8.0, y), g, Color32::WHITE);
        y += gh + 2.0;
    }
}

fn arc_points(c: Pos2, r: f32, a0: f32, a1: f32) -> Vec<Pos2> {
    let steps = (((a1 - a0).abs() / (PI / 48.0)).ceil() as usize).max(2);
    (0..=steps)
        .map(|i| {
            let a = a0 + (a1 - a0) * i as f32 / steps as f32;
            c + Vec2::new(a.cos(), a.sin()) * r
        })
        .collect()
}

const KNOB_START: f32 = PI * 0.75;
const KNOB_SWEEP: f32 = PI * 1.5;

/// Large metal knob with a glowing pink value ring, bound to a FloatParam.
pub fn knob(ui: &mut Ui, center: Pos2, radius: f32, name: &str, param: &FloatParam, setter: &ParamSetter) {
    let hit = Rect::from_center_size(center, Vec2::splat(radius * 2.4));
    let resp = ui.interact(hit, ui.id().with(("knob", name)), Sense::click_and_drag());

    if resp.double_clicked() {
        setter.begin_set_parameter(param);
        setter.set_parameter(param, param.default_plain_value());
        setter.end_set_parameter(param);
    } else {
        if resp.drag_started() {
            setter.begin_set_parameter(param);
        }
        if resp.dragged() {
            let fine = ui.input(|i| i.modifiers.shift);
            let delta = -resp.drag_delta().y / if fine { 1200.0 } else { 220.0 };
            let v = (param.unmodulated_normalized_value() + delta).clamp(0.0, 1.0);
            setter.set_parameter_normalized(param, v);
        }
        if resp.drag_stopped() {
            setter.end_set_parameter(param);
        }
    }

    let norm = param.unmodulated_normalized_value();
    let p = ui.painter();
    let ring_r = radius + 9.0;

    // Ring track + glowing value arc.
    p.add(Shape::line(arc_points(center, ring_r, KNOB_START, KNOB_START + KNOB_SWEEP), Stroke::new(4.0_f32, Color32::from_rgb(34, 34, 38))));
    if norm > 0.002 {
        let pts = arc_points(center, ring_r, KNOB_START, KNOB_START + KNOB_SWEEP * norm);
        for (w, a) in [(14.0, 0.07), (9.0_f32, 0.14), (6.0, 0.3)] {
            p.add(Shape::line(pts.clone(), PathStroke::new(w, alpha(PINK, a))));
        }
        p.add(Shape::line(pts, Stroke::new(3.0_f32, PINK)));
    }
    // Tick marks
    for i in 0..=10 {
        let a = KNOB_START + KNOB_SWEEP * i as f32 / 10.0;
        let dir = Vec2::new(a.cos(), a.sin());
        p.line_segment([center + dir * (ring_r + 5.0), center + dir * (ring_r + 8.0)], Stroke::new(1.0_f32, TEXT_DIM));
    }

    // Body: drop shadow, outer bevel, brushed cap.
    p.circle_filled(center + Vec2::new(0.0, 3.0), radius + 1.0, alpha(Color32::BLACK, 0.7));
    p.circle_filled(center, radius, Color32::from_rgb(24, 24, 27));
    for i in 0..8 {
        let t = i as f32 / 8.0;
        let v = (52.0 + 30.0 * (1.0 - t)) as u8;
        p.circle_filled(center - Vec2::new(0.0, radius * 0.08 * (1.0 - t)), radius * (0.9 - t * 0.08), Color32::from_rgb(v, v, v + 3));
    }
    p.circle_filled(center, radius * 0.72, Color32::from_rgb(46, 46, 50));
    p.circle_stroke(center, radius * 0.72, Stroke::new(1.0_f32, Color32::from_rgb(30, 30, 34)));
    p.circle_filled(center - Vec2::new(radius * 0.18, radius * 0.25), radius * 0.28, alpha(Color32::WHITE, 0.05));
    p.circle_stroke(center, radius, Stroke::new(1.0_f32, Color32::from_rgb(90, 90, 96)));

    // Pointer
    let a = KNOB_START + KNOB_SWEEP * norm;
    let dir = Vec2::new(a.cos(), a.sin());
    p.line_segment([center + dir * radius * 0.3, center + dir * radius * 0.82], Stroke::new(5.0_f32, alpha(PINK, 0.25)));
    p.line_segment([center + dir * radius * 0.3, center + dir * radius * 0.82], Stroke::new(2.5_f32, Color32::WHITE));

    label(ui, center + Vec2::new(0.0, ring_r + 18.0), Align2::CENTER_CENTER, &spaced(name), 11.0, TEXT);
    let active = resp.hovered() || resp.dragged();
    value_pill(ui, center + Vec2::new(0.0, ring_r + 38.0), &param.to_string(), if active { Color32::WHITE } else { PINK });
}
