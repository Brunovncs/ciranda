//! The sound of a call, done here rather than by WebRTC's device module, so each person can have
//! their own volume (to 350%), deafening silences voices but not the soundpad, and the echo
//! canceller hears exactly what the speakers play.
//!
//! The speakers play a mix of sources (each voice, a screen's audio, cues, soundpad clips). The
//! microphone goes through the audio processing module (echo cancellation and noise suppression,
//! fed the mix as its reference), then the input volume and the noise gate, then to whatever
//! WebRTC source is listening. Everything inside runs at 48 kHz.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SizedSample};
use libwebrtc::native::apm::AudioProcessingModule;
use parking_lot::{Condvar, Mutex};
use rtrb::{Consumer, Producer, RingBuffer};
use rubato::Resampler as _;
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, OnceLock, mpsc};
use std::time::{Duration, Instant};

pub const RATE: u32 = 48_000;
/// 10 ms of mono audio: the unit WebRTC and the processing module work in.
pub const FRAME: usize = (RATE / 100) as usize;
/// The most a voice may queue before its oldest audio is dropped, so delay can't build up.
const MAX_VOICE_MS: usize = 160;
/// Volumes go to 350%, as in the old client.
pub const MAX_GAIN: f32 = 3.5;

/// An f32 kept in an atomic, for values the audio thread reads every callback.
#[derive(Default)]
pub struct AtomicF32(AtomicU32);

impl AtomicF32 {
    pub fn new(v: f32) -> AtomicF32 {
        AtomicF32(AtomicU32::new(v.to_bits()))
    }
    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
    pub fn set(&self, v: f32) {
        self.0.store(v.to_bits(), Ordering::Relaxed)
    }
    /// Raises the value to `v` if it is higher; returns nothing.
    pub fn max(&self, v: f32) {
        let mut cur = self.0.load(Ordering::Relaxed);
        while f32::from_bits(cur) < v {
            match self.0.compare_exchange_weak(cur, v.to_bits(), Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => break,
                Err(x) => cur = x,
            }
        }
    }
    pub fn take(&self) -> f32 {
        f32::from_bits(self.0.swap(0, Ordering::Relaxed))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SourceKind {
    /// Someone's microphone: silenced by deafening.
    Voice,
    /// A screen share's sound.
    Stream,
    /// Cues and soundpad clips.
    Effect,
}

/// Something the speakers play: interleaved stereo at 48 kHz, pushed by whoever produces it.
pub struct Source {
    pub kind: SourceKind,
    feed: Feed,
    pub gain: AtomicF32,
    /// The loudest 10 ms (RMS) since the meter last looked, for speaking rings.
    pub peak: AtomicF32,
    /// Removed from the mix once drained.
    pub one_shot: bool,
}

/// How a source's samples reach the mix.
enum Feed {
    /// Someone's voice or screen, from the network: a lock-free queue to the output callback,
    /// played out at a steady fill. Only pushers take `input`'s lock (one task, two while a
    /// reconnect overlaps) and only the output callback takes `output`'s, so it never waits.
    /// `overflowed`: a push found the queue full.
    Stream { input: Mutex<Producer<f32>>, output: Mutex<Playout>, overflowed: AtomicBool },
    /// Played as it comes.
    Queue { buf: Mutex<VecDeque<f32>> },
    /// The cues' bus: each cue (mono, rendered once and shared) plays from when it arrives, on top
    /// of any still playing, and the sum goes through one limiter. A cue is handed over whole,
    /// as a pointer: `incoming` is only held for a push, and the output callback only tries it,
    /// so neither side waits while the other walks samples. `playing` is the callback's own.
    Bus { incoming: Mutex<Vec<Arc<[f32]>>>, playing: Mutex<Vec<(Arc<[f32]>, usize)>>, limiter: Mutex<Limiter> },
    /// A soundpad clip, played straight from its shared samples (and how far it got) instead of
    /// being copied.
    Clip(Arc<[i16]>, AtomicUsize),
}

/// `CIRANDA_AUDIO_DEBUG` is set; looked up once, since it is asked every 10 ms for each stream.
fn audio_debug() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("CIRANDA_AUDIO_DEBUG").is_some())
}

impl Source {
    pub fn new(kind: SourceKind, gain: f32, one_shot: bool) -> Arc<Source> {
        let feed = match kind {
            SourceKind::Voice | SourceKind::Stream if !one_shot => {
                let (input, output) = Playout::new(kind);
                Feed::Stream { input: Mutex::new(input), output: Mutex::new(output), overflowed: AtomicBool::new(false) }
            }
            _ => Feed::Queue { buf: Mutex::new(VecDeque::new()) },
        };
        Arc::new(Source { kind, feed, gain: AtomicF32::new(gain), peak: AtomicF32::default(), one_shot })
    }

    /// The cues' bus, which stays in the mix.
    fn bus() -> Arc<Source> {
        Arc::new(Source {
            kind: SourceKind::Effect,
            feed: Feed::Bus {
                incoming: Mutex::new(Vec::with_capacity(BUS_VOICES)),
                playing: Mutex::new(Vec::with_capacity(BUS_VOICES)),
                limiter: Mutex::default(),
            },
            gain: AtomicF32::new(1.),
            peak: AtomicF32::default(),
            one_shot: false,
        })
    }

    /// A cue (mono) to play on the bus from now, over whatever is still playing.
    fn play_cue(&self, samples: Arc<[f32]>) {
        if let Feed::Bus { incoming, .. } = &self.feed {
            incoming.lock().push(samples);
        }
    }

    /// Mono or stereo samples in -1..1 at 48 kHz.
    #[cfg(test)]
    pub fn push(&self, samples: &[f32], channels: usize) {
        self.push_with(samples, channels, |s| s);
    }

    pub fn push_i16(&self, samples: &[i16], channels: usize) {
        if audio_debug() {
            static N: AtomicU64 = AtomicU64::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            if n.is_multiple_of(200) {
                let sum = samples.iter().map(|&s| (s as f32 / 32768.).powi(2)).sum::<f32>();
                let rms = (sum / samples.len().max(1) as f32).sqrt();
                log::info!("remote audio: frame {n}, {} samples x{channels}, rms {rms:.4}", samples.len());
            }
        }
        self.push_with(samples, channels, |s| s as f32 / 32768.);
    }

    fn push_with<T: Copy>(&self, samples: &[T], channels: usize, to_f32: impl Fn(T) -> f32) {
        let channels = channels.max(1);
        let mut sum = 0.;
        let stereo = samples.chunks_exact(channels).map(|f| {
            let (l, r) = if channels == 1 { (to_f32(f[0]), to_f32(f[0])) } else { (to_f32(f[0]), to_f32(f[1])) };
            sum += 0.25 * (l + r) * (l + r);
            [l, r]
        });
        match &self.feed {
            Feed::Stream { input, overflowed, .. } => {
                let mut input = input.lock();
                let frames = (input.slots() / 2).min(samples.len() / channels);
                if frames < samples.len() / channels {
                    // Only while the speakers are away: when back, they drop what queued meanwhile.
                    overflowed.store(true, Ordering::Relaxed);
                }
                if let Ok(chunk) = input.write_chunk_uninit(frames * 2) {
                    chunk.fill_from_iter(stereo.flatten());
                }
            }
            Feed::Queue { buf } => buf.lock().extend(stereo.flatten()),
            Feed::Clip(..) | Feed::Bus { .. } => return,
        }
        let frames = (samples.len() / channels).max(1);
        self.peak.max((sum / frames as f32).sqrt());
    }

    /// Lets go of cues waiting to play, for speakers that aren't there to play them.
    fn drop_cues(&self) {
        if let Feed::Bus { incoming, playing, .. } = &self.feed {
            incoming.lock().clear();
            playing.lock().clear();
        }
    }

    fn is_empty(&self) -> bool {
        match &self.feed {
            Feed::Clip(samples, at) => at.load(Ordering::Relaxed) >= samples.len(),
            Feed::Queue { buf } => buf.lock().is_empty(),
            Feed::Stream { output, .. } => output.lock().queue.is_empty(),
            Feed::Bus { incoming, playing, .. } => incoming.lock().is_empty() && playing.lock().is_empty(),
        }
    }

    /// Adds this source's next `out.len() / 2` frames into `out`, at `gain`. On the output
    /// callback; whatever it is done with goes to `retire` rather than being freed there. False
    /// only for the cues' bus with nothing to play: every other source keeps the speakers open
    /// for as long as it is in the mix.
    fn mix_into(&self, out: &mut [f32], gain: f32, retire: &mut Producer<Retired>) -> bool {
        match &self.feed {
            Feed::Clip(samples, at) => {
                let from = at.load(Ordering::Relaxed).min(samples.len());
                let n = out.len().min(samples.len() - from);
                for (o, &v) in out.iter_mut().zip(&samples[from..from + n]) {
                    *o += v as f32 / 32768. * gain;
                }
                at.store(from + n, Ordering::Relaxed);
            }
            Feed::Stream { output, overflowed, .. } => output.lock().pull(out, gain, overflowed.swap(false, Ordering::Relaxed)),
            Feed::Queue { buf } => {
                let mut buf = buf.lock();
                let n = out.len().min(buf.len());
                for (o, v) in out.iter_mut().zip(buf.drain(..n)) {
                    *o += v * gain;
                }
            }
            Feed::Bus { incoming, playing, limiter } => {
                let mut playing = playing.lock();
                if let Some(mut new) = incoming.try_lock() {
                    for cue in new.drain(..) {
                        // Within the capacity set aside, so pushing never allocates here; a cue
                        // past that many at once would not be heard anyway.
                        if playing.len() < playing.capacity() {
                            playing.push((cue, 0));
                        } else {
                            retire_on(retire, Retired::Cue(cue));
                        }
                    }
                }
                if playing.is_empty() {
                    return false;
                }
                let mut limiter = limiter.lock();
                for (k, o) in out.chunks_exact_mut(2).enumerate() {
                    let v = playing.iter().map(|(s, at)| s.get(at + k).copied().unwrap_or(0.)).sum::<f32>() * gain;
                    let (l, r) = limiter.process(v, v);
                    o[0] += l;
                    o[1] += r;
                }
                let frames = out.len() / 2;
                let mut i = 0;
                while i < playing.len() {
                    playing[i].1 += frames;
                    if playing[i].1 >= playing[i].0.len() {
                        let (cue, _) = playing.swap_remove(i);
                        retire_on(retire, Retired::Cue(cue));
                    } else {
                        i += 1;
                    }
                }
            }
        }
        true
    }
}

/// Cues the bus can play at once.
const BUS_VOICES: usize = 16;

/// What the output callback is done with: freed by whoever collects it, not by the callback.
/// Only ever held, to be dropped elsewhere.
#[allow(dead_code)]
enum Retired {
    Source(Arc<Source>),
    Cue(Arc<[f32]>),
}

fn retire_on(retire: &mut Producer<Retired>, r: Retired) {
    // Full only if nothing has collected for a long while; then the callback frees it itself,
    // which is what used to happen every time.
    if let Err(rtrb::PushError::Full(r)) = retire.push(r) {
        drop(r);
    }
}

/// How a stream is played out, in milliseconds: the fill it is held at, how far it may stray
/// before it is brought back, and the backlog that is dropped outright.
struct Pace {
    target: usize,
    slack: usize,
    cap: usize,
}

impl Pace {
    fn of(kind: SourceKind) -> Pace {
        match kind {
            // Enough for the 10 ms bursts and the scheduling jitter on top, and no more.
            SourceKind::Voice => Pace { target: 30, slack: 8, cap: MAX_VOICE_MS },
            // A screen's sound goes with its video, which arrives later anyway, and is often
            // music, where a dropped or repeated frame is easier to hear: more room, fewer fixes.
            _ => Pace { target: 80, slack: 20, cap: 300 },
        }
    }
}

/// Output frames over which the fill's low point is taken; two windows in a row out of bounds
/// set a correction.
const WINDOW: usize = RATE as usize / 2;
/// 2 ms: starts and stops are faded over this, so they don't click.
const FADE: usize = RATE as usize / 500;
/// Below this (about -50 dBFS) a block is silence, and frames are dropped or repeated in bulk.
const QUIET: f32 = 0.003;

const fn frames(ms: usize) -> usize {
    ms * RATE as usize / 1000
}

/// The output callback's end of a stream. LiveKit hands over remote audio on a 10 ms software
/// timer while the speakers run on their own clock, so a plain queue drifts between running dry
/// and overflowing. This one starts once `target` is queued (again after running dry, instead
/// of crackling sample by sample) and holds the fill near it by dropping or repeating single
/// frames where the sound is quietest.
struct Playout {
    queue: Consumer<f32>,
    /// In frames, a stereo pair each.
    target: usize,
    slack: usize,
    cap: usize,
    playing: bool,
    /// Frames played of the fade-in since playing started.
    faded_in: usize,
    /// The least queued at a pull this window, how much of the window has played, and the last
    /// window's least.
    low: usize,
    seen: usize,
    last_low: Option<usize>,
    /// Frames still to drop (above zero) or repeat (below zero) to get back to target.
    skew: isize,
    /// The block being pulled, copied out of the queue.
    block: Vec<[f32; 2]>,
}

impl Playout {
    fn new(kind: SourceKind) -> (Producer<f32>, Playout) {
        let pace = Pace::of(kind);
        let (target, slack, cap) = (frames(pace.target), frames(pace.slack), frames(pace.cap));
        // Twice the cap, so a backlog shows as one before the pusher has to drop anything.
        let (input, queue) = RingBuffer::new(cap * 4);
        let playout = Playout {
            queue,
            target,
            slack,
            cap,
            playing: false,
            faded_in: 0,
            low: usize::MAX,
            seen: 0,
            last_low: None,
            skew: 0,
            block: Vec::with_capacity(frames(40)),
        };
        (input, playout)
    }

    /// Adds the next `out.len() / 2` frames, at `gain`, into `out`. `stale`: the queue overflowed
    /// since the last pull, so what it holds is old.
    fn pull(&mut self, out: &mut [f32], gain: f32, stale: bool) {
        let want = out.len() / 2;
        // A long device block needs a burst and the jitter on top of itself queued.
        let target = self.target.max(want + frames(20));
        let mut queued = self.queue.slots() / 2;
        if stale {
            self.discard(queued);
            self.playing = false;
            return;
        }
        if queued > self.cap.max(2 * target) {
            // A backlog (the speakers were away): only the newest of it is worth playing.
            self.discard(queued - target);
            queued = target;
        }
        if !self.playing {
            if queued < target {
                return;
            }
            self.playing = true;
            self.faded_in = 0;
            (self.low, self.seen, self.last_low, self.skew) = (usize::MAX, 0, None, 0);
        }
        self.low = self.low.min(queued);

        // This block and a few frames over, in case some are dropped.
        let avail = queued.min(want + want / 8 + 1);
        self.block.clear();
        if let Ok(chunk) = self.queue.read_chunk(avail * 2) {
            let (a, b) = chunk.as_slices();
            self.block.extend(a.chunks_exact(2).chain(b.chunks_exact(2)).map(|f| [f[0], f[1]]));
        }
        let peak = self.block.iter().take(want).fold(0f32, |p, f| p.max(f[0].abs()).max(f[1].abs()));
        let room = if want < 16 {
            0
        } else if peak < QUIET {
            want / 8
        } else {
            1
        };
        let (mut drop, mut repeat) = match self.skew {
            s if s > 0 => ((s as usize).min(room).min(avail.saturating_sub(want)), 0),
            s if s < 0 => (0, s.unsigned_abs().min(room)),
            _ => (0, 0),
        };
        let mut take = want + drop - repeat;
        let dry = take > avail;
        if dry {
            (drop, repeat, take) = (0, 0, avail);
        }
        self.skew += repeat as isize - drop as isize;
        let played = if dry { avail } else { want };

        // Where a frame goes or doubles: the quietest one when only one does, else spread out.
        let marks = drop + repeat;
        let energy = |f: &[f32; 2]| f[0].abs() + f[1].abs();
        let quietest = if marks == 1 { (0..take).min_by(|&i, &j| energy(&self.block[i]).total_cmp(&energy(&self.block[j]))) } else { None };
        let mark = |k: usize| quietest.unwrap_or(k * take / (marks + 1));
        let fade_out = if dry { FADE.min(played) } else { 0 };
        let (mut o, mut k) = (0, 0);
        for s in 0..take {
            let copies = if k < marks && s == mark(k + 1) {
                k += 1;
                if drop > 0 { 0 } else { 2 }
            } else {
                1
            };
            for _ in 0..copies {
                let mut g = gain;
                if self.faded_in < FADE {
                    g *= self.faded_in as f32 / FADE as f32;
                    self.faded_in += 1;
                }
                if played - o <= fade_out {
                    g *= (played - o) as f32 / fade_out as f32;
                }
                let [l, r] = self.block[s];
                out[2 * o] += l * g;
                out[2 * o + 1] += r * g;
                o += 1;
            }
        }
        debug_assert_eq!(o, played);
        self.discard(take);
        if dry {
            self.playing = false;
            return;
        }
        self.seen += want;
        if self.seen >= WINDOW {
            self.settle(target);
        }
    }

    /// At the end of a window: when it and the one before both sat outside target ± slack, aims
    /// to bring the fill back by the smaller of their two misses.
    fn settle(&mut self, target: usize) {
        let low = std::mem::replace(&mut self.low, usize::MAX);
        self.seen = 0;
        if let Some(last) = self.last_low.replace(low) {
            let (lo, hi) = (low.min(last), low.max(last));
            if lo > target + self.slack {
                self.skew = (lo - target) as isize;
            } else if hi + self.slack < target {
                self.skew = -((target - hi) as isize);
            }
        }
    }

    fn discard(&mut self, frames: usize) {
        if let Ok(chunk) = self.queue.read_chunk(frames * 2) {
            chunk.commit_all();
        }
    }
}

/// What the speakers play, and the mix the echo canceller listens to.
pub struct Mixer {
    /// Sources added or taken out since the output callback last looked. The callback only ever
    /// tries this lock: while someone holds it, the change waits for the next callback.
    changes: Mutex<Vec<Change>>,
    /// The output callback's own: the sources it plays. Nothing else takes this lock (tests
    /// aside), so the callback never waits on it, nor holds up anyone adding a source while it
    /// works through them.
    playing: Mutex<Playing>,
    /// What the callback is done with, freed here instead: dropping the last hold on a soundpad
    /// clip or a stream's queue frees memory, which has no business against the speakers'
    /// deadline. Emptied by `collect`.
    retired: Mutex<Consumer<Retired>>,
    pub deafened: AtomicBool,
    /// The mix, mono, for the echo canceller's reference.
    far: Mutex<VecDeque<f32>>,
    /// Frames mixed since anything was there to play: nobody's voice or screen, no clip, the cues'
    /// bus silent. The audio thread closes the speakers once this is long enough.
    quiet: AtomicUsize,
    /// The speakers were closed for having nothing to play, and open again for the next sound.
    parked: AtomicBool,
    /// The audio thread, to ask for the speakers back.
    wake: OnceLock<mpsc::Sender<Command>>,
}

enum Change {
    Add(Arc<Source>),
    /// The source's address: holding no `Arc`, it leaves nothing to drop on the callback.
    Remove(usize),
}

struct Playing {
    sources: Vec<Arc<Source>>,
    retire: Producer<Retired>,
}

impl Mixer {
    fn new() -> Arc<Mixer> {
        let (retire, retired) = RingBuffer::new(256);
        Arc::new(Mixer {
            changes: Mutex::new(Vec::new()),
            // Room for more sources than a call has, so adding one doesn't allocate on the callback.
            playing: Mutex::new(Playing { sources: Vec::with_capacity(64), retire }),
            retired: Mutex::new(retired),
            deafened: AtomicBool::new(false),
            far: Mutex::new(VecDeque::new()),
            quiet: AtomicUsize::new(0),
            parked: AtomicBool::new(false),
            wake: OnceLock::new(),
        })
    }

    pub fn add(&self, s: Arc<Source>) {
        self.changes.lock().push(Change::Add(s));
        self.collect();
        self.wake();
    }

    /// Something is about to play: the speakers come back if they were closed for the quiet.
    fn wake(&self) {
        if self.parked.load(Ordering::Acquire)
            && let Some(tx) = self.wake.get()
        {
            let _ = tx.send(Command::Resume);
        }
    }

    /// Whether anything waits to be played: a change not taken in yet, or a source with sound
    /// queued. Off the output callback only (it takes the callback's locks).
    fn pending(&self) -> bool {
        !self.changes.lock().is_empty() || self.playing.lock().sources.iter().any(|s| !s.is_empty())
    }

    pub fn remove(&self, s: &Arc<Source>) {
        self.changes.lock().push(Change::Remove(Arc::as_ptr(s) as usize));
        self.collect();
    }

    /// Frees what the output callback is done with.
    fn collect(&self) {
        let mut retired = self.retired.lock();
        while let Ok(r) = retired.pop() {
            drop(r);
        }
    }

    /// With no speakers open nothing calls `mix`: the changes are applied here instead, off any
    /// deadline, so a source taken out is freed now and not held until speakers come back. What
    /// would only sound once (a cue, a soundpad clip) is let go too, rather than all of it
    /// sounding at once when they do. Only while no output stream exists.
    fn idle(&self) {
        let mut playing = self.playing.lock();
        let sources = &mut playing.sources;
        for change in self.changes.lock().drain(..) {
            match change {
                Change::Add(s) => sources.push(s),
                Change::Remove(id) => sources.retain(|s| Arc::as_ptr(s) as usize != id),
            }
        }
        sources.retain(|s| !s.one_shot);
        for s in sources.iter() {
            s.drop_cues();
        }
        drop(playing);
        self.collect();
    }

    /// Fills `out` (interleaved stereo, 48 kHz).
    fn mix(&self, out: &mut [f32]) {
        out.fill(0.);
        let deaf = self.deafened.load(Ordering::Relaxed);
        let mut playing = self.playing.lock();
        let Playing { sources, retire } = &mut *playing;
        if let Some(mut changes) = self.changes.try_lock() {
            for change in changes.drain(..) {
                match change {
                    Change::Add(s) => sources.push(s),
                    Change::Remove(id) => {
                        if let Some(i) = sources.iter().position(|s| Arc::as_ptr(s) as usize == id) {
                            retire_on(retire, Retired::Source(sources.remove(i)));
                        }
                    }
                }
            }
        }
        let mut busy = false;
        for s in sources.iter() {
            let gain = if deaf && s.kind == SourceKind::Voice { 0. } else { s.gain.get() };
            busy |= s.mix_into(out, gain, retire);
        }
        if busy {
            self.quiet.store(0, Ordering::Relaxed);
        } else {
            self.quiet.fetch_add(out.len() / 2, Ordering::Relaxed);
        }
        let mut i = 0;
        while i < sources.len() {
            if sources[i].one_shot && sources[i].is_empty() {
                retire_on(retire, Retired::Source(sources.remove(i)));
            } else {
                i += 1;
            }
        }
        drop(playing);
        for o in out.iter_mut() {
            *o = soft_clip(*o);
        }
        let mut far = self.far.lock();
        for f in out.chunks_exact(2) {
            far.push_back(0.5 * (f[0] + f[1]));
        }
        let over = far.len().saturating_sub(RATE as usize / 2);
        far.drain(..over);
    }
}

/// Keeps loud mixes (several people at 350%) from wrapping around.
fn soft_clip(x: f32) -> f32 {
    if x.abs() <= 0.9 { x } else { x.signum() * (0.9 + 0.1 * ((x.abs() - 0.9) / 0.1).tanh()) }
}

/// Input frames the resampler works on at a time: a little over a millisecond, so it adds next to
/// no delay to the speakers or the microphone.
const RESAMPLE_CHUNK: usize = 64;

fn sinc(from: u32, to: u32, sinc_len: usize, chunk: usize, channels: usize) -> rubato::Async<f32> {
    let params = rubato::SincInterpolationParameters::new(sinc_len, rubato::WindowFunction::BlackmanHarris2);
    rubato::Async::new_sinc(to as f64 / from as f64, 1., &params, chunk, channels, rubato::FixedAsync::Input)
        .expect("rates and chunk are positive")
}

/// Converts interleaved audio from one rate to another, keeping its place between calls. What lies
/// above the lower rate's Nyquist frequency is filtered out first (a windowed sinc, rubato's):
/// interpolating without that folded it back as noise, worst on a Bluetooth headset's hands-free
/// 16 or 8 kHz. It works in chunks of `RESAMPLE_CHUNK` input frames, holding back what doesn't
/// fill one, and after its first call allocates nothing, so it can run in a device's callback.
pub struct Resampler {
    channels: usize,
    /// None when both rates are the same: audio passes through as it is.
    sinc: Option<Box<rubato::Async<f32>>>,
    /// Input still short of a whole chunk.
    pending: Vec<f32>,
    /// One chunk's output.
    chunk: Vec<f32>,
}

impl Resampler {
    pub fn new(from: u32, to: u32, channels: usize) -> Resampler {
        let channels = channels.max(1);
        // 128 taps: flat to within a few percent of Nyquist, at a cost a callback doesn't notice.
        let sinc = (from != to && from > 0 && to > 0).then(|| Box::new(sinc(from, to, 128, RESAMPLE_CHUNK, channels)));
        let out = sinc.as_ref().map_or(0, |s| s.output_frames_max() * channels);
        Resampler { channels, sinc, pending: Vec::with_capacity(RESAMPLE_CHUNK * channels), chunk: vec![0.; out] }
    }

    /// Input frames it takes to produce any output: worth handing over at least this many.
    pub fn chunk(&self) -> usize {
        if self.sinc.is_some() { RESAMPLE_CHUNK } else { 1 }
    }

    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let ch = self.channels;
        let Some(sinc) = self.sinc.as_mut() else {
            out.extend_from_slice(input);
            return;
        };
        let whole = RESAMPLE_CHUNK * ch;
        let mut input = &input[..input.len() / ch * ch];
        while !input.is_empty() {
            let take = (whole - self.pending.len()).min(input.len());
            self.pending.extend_from_slice(&input[..take]);
            input = &input[take..];
            if self.pending.len() < whole {
                break;
            }
            let frames_out = self.chunk.len() / ch;
            let made = match (
                InterleavedSlice::new(&self.pending[..], ch, RESAMPLE_CHUNK),
                InterleavedSlice::new_mut(&mut self.chunk[..], ch, frames_out),
            ) {
                (Ok(src), Ok(mut dst)) => sinc.process_into_buffer(&src, &mut dst, None).map_or(0, |(_, n)| n),
                _ => 0,
            };
            out.extend_from_slice(&self.chunk[..made * ch]);
            self.pending.clear();
        }
    }
}

/// A whole clip (interleaved) at another rate, without the filter's delay in front or anything
/// cut off at the end: for sounds decoded ahead of playing, not for a live stream.
pub fn resample_clip(input: &[f32], from: u32, to: u32, channels: usize) -> Vec<f32> {
    let channels = channels.max(1);
    let frames = input.len() / channels;
    if from == to || from == 0 || to == 0 || frames == 0 {
        return input.to_vec();
    }
    let mut r = sinc(from, to, 256, 1024, channels);
    match InterleavedSlice::new(input, channels, frames).map(|src| r.process_all(&src, frames, None)) {
        Ok(Ok(out)) => out.take_data(),
        _ => Vec::new(),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
}

/// The audio host, made once: PulseAudio's connects to the server, and devices are looked up
/// every second while a chosen one is missing. Let go after something fails, so a server that
/// restarted is connected to again.
static HOST: Mutex<Option<cpal::Host>> = parking_lot::const_mutex(None);

pub(super) fn with_host<T>(f: impl FnOnce(&cpal::Host) -> T) -> T {
    let mut host = HOST.lock();
    f(host.get_or_insert_with(cpal::default_host))
}

pub(super) fn forget_host() {
    *HOST.lock() = None;
}

pub fn input_devices() -> Vec<DeviceInfo> {
    let devices = with_host(|h| h.input_devices().map(|d| d.collect::<Vec<_>>()));
    list(devices).into_iter().filter(|d| !not_a_microphone(&d.id)).collect()
}

pub fn output_devices() -> Vec<DeviceInfo> {
    list(with_host(|h| h.output_devices().map(|d| d.collect::<Vec<_>>())))
}

/// Inputs that are not microphones: PulseAudio's monitor of each output, and on macOS the device a
/// share's sound is taken through while it runs.
fn not_a_microphone(id: &str) -> bool {
    id.ends_with(".monitor") || id.contains(super::loopback::TAP_DEVICE)
}

fn list(devices: Result<Vec<cpal::Device>, cpal::Error>) -> Vec<DeviceInfo> {
    match devices {
        Ok(d) => d.iter().filter_map(info).collect(),
        Err(e) => {
            log::warn!("audio devices: {e}");
            forget_host();
            Vec::new()
        }
    }
}

fn info(d: &cpal::Device) -> Option<DeviceInfo> {
    Some(DeviceInfo { id: d.id().ok()?.to_string(), name: d.description().ok()?.name().to_string() })
}

fn present(id: &str) -> bool {
    id.parse::<cpal::DeviceId>().is_ok_and(|id| with_host(|h| h.device_by_id(&id).is_some()))
}

fn find(id: &str, input: bool) -> Option<cpal::Device> {
    with_host(|host| {
        if !id.is_empty()
            && let Ok(parsed) = id.parse::<cpal::DeviceId>()
            && let Some(d) = host.device_by_id(&parsed)
        {
            return Some(d);
        }
        // The saved device is gone (unplugged): use the default, without forgetting the choice.
        if input { host.default_input_device() } else { host.default_output_device() }
    })
}

/// The microphone after processing: what the call publishes and what the meter shows.
pub struct Mic {
    /// Input volume, 0..2.
    pub gain: AtomicF32,
    /// Gate threshold as RMS; 0 is off.
    pub gate: AtomicF32,
    pub muted: AtomicBool,
    pub echo_cancellation: AtomicBool,
    pub noise_suppression: AtomicBool,
    /// Loudest 10 ms since last read, after the input volume, before the gate.
    pub level: AtomicF32,
    /// Whether the gate let the last frame through.
    pub open: AtomicBool,
    /// Whether a capture stream is open. While none is (muted, or no device), the publisher still
    /// gets silence every 10 ms, so the people listening don't take the stream for stalled.
    capturing: AtomicBool,
    /// Capture callbacks so far: the audio thread watches it for a microphone that stopped.
    frames: AtomicU64,
    /// Where processed 10 ms frames go: the voice publisher, when there is one.
    sink: Mutex<Option<MicSink>>,
    /// Whether `sink` holds one, read without its lock.
    has_sink: AtomicBool,
    users: AtomicU64,
}

/// Raw microphone audio at 48 kHz on its way to the processing thread, which waits on the condvar.
type Near = Arc<(Mutex<VecDeque<f32>>, Condvar)>;

/// Takes each processed 10 ms of microphone audio.
pub type MicSink = Box<dyn FnMut(&[i16]) + Send>;

/// Commands for the audio thread, which owns the device streams.
enum Command {
    Output(String),
    Input(Option<String>),
    /// The capture's device went away, or it was Windows' default and the default changed.
    InputLost,
    /// The same for the speakers.
    OutputLost,
    /// Something to play while the speakers are closed for the quiet.
    Resume,
}

/// The microphone changed under a capture without anyone choosing it: unplugged, Windows' default
/// moved to another, or the chosen one came back.
#[derive(Clone, Debug)]
pub struct MicSwitch {
    /// The one captured before; `None` when there never was one.
    pub from: Option<DeviceInfo>,
    /// `None` when no microphone is left.
    pub to: Option<DeviceInfo>,
}

type Channel<T> = (async_channel::Sender<T>, async_channel::Receiver<T>);

static MIC_SWITCHES: LazyLock<Channel<MicSwitch>> = LazyLock::new(async_channel::unbounded);

/// Every microphone switch nobody chose, as it happens.
pub fn mic_switches() -> async_channel::Receiver<MicSwitch> {
    MIC_SWITCHES.1.clone()
}

/// Which microphone the capture is on, to tell a switch from a reopen of the same one.
#[derive(Default)]
struct Using {
    /// The last one that opened, kept while none will.
    device: Option<DeviceInfo>,
    missing: bool,
}

impl Using {
    /// Notes what a capture opened on (`None`: nothing would), telling the interface when it
    /// changed and nobody asked.
    fn opened(&mut self, now: Option<DeviceInfo>, asked: bool) {
        let changed = match &now {
            Some(d) => self.missing || self.device.as_ref().is_none_or(|u| u.id != d.id),
            None => !self.missing,
        };
        if changed && !asked {
            let _ = MIC_SWITCHES.0.try_send(MicSwitch { from: self.device.clone(), to: now.clone() });
        }
        self.missing = now.is_none();
        if now.is_some() {
            self.device = now;
        }
    }
}

pub struct Audio {
    pub mixer: Arc<Mixer>,
    pub mic: Arc<Mic>,
    commands: mpsc::Sender<Command>,
    input_id: Mutex<String>,
    near: Near,
}

static AUDIO: OnceLock<Audio> = OnceLock::new();

/// The one audio engine, started the first time it is needed.
pub fn audio() -> &'static Audio {
    AUDIO.get_or_init(Audio::start)
}

impl Audio {
    fn start() -> Audio {
        let mixer = Mixer::new();
        let mic = Arc::new(Mic {
            gain: AtomicF32::new(1.),
            gate: AtomicF32::new(0.),
            muted: AtomicBool::new(false),
            echo_cancellation: AtomicBool::new(true),
            noise_suppression: AtomicBool::new(true),
            level: AtomicF32::default(),
            open: AtomicBool::new(false),
            capturing: AtomicBool::new(false),
            frames: AtomicU64::new(0),
            sink: Mutex::new(None),
            has_sink: AtomicBool::new(false),
            users: AtomicU64::new(0),
        });
        let near: Near = Arc::new((Mutex::new(VecDeque::new()), Condvar::new()));
        let (tx, rx) = mpsc::channel();
        let _ = mixer.wake.set(tx.clone());
        {
            let (mixer, mic, near, tx) = (mixer.clone(), mic.clone(), near.clone(), tx.clone());
            std::thread::Builder::new().name("ciranda-audio".into()).spawn(move || device_thread(rx, tx, mixer, mic, near)).ok();
        }
        {
            let (mixer, mic, near) = (mixer.clone(), mic.clone(), near.clone());
            std::thread::Builder::new().name("ciranda-mic".into()).spawn(move || dsp_thread(mixer, mic, near)).ok();
        }
        let _ = tx.send(Command::Output(String::new()));
        Audio { mixer, mic, commands: tx, input_id: Mutex::new(String::new()), near }
    }

    pub fn set_output(&self, id: &str) {
        let _ = self.commands.send(Command::Output(id.to_string()));
    }

    /// Reopens the capture only when the device actually changes.
    pub fn set_input(&self, id: &str) {
        {
            let mut current = self.input_id.lock();
            if *current == id {
                return;
            }
            *current = id.to_string();
        }
        if self.mic.users.load(Ordering::Relaxed) > 0 {
            let _ = self.commands.send(Command::Input(Some(id.to_string())));
        }
    }

    /// Opens the microphone for as long as the returned guard lives (a call, the settings meter).
    pub fn acquire_mic(&'static self) -> MicGuard {
        if self.mic.users.fetch_add(1, Ordering::Relaxed) == 0 {
            let _ = self.commands.send(Command::Input(Some(self.input_id.lock().clone())));
        }
        MicGuard(self)
    }

    /// Where processed microphone frames go (10 ms, mono, 48 kHz).
    pub fn set_mic_sink(&self, sink: Option<MicSink>) {
        let some = sink.is_some();
        *self.mic.sink.lock() = sink;
        self.mic.has_sink.store(some, Ordering::Release);
        // Under the lock the processing thread checks before it waits, so it can't miss this.
        let (buf, cv) = &*self.near;
        let _held = buf.lock();
        cv.notify_all();
    }
}

pub struct MicGuard(&'static Audio);

impl Drop for MicGuard {
    fn drop(&mut self) {
        if self.0.mic.users.fetch_sub(1, Ordering::Relaxed) == 1 {
            let _ = self.0.commands.send(Command::Input(None));
        }
    }
}

/// A capture that delivers nothing for this long is taken for dead and opened again: the device
/// was unplugged, went to sleep, or a headset switched mode. Nothing else would notice, since the
/// publisher fills the gap with silence.
const CAPTURE_STALL: Duration = Duration::from_secs(3);
/// How often a microphone that would not open is tried again, for one plugged back in.
const CAPTURE_RETRY: Duration = Duration::from_secs(10);
/// Nothing to play for this long and the speakers are closed, so an idle Ciranda doesn't keep the
/// sound device (and the processor) awake mixing silence. The next sound opens them again.
const SPEAKERS_IDLE: Duration = Duration::from_secs(30);

fn device_thread(rx: mpsc::Receiver<Command>, tx: mpsc::Sender<Command>, mixer: Arc<Mixer>, mic: Arc<Mic>, near: Near) {
    let mut output: Option<cpal::Stream> = None;
    let mut input: Option<cpal::Stream> = None;
    // The microphone wanted, while one is; and the frame count last seen, and when it moved.
    let mut wanted: Option<String> = None;
    let mut seen = (0, Instant::now());
    let mut using = Using::default();
    let mut speakers = Speakers { wanted: String::new(), on: None, tried: Instant::now() };
    let idle_frames = SPEAKERS_IDLE.as_secs() as usize * RATE as usize;
    // Closed before opening again: some devices take one capture at a time.
    let reopen = |input: &mut Option<cpal::Stream>, wanted: Option<&str>, using: &mut Using, asked: bool| {
        drop(input.take());
        let opened = open_capture(wanted, &mic, &near, &tx);
        if wanted.is_some() {
            using.opened(opened.as_ref().map(|(_, d)| d.clone()), asked);
        }
        *input = opened.map(|(s, _)| s);
        (mic.frames.load(Ordering::Relaxed), Instant::now())
    };
    loop {
        let command = rx.recv_timeout(Duration::from_secs(1));
        mixer.collect();
        // No callback is running without an output stream: nothing would apply the mixer's
        // changes. Closed for the quiet, though, what was just added is what opens them again.
        let parked = mixer.parked.load(Ordering::Acquire);
        if output.is_none() && !parked {
            mixer.idle();
        }
        match command {
            Ok(Command::Output(id)) => {
                speakers.wanted = id;
                speakers.open(&mut output, &mixer, &tx);
            }
            Ok(Command::OutputLost) => {
                log::warn!("the speakers went away; opening them again");
                speakers.open(&mut output, &mixer, &tx);
            }
            Ok(Command::Resume) => {
                if parked && output.is_none() {
                    speakers.open(&mut output, &mixer, &tx);
                }
            }
            Ok(Command::Input(id)) => {
                wanted = id;
                seen = reopen(&mut input, wanted.as_deref(), &mut using, true);
            }
            Ok(Command::InputLost) => {
                if input.is_some() {
                    seen = reopen(&mut input, wanted.as_deref(), &mut using, false);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Speakers that would not open are tried again now and then; the chosen ones,
                // when they come back after playing on Windows' default meanwhile.
                let back = !speakers.wanted.is_empty()
                    && output.is_some()
                    && speakers.on.as_deref().is_some_and(|on| on != speakers.wanted)
                    && present(&speakers.wanted);
                if back || (output.is_none() && !parked && speakers.tried.elapsed() >= CAPTURE_RETRY) {
                    speakers.open(&mut output, &mixer, &tx);
                } else if output.is_some() && mixer.quiet.load(Ordering::Relaxed) >= idle_frames {
                    // Said first, so a sound that comes as they close asks for them back; and
                    // one that slipped in before is looked for once they are closed.
                    mixer.parked.store(true, Ordering::Release);
                    drop(output.take());
                    log::debug!("nothing to play for {}s: the speakers are closed until there is", SPEAKERS_IDLE.as_secs());
                    if mixer.pending() {
                        speakers.open(&mut output, &mixer, &tx);
                    }
                }
                let Some(w) = wanted.as_deref() else { continue };
                // On Windows' default because the chosen one was gone: back to it once it's in.
                if !w.is_empty() && input.is_some() && using.device.as_ref().is_some_and(|d| d.id != w) && present(w) {
                    seen = reopen(&mut input, Some(w), &mut using, false);
                    continue;
                }
                let frames = mic.frames.load(Ordering::Relaxed);
                if frames != seen.0 {
                    seen = (frames, Instant::now());
                    continue;
                }
                if seen.1.elapsed() >= if input.is_some() { CAPTURE_STALL } else { CAPTURE_RETRY } {
                    if input.is_some() {
                        log::warn!("the microphone stopped delivering sound; opening it again");
                    }
                    seen = reopen(&mut input, Some(w), &mut using, false);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    drop((output, input));
}

/// The speakers asked for (empty for Windows' default), the ones playing, and when they were
/// last opened.
struct Speakers {
    wanted: String,
    on: Option<String>,
    tried: Instant,
}

impl Speakers {
    fn open(&mut self, output: &mut Option<cpal::Stream>, mixer: &Arc<Mixer>, lost: &mpsc::Sender<Command>) {
        drop(output.take());
        self.tried = Instant::now();
        mixer.quiet.store(0, Ordering::Relaxed);
        mixer.parked.store(false, Ordering::Release);
        match open_output(&self.wanted, mixer.clone(), lost.clone()) {
            Ok((stream, device)) => {
                *output = Some(stream);
                self.on = Some(device.id);
            }
            Err(e) => {
                log::warn!("speakers did not open: {e}");
                self.on = None;
                forget_host();
            }
        }
    }
}

/// Opens the capture for `id`, with the device it landed on; none for `None`, or for a
/// microphone that will not open.
fn open_capture(id: Option<&str>, mic: &Arc<Mic>, near: &Near, tx: &mpsc::Sender<Command>) -> Option<(cpal::Stream, DeviceInfo)> {
    mic.capturing.store(false, Ordering::Relaxed);
    near.0.lock().clear();
    match open_input(id?, near.clone(), mic.clone(), tx.clone()) {
        Ok(s) => {
            mic.capturing.store(true, Ordering::Relaxed);
            Some(s)
        }
        Err(e) => {
            log::warn!("microphone did not open: {e}");
            forget_host();
            None
        }
    }
}

/// An input stream in whatever sample format the device gives (cpal's defaults prefer the
/// device's own, often 32- or 24-bit integers), handed over as f32. The copy is kept between
/// callbacks, so after the first few it allocates nothing.
pub(super) fn build_input(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    take: impl FnMut(&[f32]) + Send + 'static,
    err: impl FnMut(cpal::Error) + Send + 'static,
) -> anyhow::Result<cpal::Stream> {
    fn build<T: SizedSample>(
        device: &cpal::Device,
        config: cpal::StreamConfig,
        mut take: impl FnMut(&[f32]) + Send + 'static,
        err: impl FnMut(cpal::Error) + Send + 'static,
    ) -> Result<cpal::Stream, cpal::Error>
    where
        f32: FromSample<T>,
    {
        let mut tmp: Vec<f32> = Vec::with_capacity(16_384);
        let data = move |i: &[T], _: &cpal::InputCallbackInfo| {
            tmp.clear();
            tmp.extend(i.iter().map(|&s| f32::from_sample(s)));
            take(&tmp);
        };
        device.build_input_stream::<T, _, _>(config, data, err, None)
    }
    use cpal::SampleFormat as F;
    let c = stream_config(device, config, 10);
    Ok(match config.sample_format() {
        F::F32 => build::<f32>(device, c, take, err)?,
        F::F64 => build::<f64>(device, c, take, err)?,
        F::I8 => build::<i8>(device, c, take, err)?,
        F::I16 => build::<i16>(device, c, take, err)?,
        F::I24 => build::<cpal::I24>(device, c, take, err)?,
        F::I32 => build::<i32>(device, c, take, err)?,
        F::I64 => build::<i64>(device, c, take, err)?,
        F::U8 => build::<u8>(device, c, take, err)?,
        F::U16 => build::<u16>(device, c, take, err)?,
        F::U24 => build::<cpal::U24>(device, c, take, err)?,
        F::U32 => build::<u32>(device, c, take, err)?,
        F::U64 => build::<u64>(device, c, take, err)?,
        other => anyhow::bail!("the device gives {other:?} samples"),
    })
}

/// The device's own config, with a buffer of `ms` where the host would otherwise choose a long
/// one: PulseAudio's default is some two seconds, playing and recording alike, which no call
/// (nor the echo canceller, which looks half a second back) could live with.
fn stream_config(device: &cpal::Device, config: &cpal::SupportedStreamConfig, ms: u32) -> cpal::StreamConfig {
    let c = config.config();
    #[cfg(target_os = "linux")]
    if device.id().is_ok_and(|id| id.host() == cpal::HostId::PulseAudio) {
        let mut frames = (c.sample_rate * ms / 1000).max(1);
        if let cpal::SupportedBufferSize::Range { min, max } = *config.buffer_size() {
            frames = frames.clamp(min, max.max(min));
        }
        return cpal::StreamConfig { buffer_size: cpal::BufferSize::Fixed(frames), ..c };
    }
    let _ = (device, ms);
    c
}

/// The output's counterpart of `build_input`: `fill` mixes in f32, converted to the device's
/// format on the way out.
fn build_output(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    fill: impl FnMut(&mut [f32]) + Send + 'static,
    err: impl FnMut(cpal::Error) + Send + 'static,
) -> anyhow::Result<cpal::Stream> {
    fn build<T: SizedSample + FromSample<f32>>(
        device: &cpal::Device,
        config: cpal::StreamConfig,
        mut fill: impl FnMut(&mut [f32]) + Send + 'static,
        err: impl FnMut(cpal::Error) + Send + 'static,
    ) -> Result<cpal::Stream, cpal::Error> {
        let mut tmp: Vec<f32> = Vec::with_capacity(16_384);
        let data = move |o: &mut [T], _: &cpal::OutputCallbackInfo| {
            tmp.resize(o.len(), 0.);
            fill(&mut tmp);
            for (d, s) in o.iter_mut().zip(&tmp) {
                *d = T::from_sample(s.clamp(-1., 1.));
            }
        };
        device.build_output_stream::<T, _, _>(config, data, err, None)
    }
    use cpal::SampleFormat as F;
    let c = stream_config(device, config, 20);
    Ok(match config.sample_format() {
        F::F32 => build::<f32>(device, c, fill, err)?,
        F::F64 => build::<f64>(device, c, fill, err)?,
        F::I8 => build::<i8>(device, c, fill, err)?,
        F::I16 => build::<i16>(device, c, fill, err)?,
        F::I24 => build::<cpal::I24>(device, c, fill, err)?,
        F::I32 => build::<i32>(device, c, fill, err)?,
        F::I64 => build::<i64>(device, c, fill, err)?,
        F::U8 => build::<u8>(device, c, fill, err)?,
        F::U16 => build::<u16>(device, c, fill, err)?,
        F::U24 => build::<cpal::U24>(device, c, fill, err)?,
        F::U32 => build::<u32>(device, c, fill, err)?,
        F::U64 => build::<u64>(device, c, fill, err)?,
        other => anyhow::bail!("the speakers want {other:?} samples"),
    })
}

fn open_output(id: &str, mixer: Arc<Mixer>, lost: mpsc::Sender<Command>) -> anyhow::Result<(cpal::Stream, DeviceInfo)> {
    let device = find(id, false).ok_or_else(|| anyhow::anyhow!("no output device"))?;
    let playing = info(&device).ok_or_else(|| anyhow::anyhow!("the speakers have no name"))?;
    let config = device.default_output_config()?;
    let (rate, channels) = (config.sample_rate(), config.channels() as usize);
    let mut resampler = Resampler::new(RATE, rate, 2);
    // Kept between callbacks: after the first few they only reuse their capacity.
    let mut mixed = Vec::new();
    let mut res = Vec::new();
    let mut ready: VecDeque<f32> = VecDeque::new();
    let fill = move |out: &mut [f32]| {
        let channels = channels.max(1);
        let frames = out.len() / channels;
        // Mix in 48 kHz stereo blocks until the device's rate and size are covered.
        while ready.len() / 2 < frames {
            // At least a chunk: the resampler gives nothing back for less.
            let need = (((frames - ready.len() / 2) as f64 * RATE as f64 / rate as f64).ceil() as usize + 2).max(resampler.chunk());
            mixed.resize(need * 2, 0.);
            mixer.mix(&mut mixed);
            res.clear();
            resampler.process(&mixed, &mut res);
            ready.extend(res.iter().copied());
        }
        for f in out.chunks_exact_mut(channels) {
            let (l, r) = (ready.pop_front().unwrap_or(0.), ready.pop_front().unwrap_or(0.));
            match channels {
                1 => f[0] = 0.5 * (l + r),
                _ => {
                    f[0] = l;
                    f[1] = r;
                    for x in f.iter_mut().skip(2) {
                        *x = 0.;
                    }
                }
            }
        }
    };
    // Unplugged, or Windows' default moved: the stream stops for good, and nothing would be
    // heard again until it's opened anew.
    let err = move |e: cpal::Error| {
        warn_stream("speakers", &e);
        if gone(&e) {
            let _ = lost.send(Command::OutputLost);
        }
    };
    let stream = build_output(&device, &config, fill, err)?;
    stream.play()?;
    Ok((stream, playing))
}

/// A stream's error, logged; being refused real-time scheduling is no news worth a warning.
pub(super) fn warn_stream(what: &str, e: &cpal::Error) {
    if e.kind() == cpal::ErrorKind::RealtimeDenied {
        log::debug!("{what}: {e}");
    } else {
        log::warn!("{what}: {e}");
    }
}

/// Whether a stream's error means its device is gone (or was the default, and moved), so the
/// stream will never deliver again.
pub(super) fn gone(e: &cpal::Error) -> bool {
    matches!(e.kind(), cpal::ErrorKind::DeviceNotAvailable | cpal::ErrorKind::DeviceChanged | cpal::ErrorKind::StreamInvalidated)
}

fn open_input(id: &str, near: Near, mic: Arc<Mic>, lost: mpsc::Sender<Command>) -> anyhow::Result<(cpal::Stream, DeviceInfo)> {
    let device = find(id, true).ok_or_else(|| anyhow::anyhow!("no microphone"))?;
    let using = info(&device).ok_or_else(|| anyhow::anyhow!("the microphone has no name"))?;
    let config = device.default_input_config()?;
    let (rate, channels) = (config.sample_rate(), config.channels() as usize);
    let mut resampler = Resampler::new(rate, RATE, 1);
    let mut downmix = Downmix::new(channels, rate);
    let mut mono = Vec::with_capacity(8192);
    let mut out = Vec::with_capacity(8192);
    let take = move |input: &[f32]| {
        mic.frames.fetch_add(1, Ordering::Relaxed);
        mono.clear();
        downmix.process(input, &mut mono);
        out.clear();
        resampler.process(&mono, &mut out);
        let (buf, cv) = &*near;
        let mut b = buf.lock();
        b.extend(out.iter().copied());
        let over = b.len().saturating_sub(RATE as usize / 2);
        b.drain(..over);
        cv.notify_one();
    };
    let err = move |e: cpal::Error| {
        warn_stream("microphone", &e);
        if gone(&e) {
            let _ = lost.send(Command::InputLost);
        }
    };
    let stream = build_input(&device, &config, take, err)?;
    stream.play()?;
    Ok((stream, using))
}

/// A whole window of a device's frames, half a second, over which each channel's level is taken.
const DOWNMIX_WINDOW_MS: usize = 500;
/// Quieter than this (about -60 dBFS RMS) a channel holds nothing worth switching to.
const DOWNMIX_FLOOR: f32 = 1e-6;
/// How long a change of channel takes, faded, so it never clicks.
const DOWNMIX_FADE_MS: usize = 20;

/// Makes a device's frames the one channel the call sends. Averaging every channel cost a
/// microphone on one input of an audio interface 6 dB on two channels and 18 dB on eight, with the
/// other inputs' hiss on top. So two channels are averaged while both carry the sound (a stereo
/// or doubled-mono microphone) and the louder taken alone when the other is all but silent; more
/// than two, the loudest is taken, measured over half a second and only left for one clearly
/// louder. Changes fade over a few milliseconds. Allocates only when made.
struct Downmix {
    channels: usize,
    /// Each channel's share of the output now, and where it is heading.
    weights: Vec<f32>,
    target: Vec<f32>,
    /// Each channel's energy over the window so far, and the frames in it.
    energy: Vec<f32>,
    seen: usize,
    window: usize,
    /// How far a weight moves in a frame.
    step: f32,
    /// The channel taken alone, or none while two are averaged.
    alone: Option<usize>,
}

impl Downmix {
    fn new(channels: usize, rate: u32) -> Downmix {
        let channels = channels.max(1);
        let rate = rate.max(1) as usize;
        // Two channels start averaged, as before; more start on the first until one is heard.
        let (target, alone) = match channels {
            2 => (vec![0.5, 0.5], None),
            n => ((0..n).map(|c| if c == 0 { 1. } else { 0. }).collect(), Some(0)),
        };
        Downmix {
            channels,
            weights: target.clone(),
            target,
            energy: vec![0.; channels],
            seen: 0,
            window: (rate * DOWNMIX_WINDOW_MS / 1000).max(1),
            step: 1. / (rate * DOWNMIX_FADE_MS / 1000).max(1) as f32,
            alone,
        }
    }

    /// Appends the mono of `input` (interleaved) to `out`.
    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let ch = self.channels;
        if ch == 1 {
            out.extend_from_slice(input);
            return;
        }
        for frame in input.chunks_exact(ch) {
            let mut v = 0.;
            let channels = frame.iter().zip(self.energy.iter_mut()).zip(self.weights.iter_mut().zip(&self.target));
            for ((&x, energy), (w, &t)) in channels {
                *energy += x * x;
                if *w != t {
                    *w = if *w < t { (*w + self.step).min(t) } else { (*w - self.step).max(t) };
                }
                v += x * *w;
            }
            out.push(v);
            self.seen += 1;
            if self.seen >= self.window {
                self.decide();
            }
        }
    }

    /// At the end of a window: which channels to take from here.
    fn decide(&mut self) {
        let n = self.seen.max(1) as f32;
        let ch = self.channels;
        let loudest = (0..ch).max_by(|&a, &b| self.energy[a].total_cmp(&self.energy[b])).unwrap_or(0);
        let level = |c: usize| self.energy[c] / n;
        if level(loudest) > DOWNMIX_FLOOR {
            if ch == 2 {
                let other = 1 - loudest;
                // 20 dB apart to take one alone, back to both under 15: no flapping in between.
                let ratio = level(loudest) / level(other).max(f32::MIN_POSITIVE);
                match self.alone {
                    None if ratio > 100. => self.alone = Some(loudest),
                    Some(_) if ratio < 30. => self.alone = None,
                    Some(c) if c != loudest && ratio > 100. => self.alone = Some(loudest),
                    _ => {}
                }
            } else if let Some(current) = self.alone
                && loudest != current
                && level(loudest) > 2. * level(current)
            {
                // 3 dB louder than the one taken, or the switch would go back and forth.
                self.alone = Some(loudest);
            }
        }
        for c in 0..ch {
            self.target[c] = match self.alone {
                Some(a) => (c == a) as u8 as f32,
                None => 1. / ch as f32,
            };
        }
        self.energy.iter_mut().for_each(|e| *e = 0.);
        self.seen = 0;
    }
}

/// The processing thread is on the microphone's way to everyone, every 10 ms, so it asks to be
/// scheduled as audio work is: Windows' multimedia class ("Pro Audio"), macOS's user-interactive
/// class. Linux has nothing an unprivileged process can ask for beyond what the audio server
/// already does for its own threads.
fn raise_priority() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{AVRT_PRIORITY_HIGH, AvSetMmThreadCharacteristicsW, AvSetMmThreadPriority};
        let task: Vec<u16> = "Pro Audio\0".encode_utf16().collect();
        let mut index = 0u32;
        // SAFETY: a NUL-terminated name and a valid out pointer; the thread keeps the class for
        // its whole life, so the handle is never reverted.
        unsafe {
            let handle = AvSetMmThreadCharacteristicsW(task.as_ptr(), &mut index);
            if handle.is_null() {
                log::debug!("the microphone thread stays at normal priority");
            } else {
                AvSetMmThreadPriority(handle, AVRT_PRIORITY_HIGH);
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn pthread_set_qos_class_self_np(class: u32, relative_priority: i32) -> i32;
        }
        const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21;
        // SAFETY: changes only the calling thread's own class.
        unsafe { pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0) };
    }
}

/// Turns raw microphone audio into what the call sends: echo cancelled against the mix, noise
/// suppressed, at the input volume, gated.
fn dsp_thread(mixer: Arc<Mixer>, mic: Arc<Mic>, near: Near) {
    raise_priority();
    let mut apm: Option<(bool, bool, AudioProcessingModule)> = None;
    let mut frame = vec![0f32; FRAME];
    let mut pcm = vec![0i16; FRAME];
    let mut far = vec![0i16; FRAME];
    // The speakers' mix, taken from the mixer whole and worked through here, so the output
    // callback never waits on the echo canceller for its lock.
    let mut far_in: VecDeque<f32> = VecDeque::new();
    let mut far_pending: VecDeque<f32> = VecDeque::new();
    let mut gate_gain = 0f32;
    let mut held_until = Instant::now();
    let tick = Duration::from_millis(10);
    let mut next_silence = Instant::now() + tick;
    loop {
        let heard = {
            let (buf, cv) = &*near;
            let mut b = buf.lock();
            loop {
                if b.len() >= FRAME {
                    for (d, s) in frame.iter_mut().zip(b.drain(..FRAME)) {
                        *d = s;
                    }
                    next_silence = Instant::now() + tick;
                    break true;
                }
                if mic.capturing.load(Ordering::Relaxed) {
                    cv.wait_for(&mut b, Duration::from_millis(200));
                } else if !mic.has_sink.load(Ordering::Acquire) {
                    // No microphone and nobody to send to: nothing to do until one of them
                    // comes, which `set_mic_sink` and the capture's first sound both say.
                    cv.wait_for(&mut b, Duration::from_secs(5));
                    next_silence = Instant::now() + tick;
                    // As below for silence: the speakers' mix meanwhile is no reference for the
                    // microphone that opens next.
                    mixer.far.lock().clear();
                    far_pending.clear();
                } else if Instant::now() >= next_silence {
                    next_silence = (next_silence + tick).max(Instant::now());
                    break false;
                } else {
                    cv.wait_until(&mut b, next_silence);
                }
            }
        };
        if !heard {
            mic.open.store(false, Ordering::Relaxed);
            gate_gain = 0.;
            // With no capture there's nothing to cancel the speakers from. Kept, the last half
            // second of them would reach the echo canceller all at once when the microphone opens
            // again, ahead of the first captured frame, and throw its delay estimate off.
            mixer.far.lock().clear();
            far_pending.clear();
            if let Some(sink) = mic.sink.lock().as_mut() {
                pcm.fill(0);
                sink(&pcm);
            }
            continue;
        }
        let (aec, ns) = (mic.echo_cancellation.load(Ordering::Relaxed), mic.noise_suppression.load(Ordering::Relaxed));
        if apm.as_ref().is_none_or(|(a, n, _)| (*a, *n) != (aec, ns)) {
            apm = Some((aec, ns, AudioProcessingModule::new(aec, false, true, ns)));
        }
        let (_, _, module) = apm.as_mut().unwrap();
        // The far end first: everything the speakers played since the last frame. The swap
        // hands the mixer back an empty queue with its capacity.
        std::mem::swap(&mut *mixer.far.lock(), &mut far_in);
        far_pending.append(&mut far_in);
        let over = far_pending.len().saturating_sub(RATE as usize / 2);
        far_pending.drain(..over);
        while far_pending.len() >= FRAME {
            for (d, s) in far.iter_mut().zip(far_pending.drain(..FRAME)) {
                *d = (s.clamp(-1., 1.) * 32767.) as i16;
            }
            let _ = module.process_reverse_stream(&mut far, RATE as i32, 1);
        }
        for (d, s) in pcm.iter_mut().zip(&frame) {
            *d = (s.clamp(-1., 1.) * 32767.) as i16;
        }
        let _ = module.process_stream(&mut pcm, RATE as i32, 1);

        let gain = mic.gain.get();
        let mut sum = 0f32;
        for s in pcm.iter_mut() {
            let v = (*s as f32 * gain).clamp(-32768., 32767.);
            sum += (v / 32768.) * (v / 32768.);
            *s = v as i16;
        }
        let rms = (sum / FRAME as f32).sqrt();
        mic.level.max(rms);

        // The noise gate: opens fast, holds 300 ms, closes softly.
        let threshold = mic.gate.get();
        let open = threshold <= 0. || rms >= threshold;
        if open {
            held_until = Instant::now() + Duration::from_millis(300);
        }
        let target = if open || Instant::now() < held_until { 1. } else { 0. };
        let muted = mic.muted.load(Ordering::Relaxed);
        mic.open.store(target > 0. && !muted, Ordering::Relaxed);
        // Per-sample smoothing: about 5 ms to open, 40 ms to close.
        let coef = if target > gate_gain { 1. - (-1. / (0.005 * RATE as f32)).exp() } else { 1. - (-1. / (0.040 * RATE as f32)).exp() };
        for s in pcm.iter_mut() {
            gate_gain += (target - gate_gain) * coef;
            *s = if muted { 0 } else { (*s as f32 * gate_gain.clamp(0., 1.)) as i16 };
        }
        if let Some(sink) = mic.sink.lock().as_mut() {
            static FRAMES: AtomicU64 = AtomicU64::new(0);
            if FRAMES.fetch_add(1, Ordering::Relaxed).is_multiple_of(500) {
                log::debug!("microphone: frame to the publisher, rms {rms:.4}");
            }
            sink(&pcm);
        }
    }
}

/// The little noises, as the old client's 3.x plays them. Rising means arriving or switching on,
/// falling means leaving or switching off; two notes for other people, a triad for you.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cue {
    /// Someone else arrived in your channel, or left it.
    Join,
    Leave,
    /// You connected to voice, or left it.
    Connect,
    Disconnect,
    /// A screen share started or stopped: brighter than arrivals, since it is news.
    StreamStart,
    StreamStop,
    /// Your microphone: short, so it is over before your next word.
    Mute,
    Unmute,
    /// Your ears: lower than mute, to tell them apart without looking.
    Deafen,
    Undeafen,
    Mention,
    /// A message somewhere you are not looking: lower and softer than a mention, which is for you.
    Message,
    /// Someone is calling you: repeated while it rings.
    Ring,
    /// You are calling someone and they have not picked up yet: quieter, repeated.
    RingBack,
}

const C5: f32 = 523.25;
const D5: f32 = 587.33;
const E5: f32 = 659.25;
const G5: f32 = 783.99;
const A5: f32 = 880.;
const B5: f32 = 987.77;
const D6: f32 = 1174.66;
const E6: f32 = 1318.51;
const F6: f32 = 1396.91;
const D4: f32 = 293.66;
const A4: f32 = 440.;

impl Cue {
    /// Its notes (frequency, start in seconds), how long each rings, and its level.
    fn voicing(self) -> (&'static [(f32, f32)], f32, f32) {
        match self {
            Cue::Join => (&[(D5, 0.), (A5, 0.09)], 0.32, 1.),
            Cue::Leave => (&[(A5, 0.), (D5, 0.09)], 0.32, 1.),
            Cue::Connect => (&[(C5, 0.), (E5, 0.07), (G5, 0.14)], 0.4, 1.),
            Cue::Disconnect => (&[(G5, 0.), (E5, 0.07), (C5, 0.14)], 0.4, 1.),
            Cue::StreamStart => (&[(E5, 0.), (B5, 0.07), (E6, 0.14)], 0.45, 0.95),
            Cue::StreamStop => (&[(E6, 0.), (B5, 0.07), (E5, 0.14)], 0.45, 0.95),
            Cue::Mute => (&[(A5, 0.), (D5, 0.055)], 0.2, 0.85),
            Cue::Unmute => (&[(D5, 0.), (A5, 0.055)], 0.2, 0.85),
            Cue::Deafen => (&[(A4, 0.), (D4, 0.07)], 0.28, 0.9),
            Cue::Undeafen => (&[(D4, 0.), (A4, 0.07)], 0.28, 0.9),
            Cue::Mention => (&[(A5, 0.), (D6, 0.08), (F6, 0.16)], 0.3, 0.9),
            Cue::Message => (&[(E5, 0.), (A5, 0.06)], 0.22, 0.6),
            Cue::Ring => (&[(E5, 0.), (G5, 0.12), (E5, 0.24), (G5, 0.36), (E5, 0.9), (G5, 1.02), (E5, 1.14), (G5, 1.26)], 0.3, 0.95),
            Cue::RingBack => (&[(D5, 0.), (D5, 0.45)], 0.35, 0.45),
        }
    }
}

/// Loudest a cue gets at 100%, as a fraction of full scale.
const CUE_PEAK: f32 = 0.32;
/// The partials of each note: multiple of the fundamental, relative level. A bare sine has no
/// harmonics, and the ear judges loudness largely by them.
const PARTIALS: [(f32, f32); 3] = [(1., 1.), (2., 0.32), (3., 0.1)];

/// From `from` to `to` over `span`, exponentially, as Web Audio ramps.
fn ramp(from: f32, to: f32, t: f32, span: f32) -> f32 {
    from * (to / from).powf((t / span).clamp(0., 1.))
}

/// A cue's samples, mono at 48 kHz, at `volume` (0..2, the sound effects volume).
pub fn render(kind: Cue, volume: f32) -> Vec<f32> {
    let (notes, len, level) = kind.voicing();
    let level = volume.clamp(0., 2.) * CUE_PEAK * level;
    if level <= 0. {
        return Vec::new();
    }
    let rate = RATE as f32;
    let total = notes.iter().map(|n| n.1).fold(0., f32::max) + len;
    let mut out = vec![0f32; (total * rate) as usize];
    let n = (len * rate) as usize;
    for &(freq, at) in notes {
        let start = (at * rate) as usize;
        let mut phase = [0f32; PARTIALS.len()];
        for i in 0..n {
            let t = i as f32 / rate;
            // A 6 ms strike, down to a third by a third of the way through, then away.
            let env = if t < 0.006 {
                ramp(0.0001, level, t, 0.006)
            } else if t < len * 0.35 {
                ramp(level, level * 0.35, t - 0.006, len * 0.35 - 0.006)
            } else {
                ramp(level * 0.35, 0.0001, t - len * 0.35, len * 0.65)
            };
            // The settle: 3% sharp at the strike, at pitch 40 ms later. Inaudible as a glide,
            // audible as a softer attack.
            let pitch = freq * ramp(1.03, 1., t, 0.04);
            let mut sum = 0.;
            for (p, &(multiple, weight)) in phase.iter_mut().zip(&PARTIALS) {
                *p = (*p + std::f32::consts::TAU * pitch * multiple / rate) % std::f32::consts::TAU;
                // Higher partials die faster, as on anything struck.
                sum += p.sin() * ramp(weight, weight * 0.05, t, len / multiple);
            }
            if let Some(o) = out.get_mut(start + i) {
                *o += sum * env;
            }
        }
    }
    out
}

/// Plays a cue at `volume` (0..2). Every cue goes through one limiter, so two landing together
/// (someone joins as someone else starts streaming) cannot clip.
pub fn cue(kind: Cue, volume: f32) {
    let samples = rendered(kind, volume);
    if !samples.is_empty() {
        cue_bus().play_cue(samples);
        audio().mixer.wake();
    }
}

/// A cue's samples at `volume`, rendered once: a ring repeats every couple of seconds, and each
/// render is some 10^5 sines and powers on the interface's thread. Only the latest volume is
/// kept, since it changes by a slider.
fn rendered(kind: Cue, volume: f32) -> Arc<[f32]> {
    /// The volume (its bits), and each cue rendered at it so far.
    type Rendered = (u32, Vec<(Cue, Arc<[f32]>)>);
    static CACHE: Mutex<Rendered> = Mutex::new((0, Vec::new()));
    let mut cache = CACHE.lock();
    if cache.0 != volume.to_bits() {
        *cache = (volume.to_bits(), Vec::new());
    }
    if let Some((_, s)) = cache.1.iter().find(|(k, _)| *k == kind) {
        return s.clone();
    }
    let s: Arc<[f32]> = render(kind, volume).into();
    cache.1.push((kind, s.clone()));
    s
}

fn cue_bus() -> &'static Arc<Source> {
    static BUS: OnceLock<Arc<Source>> = OnceLock::new();
    BUS.get_or_init(|| {
        let bus = Source::bus();
        audio().mixer.add(bus.clone());
        bus
    })
}

/// A compressor hard enough to call a limiter: above -6 dBFS, 12:1. The attack is instant, so
/// nothing gets past it; the release takes about 120 ms.
struct Limiter {
    envelope: f32,
    /// The release's fall per sample, worked out once rather than per sample on the callback.
    release: f32,
}

impl Default for Limiter {
    fn default() -> Limiter {
        Limiter { envelope: 0., release: (-1. / (0.12 * RATE as f32)).exp() }
    }
}

const LIMIT_AT: f32 = 0.5;
const LIMIT_RATIO: f32 = 12.;

impl Limiter {
    fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        let peak = l.abs().max(r.abs());
        self.envelope = if peak > self.envelope { peak } else { self.envelope * self.release };
        if self.envelope <= LIMIT_AT {
            return (l, r);
        }
        let g = LIMIT_AT * (self.envelope / LIMIT_AT).powf(1. / LIMIT_RATIO) / self.envelope;
        (l * g, r * g)
    }
}

/// Plays a clip (interleaved stereo at 48 kHz) once without copying it, so a clip can be shared
/// by every play of it.
pub fn play_shared(samples: Arc<[i16]>, gain: f32) {
    let s = Source {
        kind: SourceKind::Effect,
        feed: Feed::Clip(samples, AtomicUsize::new(0)),
        gain: AtomicF32::new(gain),
        peak: AtomicF32::default(),
        one_shot: true,
    };
    audio().mixer.add(Arc::new(s));
}

/// Decodes a sound file (mp3, wav, ogg/vorbis, flac) to 48 kHz stereo.
pub fn decode(bytes: Vec<u8>) -> anyhow::Result<Vec<f32>> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;
    let mss = MediaSourceStream::new(Box::new(std::io::Cursor::new(bytes)), Default::default());
    let probed = symphonia::default::get_probe().format(&Hint::new(), mss, &FormatOptions::default(), &MetadataOptions::default())?;
    let mut format = probed.format;
    let track = format.default_track().ok_or_else(|| anyhow::anyhow!("no audio in the file"))?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())?;
    let mut stereo = Vec::new();
    let mut rate = None;
    while let Ok(packet) = format.next_packet() {
        if packet.track_id() != track_id {
            continue;
        }
        let Ok(decoded) = decoder.decode(&packet) else { continue };
        let spec = *decoded.spec();
        let ch = spec.channels.count();
        if ch == 0 {
            continue;
        }
        rate.get_or_insert(spec.rate);
        let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buf.copy_interleaved_ref(decoded);
        stereo.extend(buf.samples().chunks_exact(ch).flat_map(|f| if ch == 1 { [f[0], f[0]] } else { [f[0], f[1]] }));
    }
    // Whole, so the filter's delay is trimmed and its tail kept.
    Ok(rate.map_or_else(Vec::new, |r| resample_clip(&stereo, r, RATE, 2)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_microphone_switch_nobody_chose_is_told() {
        let dev = |id: &str| DeviceInfo { id: id.into(), name: id.to_uppercase() };
        let told = || std::iter::from_fn(|| MIC_SWITCHES.1.try_recv().ok()).collect::<Vec<_>>();
        told();
        let mut u = Using::default();
        u.opened(Some(dev("headset")), true);
        u.opened(Some(dev("laptop")), true);
        assert!(told().is_empty(), "choosing one, or joining a call, is no news");
        u.opened(Some(dev("laptop")), false);
        assert!(told().is_empty(), "the same one opened again");
        u.opened(Some(dev("headset")), false);
        let t = told();
        assert_eq!((t[0].from.as_ref().unwrap().id.as_str(), t[0].to.as_ref().unwrap().id.as_str()), ("laptop", "headset"));
        u.opened(None, false);
        u.opened(None, false);
        let t = told();
        assert_eq!(t.len(), 1, "gone is told once, not on every retry");
        assert!(t[0].to.is_none());
        u.opened(Some(dev("headset")), false);
        assert_eq!(told().len(), 1, "back after none is news, even the same one");
    }

    #[test]
    fn resampling_keeps_length_in_proportion() {
        let mut r = Resampler::new(44_100, 48_000, 1);
        let mut out = Vec::new();
        for _ in 0..10 {
            r.process(&[0.5; 441], &mut out);
        }
        // Less what is held back short of a chunk.
        let held = (RESAMPLE_CHUNK as f64 * 48_000. / 44_100.).ceil() as i64;
        assert!((4800 - held - 2..=4802).contains(&(out.len() as i64)), "{}", out.len());
        // Past the filter's delay, a constant stays that constant.
        assert!(out.iter().skip(200).all(|&s| (s - 0.5).abs() < 1e-3), "{:?}", &out[200..210]);
        let mut same = Resampler::new(48_000, 48_000, 2);
        let mut out = Vec::new();
        same.process(&[0.1, 0.2, 0.3, 0.4], &mut out);
        assert_eq!(out, [0.1, 0.2, 0.3, 0.4], "the same rate passes through");
    }

    fn tone(freq: f32, rate: u32, frames: usize) -> Vec<f32> {
        (0..frames).map(|i| 0.5 * (i as f32 * freq * std::f32::consts::TAU / rate as f32).sin()).collect()
    }

    fn rms(s: &[f32]) -> f32 {
        (s.iter().map(|x| x * x).sum::<f32>() / s.len().max(1) as f32).sqrt()
    }

    /// 48 kHz to a hands-free headset's 16 kHz: a 12 kHz tone, above the 8 kHz the lower rate can
    /// hold, has to go rather than fold back to 4 kHz; 1 kHz passes at its level. Fed in uneven
    /// blocks, as a device callback hands them over.
    #[test]
    fn resampling_down_filters_out_what_the_lower_rate_cannot_hold() {
        let through = |freq: f32| {
            let mut r = Resampler::new(48_000, 16_000, 1);
            let mut out = Vec::new();
            for block in tone(freq, 48_000, 48_000).chunks(441) {
                r.process(block, &mut out);
            }
            rms(&out[1600..])
        };
        let (high, low) = (through(12_000.), through(1_000.));
        println!("16 kHz: 12 kHz comes out at {high:.6} rms, 1 kHz at {low:.4}");
        assert!(high < 0.5 / 2f32.sqrt() * 1e-3, "12 kHz aliased at {high} rms (under -60 dB wanted)");
        assert!((low / (0.5 / 2f32.sqrt()) - 1.).abs() < 0.02, "1 kHz came out at {low} rms");

        // And up: 8 kHz to 48 kHz puts nothing near 12 kHz's image (16 - 4 kHz).
        let mut r = Resampler::new(8_000, 48_000, 2);
        let stereo: Vec<f32> = tone(3_000., 8_000, 8_000).into_iter().flat_map(|s| [s, s]).collect();
        let mut out = Vec::new();
        r.process(&stereo, &mut out);
        assert!(out.len() / 2 >= 47_000);
        // 3 kHz is a whole number of samples at 48 kHz, so it is fitted exactly over whole periods.
        let left: Vec<f64> = out.chunks_exact(2).map(|f| f[0] as f64).skip(2000).take(40_000).collect();
        let phase = |i: usize| i as f64 * 3_000. * std::f64::consts::TAU / 48_000.;
        let (c, s) = left.iter().enumerate().fold((0., 0.), |(c, s), (i, &x)| (c + x * phase(i).cos(), s + x * phase(i).sin()));
        let (a, b) = (2. * c / left.len() as f64, 2. * s / left.len() as f64);
        let amp = (a * a + b * b).sqrt();
        // What is left once the 3 kHz is taken out: the images at 5, 11, 13 kHz and on.
        let rest: Vec<f32> = left.iter().enumerate().map(|(i, &x)| (x - a * phase(i).cos() - b * phase(i).sin()) as f32).collect();
        let residue = rms(&rest);
        println!("8 to 48 kHz: 3 kHz at {amp:.4}, everything else {residue:.6} rms");
        assert!((amp - 0.5).abs() < 0.02 && residue < 0.5e-3, "{amp} {residue}");
    }

    #[test]
    fn a_clip_is_resampled_whole() {
        let clip = tone(440., 44_100, 44_100);
        let out = resample_clip(&clip, 44_100, 48_000, 1);
        assert!((out.len() as i64 - 48_000).abs() <= 1, "{}", out.len());
        // No delay in front: it starts where the tone does, rising from zero.
        assert!(out[0].abs() < 0.02 && out[10] > 0.1, "{:?}", &out[..12]);
    }

    #[test]
    fn a_backlog_from_while_the_speakers_were_away_is_dropped() {
        let m = Mixer::new();
        let s = Source::new(SourceKind::Voice, 1., false);
        m.add(s.clone());
        s.push(&vec![0.1; RATE as usize], 1);
        let mut out = vec![0.; 2 * FRAME];
        m.mix(&mut out);
        assert!(out.iter().all(|&x| x == 0.), "the old second is not played");
        // What comes next plays once there is enough of it.
        s.push(&vec![0.3; frames(40)], 1);
        m.mix(&mut out);
        assert!((out[2 * FADE] - 0.3).abs() < 1e-6, "{}", out[2 * FADE]);
    }

    #[test]
    fn the_mix_applies_gains_and_deafening() {
        let m = Mixer::new();
        let voice = Source::new(SourceKind::Voice, 2., false);
        let fx = Source::new(SourceKind::Effect, 1., true);
        voice.push(&[0.1; frames(40)], 1);
        fx.push(&[0.2; FRAME], 1);
        m.add(voice.clone());
        m.add(fx);
        let mut out = vec![0.; 2 * FRAME];
        m.mix(&mut out);
        // Past the voice's fade-in.
        assert!((out[2 * FADE] - 0.4).abs() < 1e-6, "{}", out[2 * FADE]);
        m.deafened.store(true, Ordering::Relaxed);
        voice.push(&[0.1; FRAME], 1);
        m.mix(&mut out);
        assert!(out.iter().all(|&x| x == 0.));
        assert_eq!(m.playing.lock().sources.len(), 1, "the drained effect is gone");
        assert_eq!(m.retired.lock().slots(), 1, "and left to be freed off the callback");
        m.collect();
        m.remove(&voice);
        m.mix(&mut out);
        assert!(m.playing.lock().sources.is_empty());
        m.collect();
        assert_eq!(Arc::strong_count(&voice), 1, "the mixer let go of it");
    }

    #[test]
    fn sources_added_while_the_callback_is_busy_wait_for_the_next() {
        let m = Mixer::new();
        let fx = Source::new(SourceKind::Effect, 1., true);
        fx.push(&[0.25; FRAME], 1);
        let mut out = vec![0.; 2 * FRAME];
        m.add(fx);
        let held = m.changes.lock();
        m.mix(&mut out);
        assert!(out.iter().all(|&x| x == 0.), "the callback doesn't wait for the lock");
        drop(held);
        m.mix(&mut out);
        assert!((out[0] - 0.25).abs() < 1e-6);
    }

    #[test]
    fn a_stream_waits_for_its_target_before_it_plays_and_after_it_runs_dry() {
        let (mut input, mut p) = Playout::new(SourceKind::Voice);
        let mut out = vec![0.; 2 * FRAME];
        input.push_entire_slice(&[0.5; 2 * FRAME]).unwrap();
        p.pull(&mut out, 1., false);
        assert!(out.iter().all(|&x| x == 0.), "10 ms isn't enough to start on");
        input.push_entire_slice(&vec![0.5; 2 * frames(30)]).unwrap();
        p.pull(&mut out, 1., false);
        assert_eq!(out[0], 0., "it fades in");
        assert_eq!(out[2 * FRAME - 1], 0.5);
        // 30 ms left: three blocks, then dry, the last of it faded out.
        for _ in 0..3 {
            out.fill(0.);
            p.pull(&mut out, 1., false);
        }
        assert!(p.playing);
        out.fill(0.);
        p.pull(&mut out, 1., false);
        assert!(!p.playing && out.iter().all(|&x| x == 0.));
        input.push_entire_slice(&[0.5; 2 * FRAME]).unwrap();
        p.pull(&mut out, 1., false);
        assert!(out.iter().all(|&x| x == 0.), "it fills to target again first");
    }

    /// How a run of `Playout` went: the fill at each pull from 20 s on, in frames, and how often it
    /// ran dry after starting.
    struct Run {
        fills: Vec<usize>,
        dry: usize,
    }

    impl Run {
        fn mean_ms(&self) -> f64 {
            self.fills.iter().sum::<usize>() as f64 / self.fills.len() as f64 * 1000. / RATE as f64
        }
        fn max_ms(&self) -> f64 {
            *self.fills.iter().max().unwrap() as f64 * 1000. / RATE as f64
        }
    }

    /// A minute of a stream: the far end sends 10 ms every 10 ms of its own clock, each late by
    /// up to `jitter_ms`, and the speakers take `block` frames at a time on a clock `ppm` fast
    /// (slow below zero). `sound` is the sample at each frame.
    fn play(kind: SourceKind, ppm: f64, jitter_ms: f64, block: usize, sound: impl Fn(usize) -> f32) -> Run {
        let (mut input, mut p) = Playout::new(kind);
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut random = || {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        let period = block as f64 / RATE as f64 / (1. + ppm * 1e-6);
        let (mut sent, mut next_send, mut pulls) = (0usize, 0f64, 0usize);
        let mut out = vec![0.; 2 * block];
        let mut run = Run { fills: Vec::new(), dry: 0 };
        let mut started = false;
        while pulls as f64 * period < 60. {
            let pull_at = pulls as f64 * period;
            if next_send <= pull_at {
                let chunk: Vec<f32> = (sent..sent + FRAME).flat_map(|i| [sound(i), sound(i)]).collect();
                input.push_entire_slice(&chunk).unwrap();
                sent += FRAME;
                // In order, as one sender's are.
                next_send = (sent / FRAME) as f64 * 0.01 + random() * jitter_ms / 1000.;
                next_send = next_send.max(pull_at);
                continue;
            }
            if pull_at >= 20. {
                run.fills.push(p.queue.slots() / 2);
            }
            let was = p.playing;
            out.fill(0.);
            p.pull(&mut out, 1., false);
            started |= p.playing;
            if started && was && !p.playing {
                run.dry += 1;
            }
            pulls += 1;
        }
        run
    }

    /// Speech-like: a tone that stops for a breath now and then.
    fn speech(i: usize) -> f32 {
        if i % 24_000 < 16_000 { 0.3 * (i as f32 * 0.05).sin() } else { 0. }
    }

    fn music(i: usize) -> f32 {
        0.3 * (i as f32 * 0.05).sin() + 0.2 * (i as f32 * 0.013).sin()
    }

    #[test]
    fn a_voice_stays_near_its_target_however_the_clocks_drift() {
        let target = Pace::of(SourceKind::Voice).target as f64;
        for ppm in [-2000., -300., 0., 300., 2000.] {
            for block in [FRAME, 441, 1024] {
                let run = play(SourceKind::Voice, ppm, 8., block, speech);
                let target = target.max(block as f64 * 1000. / RATE as f64 + 20.);
                println!(
                    "voice, {ppm} ppm, {block}-frame blocks: fill {:.1} ms on average, {:.1} at most, dry {}",
                    run.mean_ms(),
                    run.max_ms(),
                    run.dry
                );
                assert!((run.mean_ms() - target).abs() < 15., "{ppm} ppm, {block}: {:.1} ms", run.mean_ms());
                assert!(run.max_ms() < target + 30., "{ppm} ppm, {block}: grew to {:.1} ms", run.max_ms());
                assert_eq!(run.dry, 0, "{ppm} ppm, {block}");
            }
        }
    }

    #[test]
    fn music_with_no_pauses_still_keeps_its_fill() {
        let target = Pace::of(SourceKind::Stream).target as f64;
        for ppm in [-1000., 0., 1000.] {
            let run = play(SourceKind::Stream, ppm, 8., FRAME, music);
            println!("screen sound, {ppm} ppm: fill {:.1} ms on average, {:.1} at most, dry {}", run.mean_ms(), run.max_ms(), run.dry);
            assert!((run.mean_ms() - target).abs() < 30., "{ppm} ppm: {:.1} ms", run.mean_ms());
            assert!(run.max_ms() < target + 50., "{ppm} ppm: grew to {:.1} ms", run.max_ms());
            assert_eq!(run.dry, 0, "{ppm} ppm");
        }
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0., |p, s| p.max(s.abs()))
    }

    /// 16-bit mono at 48 kHz, for listening to what a test rendered.
    fn write_wav(path: &std::path::Path, samples: &[f32]) {
        let data: Vec<u8> = samples.iter().flat_map(|s| ((s.clamp(-1., 1.) * 32767.) as i16).to_le_bytes()).collect();
        let mut w = Vec::new();
        w.extend_from_slice(b"RIFF");
        w.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        w.extend_from_slice(b"WAVEfmt ");
        for v in [16u32, 1 | 1 << 16, RATE, RATE * 2, 2 | 16 << 16] {
            w.extend_from_slice(&v.to_le_bytes());
        }
        w.extend_from_slice(b"data");
        w.extend_from_slice(&(data.len() as u32).to_le_bytes());
        w.extend_from_slice(&data);
        std::fs::write(path, w).unwrap();
    }

    /// Set `CIRANDA_CUE_WAVS` to a folder to get every cue, and the worst overlap, as WAV files.
    #[test]
    fn cues_are_loud_and_two_together_do_not_clip() {
        let wavs = std::env::var_os("CIRANDA_CUE_WAVS").map(std::path::PathBuf::from);
        use Cue::*;
        for kind in [Join, Leave, Connect, Disconnect, StreamStart, StreamStop, Mute, Unmute, Deafen, Undeafen, Mention] {
            let s = render(kind, 1.);
            let p = peak(&s);
            println!("{kind:?}: {:.0} ms, peak {p:.3}", s.len() as f32 / RATE as f32 * 1000.);
            assert!((0.2..0.5).contains(&p), "{kind:?} peaks at {p}");
            assert!(s.iter().all(|x| x.is_finite()));
            assert!(s.last().unwrap().abs() < 0.001, "{kind:?} ends in a click");
            if let Some(dir) = &wavs {
                write_wav(&dir.join(format!("{kind:?}.wav")), &s);
            }
        }
        assert!(render(Cue::Join, 0.).is_empty());
        assert!((peak(&render(Cue::Join, 2.)) / peak(&render(Cue::Join, 1.)) - 2.).abs() < 0.01);

        // The two longest at 200%, the second 10 ms after the first: past full scale summed.
        let (a, b) = (render(Cue::StreamStart, 2.), render(Cue::Connect, 2.));
        let lag = 480;
        let raw: Vec<f32> = (0..a.len().max(b.len() + lag))
            .map(|i| a.get(i).unwrap_or(&0.) + i.checked_sub(lag).and_then(|j| b.get(j)).unwrap_or(&0.))
            .collect();
        println!("overlap at 200%: raw peak {:.3}", peak(&raw));
        assert!(peak(&raw) > 1.);
        let m = Mixer::new();
        let bus = Source::bus();
        m.add(bus.clone());
        bus.play_cue(a.clone().into());
        let mut out = vec![0.; 2 * lag];
        m.mix(&mut out);
        let mut mixed: Vec<f32> = out.chunks_exact(2).map(|f| f[0]).collect();
        bus.play_cue(b.clone().into());
        let mut out = vec![0.; 2 * raw.len()];
        m.mix(&mut out);
        mixed.extend(out.chunks_exact(2).map(|f| f[0]));
        let limited = peak(&mixed);
        println!("overlap at 200%: through the limiter {limited:.3}");
        // Under the soft clip's knee too: the limiter did this, not the clip.
        assert!(limited < 0.9, "{limited}");
        assert!(bus.is_empty() && m.playing.lock().sources.len() == 1, "the bus stays for the next cue");
        assert!(Arc::ptr_eq(&rendered(Cue::Ring, 0.8), &rendered(Cue::Ring, 0.8)), "a repeated cue is rendered once");
        if let Some(dir) = &wavs {
            write_wav(&dir.join("overlap-raw.wav"), &raw);
            write_wav(&dir.join("overlap-limited.wav"), &mixed);
        }
    }

    /// `secs` of interleaved frames at 48 kHz, each channel's sample from `f(channel, frame)`.
    fn interleaved(channels: usize, secs: f32, f: impl Fn(usize, usize) -> f32) -> Vec<f32> {
        let frames = (secs * RATE as f32) as usize;
        (0..frames).flat_map(|i| (0..channels).map(move |c| (c, i))).map(|(c, i)| f(c, i)).collect()
    }

    /// Through a downmix in device-sized blocks, as callbacks hand them over.
    fn downmixed(channels: usize, input: &[f32]) -> Vec<f32> {
        let mut d = Downmix::new(channels, RATE);
        let mut out = Vec::new();
        for block in input.chunks(441 * channels) {
            d.process(block, &mut out);
        }
        out
    }

    #[test]
    fn a_microphone_on_one_input_keeps_its_level() {
        let voice = |i: usize| 0.3 * (i as f32 * 0.05).sin();
        // Mono passes as it is.
        assert_eq!(downmixed(1, &[0.1, 0.2, 0.3]), [0.1, 0.2, 0.3]);
        // The same on both channels: the average is the sound itself.
        let both = downmixed(2, &interleaved(2, 1., |_, i| voice(i)));
        assert!((rms(&both[24_000..]) - rms(&tone_of(voice, 24_000))).abs() < 1e-3);
        // Only on the right: half a second in, it is taken alone, not halved.
        let right = downmixed(2, &interleaved(2, 2., |c, i| if c == 1 { voice(i) } else { 0. }));
        let full = rms(&tone_of(voice, 48_000));
        assert!((rms(&right[48_000..]) / full - 1.).abs() < 0.01, "{}", rms(&right[48_000..]) / full);
        // Eight inputs, the voice on the sixth and a little hiss on the rest: the voice alone.
        let hiss = |c: usize, i: usize| 0.002 * (((i * 7919 + c * 104_729) % 1000) as f32 / 500. - 1.);
        let eight = downmixed(8, &interleaved(8, 2., |c, i| if c == 5 { voice(i) } else { hiss(c, i) }));
        assert!((rms(&eight[48_000..]) / full - 1.).abs() < 0.01, "{}", rms(&eight[48_000..]) / full);
    }

    fn tone_of(f: impl Fn(usize) -> f32, frames: usize) -> Vec<f32> {
        (0..frames).map(f).collect()
    }

    #[test]
    fn the_downmix_does_not_flap_between_close_channels() {
        // Two inputs a dB apart, swapping which is louder every quarter second: once on one, it
        // stays, since neither is ever 3 dB above the other.
        let mut d = Downmix::new(4, RATE);
        let mut out = Vec::new();
        let input = interleaved(4, 6., |c, i| {
            let swap = (i / 12_000) % 2 == 0;
            match (c, swap) {
                (0, true) | (1, false) => 0.30,
                (0, false) | (1, true) => 0.27,
                _ => 0.,
            }
        });
        let mut picks = Vec::new();
        for block in input.chunks(480 * 4) {
            d.process(block, &mut out);
            picks.push(d.alone);
        }
        picks.dedup();
        assert!(picks.len() <= 2, "{picks:?}");
        // And a change fades: no sample jumps by more than a fade step's worth.
        let mut d = Downmix::new(2, RATE);
        let mut out = Vec::new();
        d.process(&interleaved(2, 1., |c, _| if c == 0 { 0.5 } else { 0. }), &mut out);
        assert!(out.windows(2).all(|w| (w[1] - w[0]).abs() <= 0.5 * d.step + 1e-6));
        assert!((out.last().unwrap() - 0.5).abs() < 1e-6, "{}", out.last().unwrap());
    }

    #[test]
    fn the_mix_says_when_there_is_nothing_to_play() {
        let m = Mixer::new();
        let bus = Source::bus();
        m.add(bus.clone());
        let mut out = vec![0.; 2 * FRAME];
        m.mix(&mut out);
        m.mix(&mut out);
        assert_eq!(m.quiet.load(Ordering::Relaxed), 2 * FRAME, "an empty bus is quiet");
        bus.play_cue(vec![0.1; 2 * FRAME].into());
        assert!(m.pending(), "a cue waiting counts");
        m.mix(&mut out);
        assert_eq!(m.quiet.load(Ordering::Relaxed), 0);
        let voice = Source::new(SourceKind::Voice, 1., false);
        m.add(voice.clone());
        for _ in 0..4 {
            m.mix(&mut out);
        }
        assert_eq!(m.quiet.load(Ordering::Relaxed), 0, "someone in the call keeps the speakers, silent or not");
        m.remove(&voice);
        m.mix(&mut out);
        m.mix(&mut out);
        assert!(m.quiet.load(Ordering::Relaxed) > 0);
        assert!(!m.pending());
    }
}
