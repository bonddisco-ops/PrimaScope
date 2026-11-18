use nih_plug::prelude::*;
use nih_plug_egui::EguiState;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

mod dsp;
mod editor;
mod style;

const RING_CAPACITY: usize = 1 << 19;

#[derive(Clone, Copy, Default)]
pub struct Frame {
    pub l: f32,
    pub r: f32,
    pub sl: f32,
    pub sr: f32,
    /// Host beat position of this sample, NaN when transport is stopped or unknown.
    pub beat: f64,
}

pub struct Shared {
    pub sample_rate: AtomicF32,
    pub tempo: AtomicF32,
    pub beats_per_bar: AtomicF32,
    pub playing: AtomicBool,
    pub sc_peak: AtomicF32,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChannelMode {
    LR,
    MS,
    Left,
    Right,
    Mid,
    Side,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct UiSettings {
    pub channel_mode: ChannelMode,
    pub show_sidechain: bool,
    pub live_window_ms: f32,
    pub vzoom: f32,
    pub rec_bars: bool,
    pub rec_bars_idx: usize,
    pub rec_secs_idx: usize,
    pub sync: bool,
    pub sync_idx: usize,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            channel_mode: ChannelMode::LR,
            show_sidechain: true,
            live_window_ms: 1000.0,
            vzoom: 1.0,
            rec_bars: true,
            rec_bars_idx: 2,
            rec_secs_idx: 1,
            sync: false,
            sync_idx: 2,
        }
    }
}

#[derive(Params)]
pub struct ScopeParams {
    #[persist = "editor-state"]
    pub editor_state: Arc<EguiState>,

    /// Level lines in dBFS; they stay until the user removes them and are saved with the project.
    #[persist = "level-lines"]
    pub level_lines: Arc<RwLock<Vec<f32>>>,

    #[persist = "ui-settings"]
    pub ui: Arc<RwLock<UiSettings>>,

    #[id = "comp_on"]
    pub comp_on: BoolParam,
    #[id = "comp_key_sc"]
    pub comp_key_sc: BoolParam,
    #[id = "comp_thr"]
    pub threshold: FloatParam,
    #[id = "comp_ratio"]
    pub ratio: FloatParam,
    #[id = "comp_atk"]
    pub attack: FloatParam,
    #[id = "comp_rel"]
    pub release: FloatParam,
    #[id = "comp_knee"]
    pub knee: FloatParam,
    #[id = "comp_makeup"]
    pub makeup: FloatParam,
}

impl Default for ScopeParams {
    fn default() -> Self {
        Self {
            editor_state: EguiState::from_size(editor::WIDTH, editor::HEIGHT),
            level_lines: Arc::new(RwLock::new(Vec::new())),
            ui: Arc::new(RwLock::new(UiSettings::default())),

            comp_on: BoolParam::new("Comp Preview", false),
            comp_key_sc: BoolParam::new("Comp Key Sidechain", false),
            threshold: FloatParam::new(
                "Threshold",
                -18.0,
                FloatRange::Linear { min: -60.0, max: 0.0 },
            )
            .with_unit(" dB")
            .with_step_size(0.1),
            ratio: FloatParam::new(
                "Ratio",
                4.0,
                FloatRange::Skewed { min: 1.0, max: 20.0, factor: FloatRange::skew_factor(-1.5) },
            )
            .with_unit(":1")
            .with_step_size(0.1),
            attack: FloatParam::new(
                "Attack",
                10.0,
                FloatRange::Skewed { min: 0.1, max: 200.0, factor: FloatRange::skew_factor(-2.0) },
            )
            .with_unit(" ms")
            .with_step_size(0.1),
            release: FloatParam::new(
                "Release",
                120.0,
                FloatRange::Skewed { min: 5.0, max: 2000.0, factor: FloatRange::skew_factor(-2.0) },
            )
            .with_unit(" ms")
            .with_step_size(1.0),
            knee: FloatParam::new("Knee", 6.0, FloatRange::Linear { min: 0.0, max: 24.0 })
                .with_unit(" dB")
                .with_step_size(0.1),
            makeup: FloatParam::new("Makeup", 0.0, FloatRange::Linear { min: -12.0, max: 24.0 })
                .with_unit(" dB")
                .with_step_size(0.1),
        }
    }
}

pub struct PrimaScope {
    params: Arc<ScopeParams>,
    shared: Arc<Shared>,
    producer: rtrb::Producer<Frame>,
    editor_data: Arc<Mutex<editor::EditorData>>,
}

impl Default for PrimaScope {
    fn default() -> Self {
        let (producer, consumer) = rtrb::RingBuffer::new(RING_CAPACITY);
        let shared = Arc::new(Shared {
            sample_rate: AtomicF32::new(48000.0),
            tempo: AtomicF32::new(0.0),
            beats_per_bar: AtomicF32::new(4.0),
            playing: AtomicBool::new(false),
            sc_peak: AtomicF32::new(0.0),
        });
        Self {
            params: Arc::new(ScopeParams::default()),
            editor_data: Arc::new(Mutex::new(editor::EditorData::new(consumer, shared.clone()))),
            shared,
            producer,
        }
    }
}

impl Plugin for PrimaScope {
    const NAME: &'static str = "PrimaScope";
    const VENDOR: &'static str = "PrimaScope";
    const URL: &'static str = "https://github.com/bonddisco-ops/PrimaScope";
    const EMAIL: &'static str = "";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[
        AudioIOLayout {
            main_input_channels: NonZeroU32::new(2),
            main_output_channels: NonZeroU32::new(2),
            aux_input_ports: &[new_nonzero_u32(2)],
            names: PortNames {
                layout: Some("Stereo + Sidechain"),
                main_input: Some("Input"),
                main_output: Some("Output"),
                aux_inputs: &["Sidechain"],
                aux_outputs: &[],
            },
            ..AudioIOLayout::const_default()
        },
        AudioIOLayout {
            main_input_channels: NonZeroU32::new(2),
            main_output_channels: NonZeroU32::new(2),
            ..AudioIOLayout::const_default()
        },
        AudioIOLayout {
            main_input_channels: NonZeroU32::new(1),
            main_output_channels: NonZeroU32::new(1),
            aux_input_ports: &[new_nonzero_u32(1)],
            names: PortNames {
                layout: Some("Mono + Sidechain"),
                main_input: Some("Input"),
                main_output: Some("Output"),
                aux_inputs: &["Sidechain"],
                aux_outputs: &[],
            },
            ..AudioIOLayout::const_default()
        },
    ];

    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn editor(&mut self, _async_executor: AsyncExecutor<Self>) -> Option<Box<dyn Editor>> {
        editor::create(self.params.clone(), self.editor_data.clone())
    }

    fn initialize(
        &mut self,
        _audio_io_layout: &AudioIOLayout,
        buffer_config: &BufferConfig,
        _context: &mut impl InitContext<Self>,
    ) -> bool {
        self.shared.sample_rate.store(buffer_config.sample_rate, Ordering::Relaxed);
        true
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        aux: &mut AuxiliaryBuffers,
        context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        let transport = context.transport();
        let sr = transport.sample_rate;
        self.shared.sample_rate.store(sr, Ordering::Relaxed);
        self.shared.tempo.store(transport.tempo.unwrap_or(0.0) as f32, Ordering::Relaxed);
        let bpb = match (transport.time_sig_numerator, transport.time_sig_denominator) {
            (Some(n), Some(d)) if n > 0 && d > 0 => n as f32 * 4.0 / d as f32,
            _ => 4.0,
        };
        self.shared.beats_per_bar.store(bpb, Ordering::Relaxed);
        self.shared.playing.store(transport.playing, Ordering::Relaxed);

        if !self.params.editor_state.is_open() {
            return ProcessStatus::Normal;
        }

        let beat0 = if transport.playing { transport.pos_beats() } else { None };
        let beat_inc = transport.tempo.map(|t| t / 60.0 / sr as f64);

        let n = buffer.samples();
        let main = buffer.as_slice_immutable();
        let sc: &[&mut [f32]] = match aux.inputs.first() {
            Some(b) if b.samples() == n => b.as_slice_immutable(),
            _ => &[],
        };

        let mut sc_peak = 0.0f32;
        for i in 0..n {
            let l = main[0][i];
            let r = if main.len() > 1 { main[1][i] } else { l };
            let (sl, srr) = match sc.len() {
                0 => (0.0, 0.0),
                1 => (sc[0][i], sc[0][i]),
                _ => (sc[0][i], sc[1][i]),
            };
            sc_peak = sc_peak.max(sl.abs()).max(srr.abs());
            let beat = match (beat0, beat_inc) {
                (Some(b), Some(inc)) => b + inc * i as f64,
                _ => f64::NAN,
            };
            if self.producer.push(Frame { l, r, sl, sr: srr, beat }).is_err() {
                break;
            }
        }
        self.shared.sc_peak.store(sc_peak, Ordering::Relaxed);

        ProcessStatus::Normal
    }
}

impl ClapPlugin for PrimaScope {
    const CLAP_ID: &'static str = "com.primascope.primascope";
    const CLAP_DESCRIPTION: Option<&'static str> =
        Some("Oscilloscope with sidechain, capture, compressor preview, ruler and level lines");
    const CLAP_MANUAL_URL: Option<&'static str> = Some(Self::URL);
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] =
        &[ClapFeature::AudioEffect, ClapFeature::Analyzer, ClapFeature::Stereo, ClapFeature::Mono];
}

impl Vst3Plugin for PrimaScope {
    const VST3_CLASS_ID: [u8; 16] = *b"PrimaScopeOscil1";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] =
        &[Vst3SubCategory::Fx, Vst3SubCategory::Analyzer];
}

nih_export_clap!(PrimaScope);
nih_export_vst3!(PrimaScope);
