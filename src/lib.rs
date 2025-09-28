use nih_plug::prelude::*;
use parking_lot::Mutex;
use std::sync::Arc;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::collections::HashMap;

const MAX_CHANNELS: usize = 64;
// capacity must be power of two
const RING_CAP_POW2: usize = 1 << 16;
const DESIRED_DELAY_SAMPLES: usize = 16;

struct Ring {
    buf: Vec<AtomicU32>, // store f32 as bits in AtomicU32
}

impl Ring {
    fn new(capacity_pow2: usize) -> Self {
        let mut v = Vec::with_capacity(capacity_pow2);
        for _ in 0..capacity_pow2 {
            v.push(AtomicU32::new(0));
        }
        Self { buf: v }
    }

    #[inline]
    fn store_at(&self, idx: usize, sample: f32) {
        let bits = sample.to_bits();
        let i = idx & (self.buf.len() - 1);
        self.buf[i].store(bits, Ordering::Release);
    }

    #[inline]
    fn load_at(&self, idx: usize) -> f32 {
        let i = idx & (self.buf.len() - 1);
        let bits = self.buf[i].load(Ordering::Acquire);
        f32::from_bits(bits)
    }
}

struct ChannelRing {
    /// write_pos counts written FRAMES (not samples across all physical channels).
    /// In other words, if we've written N frames (each frame = one sample per physical channel),
    /// write_pos == N.
    write_pos: AtomicUsize,
    rings: Vec<Ring>, // rings.len() == num_physical_channels
}

// Use a more flexible storage that can handle different channel counts per instance
static mut GLOBAL_CHANNEL_RINGS: Option<Arc<Mutex<HashMap<(usize, usize), Arc<ChannelRing>>>>> = None;

fn global_channel_rings() -> Arc<Mutex<HashMap<(usize, usize), Arc<ChannelRing>>>> {
    unsafe {
        if let Some(ref v) = GLOBAL_CHANNEL_RINGS {
            return v.clone();
        }
        let map = HashMap::new();
        let arc = Arc::new(Mutex::new(map));
        GLOBAL_CHANNEL_RINGS = Some(arc.clone());
        arc
    }
}

#[derive(Params)]
struct EasySendParams {
    #[id = "channel"]
    pub channel: IntParam, // 0..63

    #[id = "mode"]
    pub mode: EnumParam<Mode>,

    #[id = "amount"]
    pub amount: FloatParam, // linear scale 0..1 (send level)

    #[id = "output"]
    pub output: EnumParam<OutputMode>,
}

#[derive(Clone, Copy, PartialEq, Eq, Enum)]
enum Mode {
    Send,
    Return,
}

#[derive(Clone, Copy, PartialEq, Eq, Enum)]
enum OutputMode {
    PassThrough,
    Redirect,
}

struct EasySend {
    params: Arc<EasySendParams>,
    // reader state (for Return instances)
    read_pos: usize,
    read_initialized: bool,
    last_channel: usize,
    last_num_channels: usize,
}

impl Default for EasySend {
    fn default() -> Self {
        Self {
            params: Arc::new(EasySendParams {
                channel: IntParam::new("Channel", 0, IntRange::Linear { min: 0, max: 63 }),
                mode: EnumParam::new("Mode", Mode::Send),
                amount: FloatParam::new("Amount", 1.0, FloatRange::Linear { min: 0.0, max: 1.0 }),
                output: EnumParam::new("Output", OutputMode::PassThrough),
            }),
            read_pos: 0,
            read_initialized: false,
            last_channel: 0,
            last_num_channels: 0,
        }
    }
}

impl Plugin for EasySend {
    const NAME: &'static str = "Easy Send";
    const VENDOR: &'static str = "Lath Audio";
    const URL: &'static str = "https://www.instagram.com/lathymeria/";
    const EMAIL: &'static str = "broadenyourmindz@gmail.con";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[
        AudioIOLayout {
            main_input_channels: NonZeroU32::new(2),
            main_output_channels: NonZeroU32::new(2),
            aux_input_ports: &[],
            aux_output_ports: &[],
            names: PortNames::const_default(),
        },
    ];

    const MIDI_INPUT: MidiConfig = MidiConfig::None;
    const SAMPLE_ACCURATE_AUTOMATION: bool = true;

    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn initialize(
        &mut self,
        _audio_io_layout: &AudioIOLayout,
        _buffer_config: &BufferConfig,
        _context: &mut impl InitContext<Self>,
    ) -> bool {
        self.read_initialized = false;
        self.last_channel = 0;
        self.last_num_channels = 0;
        true
    }

    fn reset(&mut self) {
        self.read_pos = 0;
        self.read_initialized = false;
        self.last_channel = 0;
        self.last_num_channels = 0;
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        _context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        let channel_idx = (self.params.channel.value() as usize).min(MAX_CHANNELS - 1);
        let amount = self.params.amount.value();
        let mode = self.params.mode.value();
        let output_mode = self.params.output.value();

        let rings_store = global_channel_rings();

        // metadata
        let num_physical = buffer.channels();
        if num_physical == 0 {
            return ProcessStatus::Normal;
        }
        let frame_count = buffer.samples(); // frames per process call

        // Check if channel or channel count changed
        let channel_changed = channel_idx != self.last_channel || num_physical != self.last_num_channels;
        if channel_changed {
            self.read_initialized = false;
            self.last_channel = channel_idx;
            self.last_num_channels = num_physical;
        }

        // Get or create the ChannelRing for this (channel_idx, num_physical) combination
        let ch_ring = {
            let mut store = rings_store.lock();
            let key = (channel_idx, num_physical);
            
            if let Some(existing) = store.get(&key) {
                existing.clone()
            } else {
                // create one ChannelRing with num_physical rings
                let mut vec_rings = Vec::with_capacity(num_physical);
                for _ in 0..num_physical {
                    vec_rings.push(Ring::new(RING_CAP_POW2));
                }
                let cr = Arc::new(ChannelRing {
                    write_pos: AtomicUsize::new(0),
                    rings: vec_rings,
                });
                store.insert(key, cr.clone());
                cr
            }
        };

        // now it's safe to get &mut slices
        let slices = buffer.as_slice(); // &mut [ChannelSamples]

        match mode {
            Mode::Send => {
                // IMPORTANT: do fetch_add ONCE, with step = frame_count (frames).
                // base_frame is the position (in frames) where this block should start.
                let base_frame = ch_ring.write_pos.fetch_add(frame_count, Ordering::AcqRel);

                // For each physical track write samples into its ring at indices base_frame + i
                for (phys_idx, slice) in slices.iter().enumerate() {
                    let ring = &ch_ring.rings[phys_idx];
                    for (i, &s) in slice.iter().enumerate() {
                        let idx = base_frame + i;
                        // Amount is applied here — this is the send level
                        ring.store_at(idx, s * amount);
                    }
                }

                // Redirect — zero outputs, PassThrough — leave as is
                if output_mode == OutputMode::Redirect {
                    for slice in slices.iter_mut() {
                        for s in slice.iter_mut() {
                            *s = 0.0;
                        }
                    }
                }
            }

            Mode::Return => {
                // read current write_pos (in frames)
                let write_pos = ch_ring.write_pos.load(Ordering::Acquire);

                if !self.read_initialized {
                    // Reset read position to be DESIRED_DELAY_SAMPLES behind current write position
                    // but ensure we don't go negative
                    if write_pos >= DESIRED_DELAY_SAMPLES {
                        self.read_pos = write_pos - DESIRED_DELAY_SAMPLES;
                        self.read_initialized = true;
                    } else {
                        // not enough data yet — output silence
                        for slice in slices.iter_mut() {
                            for s in slice.iter_mut() { 
                                *s = 0.0; 
                            }
                        }
                        return ProcessStatus::Normal;
                    }
                }

                // check if enough data is available
                let available = write_pos.saturating_sub(self.read_pos);
                if available < frame_count {
                    // no full block yet — silence
                    for slice in slices.iter_mut() {
                        for s in slice.iter_mut() { 
                            *s = 0.0; 
                        }
                    }
                    return ProcessStatus::Normal;
                }

                // data available — read per-channel
                for (phys_idx, slice) in slices.iter_mut().enumerate() {
                    let ring = &ch_ring.rings[phys_idx];
                    for i in 0..frame_count {
                        let val = ring.load_at(self.read_pos + i);
                        slice[i] = val; // amount was applied on send
                    }
                }

                // advance read_pos
                self.read_pos = self.read_pos.wrapping_add(frame_count);
            }
        }

        ProcessStatus::Normal
    }

    fn deactivate(&mut self) {}
}

impl ClapPlugin for EasySend {
    const CLAP_ID: &'static str = "com.lath-audio.easy-send";
    const CLAP_DESCRIPTION: Option<&'static str> = Some("Send audio from one instance to another with low delay");
    const CLAP_MANUAL_URL: Option<&'static str> = Some(Self::URL);
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[
        ClapFeature::AudioEffect,
        ClapFeature::Stereo,
        ClapFeature::Mono,
        ClapFeature::Utility,
    ];
}

impl Vst3Plugin for EasySend {
    const VST3_CLASS_ID: [u8; 16] = *b"LathEasySendPlug";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] =
        &[Vst3SubCategory::Fx, Vst3SubCategory::Tools];
}

nih_export_clap!(EasySend);
nih_export_vst3!(EasySend);