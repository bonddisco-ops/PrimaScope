use crate::dsp::{beat_label, CompSettings, FreqAnalyzer};
use crate::style::*;
use crate::{ChannelMode, Frame, ScopeParams, Shared};
use nih_plug::prelude::*;
use nih_plug_egui::create_egui_editor;
use nih_plug_egui::egui::{
    self, Align2, Color32, FontData, FontDefinitions, FontFamily, Mesh, Pos2, Rect, Sense, Shape,
    Stroke, TextureHandle, Vec2,
};
use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

pub const WIDTH: u32 = 1100;
pub const HEIGHT: u32 = 760;

const MAX_CAPTURE_SECS: f32 = 30.0;
const LIVE_SECS: f32 = 4.0;
pub const BAR_OPTIONS: [(f64, &str); 6] = [
    (0.25, "1/4 BAR"),
    (0.5, "1/2 BAR"),
    (1.0, "1 BAR"),
    (2.0, "2 BARS"),
    (4.0, "4 BARS"),
    (8.0, "8 BARS"),
];
pub const SEC_OPTIONS: [(f32, &str); 6] =
    [(1.0, "1 S"), (2.0, "2 S"), (5.0, "5 S"), (10.0, "10 S"), (20.0, "20 S"), (30.0, "30 S")];
const LIVE_WINDOWS: [f32; 8] = [10.0, 20.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2000.0];

const LEVEL_COLOR: Color32 = Color32::from_rgb(235, 235, 245);
const COMP_COLOR: Color32 = Color32::from_rgb(255, 255, 255);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    L,
    R,
    M,
    S,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::L => "LEFT",
            Kind::R => "RIGHT",
            Kind::M => "MID",
            Kind::S => "SIDE",
        }
    }
}

#[inline]
fn val(f: &Frame, k: Kind, sc: bool) -> f32 {
    let (l, r) = if sc { (f.sl, f.sr) } else { (f.l, f.r) };
    match k {
        Kind::L => l,
        Kind::R => r,
        Kind::M => (l + r) * 0.5,
        Kind::S => (l - r) * 0.5,
    }
}

fn lanes_for(m: ChannelMode) -> &'static [Kind] {
    match m {
        ChannelMode::LR => &[Kind::L, Kind::R],
        ChannelMode::MS => &[Kind::M, Kind::S],
        ChannelMode::Left => &[Kind::L],
        ChannelMode::Right => &[Kind::R],
        ChannelMode::Mid => &[Kind::M],
        ChannelMode::Side => &[Kind::S],
    }
}

fn to_db(a: f32) -> f32 {
    20.0 * a.abs().max(1e-9).log10()
}

fn from_db(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

enum RecState {
    Idle,
    /// Waiting for the next bar line before recording `len` samples.
    Armed { len: usize, prev_beat: f64 },
    Recording { len: usize },
}

pub struct Capture {
    frames: Vec<Frame>,
    sr: f32,
    tempo: f32,
    bpb: f32,
}

#[derive(Default)]
struct CompCache {
    key: Option<(CompSettings, bool, usize, usize, bool)>,
    start: usize,
    gain: Vec<f32>,
    gr: Vec<f32>,
}

pub struct EditorData {
    consumer: rtrb::Consumer<Frame>,
    shared: Arc<Shared>,
    live: VecDeque<Frame>,
    rec: RecState,
    rec_buf: Vec<Frame>,
    capture: Option<Capture>,
    frozen: bool,
    view_start: f64,
    view_len: f64,
    /// Ruler endpoints as source indices (frozen) or offsets from the newest sample (live).
    ruler: Option<(f64, f64)>,
    ruler_dragging: bool,
    ruler_on: bool,
    line_mode: bool,
    drag_line: Option<usize>,
    show_adv: bool,
    comp: CompCache,
    analyzer: Option<FreqAnalyzer>,
    freq_cache: Option<((i64, u8, bool), Option<f32>)>,
    msg: Option<(String, f64)>,
    metal: Option<TextureHandle>,
    logo: Option<TextureHandle>,
}

impl EditorData {
    pub fn new(consumer: rtrb::Consumer<Frame>, shared: Arc<Shared>) -> Self {
        Self {
            consumer,
            shared,
            live: VecDeque::new(),
            rec: RecState::Idle,
            rec_buf: Vec::new(),
            capture: None,
            frozen: false,
            view_start: 0.0,
            view_len: 0.0,
            ruler: None,
            ruler_dragging: false,
            ruler_on: false,
            line_mode: false,
            drag_line: None,
            show_adv: true,
            comp: CompCache::default(),
            analyzer: None,
            freq_cache: None,
            msg: None,
            metal: None,
            logo: None,
        }
    }

    fn say(&mut self, now: f64, s: impl Into<String>) {
        self.msg = Some((s.into(), now + 4.0));
    }

    /// Drain the audio ring buffer, feed the live view and the recorder.
    fn pull(&mut self) {
        let sr = self.shared.sample_rate.load(Ordering::Relaxed).max(1.0);
        let bpb = self.shared.beats_per_bar.load(Ordering::Relaxed).max(0.25) as f64;
        let cap = (sr * LIVE_SECS) as usize;
        let n = self.consumer.slots();
        if n == 0 {
            return;
        }
        let Ok(chunk) = self.consumer.read_chunk(n) else { return };
        let (a, b) = chunk.as_slices();
        let mut finished = false;
        for f in a.iter().chain(b.iter()) {
            self.live.push_back(*f);
            match &mut self.rec {
                RecState::Idle => {}
                RecState::Armed { len, prev_beat } => {
                    let beat = f.beat;
                    if beat.is_finite() {
                        let cur_bar = (beat / bpb + 1e-9).floor();
                        let at_line = if prev_beat.is_finite() {
                            cur_bar != (*prev_beat / bpb + 1e-9).floor()
                        } else {
                            (beat / bpb).fract() < 1e-6
                        };
                        *prev_beat = beat;
                        if at_line {
                            let len = *len;
                            self.rec_buf.clear();
                            self.rec_buf.reserve(len);
                            self.rec_buf.push(*f);
                            self.rec = RecState::Recording { len };
                        }
                    } else {
                        *prev_beat = f64::NAN;
                    }
                }
                RecState::Recording { len } => {
                    if self.rec_buf.len() < *len {
                        self.rec_buf.push(*f);
                    }
                    if self.rec_buf.len() >= *len {
                        finished = true;
                    }
                }
            }
        }
        chunk.commit_all();
        while self.live.len() > cap {
            self.live.pop_front();
        }
        if finished {
            self.finish_recording();
        }
    }

    fn finish_recording(&mut self) {
        self.rec = RecState::Idle;
        if self.rec_buf.is_empty() {
            return;
        }
        let frames = std::mem::take(&mut self.rec_buf);
        self.view_start = 0.0;
        self.view_len = frames.len() as f64;
        self.capture = Some(Capture {
            frames,
            sr: self.shared.sample_rate.load(Ordering::Relaxed),
            tempo: self.shared.tempo.load(Ordering::Relaxed),
            bpb: self.shared.beats_per_bar.load(Ordering::Relaxed),
        });
        self.frozen = true;
        self.ruler = None;
        self.comp.key = None;
        self.freq_cache = None;
    }
}

pub fn create(
    params: Arc<ScopeParams>,
    data: Arc<Mutex<EditorData>>,
) -> Option<Box<dyn Editor>> {
    let build_data = data.clone();
    create_egui_editor(
        params.editor_state.clone(),
        (),
        move |ctx, _| {
            let mut fonts = FontDefinitions::default();
            fonts.font_data.insert(
                "montserrat".into(),
                Arc::new(FontData::from_static(include_bytes!(
                    "../assets/fonts/Montserrat-Bold.ttf"
                ))),
            );
            fonts
                .families
                .get_mut(&FontFamily::Proportional)
                .unwrap()
                .insert(0, "montserrat".into());
            ctx.set_fonts(fonts);
            let mut d = build_data.lock().unwrap();
            d.metal = Some(metal_texture(ctx));
            d.logo = logo_texture(ctx);
        },
        move |ctx, setter, _| {
            let mut guard = data.lock().unwrap();
            if guard.metal.is_none() {
                guard.metal = Some(metal_texture(ctx));
                guard.logo = logo_texture(ctx);
            }
            egui::CentralPanel::default().frame(egui::Frame::NONE).show(ctx, |ui| {
                draw(ui, setter, &params, &mut guard);
            });
            ctx.request_repaint();
        },
    )
}

fn draw(ui: &mut egui::Ui, setter: &ParamSetter, params: &ScopeParams, d: &mut EditorData) {
    let now = ui.input(|i| i.time);
    d.pull();

    let full = ui.max_rect();
    if let Some(metal) = &d.metal {
        draw_background(ui, full, metal);
    }

    // ---------------------------------------------------------------- header
    let header_y = full.top() + 40.0;
    if let Some(logo) = &d.logo {
        let h = 50.0;
        let w = h * logo.size()[0] as f32 / logo.size()[1] as f32;
        let r = Rect::from_center_size(Pos2::new(full.left() + 62.0, header_y), Vec2::new(w, h));
        glow_rect(ui, r.shrink(10.0), 12.0, PINK, 0.5);
        ui.painter().image(
            logo.id(),
            r,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    }
    let title_pos = Pos2::new(full.left() + 100.0, header_y);
    for (off, a) in [(2.0, 0.12), (1.0, 0.2)] {
        for dxy in [Vec2::new(off, 0.0), Vec2::new(-off, 0.0), Vec2::new(0.0, off), Vec2::new(0.0, -off)] {
            label(ui, title_pos + dxy, Align2::LEFT_CENTER, &spaced("PRIMASCOPE"), 22.0, alpha(PINK, a));
        }
    }
    label(ui, title_pos, Align2::LEFT_CENTER, &spaced("PRIMASCOPE"), 22.0, PINK);

    let mut settings = params.ui.read().unwrap().clone();
    let settings_before = settings.clone();

    // Channel mode pills
    let modes = [
        ("L / R", ChannelMode::LR),
        ("M / S", ChannelMode::MS),
        ("L", ChannelMode::Left),
        ("R", ChannelMode::Right),
        ("M", ChannelMode::Mid),
        ("S", ChannelMode::Side),
    ];
    let mut x = full.right() - 40.0;
    let sc_rect = Rect::from_min_size(Pos2::new(x - 70.0, header_y - 13.0), Vec2::new(70.0, 26.0));
    let sc_resp = pill(ui, sc_rect, "SC", settings.show_sidechain);
    if sc_resp.clicked() {
        settings.show_sidechain = !settings.show_sidechain;
    }
    x -= 90.0;
    for (name, mode) in modes.iter().rev() {
        let w = if name.len() > 2 { 64.0 } else { 36.0 };
        let r = Rect::from_min_size(Pos2::new(x - w, header_y - 13.0), Vec2::new(w, 26.0));
        if pill(ui, r, name, settings.channel_mode == *mode).clicked() {
            settings.channel_mode = *mode;
        }
        x -= w + 8.0;
    }
    let sc_peak = d.shared.sc_peak.load(Ordering::Relaxed);
    if settings.show_sidechain {
        let txt = if sc_peak > 1e-6 { "SIDECHAIN" } else { "SC: NO SIGNAL" };
        let col = if sc_peak > 1e-6 { CYAN } else { TEXT_DIM };
        label(ui, Pos2::new(sc_rect.center().x, sc_rect.bottom() + 10.0), Align2::CENTER_CENTER, txt, 9.0, col);
    }

    // ---------------------------------------------------------------- layout
    let adv_h = if d.show_adv { 170.0 } else { 0.0 };
    let screen_rect = Rect::from_min_max(
        Pos2::new(full.left() + 28.0, full.top() + 84.0),
        Pos2::new(full.right() - 28.0, full.bottom() - 28.0 - adv_h - 34.0 - 48.0),
    );
    screen(ui, screen_rect);

    // ---------------------------------------------------------------- data source
    let sr_live = d.shared.sample_rate.load(Ordering::Relaxed).max(1.0);
    if d.frozen && d.capture.is_none() {
        d.frozen = false;
    }
    let (src, sr, frozen): (&[Frame], f32, bool) = match (&d.capture, d.frozen) {
        (Some(c), true) => (&c.frames[..], c.sr.max(1.0), true),
        _ => (&d.live.make_contiguous()[..], sr_live, false),
    };
    let n = src.len();
    let (v0, vlen) = if frozen {
        let vlen = d.view_len.clamp(32.0_f64.min(n as f64), n as f64);
        let v0 = d.view_start.clamp(0.0, (n as f64 - vlen).max(0.0));
        (v0, vlen)
    } else {
        let win = (settings.live_window_ms as f64 * 0.001 * sr as f64).min(n as f64);
        (n as f64 - win, win)
    };
    let base = if frozen { 0.0 } else { n as f64 };

    let inner = screen_rect.shrink2(Vec2::new(44.0, 10.0));
    let inner = Rect::from_min_max(Pos2::new(screen_rect.left() + 44.0, inner.top()), Pos2::new(screen_rect.right() - 12.0, inner.bottom() - 16.0));
    let w = inner.width().max(1.0);
    let x_to_idx = |px: f32| v0 + ((px - inner.left()) / w) as f64 * vlen;
    let idx_to_x = |i: f64| inner.left() + ((i - v0) / vlen.max(1.0)) as f32 * w;

    let lanes = lanes_for(settings.channel_mode);
    let lane_h = inner.height() / lanes.len() as f32;
    let lane_rects: Vec<Rect> = (0..lanes.len())
        .map(|i| {
            Rect::from_min_size(Pos2::new(inner.left(), inner.top() + i as f32 * lane_h), Vec2::new(w, lane_h))
                .shrink2(Vec2::new(0.0, 6.0))
        })
        .collect();
    let vz = settings.vzoom.clamp(0.25, 64.0);
    let half = |r: &Rect| r.height() / 2.0 - 2.0;
    let y_of = |r: &Rect, a: f32| r.center().y - (a * vz).clamp(-1.05, 1.05) * half(r);
    let amp_of = |r: &Rect, y: f32| ((r.center().y - y) / half(r) / vz).abs();

    // ---------------------------------------------------------------- compressor preview
    let comp_on = params.comp_on.value();
    let key_sc = params.comp_key_sc.value();
    let cs = CompSettings {
        threshold_db: params.threshold.value(),
        ratio: params.ratio.value(),
        attack_ms: params.attack.value(),
        release_ms: params.release.value(),
        knee_db: params.knee.value(),
        makeup_db: params.makeup.value(),
    };
    if comp_on && n > 0 {
        let (start, end) = if frozen {
            (0, n)
        } else {
            let pre = (sr * 0.5) as usize;
            ((v0 as usize).saturating_sub(pre), n)
        };
        let key = (cs, key_sc, start, end, frozen);
        if frozen && d.comp.key == Some(key) {
            // cached
        } else {
            let det = |i: usize| {
                let f = &src[start + i];
                if key_sc {
                    f.sl.abs().max(f.sr.abs())
                } else {
                    f.l.abs().max(f.r.abs())
                }
            };
            cs.run(end - start, sr, det, &mut d.comp.gain, &mut d.comp.gr);
            d.comp.start = start;
            d.comp.key = Some(key);
        }
    }
    let comp_ok = comp_on && d.comp.key.is_some() && !d.comp.gain.is_empty();
    let gain_at = |i: usize| -> f32 {
        if i >= d.comp.start && i - d.comp.start < d.comp.gain.len() {
            d.comp.gain[i - d.comp.start]
        } else {
            1.0
        }
    };
    let gr_at = |i: usize| -> f32 {
        if i >= d.comp.start && i - d.comp.start < d.comp.gr.len() {
            d.comp.gr[i - d.comp.start]
        } else {
            0.0
        }
    };

    // ---------------------------------------------------------------- columns
    let cols = w.floor() as usize;
    let spp = vlen / cols.max(1) as f64;
    let col_range = |c: usize| -> (usize, usize) {
        let a = (v0 + c as f64 * spp).floor().max(0.0) as usize;
        let b = ((v0 + (c + 1) as f64 * spp).floor() as usize).max(a + 1).min(n);
        (a.min(n), b)
    };
    let minmax = |f: &dyn Fn(usize) -> f32| -> Vec<(f32, f32)> {
        (0..cols)
            .map(|c| {
                let (a, b) = col_range(c);
                let mut lo = f32::MAX;
                let mut hi = f32::MIN;
                for i in a..b {
                    let v = f(i);
                    lo = lo.min(v);
                    hi = hi.max(v);
                }
                if lo > hi {
                    (0.0, 0.0)
                } else {
                    (lo, hi)
                }
            })
            .collect()
    };

    // Beat/bar grid from per-sample host beat positions.
    let bpb = if frozen {
        d.capture.as_ref().map(|c| c.bpb).unwrap_or(4.0)
    } else {
        d.shared.beats_per_bar.load(Ordering::Relaxed)
    }
    .max(0.25) as f64;
    let mut grid: Vec<(f32, bool, String)> = Vec::new();
    let mut have_beats = false;
    if n > 0 && cols > 0 {
        let mut prev: Option<f64> = None;
        for c in 0..cols {
            let (a, _) = col_range(c);
            let beat = src.get(a).map(|f| f.beat).unwrap_or(f64::NAN);
            if beat.is_finite() {
                have_beats = true;
                if let Some(p) = prev {
                    if (beat + 1e-6).floor() != (p + 1e-6).floor() {
                        let bi = (beat + 1e-6).floor();
                        let is_bar = ((bi / bpb) - (bi / bpb).round()).abs() < 1e-6;
                        let bar_no = (bi / bpb).floor() as i64 + 1;
                        let beat_no = (bi - (bar_no - 1) as f64 * bpb) as i64 + 1;
                        grid.push((inner.left() + c as f32, is_bar, format!("{bar_no}.{beat_no}")));
                    }
                }
                prev = Some(beat);
            } else {
                prev = None;
            }
        }
    }
    if !have_beats && n > 0 {
        let total_ms = vlen / sr as f64 * 1000.0;
        let steps = [0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0];
        let step = *steps.iter().find(|s| (**s / total_ms) * w as f64 >= 80.0).unwrap_or(&5000.0);
        let t0 = (v0 - base) / sr as f64 * 1000.0;
        let t1 = t0 + total_ms;
        let mut t = (t0 / step).ceil() * step;
        while t <= t1 {
            let i = t / 1000.0 * sr as f64 + base;
            grid.push((idx_to_x(i), false, format!("{} ms", trim_num(t))));
            t += step;
        }
    }

    // ---------------------------------------------------------------- draw lanes
    let painter = ui.painter().with_clip_rect(screen_rect.shrink(2.0));
    let level_lines: Vec<f32> = params.level_lines.read().unwrap().clone();
    for (li, (&kind, lr)) in lanes.iter().zip(lane_rects.iter()).enumerate() {
        // grid
        for (gx, is_bar, _) in &grid {
            let col = if *is_bar { alpha(PINK, 0.35) } else { Color32::from_rgb(34, 34, 38) };
            painter.line_segment([Pos2::new(*gx, lr.top()), Pos2::new(*gx, lr.bottom())], Stroke::new(1.0_f32, col));
        }
        for db in [0.0f32, -6.0, -12.0, -18.0, -24.0, -36.0] {
            let a = from_db(db);
            if a * vz > 1.02 {
                continue;
            }
            for s in [1.0, -1.0] {
                let y = y_of(lr, a * s);
                painter.line_segment([Pos2::new(lr.left(), y), Pos2::new(lr.right(), y)], Stroke::new(1.0_f32, Color32::from_rgb(30, 30, 34)));
            }
            label(ui, Pos2::new(screen_rect.left() + 38.0, y_of(lr, a)), Align2::RIGHT_CENTER, &trim_num(db as f64), 9.0, TEXT_DIM);
        }
        painter.line_segment([Pos2::new(lr.left(), lr.center().y), Pos2::new(lr.right(), lr.center().y)], Stroke::new(1.0_f32, Color32::from_rgb(48, 48, 54)));
        if li > 0 {
            let y = lr.top() - 6.0;
            painter.line_segment([Pos2::new(screen_rect.left() + 6.0, y), Pos2::new(screen_rect.right() - 6.0, y)], Stroke::new(1.0_f32, alpha(PINK, 0.25)));
        }

        // waveforms
        let draw_cols = |cols_v: &[(f32, f32)], color: Color32, glow: bool| {
            let mut mesh = Mesh::default();
            let mut glow_mesh = Mesh::default();
            for (c, (lo, hi)) in cols_v.iter().enumerate() {
                let x0 = inner.left() + c as f32;
                let ya = y_of(lr, *hi);
                let yb = y_of(lr, *lo).max(ya + 1.0);
                mesh.add_colored_rect(Rect::from_min_max(Pos2::new(x0, ya), Pos2::new(x0 + 1.0, yb)), color);
                if glow {
                    glow_mesh.add_colored_rect(Rect::from_min_max(Pos2::new(x0 - 1.0, ya - 1.5), Pos2::new(x0 + 2.0, yb + 1.5)), alpha(color, 0.18));
                }
            }
            if glow {
                painter.add(Shape::mesh(glow_mesh));
            }
            painter.add(Shape::mesh(mesh));
        };
        let draw_line = |f: &dyn Fn(usize) -> f32, color: Color32, width: f32| {
            let a = v0.floor().max(0.0) as usize;
            let b = ((v0 + vlen).ceil() as usize + 1).min(n);
            let pts: Vec<Pos2> = (a..b).map(|i| Pos2::new(idx_to_x(i as f64), y_of(lr, f(i)))).collect();
            if pts.len() > 1 {
                painter.add(Shape::line(pts.clone(), Stroke::new(width + 3.0, alpha(color, 0.18))));
                painter.add(Shape::line(pts, Stroke::new(width, color)));
            }
        };
        let fine = spp < 1.5;
        let plot = |f: &dyn Fn(usize) -> f32, color: Color32, glow: bool| {
            if fine {
                draw_line(f, color, 1.4);
            } else {
                draw_cols(&minmax(f), color, glow);
            }
        };

        if settings.show_sidechain {
            plot(&|i| val(&src[i], kind, true), alpha(CYAN, 0.75), false);
        }
        let main_col = if comp_ok { alpha(PINK, 0.35) } else { PINK };
        plot(&|i| val(&src[i], kind, false), main_col, !comp_ok);
        if comp_ok {
            plot(&|i| val(&src[i], kind, false) * gain_at(i), alpha(COMP_COLOR, 0.9), false);
            // Gain-reduction curve hanging from the top of the lane (0..24 dB).
            let gr_pts: Vec<Pos2> = (0..cols)
                .map(|c| {
                    let (a, b) = col_range(c);
                    let mut g = 0.0f32;
                    for i in a..b {
                        g = g.max(gr_at(i));
                    }
                    Pos2::new(inner.left() + c as f32, lr.top() + (g / 24.0).min(1.0) * lr.height() * 0.45)
                })
                .collect();
            painter.add(Shape::line(gr_pts.clone(), Stroke::new(4.0_f32, alpha(AMBER, 0.15))));
            painter.add(Shape::line(gr_pts, Stroke::new(1.5_f32, AMBER)));
            let ta = from_db(cs.threshold_db);
            for s in [1.0, -1.0] {
                let y = y_of(lr, ta * s);
                dashed(&painter, lr.left(), lr.right(), y, alpha(AMBER, 0.7));
            }
        }

        // level lines
        for (k, db) in level_lines.iter().enumerate() {
            let a = from_db(*db);
            let active = d.drag_line == Some(k);
            for s in [1.0, -1.0] {
                let y = y_of(lr, a * s);
                painter.line_segment([Pos2::new(lr.left(), y), Pos2::new(lr.right(), y)], Stroke::new(5.0_f32, alpha(PINK, 0.25)));
                painter.line_segment([Pos2::new(lr.left(), y), Pos2::new(lr.right(), y)], Stroke::new(if active { 2.0 } else { 1.3_f32 }, LEVEL_COLOR));
            }
            value_pill(ui, Pos2::new(lr.right() - 40.0, y_of(lr, a) - 10.0), &format!("{:.1} dB", db), LEVEL_COLOR);
        }

        // lane title + peak readout
        label(ui, Pos2::new(lr.left() + 8.0, lr.top() + 10.0), Align2::LEFT_CENTER, &spaced(kind.name()), 10.0, alpha(PINK, 0.9));
        let a = v0.floor().max(0.0) as usize;
        let b = ((v0 + vlen) as usize).min(n);
        let peak = (a..b).fold(0.0f32, |m, i| m.max(val(&src[i], kind, false).abs()));
        let mut ptxt = format!("PEAK {:.1} dB", to_db(peak));
        if comp_ok {
            let pc = (a..b).fold(0.0f32, |m, i| m.max((val(&src[i], kind, false) * gain_at(i)).abs()));
            ptxt = format!("{ptxt}  →  {:.1} dB", to_db(pc));
        }
        if settings.show_sidechain && sc_peak > 1e-6 || frozen && settings.show_sidechain {
            let ps = (a..b).fold(0.0f32, |m, i| m.max(val(&src[i], kind, true).abs()));
            if ps > 1e-6 {
                label(ui, Pos2::new(lr.right() - 8.0, lr.top() + 24.0), Align2::RIGHT_CENTER, &format!("SC {:.1} dB", to_db(ps)), 10.0, CYAN);
            }
        }
        label(ui, Pos2::new(lr.right() - 8.0, lr.top() + 10.0), Align2::RIGHT_CENTER, &ptxt, 10.0, TEXT);
    }

    // time / bar labels
    let mut last_x = f32::MIN;
    for (gx, is_bar, txt) in &grid {
        if have_beats && !is_bar && spp * 80.0 > (60.0 / d.shared.tempo.load(Ordering::Relaxed).max(1.0) * sr) as f64 {
            continue;
        }
        if *gx - last_x < 44.0 {
            continue;
        }
        last_x = *gx;
        label(ui, Pos2::new(*gx + 3.0, inner.bottom() + 8.0), Align2::LEFT_CENTER, txt, 9.0, if *is_bar { PINK } else { TEXT_DIM });
    }

    if n == 0 {
        label(ui, screen_rect.center(), Align2::CENTER_CENTER, &spaced("PLAY SOMETHING"), 14.0, TEXT_DIM);
    }

    // ---------------------------------------------------------------- interaction
    let resp = ui.interact(screen_rect, ui.id().with("scope"), Sense::click_and_drag());
    let hover = resp.hover_pos().filter(|p| inner.expand2(Vec2::new(0.0, 8.0)).contains(*p));
    let lane_at = |p: Pos2| lane_rects.iter().position(|r| r.expand2(Vec2::new(0.0, 6.0)).contains(p));
    let line_near = |p: Pos2| -> Option<usize> {
        let li = lane_at(p)?;
        let lr = &lane_rects[li];
        level_lines.iter().position(|db| {
            let a = from_db(*db);
            (p.y - y_of(lr, a)).abs() < 5.0 || (p.y - y_of(lr, -a)).abs() < 5.0
        })
    };

    if let Some(p) = hover {
        if line_near(p).is_some() || d.drag_line.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        } else if d.line_mode || d.ruler_on {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
    }

    // scroll: zoom
    if resp.hovered() {
        let (scroll, ctrl) = ui.input(|i| (i.smooth_scroll_delta.y, i.modifiers.command || i.modifiers.shift));
        if scroll.abs() > 0.1 {
            if ctrl {
                settings.vzoom = (settings.vzoom * (scroll * 0.004).exp()).clamp(0.25, 64.0);
            } else if frozen {
                let f = (-scroll as f64 * 0.004).exp();
                let new_len = (vlen * f).clamp(32.0_f64.min(n as f64), n as f64);
                let anchor = hover.map(|p| x_to_idx(p.x)).unwrap_or(v0 + vlen / 2.0);
                d.view_start = (anchor - (anchor - v0) * new_len / vlen).clamp(0.0, (n as f64 - new_len).max(0.0));
                d.view_len = new_len;
            } else {
                let f = (-scroll * 0.004).exp();
                settings.live_window_ms = (settings.live_window_ms * f).clamp(5.0, LIVE_SECS * 1000.0);
            }
        }
    }

    if resp.drag_started() {
        if let Some(p) = resp.interact_pointer_pos() {
            if let Some(k) = line_near(p) {
                d.drag_line = Some(k);
            } else if d.ruler_on {
                let i = x_to_idx(p.x) - base;
                d.ruler = Some((i, i));
                d.ruler_dragging = true;
            }
        }
    }
    if resp.dragged() {
        if let Some(p) = resp.interact_pointer_pos() {
            if let Some(k) = d.drag_line {
                if let Some(li) = lane_at(p).or(Some(0)) {
                    let a = amp_of(&lane_rects[li], p.y);
                    let db = to_db(a).clamp(-60.0, 6.0);
                    if let Some(v) = params.level_lines.write().unwrap().get_mut(k) {
                        *v = (db * 10.0).round() / 10.0;
                    }
                }
            } else if d.ruler_dragging {
                if let Some((a, _)) = d.ruler {
                    d.ruler = Some((a, x_to_idx(p.x.clamp(inner.left(), inner.right())) - base));
                }
            } else if frozen {
                let dx = resp.drag_delta().x as f64;
                d.view_start = (d.view_start - dx / w as f64 * vlen).clamp(0.0, (n as f64 - vlen).max(0.0));
            }
        }
    }
    if resp.drag_stopped() {
        d.drag_line = None;
        d.ruler_dragging = false;
    }
    if resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            if d.line_mode {
                if let Some(li) = lane_at(p) {
                    let db = to_db(amp_of(&lane_rects[li], p.y)).clamp(-60.0, 6.0);
                    params.level_lines.write().unwrap().push((db * 10.0).round() / 10.0);
                    d.line_mode = false;
                }
            }
        }
    }
    if resp.secondary_clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            if let Some(k) = line_near(p) {
                params.level_lines.write().unwrap().remove(k);
            } else {
                d.ruler = None;
            }
        }
    }
    if resp.double_clicked() && frozen && !d.ruler_on && !d.line_mode {
        d.view_start = 0.0;
        d.view_len = n as f64;
    }

    // ---------------------------------------------------------------- ruler overlay
    if let Some((ra, rb)) = d.ruler {
        let (ia, ib) = (ra + base, rb + base);
        let (xa, xb) = (idx_to_x(ia), idx_to_x(ib));
        let top = inner.top();
        let bot = inner.bottom();
        for xx in [xa, xb] {
            painter.line_segment([Pos2::new(xx, top), Pos2::new(xx, bot)], Stroke::new(4.0_f32, alpha(Color32::WHITE, 0.12)));
            painter.line_segment([Pos2::new(xx, top), Pos2::new(xx, bot)], Stroke::new(1.2_f32, Color32::WHITE));
        }
        let (l, r) = (xa.min(xb), xa.max(xb));
        painter.rect_filled(Rect::from_min_max(Pos2::new(l, top), Pos2::new(r, bot)), 0.0, alpha(Color32::WHITE, 0.04));
        let yb = top + 14.0;
        painter.line_segment([Pos2::new(l, yb), Pos2::new(r, yb)], Stroke::new(1.0_f32, Color32::WHITE));
        let dt = (ib - ia).abs() / sr as f64;
        let mut txt = format!("Δ {} ms", fmt_ms(dt * 1000.0));
        if dt > 0.0 {
            txt.push_str(&format!("  ·  {:.2} Hz", 1.0 / dt));
        }
        let tempo = if frozen {
            d.capture.as_ref().map(|c| c.tempo).unwrap_or(0.0)
        } else {
            d.shared.tempo.load(Ordering::Relaxed)
        };
        if tempo > 0.0 {
            let beats = dt * tempo as f64 / 60.0;
            txt.push_str(&format!("  ·  {:.3} beats", beats));
            if let Some(lbl) = beat_label(beats) {
                txt.push_str(&format!(" = {lbl}"));
            }
        }
        if let Some(lr) = lane_rects.first() {
            let kind = lanes[0];
            let pk = |i: f64| {
                let c = i.round() as isize;
                let span = (spp.max(1.0) * 1.5) as isize;
                (c - span..=c + span)
                    .filter_map(|j| src.get(j as usize).filter(|_| j >= 0))
                    .fold(0.0f32, |m, f| m.max(val(f, kind, false).abs()))
            };
            let dd = to_db(pk(ib)) - to_db(pk(ia));
            txt.push_str(&format!("  ·  Δ {:+.1} dB", dd));
            let _ = lr;
        }
        value_pill(ui, Pos2::new(((l + r) / 2.0).clamp(inner.left() + 160.0, inner.right() - 160.0), yb + 14.0), &txt, Color32::WHITE);
    }

    // ---------------------------------------------------------------- hover info
    if let (Some(p), true) = (hover, d.drag_line.is_none() && !d.ruler_dragging && n > 0) {
        if let Some(li) = lane_at(p) {
            let kind = lanes[li];
            let lr = &lane_rects[li];
            painter.line_segment([Pos2::new(p.x, inner.top()), Pos2::new(p.x, inner.bottom())], Stroke::new(1.0_f32, alpha(Color32::WHITE, 0.25)));
            painter.line_segment([Pos2::new(inner.left(), p.y), Pos2::new(inner.right(), p.y)], Stroke::new(1.0_f32, alpha(Color32::WHITE, 0.12)));
            let idx = x_to_idx(p.x).clamp(0.0, (n - 1) as f64);
            let i = idx.round() as usize;
            let span = (spp * 0.5).ceil().max(0.0) as usize;
            let (a, b) = (i.saturating_sub(span), (i + span + 1).min(n));
            let pk = (a..b).fold(0.0f32, |m, j| m.max(val(&src[j], kind, false).abs()));
            let t_ms = (idx - base) / sr as f64 * 1000.0;
            let mut lines = vec![
                (format!("{}  ·  {} ms", kind.name(), fmt_ms(t_ms)), TEXT),
                (format!("LEVEL  {:.1} dB", to_db(pk)), PINK),
                (format!("CURSOR {:.1} dB", to_db(amp_of(lr, p.y))), TEXT_DIM),
            ];
            let beat = src[i].beat;
            if beat.is_finite() {
                let bar = (beat / bpb).floor();
                let in_bar = beat - bar * bpb;
                lines.insert(1, (format!("BAR {}.{}.{:02}", bar as i64 + 1, in_bar.floor() as i64 + 1, ((in_bar.fract()) * 100.0) as i64), TEXT_DIM));
            }
            if comp_ok {
                let pc = (a..b).fold(0.0f32, |m, j| m.max((val(&src[j], kind, false) * gain_at(j)).abs()));
                lines.push((format!("COMP   {:.1} dB  (GR {:.1})", to_db(pc), gr_at(i)), COMP_COLOR));
            }
            if settings.show_sidechain {
                let ps = (a..b).fold(0.0f32, |m, j| m.max(val(&src[j], kind, true).abs()));
                if ps > 1e-7 {
                    lines.push((format!("SC     {:.1} dB", to_db(ps)), CYAN));
                }
            }
            // frequency
            if d.analyzer.as_ref().map(|a| (a.sample_rate() - sr).abs() > 1.0).unwrap_or(true) {
                d.analyzer = Some(FreqAnalyzer::new(sr));
                d.freq_cache = None;
            }
            let key = ((i as i64 - base as i64) / 128, li as u8, frozen);
            let freq = match d.freq_cache {
                Some((k, f)) if k == key && frozen => f,
                _ => {
                    let an = d.analyzer.as_mut().unwrap();
                    let f = an.dominant(i as isize, |j| {
                        if j >= 0 && (j as usize) < n {
                            Some(val(&src[j as usize], kind, false))
                        } else {
                            None
                        }
                    });
                    d.freq_cache = Some((key, f));
                    f
                }
            };
            if let Some(f) = freq {
                lines.push((format!("FREQ   {}  {}", fmt_hz(f), note_name(f)), Color32::WHITE));
            }
            info_box(ui, p, screen_rect, &lines);
        }
    }

    // ---------------------------------------------------------------- control row
    let row_y = screen_rect.bottom() + 18.0;
    let h = 28.0;
    let mut x = screen_rect.left();
    let place = |x: &mut f32, wd: f32| {
        let r = Rect::from_min_size(Pos2::new(*x, row_y), Vec2::new(wd, h));
        *x += wd + 10.0;
        r
    };

    let rec_txt = match &d.rec {
        RecState::Idle => "● REC".to_string(),
        RecState::Armed { .. } => "ARMED…".to_string(),
        RecState::Recording { len } => format!("REC {:.0}%", d.rec_buf.len() as f32 / *len as f32 * 100.0),
    };
    let rec_active = !matches!(d.rec, RecState::Idle);
    if pill(ui, place(&mut x, 96.0), &rec_txt, rec_active).clicked() {
        match d.rec {
            RecState::Idle => {
                let max_len = (MAX_CAPTURE_SECS * sr_live) as usize;
                if settings.rec_bars {
                    let tempo = d.shared.tempo.load(Ordering::Relaxed);
                    let bpb_live = d.shared.beats_per_bar.load(Ordering::Relaxed);
                    if tempo <= 0.0 {
                        d.say(now, "Host gives no tempo — use SEC mode");
                    } else {
                        let bars = BAR_OPTIONS[settings.rec_bars_idx.min(5)].0;
                        let secs = bars * bpb_live as f64 * 60.0 / tempo as f64;
                        let mut len = (secs * sr_live as f64) as usize;
                        if len > max_len {
                            len = max_len;
                            d.say(now, format!("{} = {:.1} s at {:.0} BPM, capped to 30 s", BAR_OPTIONS[settings.rec_bars_idx.min(5)].1, secs, tempo));
                        } else if !d.shared.playing.load(Ordering::Relaxed) {
                            d.say(now, "Armed: recording starts on the next bar after you press Play");
                        }
                        d.rec = RecState::Armed { len, prev_beat: f64::NAN };
                    }
                } else {
                    let secs = SEC_OPTIONS[settings.rec_secs_idx.min(5)].0;
                    let len = ((secs * sr_live) as usize).min(max_len);
                    d.rec_buf.clear();
                    d.rec_buf.reserve(len);
                    d.rec = RecState::Recording { len };
                }
            }
            RecState::Armed { .. } => d.rec = RecState::Idle,
            RecState::Recording { .. } => d.finish_recording(),
        }
    }
    if pill(ui, place(&mut x, 70.0), if settings.rec_bars { "BARS" } else { "SEC" }, false).clicked() {
        settings.rec_bars = !settings.rec_bars;
    }
    if pill(ui, place(&mut x, 28.0), "‹", false).clicked() {
        if settings.rec_bars {
            settings.rec_bars_idx = settings.rec_bars_idx.saturating_sub(1);
        } else {
            settings.rec_secs_idx = settings.rec_secs_idx.saturating_sub(1);
        }
    }
    let len_txt = if settings.rec_bars { BAR_OPTIONS[settings.rec_bars_idx.min(5)].1 } else { SEC_OPTIONS[settings.rec_secs_idx.min(5)].1 };
    let lr_ = place(&mut x, 78.0);
    panel(ui, lr_, h / 2.0);
    label(ui, lr_.center(), Align2::CENTER_CENTER, len_txt, 12.0, PINK);
    if pill(ui, place(&mut x, 28.0), "›", false).clicked() {
        if settings.rec_bars {
            settings.rec_bars_idx = (settings.rec_bars_idx + 1).min(5);
        } else {
            settings.rec_secs_idx = (settings.rec_secs_idx + 1).min(5);
        }
    }
    x += 10.0;
    if pill(ui, place(&mut x, 70.0), "LIVE", !d.frozen).clicked() {
        d.frozen = false;
        d.ruler = None;
    }
    if d.capture.is_some() && pill(ui, place(&mut x, 84.0), "CAPTURE", d.frozen).clicked() {
        d.frozen = true;
        d.ruler = None;
    }
    x += 10.0;
    if pill(ui, place(&mut x, 80.0), "RULER", d.ruler_on).clicked() {
        d.ruler_on = !d.ruler_on;
        d.line_mode = false;
        if !d.ruler_on {
            d.ruler = None;
        }
    }
    if pill(ui, place(&mut x, 80.0), "LINE +", d.line_mode).clicked() {
        d.line_mode = !d.line_mode;
        d.ruler_on = false;
    }
    if pill(ui, place(&mut x, 70.0), "CLEAR", false).clicked() {
        params.level_lines.write().unwrap().clear();
        d.ruler = None;
    }
    let live_txt = if frozen {
        format!("{} / {}", fmt_ms(vlen / sr as f64 * 1000.0), fmt_ms(n as f64 / sr as f64 * 1000.0))
    } else {
        format!("WINDOW {} ms", fmt_ms(settings.live_window_ms as f64))
    };
    label(ui, Pos2::new(screen_rect.right(), row_y + h / 2.0), Align2::RIGHT_CENTER, &live_txt, 11.0, TEXT_DIM);
    let _ = LIVE_WINDOWS;

    let hint = if d.line_mode {
        Some("Click on the waveform to place a level line".to_string())
    } else if d.ruler_on {
        Some("Drag to measure · right-click clears".to_string())
    } else {
        None
    };
    let msg = d.msg.as_ref().filter(|(_, t)| *t > now).map(|(s, _)| s.clone()).or(hint);
    if let Some(m) = msg {
        label(ui, Pos2::new(screen_rect.center().x, screen_rect.bottom() + 58.0), Align2::CENTER_CENTER, &m, 11.0, PINK);
    }

    // ---------------------------------------------------------------- advanced
    let tog_y = full.bottom() - 28.0 - adv_h - 14.0;
    let tog = Rect::from_center_size(Pos2::new(full.center().x, tog_y), Vec2::new(200.0, 24.0));
    let tr = ui.interact(tog, ui.id().with("adv"), Sense::click());
    let arrow = if d.show_adv { "▴" } else { "▾" };
    label(ui, tog.center(), Align2::CENTER_CENTER, &format!("{}  {}", spaced(if d.show_adv { "HIDE ADVANCED" } else { "SHOW ADVANCED" }), arrow), 11.0, if tr.hovered() { Color32::WHITE } else { TEXT });
    if tr.clicked() {
        d.show_adv = !d.show_adv;
    }
    if d.show_adv {
        let pr = Rect::from_min_max(Pos2::new(full.left() + 28.0, full.bottom() - 28.0 - adv_h + 6.0), Pos2::new(full.right() - 28.0, full.bottom() - 28.0));
        panel(ui, pr, 14.0);
        label(ui, Pos2::new(pr.left() + 24.0, pr.top() + 22.0), Align2::LEFT_CENTER, &spaced("COMPRESSOR PREVIEW"), 12.0, PINK);
        label(ui, Pos2::new(pr.left() + 24.0, pr.top() + 40.0), Align2::LEFT_CENTER, "visual only — audio is untouched", 9.5, TEXT_DIM);
        let b1 = Rect::from_min_size(Pos2::new(pr.left() + 24.0, pr.top() + 62.0), Vec2::new(120.0, 28.0));
        if pill(ui, b1, if comp_on { "PREVIEW ON" } else { "PREVIEW OFF" }, comp_on).clicked() {
            setter.begin_set_parameter(&params.comp_on);
            setter.set_parameter(&params.comp_on, !comp_on);
            setter.end_set_parameter(&params.comp_on);
        }
        let b2 = Rect::from_min_size(Pos2::new(pr.left() + 24.0, pr.top() + 102.0), Vec2::new(120.0, 28.0));
        if pill(ui, b2, if key_sc { "KEY: SIDECHAIN" } else { "KEY: INPUT" }, key_sc).clicked() {
            setter.begin_set_parameter(&params.comp_key_sc);
            setter.set_parameter(&params.comp_key_sc, !key_sc);
            setter.end_set_parameter(&params.comp_key_sc);
        }
        let knobs: [(&str, &FloatParam); 6] = [
            ("THRESHOLD", &params.threshold),
            ("RATIO", &params.ratio),
            ("ATTACK", &params.attack),
            ("RELEASE", &params.release),
            ("KNEE", &params.knee),
            ("MAKEUP", &params.makeup),
        ];
        let x0 = pr.left() + 230.0;
        let step = (pr.right() - 60.0 - x0) / 5.0;
        for (k, (name, p)) in knobs.iter().enumerate() {
            knob(ui, Pos2::new(x0 + k as f32 * step, pr.top() + 58.0), 25.0, name, p, setter);
        }
    }

    if settings.channel_mode != settings_before.channel_mode
        || settings.show_sidechain != settings_before.show_sidechain
        || settings.live_window_ms != settings_before.live_window_ms
        || settings.vzoom != settings_before.vzoom
        || settings.rec_bars != settings_before.rec_bars
        || settings.rec_bars_idx != settings_before.rec_bars_idx
        || settings.rec_secs_idx != settings_before.rec_secs_idx
    {
        if settings.channel_mode != settings_before.channel_mode {
            d.ruler = None;
            d.freq_cache = None;
        }
        *params.ui.write().unwrap() = settings;
    }
}

fn dashed(p: &egui::Painter, x0: f32, x1: f32, y: f32, c: Color32) {
    let mut x = x0;
    while x < x1 {
        p.line_segment([Pos2::new(x, y), Pos2::new((x + 6.0).min(x1), y)], Stroke::new(1.2_f32, c));
        x += 11.0;
    }
}

fn trim_num(v: f64) -> String {
    if (v - v.round()).abs() < 1e-6 {
        format!("{}", v.round() as i64)
    } else {
        format!("{:.1}", v)
    }
}

fn fmt_ms(ms: f64) -> String {
    if ms.abs() >= 100.0 {
        format!("{:.0}", ms)
    } else if ms.abs() >= 10.0 {
        format!("{:.1}", ms)
    } else {
        format!("{:.2}", ms)
    }
}

fn fmt_hz(f: f32) -> String {
    if f >= 1000.0 {
        format!("{:.2} kHz", f / 1000.0)
    } else {
        format!("{:.1} Hz", f)
    }
}

fn note_name(f: f32) -> String {
    const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    let midi = 69.0 + 12.0 * (f / 440.0).log2();
    let m = midi.round();
    let cents = ((midi - m) * 100.0).round() as i32;
    let idx = (m as i32).rem_euclid(12) as usize;
    let oct = (m as i32) / 12 - 1;
    format!("{}{} {:+}c", NAMES[idx], oct, cents)
}
