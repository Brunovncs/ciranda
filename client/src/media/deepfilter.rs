//! DeepFilterNet3 (Schröter et al., 2023), the neural noise suppression: a small network that
//! learned what a voice sounds like and takes out the rest, keyboards, dogs and dishes included,
//! where the processing module's suppressor only knows noise that holds still. Full band at
//! 48 kHz, 10 ms in and 10 ms out, 30 ms behind: half its 20 ms window, and two frames of
//! lookahead.
//!
//! Ported from libDF's `lib.rs` and `tract.rs` (DeepFilterNet 0.5.6, MIT or Apache 2.0, © 2021
//! Hendrik Schröter) onto the tract this client already runs for the camera: mono only, the
//! model's settings written in rather than read from its `config.ini`, and no allocation per
//! frame outside the nets. The three nets are the published DeepFilterNet3 export, run pulsed:
//! one frame per call, each keeping its own state between calls.

use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::collections::VecDeque;
use std::io::Cursor;
use std::sync::Arc;
use tract_onnx::prelude::*;
use tract_onnx::tract_hir::shapefactoid;
use tract_pulse::internal::{PulsedModel, PulsedModelExt};

static ENC: &[u8] = include_bytes!("../../assets/models/deepfilternet3/enc.onnx");
static ERB_DEC: &[u8] = include_bytes!("../../assets/models/deepfilternet3/erb_dec.onnx");
static DF_DEC: &[u8] = include_bytes!("../../assets/models/deepfilternet3/df_dec.onnx");

// The export's `config.ini`.
const SR: usize = 48_000;
const FFT: usize = 960;
/// One call's worth: the same 10 ms the processing module works in.
pub const HOP: usize = 480;
const FREQS: usize = FFT / 2 + 1;
const NB_ERB: usize = 32;
const MIN_ERB_FREQS: usize = 2;
/// The low bins the deep filter works on, up to about 4.8 kHz; above, the band gains alone.
const NB_DF: usize = 96;
const DF_ORDER: usize = 5;
/// Frames the nets look ahead (`conv_lookahead` and `df_lookahead` are both 2).
const LOOKAHEAD: usize = 2;
const CONV_CH: usize = 64;
/// The feature normalisation's smoothing, exp(-hop / sr / 1 s) to three places as libDF rounds it.
const ALPHA: f32 = 0.99;
/// The analysis scale, 1 / (fft² / 2·hop).
const WNORM: f32 = (2 * HOP) as f32 / (FFT * FFT) as f32;

pub struct DeepFilter {
    enc: TypedSimpleState,
    erb_dec: TypedSimpleState,
    df_dec: TypedSimpleState,
    forward: Arc<dyn RealToComplex<f32>>,
    inverse: Arc<dyn ComplexToReal<f32>>,
    /// Vorbis window, for analysis and synthesis both.
    window: Vec<f32>,
    /// How many FFT bins each ERB band spans.
    erb: Vec<usize>,
    analysis_mem: Vec<f32>,
    synthesis_mem: Vec<f32>,
    time: Vec<f32>,
    forward_scratch: Vec<Complex32>,
    inverse_scratch: Vec<Complex32>,
    spec: Vec<Complex32>,
    out: Vec<Complex32>,
    feat_erb: Vec<f32>,
    feat_spec: Vec<f32>,
    mean_norm: Vec<f32>,
    unit_norm: Vec<f32>,
    /// The last frames' spectra as they came, newest last: the deep filter's input, and what the
    /// attenuation limit mixes back in.
    noisy: VecDeque<Vec<Complex32>>,
    /// The same, with the band gains applied once their frame's turn comes.
    gained: VecDeque<Vec<Complex32>>,
    /// The share of the noisy signal kept in the output, so noise is lowered, never cut to
    /// nothing; 0 is no limit.
    floor: f32,
}

impl DeepFilter {
    /// Builds the three nets for streaming: some 60 ms of a core in a release build, several
    /// times that in a debug one, so not on the audio thread.
    pub fn load() -> anyhow::Result<DeepFilter> {
        let hidden = CONV_CH * NB_ERB / 4;
        let enc = net(ENC, &["feat_erb", "feat_spec"], &["e0", "e1", "e2", "e3", "emb", "c0", "lsnr"], |s| {
            vec![shapefactoid!(1, 1, s, NB_ERB), shapefactoid!(1, 2, s, NB_DF)]
        })?;
        let erb_dec = net(ERB_DEC, &["emb", "e3", "e2", "e1", "e0"], &["m"], |s| {
            vec![
                shapefactoid!(1, s, hidden),
                shapefactoid!(1, CONV_CH, s, (NB_ERB / 4)),
                shapefactoid!(1, CONV_CH, s, (NB_ERB / 4)),
                shapefactoid!(1, CONV_CH, s, (NB_ERB / 2)),
                shapefactoid!(1, CONV_CH, s, NB_ERB),
            ]
        })?;
        let df_dec = net(DF_DEC, &["emb", "c0"], &["coefs"], |s| vec![shapefactoid!(1, s, hidden), shapefactoid!(1, CONV_CH, s, NB_DF)])?;

        let mut planner = RealFftPlanner::<f32>::new();
        let forward = planner.plan_fft_forward(FFT);
        let inverse = planner.plan_fft_inverse(FFT);
        let half = FFT / 2;
        let window = (0..FFT)
            .map(|i| {
                let sin = (0.5 * std::f64::consts::PI * (i as f64 + 0.5) / half as f64).sin();
                (0.5 * std::f64::consts::PI * sin * sin).sin() as f32
            })
            .collect();
        let linspace = |from: f32, to: f32, n: usize| (0..n).map(|i| from + i as f32 * (to - from) / (n - 1) as f32).collect();
        let zeros = || vec![Complex32::default(); FREQS];
        Ok(DeepFilter {
            enc,
            erb_dec,
            df_dec,
            forward_scratch: forward.make_scratch_vec(),
            inverse_scratch: inverse.make_scratch_vec(),
            forward,
            inverse,
            window,
            erb: erb_bands(),
            analysis_mem: vec![0.; FFT - HOP],
            synthesis_mem: vec![0.; FFT - HOP],
            time: vec![0.; FFT],
            spec: zeros(),
            out: zeros(),
            feat_erb: vec![0.; NB_ERB],
            feat_spec: vec![0.; 2 * NB_DF],
            mean_norm: linspace(-60., -90., NB_ERB),
            unit_norm: linspace(0.001, 0.0001, NB_DF),
            noisy: (0..DF_ORDER.max(LOOKAHEAD)).map(|_| zeros()).collect(),
            gained: (0..DF_ORDER + LOOKAHEAD).map(|_| zeros()).collect(),
            floor: 0.,
        })
    }

    /// At most this much is taken off the noise, in dB; 100 or more is no limit.
    pub fn set_attenuation_limit(&mut self, db: f32) {
        self.floor = if db >= 100. { 0. } else { 10f32.powf(-db.max(0.) / 20.) };
    }

    /// Forgets the sound so far, for a microphone that stopped and opens again: the next frames
    /// shouldn't carry the end of the last capture. The nets' own state and the feature
    /// normalisation stay, as they describe the room more than the moment.
    pub fn reset(&mut self) {
        self.analysis_mem.fill(0.);
        self.synthesis_mem.fill(0.);
        for s in self.noisy.iter_mut().chain(self.gained.iter_mut()) {
            s.fill(Complex32::default());
        }
    }

    /// Cleans 10 ms of audio, -1..1, into `out`, 30 ms behind. Returns the nets' estimate of the
    /// frame's SNR in dB, -15 to 35.
    pub fn process(&mut self, input: &[f32], out: &mut [f32]) -> anyhow::Result<f32> {
        debug_assert_eq!(input.len(), HOP);
        debug_assert_eq!(out.len(), HOP);
        self.analysis(input);
        for queue in [&mut self.noisy, &mut self.gained] {
            let mut frame = queue.pop_front().unwrap_or_default();
            frame.clone_from(&self.spec);
            queue.push_back(frame);
        }
        self.features();

        let feat_erb = Tensor::from_shape(&[1, 1, 1, NB_ERB], &self.feat_erb)?;
        let feat_spec = Tensor::from_shape(&[1, 2, 1, NB_DF], &self.feat_spec)?;
        let enc = self.enc.run(tvec!(feat_erb.into(), feat_spec.into()))?;
        let lsnr = floats(&enc[6])?[0];

        // Both decoders run on every frame, as libDF's command line has them. Its library and
        // real-time plugin skip them on frames the encoder calls clean speech or pure noise, which
        // saves a third of the time but lets keyboard clicks through: on speech with typing,
        // 7 dB SI-SDR against 18.5.

        // The band gains go on the frame two behind, the one the lookahead has now seen past.
        let target = &mut self.gained[DF_ORDER - 1];
        let m = self.erb_dec.run(tvec!(enc[4].clone(), enc[3].clone(), enc[2].clone(), enc[1].clone(), enc[0].clone()))?;
        let mut start = 0;
        for (&size, &g) in self.erb.iter().zip(floats(&m[0])?) {
            for c in &mut target[start..start + size] {
                *c *= g;
            }
            start += size;
        }
        self.out.clone_from(target);

        // The deep filter rebuilds the low bins from the last five noisy frames.
        let coefs = self.df_dec.run(tvec!(enc[4].clone(), enc[5].clone()))?;
        let coefs = floats(&coefs[0])?;
        for (f, o) in self.out[..NB_DF].iter_mut().enumerate() {
            *o = Complex32::default();
            for (t, frame) in self.noisy.iter().enumerate() {
                let c = &coefs[(f * DF_ORDER + t) * 2..];
                *o += frame[f] * Complex32::new(c[0], c[1]);
            }
        }

        if self.floor > 0. {
            let noisy = &self.noisy[DF_ORDER.max(LOOKAHEAD) - LOOKAHEAD - 1];
            for (o, &n) in self.out.iter_mut().zip(noisy) {
                *o = *o * (1. - self.floor) + n * self.floor;
            }
        }
        self.synthesis(out);
        Ok(lsnr)
    }

    fn analysis(&mut self, input: &[f32]) {
        let (first, second) = self.window.split_at(FFT - HOP);
        for ((t, &m), &w) in self.time.iter_mut().zip(&self.analysis_mem).zip(first) {
            *t = m * w;
        }
        for ((t, &x), &w) in self.time[FFT - HOP..].iter_mut().zip(input).zip(second) {
            *t = x * w;
        }
        self.analysis_mem.copy_from_slice(input);
        let _ = self.forward.process_with_scratch(&mut self.time, &mut self.spec, &mut self.forward_scratch);
        for s in &mut self.spec {
            *s *= WNORM;
        }
    }

    fn synthesis(&mut self, out: &mut [f32]) {
        // A real signal's spectrum has none at DC and Nyquist; what the filters left there would
        // only make the FFT complain.
        self.out[0].im = 0.;
        self.out[FREQS - 1].im = 0.;
        let _ = self.inverse.process_with_scratch(&mut self.out, &mut self.time, &mut self.inverse_scratch);
        for (t, &w) in self.time.iter_mut().zip(&self.window) {
            *t *= w;
        }
        for ((o, &t), &m) in out.iter_mut().zip(&self.time[..HOP]).zip(&self.synthesis_mem) {
            *o = t + m;
        }
        self.synthesis_mem.copy_from_slice(&self.time[HOP..]);
    }

    /// The encoder's two inputs: each ERB band's level in dB, and the low bins as they are, both
    /// normalised by a running average so the nets see the sound relative to the room.
    fn features(&mut self) {
        let mut start = 0;
        for ((feat, &size), mean) in self.feat_erb.iter_mut().zip(&self.erb).zip(&mut self.mean_norm) {
            let power = self.spec[start..start + size].iter().map(|c| c.norm_sqr()).sum::<f32>() / size as f32;
            start += size;
            let db = 10. * (power + 1e-10).log10();
            *mean = db * (1. - ALPHA) + *mean * ALPHA;
            *feat = (db - *mean) / 40.;
        }
        let (re, im) = self.feat_spec.split_at_mut(NB_DF);
        for (i, (c, unit)) in self.spec[..NB_DF].iter().zip(&mut self.unit_norm).enumerate() {
            *unit = c.norm() * (1. - ALPHA) + *unit * ALPHA;
            let v = c / unit.sqrt();
            re[i] = v.re;
            im[i] = v.im;
        }
    }
}

/// One of the three nets, made to take a frame at a time.
fn net(
    onnx: &[u8],
    inputs: &[&str],
    outputs: &[&str],
    shapes: impl FnOnce(Symbol) -> Vec<tract_onnx::tract_hir::infer::ShapeFactoid>,
) -> TractResult<TypedSimpleState> {
    let mut model = tract_onnx::onnx().with_ignore_output_shapes(true).model_for_read(&mut Cursor::new(onnx))?;
    let s = model.sym("S");
    for (i, shape) in shapes(s.clone()).into_iter().enumerate() {
        model.set_input_fact(i, InferenceFact::dt_shape(f32::datum_type(), shape))?;
    }
    let mut model = model.with_input_names(inputs)?.with_outputs_by_name(outputs)?;
    model.analyse(true)?;
    let mut model = model.into_typed()?;
    model.declutter()?;
    let plan = PulsedModel::new(&model, s, &TDim::from(1))?.into_typed()?.into_optimized()?.into_runnable()?;
    TypedSimpleState::new(&plan)
}

fn floats(t: &TValue) -> TractResult<&[f32]> {
    t.try_as_plain_ram()?.as_slice::<f32>()
}

fn freq2erb(hz: f32) -> f32 {
    9.265 * (hz / (24.7 * 9.265)).ln_1p()
}

fn erb2freq(erb: f32) -> f32 {
    24.7 * 9.265 * ((erb / 9.265).exp() - 1.)
}

/// How many FFT bins each ERB band spans: even steps on the ERB scale, at least two bins each.
fn erb_bands() -> Vec<usize> {
    let width = SR as f32 / FFT as f32;
    let (low, high) = (freq2erb(0.), freq2erb((SR / 2) as f32));
    let step = (high - low) / NB_ERB as f32;
    let mut bands = vec![0; NB_ERB];
    let (mut prev, mut over) = (0i32, 0i32);
    for (i, band) in bands.iter_mut().enumerate() {
        let bin = (erb2freq(low + (i + 1) as f32 * step) / width).round() as i32;
        let mut n = bin - prev - over;
        if n < MIN_ERB_FREQS as i32 {
            over = MIN_ERB_FREQS as i32 - n;
            n = MIN_ERB_FREQS as i32;
        } else {
            over = 0;
        }
        *band = n as usize;
        prev = bin;
    }
    bands[NB_ERB - 1] += 1;
    let total: usize = bands.iter().sum();
    bands[NB_ERB - 1] -= total.saturating_sub(FREQS);
    bands
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_erb_bands_cover_the_spectrum() {
        let bands = erb_bands();
        assert_eq!(bands.len(), NB_ERB);
        assert_eq!(bands.iter().sum::<usize>(), FREQS);
        assert!(bands.iter().all(|&n| n >= MIN_ERB_FREQS));
    }

    /// White noise, a pseudo-random sequence so the test is the same every run.
    fn noise(n: usize, amp: f32) -> Vec<f32> {
        let mut x = 0x2545_f491u32;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x as f32 / u32::MAX as f32 * 2. - 1.) * amp
            })
            .collect()
    }

    fn run(df: &mut DeepFilter, input: &[f32]) -> Vec<f32> {
        let mut out = vec![0.; input.len()];
        for (i, o) in input.chunks_exact(HOP).zip(out.chunks_exact_mut(HOP)) {
            df.process(i, o).unwrap();
        }
        out
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn noise_alone_is_taken_down() {
        let mut df = DeepFilter::load().unwrap();
        let input = noise(SR * 3, 0.1);
        let out = run(&mut df, &input);
        assert!(out.iter().all(|v| v.is_finite()));
        // After the first second, the normalisation has settled.
        let (before, after) = (rms(&input[SR..]), rms(&out[SR..]));
        let db = 20. * (after / before).log10();
        assert!(db < -20., "white noise only went down {db:.1} dB");
    }

    #[test]
    fn the_limit_keeps_some_noise() {
        let mut df = DeepFilter::load().unwrap();
        df.set_attenuation_limit(12.);
        let input = noise(SR * 3, 0.1);
        let out = run(&mut df, &input);
        let db = 20. * (rms(&out[SR..]) / rms(&input[SR..])).log10();
        assert!((-15.0..-9.0).contains(&db), "a 12 dB limit took noise down {db:.1} dB");
    }

    #[test]
    fn silence_stays_silent() {
        let mut df = DeepFilter::load().unwrap();
        let out = run(&mut df, &vec![0.; HOP * 20]);
        assert!(out.iter().all(|&v| v == 0.));
    }
}
