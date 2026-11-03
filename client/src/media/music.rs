//! Cirandinha on this computer: playing the track the server says is playing, from the place it
//! says it has got to.
//!
//! No audio comes over the call. The server fetches each track once (m4a, AAC) and keeps it as an
//! ordinary upload; everyone in the voice channel downloads it and plays their own copy. What is
//! shared is the state, and the one fact that keeps the copies together: the server's clock when
//! the track was at 0:00 (`started_at`), or where it stopped (`position_ms`) while paused.
//!
//! So nothing here decides anything. Pressing a control is a request; the server changes the
//! state and tells everybody, the one who pressed it too, and every computer follows the same
//! state by the same arithmetic. How long a push took to arrive is measured, not guessed: the
//! server's clock is asked now and then and half of the shortest recent round trip is the way a
//! push travels. If the sound is somewhere else than it ought to be (a late start, a stall), it is
//! started again from the right place.
//!
//! The sound is decoded on a thread of its own per start (`Run`), into a queue the speakers' mix
//! reads (`audio::Source::track`). A seek, a pause or another track is a new run, and the old one
//! is dropped. Every track is measured here (EBU R128) and brought to one loudness under the
//! listener's own volume, so the volume chosen is the volume of every track.

use super::audio::{self, RATE, Resampler, Source};
use crate::core::types::*;
use crate::core::{self, api::ApiError};
use crate::prefs::{Prefs, prefs};
use crate::session::Session;
use gpui::{App, Context, Entity, Subscription, Task};
use serde_json::json;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions};
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;

/// How far off the sound may be, in ms, before it is started again from the right place: less
/// than this is not heard as a difference, and starting again is.
pub const DRIFT_MS: f64 = 250.;

/// How often a playing track is checked against where it ought to be.
const CHECK: Duration = Duration::from_secs(1);

/// How often the server's clock is asked, once the first few have been.
const PROBE_EVERY: u32 = 15;

/// The round trips kept: the shortest of them is the one nearest the truth.
const RTT_SAMPLES: usize = 5;

/// Tracks after the current one that are fetched (and measured) before they are wanted.
const PREFETCH: usize = 2;

/// What a run's decoder keeps ready ahead of the speakers: a second of stereo at 48 kHz.
const QUEUE_SAMPLES: usize = RATE as usize * 2;

/// A run starts this far ahead of where the music is when it is made, for the decoder's first
/// packets and the speakers' first callback.
const LEAD_MS: f64 = 20.;

// The common loudness every track is brought to, and how far it may be moved to get there.
const TARGET_LUFS: f64 = -14.;
const MAX_CUT_DB: f64 = 15.;
const MAX_BOOST_DB: f64 = 9.;
/// A quiet track is only turned up as far as this below full scale at its loudest sample.
const HEADROOM_DB: f64 = 1.5;

// ---------------------------------------------------------------------------------------- files

/// A track's bytes for symphonia, shared rather than copied by every run.
struct Shared(Arc<Vec<u8>>);

impl AsRef<[u8]> for Shared {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

struct Opened {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track: u32,
    rate: u32,
}

fn open(bytes: Arc<Vec<u8>>) -> anyhow::Result<Opened> {
    let mss = MediaSourceStream::new(Box::new(std::io::Cursor::new(Shared(bytes))), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("m4a");
    let probed = symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions { enable_gapless: true, ..Default::default() },
        &MetadataOptions::default(),
    )?;
    let format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
        .ok_or_else(|| anyhow::anyhow!("no audio in the file"))?;
    let decoder = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())?;
    let rate = track.codec_params.sample_rate.unwrap_or(44_100);
    Ok(Opened { track: track.id, rate, format, decoder })
}

/// Each decoded packet as interleaved stereo at the file's own rate, until the file ends or
/// `each` says to stop.
fn decode_packets(o: &mut Opened, mut each: impl FnMut(&[f32], u32) -> bool) {
    let mut stereo = Vec::new();
    let mut buf: Option<SampleBuffer<f32>> = None;
    while let Ok(packet) = o.format.next_packet() {
        if packet.track_id() != o.track {
            continue;
        }
        let Ok(decoded) = o.decoder.decode(&packet) else { continue };
        let spec = *decoded.spec();
        let ch = spec.channels.count();
        if ch == 0 {
            continue;
        }
        if buf.as_ref().is_none_or(|b| b.capacity() < decoded.capacity() * ch) {
            buf = Some(SampleBuffer::new(decoded.capacity() as u64, spec));
        }
        let b = buf.as_mut().expect("made above");
        b.copy_interleaved_ref(decoded);
        stereo.clear();
        stereo.extend(b.samples().chunks_exact(ch).flat_map(|f| if ch == 1 { [f[0], f[0]] } else { [f[0], f[1]] }));
        if !each(&stereo, spec.rate) {
            return;
        }
    }
}

// ------------------------------------------------------------------------------------ loudness

/// How loud a track is: integrated loudness (EBU R128), and its loudest sample, in dB.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Loudness {
    pub lufs: f64,
    pub peak_db: f64,
}

/// One second-order section, in direct form I.
#[derive(Clone, Copy, Default)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    x: [f64; 2],
    y: [f64; 2],
}

impl Biquad {
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.b[1] * self.x[0] + self.b[2] * self.x[1] - self.a[0] * self.y[0] - self.a[1] * self.y[1];
        self.x = [x, self.x[0]];
        self.y = [y, self.y[0]];
        y
    }
}

/// BS.1770's K-weighting at any rate: a high shelf for the head, then a high pass (the
/// coefficients worked out from the analogue prototypes, as libebur128 does).
fn k_weighting(rate: f64) -> [Biquad; 2] {
    let shelf = {
        let (f0, g, q) = (1681.974450955533, 3.999843853973347, 0.7071752369554196);
        let k = (std::f64::consts::PI * f0 / rate).tan();
        let vh = 10f64.powf(g / 20.);
        let vb = vh.powf(0.4996667741545416);
        let a0 = 1. + k / q + k * k;
        Biquad {
            b: [(vh + vb * k / q + k * k) / a0, 2. * (k * k - vh) / a0, (vh - vb * k / q + k * k) / a0],
            a: [2. * (k * k - 1.) / a0, (1. - k / q + k * k) / a0],
            ..Default::default()
        }
    };
    let high_pass = {
        let (f0, q) = (38.13547087602444, 0.5003270373238773);
        let k = (std::f64::consts::PI * f0 / rate).tan();
        let a0 = 1. + k / q + k * k;
        Biquad { b: [1., -2., 1.], a: [2. * (k * k - 1.) / a0, (1. - k / q + k * k) / a0], ..Default::default() }
    };
    [shelf, high_pass]
}

/// Integrated loudness of interleaved stereo, gated as BS.1770-4 says: 400 ms blocks every 100
/// ms, those under -70 LUFS left out, then those more than 10 LU under the rest.
pub struct Meter {
    filters: [[Biquad; 2]; 2],
    /// Mean square of each 100 ms step, summed over both channels.
    steps: Vec<f64>,
    sum: f64,
    n: usize,
    step: usize,
    peak: f32,
}

impl Meter {
    pub fn new(rate: u32) -> Meter {
        let rate = rate.max(1) as f64;
        let k = k_weighting(rate);
        Meter { filters: [k, k], steps: Vec::new(), sum: 0., n: 0, step: (rate / 10.).round() as usize, peak: 0. }
    }

    pub fn push(&mut self, stereo: &[f32]) {
        for f in stereo.chunks_exact(2) {
            self.peak = self.peak.max(f[0].abs()).max(f[1].abs());
            let mut e = 0.;
            for (c, filters) in self.filters.iter_mut().enumerate() {
                let v = filters.iter_mut().fold(f[c] as f64, |v, s| s.run(v));
                e += v * v;
            }
            self.sum += e;
            self.n += 1;
            if self.n == self.step {
                self.steps.push(self.sum / self.n as f64);
                self.sum = 0.;
                self.n = 0;
            }
        }
    }

    pub fn result(&self) -> Option<Loudness> {
        let blocks: Vec<f64> = self.steps.windows(4).map(|w| w.iter().sum::<f64>() / 4.).collect();
        let lufs = |power: f64| -0.691 + 10. * power.log10();
        let mean = |b: &[f64]| b.iter().sum::<f64>() / b.len() as f64;
        let audible: Vec<f64> = blocks.into_iter().filter(|&p| p > 0. && lufs(p) > -70.).collect();
        if audible.is_empty() {
            return None;
        }
        let relative = lufs(mean(&audible)) - 10.;
        let gated: Vec<f64> = audible.into_iter().filter(|&p| lufs(p) > relative).collect();
        if gated.is_empty() {
            return None;
        }
        Some(Loudness { lufs: lufs(mean(&gated)), peak_db: 20. * (self.peak.max(1e-9) as f64).log10() })
    }
}

/// How loud a whole file is; None when it cannot be read or is silence.
pub fn measure(bytes: Arc<Vec<u8>>) -> Option<Loudness> {
    let mut o = open(bytes).ok()?;
    let mut meter = Meter::new(o.rate);
    decode_packets(&mut o, |stereo, _| {
        meter.push(stereo);
        true
    });
    meter.result()
}

/// The gain, in dB, that brings a track to the common level; 0 for one that was not measured. A
/// quiet track is only turned up as far as its loudest sample allows.
pub fn gain_db_for(loudness: Option<Loudness>) -> f64 {
    let Some(l) = loudness.filter(|l| l.lufs.is_finite()) else { return 0. };
    let mut gain = TARGET_LUFS - l.lufs;
    if gain > 0. && l.peak_db.is_finite() {
        gain = gain.min((-HEADROOM_DB - l.peak_db).max(0.));
    }
    gain.clamp(-MAX_CUT_DB, MAX_BOOST_DB)
}

// ------------------------------------------------------------------------------------------ runs

/// A track playing from one place: its decoder thread and its source in the speakers' mix. Dropped,
/// both go.
pub struct Run {
    /// The track's id in the queue and its file: the same track from another place is a new run.
    key: (String, String),
    source: Arc<Source>,
    stop: Arc<AtomicBool>,
    from_ms: f64,
}

impl Run {
    fn start(key: (String, String), bytes: Arc<Vec<u8>>, from_ms: f64, gain: f32) -> Run {
        let (source, mut queue) = Source::track(gain, QUEUE_SAMPLES);
        let stop = Arc::new(AtomicBool::new(false));
        let (s, halt) = (source.clone(), stop.clone());
        let from = from_ms.max(0.);
        let spawned = std::thread::Builder::new().name("ciranda-music".into()).spawn(move || {
            if let Err(e) = decode_run(bytes, from, &mut queue, &halt) {
                log::info!("music: the track did not play: {e}");
            }
            s.end_track();
        });
        if let Err(e) = spawned {
            log::warn!("music: no thread for the track: {e}");
            source.end_track();
        }
        audio::audio().mixer.add(source.clone());
        Run { key, source, stop, from_ms: from }
    }

    /// Where in the track the sound that has gone out is.
    fn position_ms(&self) -> f64 {
        self.from_ms + self.source.played() as f64 * 1000. / RATE as f64
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        audio::audio().mixer.remove(&self.source);
    }
}

/// A run's thread: from `from_ms` to the end of the file, into the speakers' queue at 48 kHz.
fn decode_run(bytes: Arc<Vec<u8>>, from_ms: f64, queue: &mut rtrb::Producer<f32>, stop: &AtomicBool) -> anyhow::Result<()> {
    let mut o = open(bytes)?;
    // Frames to throw away after the seek: it lands on the packet before the place asked for.
    let mut skip = 0usize;
    if from_ms > 0. {
        let to = o.format.seek(SeekMode::Accurate, SeekTo::Time { time: Time::from(from_ms / 1000.), track_id: Some(o.track) })?;
        let tb = o.format.tracks().iter().find(|t| t.id == o.track).and_then(|t| t.codec_params.time_base);
        let late = to.required_ts.saturating_sub(to.actual_ts);
        skip = match tb {
            Some(tb) => {
                let t = tb.calc_time(late);
                ((t.seconds as f64 + t.frac) * o.rate as f64).round() as usize
            }
            None => late as usize,
        };
        o.decoder.reset();
    }
    let mut resampler: Option<(u32, Resampler)> = None;
    let mut out = Vec::new();
    decode_packets(&mut o, |stereo, rate| {
        let mut stereo = stereo;
        if skip > 0 {
            let k = skip.min(stereo.len() / 2);
            skip -= k;
            stereo = &stereo[k * 2..];
        }
        if resampler.as_ref().is_none_or(|(r, _)| *r != rate) {
            resampler = Some((rate, Resampler::new(rate, RATE, 2)));
        }
        out.clear();
        resampler.as_mut().expect("made above").1.process(stereo, &mut out);
        push(queue, &out, stop)
    });
    Ok(())
}

/// Puts samples in the speakers' queue as room comes; false once the run is stopped.
fn push(queue: &mut rtrb::Producer<f32>, samples: &[f32], stop: &AtomicBool) -> bool {
    let mut at = 0;
    while at < samples.len() {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        let n = queue.slots().min(samples.len() - at) & !1;
        if n == 0 {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }
        if let Ok(chunk) = queue.write_chunk_uninit(n) {
            chunk.fill_from_iter(samples[at..at + n].iter().copied());
        }
        at += n;
    }
    !stop.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------------------- player

/// A track's file, fetched and measured.
struct Ready {
    bytes: Arc<Vec<u8>>,
    /// The loudness correction, as a multiplier.
    level: f32,
}

/// The music of the voice channel you are in, as this computer plays it. Made with the call and
/// gone with it.
pub struct MusicPlayer {
    session: Entity<Session>,
    channel: ChannelId,
    rtts: VecDeque<f64>,
    /// How long a push takes to arrive, in ms.
    latency_ms: f64,
    run: Option<Run>,
    files: HashMap<String, Arc<Ready>>,
    loading: HashSet<String>,
    /// Files that could not be had; not asked for again while this call lasts.
    failed: HashSet<String>,
    /// The first few clock probes have gone out.
    probed: bool,
    _tick: Task<()>,
    _subs: Vec<Subscription>,
}

impl MusicPlayer {
    pub fn new(session: Entity<Session>, channel: ChannelId, cx: &mut Context<Self>) -> MusicPlayer {
        let subs = vec![cx.observe(&session, |this, _, cx| this.sync(cx)), cx.observe_global::<Prefs>(|this, cx| this.apply_gain(cx))];
        let tick = cx.spawn(async move |this, cx| {
            let mut n = 0u32;
            loop {
                cx.background_executor().timer(CHECK).await;
                n += 1;
                let Ok(()) = this.update(cx, |p, cx| {
                    p.sync(cx);
                    if n.is_multiple_of(PROBE_EVERY) {
                        p.probe(cx);
                    }
                }) else {
                    return;
                };
            }
        });
        let mut p = MusicPlayer {
            session,
            channel,
            rtts: VecDeque::new(),
            latency_ms: 0.,
            run: None,
            files: HashMap::new(),
            loading: HashSet::new(),
            failed: HashSet::new(),
            probed: false,
            _tick: tick,
            _subs: subs,
        };
        p.sync(cx);
        p
    }

    fn live<'a>(&self, cx: &'a App) -> Option<&'a crate::session::MusicLive> {
        self.session.read(cx).music.as_ref().filter(|m| m.channel == self.channel)
    }

    /// Where in the current track the music ought to be now, in ms.
    pub fn position_ms(&self, cx: &App) -> f64 {
        let Some(live) = self.live(cx) else { return 0. };
        match live.state.status {
            MusicStatus::Playing => {
                let server_now = live.server_now + self.latency_ms + live.at.elapsed().as_secs_f64() * 1000.;
                (server_now - live.state.started_at).max(0.)
            }
            MusicStatus::Paused => live.state.position_ms,
            _ => 0.,
        }
    }

    /// The current track's file is still on its way here (or being measured).
    pub fn fetching(&self, cx: &App) -> bool {
        self.live(cx).and_then(|l| l.state.current_track()).and_then(|t| t.hash.as_ref()).is_some_and(|h| self.loading.contains(h))
    }

    fn volume(cx: &App) -> f32 {
        let p = prefs(cx);
        if p.music_muted { 0. } else { (p.music_volume as f32 / 100.).clamp(0., audio::MAX_GAIN) }
    }

    fn apply_gain(&mut self, cx: &mut Context<Self>) {
        if let Some(run) = &self.run
            && let Some(ready) = self.files.get(&run.key.1)
        {
            run.source.gain.set(Self::volume(cx) * ready.level);
        }
    }

    /// Follows the server's state: fetches what is coming, and plays what is current from where it
    /// ought to be.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let Some(live) = self.live(cx) else {
            self.run = None;
            self.files.clear();
            return;
        };
        let state = live.state.clone();
        if !self.probed {
            self.probed = true;
            // A few at once, so the first estimate is already the best of a few.
            for wait in [0, 350, 700] {
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(Duration::from_millis(wait)).await;
                    let _ = this.update(cx, |p, cx| p.probe(cx));
                })
                .detach();
            }
        }

        // The current track and the next few: fetched, and nothing else kept.
        let at = state.current.as_ref().and_then(|c| state.queue.iter().position(|t| &t.id == c));
        let wanted: Vec<String> = state
            .queue
            .iter()
            .skip(at.unwrap_or(0))
            .take(PREFETCH + 1)
            .filter(|t| t.error.is_none())
            .filter_map(|t| t.hash.clone())
            .collect();
        self.files.retain(|h, _| wanted.contains(h));
        for hash in &wanted {
            if !self.files.contains_key(hash) && !self.loading.contains(hash) && !self.failed.contains(hash) {
                self.load(hash.clone(), cx);
            }
        }

        let track = state.current_track().cloned();
        let (Some(track), MusicStatus::Playing) = (track, state.status) else {
            self.run = None;
            return;
        };
        let Some(hash) = track.hash.clone() else {
            self.run = None;
            return;
        };
        let want = self.position_ms(cx);
        if track.duration_ms.is_some_and(|d| want >= d as f64) {
            self.run = None;
            return;
        }
        let Some(ready) = self.files.get(&hash).cloned() else {
            self.run = None;
            return;
        };
        let key = (track.id.clone(), hash);
        let gain = Self::volume(cx) * ready.level;
        match &self.run {
            Some(run) if run.key == key && (run.position_ms() - want).abs() <= DRIFT_MS => run.source.gain.set(gain),
            _ => {
                if let Some(old) = &self.run
                    && old.key == key
                {
                    log::info!("music: {:.0} ms off, starting again at {:.0} ms", old.position_ms() - want, want);
                }
                // The old one goes first, so the two never sound together.
                self.run = None;
                self.run = Some(Run::start(key, ready.bytes.clone(), want + LEAD_MS, gain));
            }
        }
    }

    /// Fetches a track's file (from the disk cache when it is there) and measures it.
    fn load(&mut self, hash: String, cx: &mut Context<Self>) {
        self.loading.insert(hash.clone());
        let (api, cache) = {
            let s = self.session.read(cx);
            (s.api.clone(), s.cache.clone())
        };
        let h = hash.clone();
        cx.spawn(async move |this, cx| {
            let got = core::run(async move {
                let (bytes, _) = cache.fetch(&h, || api.download_track(&h)).await?;
                let measuring = bytes.clone();
                let loudness = tokio::task::spawn_blocking(move || measure(measuring)).await.ok().flatten();
                Ok::<_, ApiError>((bytes, loudness))
            })
            .await;
            let _ = this.update(cx, |p, cx| {
                p.loading.remove(&hash);
                match got {
                    Ok((bytes, loudness)) => {
                        let db = gain_db_for(loudness);
                        log::info!("music: {hash} ready, {:?} -> {db:+.1} dB", loudness.map(|l| l.lufs));
                        p.files.insert(hash, Arc::new(Ready { bytes, level: 10f32.powf(db as f32 / 20.) }));
                    }
                    Err(e) => {
                        log::info!("music: {hash} could not be fetched: {}", e.message);
                        p.failed.insert(hash);
                    }
                }
                p.sync(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Times a round trip to the server's clock.
    fn probe(&mut self, cx: &mut Context<Self>) {
        if self.live(cx).is_none() {
            return;
        }
        let rt = self.session.read(cx).realtime.clone();
        let sent = Instant::now();
        let reply = Session::request(rt, "music:clock", json!({ "channelId": self.channel }));
        cx.spawn(async move |this, cx| {
            if reply.await.is_ok() {
                let rtt = sent.elapsed().as_secs_f64() * 1000.;
                let _ = this.update(cx, |p, _| {
                    p.rtts.push_back(rtt);
                    while p.rtts.len() > RTT_SAMPLES {
                        p.rtts.pop_front();
                    }
                    p.latency_ms = p.rtts.iter().copied().fold(f64::INFINITY, f64::min) / 2.;
                });
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sine at `rate`, stereo, `secs` long, at `amp` of full scale.
    fn sine(rate: u32, freq: f64, amp: f64, secs: f64) -> Vec<f32> {
        let n = (rate as f64 * secs) as usize;
        (0..n)
            .flat_map(|i| {
                let v = (amp * (2. * std::f64::consts::PI * freq * i as f64 / rate as f64).sin()) as f32;
                [v, v]
            })
            .collect()
    }

    #[test]
    fn a_full_scale_1k_sine_reads_as_the_standard_says() {
        // BS.1770: a 0 dBFS 1 kHz sine in one channel reads -3.01 LUFS; in both, +3 dB on that.
        for rate in [44_100, 48_000] {
            let mut m = Meter::new(rate);
            m.push(&sine(rate, 1000., 1., 5.));
            let l = m.result().unwrap();
            assert!((l.lufs - 0.).abs() < 0.1, "{rate}: {}", l.lufs);
            assert!(l.peak_db.abs() < 0.01);
        }
    }

    #[test]
    fn ten_db_quieter_reads_ten_lu_lower() {
        let mut loud = Meter::new(48_000);
        loud.push(&sine(48_000, 440., 0.5, 4.));
        let mut quiet = Meter::new(48_000);
        quiet.push(&sine(48_000, 440., 0.5 / 10f64.powf(0.5), 4.));
        let d = loud.result().unwrap().lufs - quiet.result().unwrap().lufs;
        assert!((d - 10.).abs() < 0.05, "{d}");
    }

    #[test]
    fn silence_has_no_loudness() {
        let mut m = Meter::new(48_000);
        m.push(&vec![0.; 48_000 * 2 * 3]);
        assert!(m.result().is_none());
        assert_eq!(gain_db_for(None), 0.);
    }

    /// 3 s of a 1 kHz tone, AAC at 44.1 kHz: as a plain MP4, and fragmented as YouTube's DASH
    /// m4a are. ffmpeg's tone is at 1/8 of full scale, turned down 20 dB, and the upmix to stereo
    /// takes 3 dB more: its own meter (`-af ebur128`) reads -41.1 LUFS.
    const TONES: [&[u8]; 2] = [include_bytes!("testdata/tone.m4a"), include_bytes!("testdata/tone-dash.m4a")];

    #[test]
    fn an_m4a_is_measured_where_its_level_is() {
        for bytes in TONES {
            let l = measure(Arc::new(bytes.to_vec())).unwrap();
            assert!((l.lufs + 41.1).abs() < 0.3, "{}", l.lufs);
            // AAC's own overshoot moves a peak by a little.
            assert!((l.peak_db + 41.1).abs() < 1., "{}", l.peak_db);
        }
    }

    #[test]
    fn a_run_starts_where_it_is_asked_to_and_plays_to_the_end() {
        for bytes in TONES {
            for from_ms in [0., 1000., 2500.] {
                let (source, mut queue) = Source::track(1., 16 * RATE as usize);
                let stop = AtomicBool::new(false);
                decode_run(Arc::new(bytes.to_vec()), from_ms, &mut queue, &stop).unwrap();
                drop(source);
                let frames = (16 * RATE as usize - queue.slots()) as f64 / 2.;
                let expected = (3000. - from_ms) / 1000. * RATE as f64;
                // Within a resampler chunk and AAC's frame either way.
                assert!((frames - expected).abs() < 2048., "{from_ms}: {frames} vs {expected}");
            }
        }
    }

    #[test]
    fn the_gain_brings_tracks_to_one_level_within_limits() {
        assert_eq!(gain_db_for(Some(Loudness { lufs: -8., peak_db: 0. })), -6.);
        // Turned up only as far as the peak allows.
        assert!((gain_db_for(Some(Loudness { lufs: -20., peak_db: -4. })) - 2.5).abs() < 1e-9);
        assert_eq!(gain_db_for(Some(Loudness { lufs: -40., peak_db: -30. })), MAX_BOOST_DB);
        assert_eq!(gain_db_for(Some(Loudness { lufs: 10., peak_db: 0. })), -MAX_CUT_DB);
    }
}
