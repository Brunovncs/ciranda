//! Replay: while you share your screen (unless it is turned off in Settings › In game), the last
//! seconds of the share are kept, and a hotkey saves them as a video in Videos\Ciranda.
//!
//! The frames and sound sent also go to a second H.264 encoder, the graphics card's through Media
//! Foundation's sink writer (Microsoft's software one where there is none), which writes MP4
//! segments of a few seconds to a temporary folder. The oldest are deleted as new ones come, so
//! what is kept costs a few megabytes of disk and no memory to speak of. Saving closes the segment
//! being written and joins the newest into one MP4 without encoding again, cut at a key frame.
//! The bitrate is chosen so that a replay fits in the chat's 25 MB.

use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long each segment is, and so how much longer than asked a replay may be when the encoder
/// puts no key frame where it is cut.
const SEGMENT: Duration = Duration::from_secs(4);
/// Frames waiting for the encoder beyond which new ones are dropped rather than queued.
const BACKLOG: usize = 8;
/// What a replay is sized to fit: the chat takes files up to 25 MB.
const FIT_BYTES: u64 = 22 << 20;
const MAX_FPS: u32 = 60;
/// Taller frames are scaled down to this before they are kept.
pub const MAX_HEIGHT: u32 = 1080;

#[derive(Clone, Copy, PartialEq, Eq)]
struct Config {
    enabled: bool,
    seconds: u32,
}

struct Recorder {
    tx: mpsc::Sender<Msg>,
    backlog: Arc<AtomicUsize>,
    last_frame: Option<Instant>,
}

struct State {
    config: Config,
    sharing: bool,
    recorder: Option<Recorder>,
}

static STATE: Mutex<State> = Mutex::new(State { config: Config { enabled: false, seconds: 30 }, sharing: false, recorder: None });

enum Msg {
    Video { w: u32, h: u32, fps: u32, nv12: Vec<u8>, at: Instant },
    Audio { samples: Vec<i16>, at: Instant },
    Save(futures::channel::oneshot::Sender<Result<PathBuf, String>>),
}

/// Follows the settings: on or off, and how long a replay is.
pub fn configure(enabled: bool, seconds: u32) {
    let mut s = STATE.lock();
    let config = Config { enabled, seconds: seconds.clamp(5, 120) };
    if s.config != config {
        s.config = config;
        s.recorder = None;
        restart(&mut s);
    }
}

/// The share started or stopped: recording follows it.
pub fn sharing(on: bool) {
    let mut s = STATE.lock();
    s.sharing = on;
    if !on {
        s.recorder = None;
    }
    restart(&mut s);
}

fn restart(s: &mut State) {
    if s.recorder.is_some() || !s.sharing || !s.config.enabled || !cfg!(windows) {
        return;
    }
    let (tx, rx) = mpsc::channel();
    let backlog = Arc::new(AtomicUsize::new(0));
    let (b, seconds) = (backlog.clone(), s.config.seconds);
    let started = std::thread::Builder::new().name("ciranda-replay".into()).spawn(move || {
        #[cfg(windows)]
        imp::run(rx, b, seconds);
        #[cfg(not(windows))]
        let _ = (rx, b, seconds);
    });
    if started.is_ok() {
        s.recorder = Some(Recorder { tx, backlog, last_frame: None });
    }
}

/// Whether frames are being kept now.
pub fn recording() -> bool {
    STATE.lock().recorder.is_some()
}

/// A frame of the share, `w` × `h` NV12 written by `fill` into a buffer of `w * h * 3 / 2`
/// bytes, rows packed. Skipped when nothing records, the encoder is behind, or it comes sooner
/// than 60 a second.
pub fn video(w: u32, h: u32, fps: u32, fill: impl FnOnce(&mut [u8])) {
    let mut s = STATE.lock();
    let Some(r) = s.recorder.as_mut() else { return };
    let now = Instant::now();
    let min_gap = Duration::from_secs_f64(0.9 / MAX_FPS as f64);
    if r.last_frame.is_some_and(|t| now.duration_since(t) < min_gap) || r.backlog.load(Ordering::Relaxed) >= BACKLOG {
        return;
    }
    r.last_frame = Some(now);
    let mut nv12 = vec![0u8; (w * h * 3 / 2) as usize];
    fill(&mut nv12);
    r.backlog.fetch_add(1, Ordering::Relaxed);
    if r.tx.send(Msg::Video { w, h, fps: fps.clamp(1, MAX_FPS), nv12, at: now }).is_err() {
        s.recorder = None;
    }
}

/// The share's sound: interleaved stereo i16 at 48 kHz.
pub fn audio(samples: &[i16]) {
    let s = STATE.lock();
    if let Some(r) = &s.recorder {
        let _ = r.tx.send(Msg::Audio { samples: samples.to_vec(), at: Instant::now() });
    }
}

/// Saves the last seconds kept, and says where.
pub fn save() -> impl Future<Output = Result<PathBuf, String>> {
    let (tx, rx) = futures::channel::oneshot::channel();
    let sent = STATE.lock().recorder.as_ref().is_some_and(|r| r.tx.send(Msg::Save(tx)).is_ok());
    async move {
        if !sent {
            return Err(
                tr!("Replay only records while you share your screen.", "O clipe só grava enquanto você compartilha a tela.").into()
            );
        }
        rx.await.unwrap_or_else(|_| Err("the replay recorder stopped".into()))
    }
}

/// The bitrate that fits `seconds` of video and sound in the chat's limit.
fn bitrate(seconds: u32) -> u32 {
    let total = FIT_BYTES * 8 / seconds.max(1) as u64;
    (total.saturating_sub(AUDIO_BITRATE as u64)).clamp(1_500_000, 12_000_000) as u32
}

const AUDIO_BITRATE: u32 = 160_000;

/// Where a replay saved now goes: Videos\Ciranda, named by the time.
fn output_path() -> PathBuf {
    let dir = dirs::video_dir().or_else(dirs::home_dir).unwrap_or_else(std::env::temp_dir).join("Ciranda");
    let _ = std::fs::create_dir_all(&dir);
    dir.join(format!("Ciranda {}.mp4", chrono::Local::now().format("%Y-%m-%d %H.%M.%S")))
}

/// A finished segment. Its file goes when the last holder lets go: a save may still be joining
/// it after the recorder has moved on.
struct Part {
    path: PathBuf,
    start: Instant,
    end: Instant,
    size: (u32, u32),
}

impl Drop for Part {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The parts a replay of `seconds` ending at the newest is made of: the newest run of one size,
/// back to the first that ends after the cut.
fn chosen(parts: &[Arc<Part>], seconds: u32) -> Vec<Arc<Part>> {
    let Some(last) = parts.last() else { return Vec::new() };
    let cut = last.end.checked_sub(Duration::from_secs(seconds as u64));
    let mut out: Vec<Arc<Part>> =
        parts.iter().rev().take_while(|p| p.size == last.size && cut.is_none_or(|c| p.end > c)).cloned().collect();
    out.reverse();
    out
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::collections::{HashSet, VecDeque};
    use std::path::Path;
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
    use windows::core::{GUID, HSTRING, Result};

    const HNS: i64 = 10_000_000;

    fn hns(d: Duration) -> i64 {
        (d.as_nanos() / 100) as i64
    }

    pub fn run(rx: mpsc::Receiver<Msg>, backlog: Arc<AtomicUsize>, seconds: u32) {
        let root = std::env::temp_dir().join("ciranda-replay");
        sweep(&root);
        let dir = root.join(format!("{}-{}", std::process::id(), rand_tag()));
        if let Err(e) = std::fs::create_dir_all(&dir) {
            log::warn!("replay: no folder for segments: {e}");
            return;
        }
        // SAFETY: paired with CoUninitialize and MFShutdown below, on this thread.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_FULL) {
                log::warn!("replay: Media Foundation is unavailable: {e}");
                CoUninitialize();
                return;
            }
        }
        log::info!("replay: recording, {seconds} s at {} kbit/s", bitrate(seconds) / 1000);
        let mut current: Option<Segment> = None;
        let mut parts: VecDeque<Arc<Part>> = VecDeque::new();
        let mut saves = Vec::new();
        let mut heard: Option<Instant> = None;
        let mut n = 0u32;
        while let Ok(msg) = rx.recv() {
            match msg {
                Msg::Video { w, h, fps, nv12, at } => {
                    backlog.fetch_sub(1, Ordering::Relaxed);
                    let due = current.as_ref().is_none_or(|s| s.size != (w, h) || at.duration_since(s.start) >= SEGMENT);
                    if due {
                        close(current.take(), &mut parts);
                        let keep = seconds as u64 + 2 * SEGMENT.as_secs();
                        while parts.len() > 1
                            && parts.back().zip(parts.get(1)).is_some_and(|(b, f)| b.end.duration_since(f.start).as_secs() >= keep)
                        {
                            parts.pop_front();
                        }
                        n += 1;
                        let sound = heard.is_some_and(|t| at.saturating_duration_since(t) < Duration::from_secs(1));
                        match Segment::open(&dir.join(format!("{n}.mp4")), (w, h), fps, bitrate(seconds), sound, at) {
                            Ok(s) => current = Some(s),
                            Err(e) => log::warn!("replay: can't start a segment: {e}"),
                        }
                    }
                    if let Some(s) = current.as_mut()
                        && let Err(e) = s.video(&nv12, at)
                    {
                        log::warn!("replay: frame refused: {e}");
                        let _ = std::fs::remove_file(current.take().map(|s| s.path).unwrap_or_default());
                    }
                }
                Msg::Audio { samples, at } => {
                    heard = Some(at);
                    if let Some(s) = current.as_mut() {
                        s.audio(&samples, at);
                    }
                }
                Msg::Save(reply) => {
                    close(current.take(), &mut parts);
                    let chosen = chosen(parts.make_contiguous(), seconds);
                    saves.push(std::thread::spawn(move || {
                        let out = output_path();
                        let result = join(&chosen, seconds, &out).map(|()| out.clone()).inspect_err(|_| {
                            let _ = std::fs::remove_file(&out);
                        });
                        let _ = reply.send(result);
                    }));
                }
            }
        }
        // The share ended: what was kept goes, once any save has read it.
        drop(current);
        parts.clear();
        for s in saves {
            let _ = s.join();
        }
        let _ = std::fs::remove_dir_all(&dir);
        // SAFETY: matches the startup above.
        unsafe {
            let _ = MFShutdown();
            CoUninitialize();
        }
        log::info!("replay: stopped");
    }

    /// Segments left by a recorder that never got to clean up (Ciranda quit or crashed while
    /// sharing): a live one writes every few seconds, so anything quiet for a minute is not.
    fn sweep(root: &Path) {
        let Ok(entries) = std::fs::read_dir(root) else { return };
        for e in entries.flatten() {
            let quiet =
                e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|d| d > Duration::from_secs(60));
            if quiet {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }

    fn rand_tag() -> u32 {
        let mut b = [0u8; 4];
        let _ = getrandom::getrandom(&mut b);
        u32::from_le_bytes(b)
    }

    fn close(segment: Option<Segment>, parts: &mut VecDeque<Arc<Part>>) {
        let Some(s) = segment else { return };
        let path = s.path.clone();
        match s.finish() {
            Ok(Some(p)) => parts.push_back(Arc::new(p)),
            Ok(None) => {
                let _ = std::fs::remove_file(&path);
            }
            Err(e) => {
                log::warn!("replay: segment not closed: {e}");
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    /// One MP4 being written.
    struct Segment {
        writer: IMFSinkWriter,
        path: PathBuf,
        start: Instant,
        size: (u32, u32),
        fps: u32,
        sound: Option<u32>,
        /// Where the next sound goes, in 100 ns from the start; none until some came.
        sound_at: Option<i64>,
        last_video: Option<(i64, Instant)>,
    }

    impl Segment {
        fn open(path: &Path, size: (u32, u32), fps: u32, bitrate: u32, sound: bool, start: Instant) -> Result<Segment> {
            // The graphics card's encoder first; where it won't take this, Microsoft's own.
            let writer = unsafe { writer(path, size, fps, bitrate, sound, true) }.or_else(|e| {
                log::info!("replay: no hardware encoder ({e}); using software");
                unsafe { writer(path, size, fps, bitrate, sound, false) }
            })?;
            Ok(Segment { writer, path: path.to_path_buf(), start, size, fps, sound: sound.then_some(1), sound_at: None, last_video: None })
        }

        fn video(&mut self, nv12: &[u8], at: Instant) -> Result<()> {
            let mut ts = hns(at.duration_since(self.start));
            if let Some((last, _)) = self.last_video {
                ts = ts.max(last + 1);
            }
            let sample = unsafe { sample(nv12, ts, HNS / self.fps as i64)? };
            unsafe { self.writer.WriteSample(0, &sample)? };
            self.last_video = Some((ts, at));
            Ok(())
        }

        fn audio(&mut self, samples: &[i16], at: Instant) {
            let (Some(stream), Some(_)) = (self.sound, self.last_video) else { return };
            let frames = (samples.len() / 2) as i64;
            let dur = frames * HNS / 48_000;
            let wall = (hns(at.saturating_duration_since(self.start)) - dur).max(0);
            // Sound runs on its own clock; it follows the samples, and starts over from the
            // wall clock only after a gap (the share paused, or the system dropped some).
            let ts = match self.sound_at {
                Some(t) if (t - wall).abs() < HNS / 5 => t,
                _ => wall,
            };
            let bytes: &[u8] = unsafe { std::slice::from_raw_parts(samples.as_ptr().cast(), samples.len() * 2) };
            let written = unsafe { sample(bytes, ts, dur).and_then(|s| self.writer.WriteSample(stream, &s)) };
            match written {
                Ok(()) => self.sound_at = Some(ts + dur),
                Err(e) => {
                    log::warn!("replay: sound refused: {e}");
                    self.sound = None;
                }
            }
        }

        /// Closes the file; `None` when it holds no picture.
        fn finish(self) -> Result<Option<Part>> {
            let Some((_, end)) = self.last_video else { return Ok(None) };
            unsafe { self.writer.Finalize()? };
            Ok(Some(Part { path: self.path.clone(), start: self.start, end, size: self.size }))
        }
    }

    unsafe fn attributes(pairs: &[(GUID, u32)], container: bool) -> Result<IMFAttributes> {
        let mut a = None;
        unsafe { MFCreateAttributes(&mut a, 4)? };
        let a = a.ok_or_else(windows::core::Error::empty)?;
        for (k, v) in pairs {
            unsafe { a.SetUINT32(k, *v)? };
        }
        if container {
            unsafe { a.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)? };
        }
        Ok(a)
    }

    unsafe fn video_type(subtype: &GUID, (w, h): (u32, u32), fps: u32) -> Result<IMFMediaType> {
        let t = unsafe { MFCreateMediaType()? };
        unsafe {
            t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
            t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            t.SetUINT64(&MF_MT_FRAME_SIZE, ((w as u64) << 32) | h as u64)?;
            t.SetUINT64(&MF_MT_FRAME_RATE, ((fps as u64) << 32) | 1)?;
            t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1 << 32) | 1)?;
        }
        Ok(t)
    }

    unsafe fn audio_type(subtype: &GUID) -> Result<IMFMediaType> {
        let t = unsafe { MFCreateMediaType()? };
        unsafe {
            t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
            t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, 48_000)?;
            t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
        }
        Ok(t)
    }

    /// A sink writer encoding NV12 to H.264 (and 16-bit stereo to AAC, with `sound`) in `path`.
    unsafe fn writer(path: &Path, size: (u32, u32), fps: u32, bitrate: u32, sound: bool, hardware: bool) -> Result<IMFSinkWriter> {
        unsafe {
            let attrs = attributes(&[(MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, hardware as u32)], true)?;
            let writer = MFCreateSinkWriterFromURL(&HSTRING::from(path.as_os_str()), None, &attrs)?;
            let out = video_type(&MFVideoFormat_H264, size, fps)?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, bitrate)?;
            out.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Main.0 as u32)?;
            // A key frame every two seconds, so a replay can start close to where it is cut.
            out.SetUINT32(&MF_MT_MAX_KEYFRAME_SPACING, fps * 2)?;
            let video = writer.AddStream(&out)?;
            let input = video_type(&MFVideoFormat_NV12, size, fps)?;
            input.SetUINT32(&MF_MT_DEFAULT_STRIDE, size.0)?;
            writer.SetInputMediaType(video, &input, None)?;
            if sound {
                let out = audio_type(&MFAudioFormat_AAC)?;
                out.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AUDIO_BITRATE / 8)?;
                let audio = writer.AddStream(&out)?;
                let input = audio_type(&MFAudioFormat_PCM)?;
                input.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 4)?;
                input.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 48_000 * 4)?;
                writer.SetInputMediaType(audio, &input, None)?;
            }
            writer.BeginWriting()?;
            Ok(writer)
        }
    }

    unsafe fn sample(data: &[u8], ts: i64, dur: i64) -> Result<IMFSample> {
        unsafe {
            let buf = MFCreateMemoryBuffer(data.len() as u32)?;
            let mut p = std::ptr::null_mut();
            buf.Lock(&mut p, None, None)?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
            buf.Unlock()?;
            buf.SetCurrentLength(data.len() as u32)?;
            let s = MFCreateSample()?;
            s.AddBuffer(&buf)?;
            s.SetSampleTime(ts)?;
            s.SetSampleDuration(dur)?;
            Ok(s)
        }
    }

    /// The streams of a file, video or sound, by the reader's index.
    unsafe fn streams(reader: &IMFSourceReader) -> Vec<(u32, GUID, IMFMediaType)> {
        let mut out = Vec::new();
        for i in 0.. {
            let Ok(t) = (unsafe { reader.GetCurrentMediaType(i) }) else { break };
            if let Ok(major) = unsafe { t.GetMajorType() }
                && (major == MFMediaType_Video || major == MFMediaType_Audio)
            {
                out.push((i, major, t));
            }
        }
        out
    }

    /// Every sample of a file in order, with its stream's index and time.
    unsafe fn each_sample(reader: &IMFSourceReader, count: usize, mut f: impl FnMut(u32, i64, IMFSample) -> Result<()>) -> Result<()> {
        let mut ended = HashSet::new();
        while ended.len() < count {
            let (mut index, mut flags, mut ts, mut sample) = (0u32, 0u32, 0i64, None);
            unsafe {
                reader.ReadSample(
                    MF_SOURCE_READER_ANY_STREAM.0 as u32,
                    0,
                    Some(&mut index),
                    Some(&mut flags),
                    Some(&mut ts),
                    Some(&mut sample),
                )?
            };
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                ended.insert(index);
            }
            if let Some(s) = sample {
                f(index, ts, s)?;
            }
        }
        Ok(())
    }

    fn reader(path: &Path) -> Result<IMFSourceReader> {
        unsafe { MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), None) }
    }

    /// Joins `parts` into `out` without encoding again: the last `seconds`, from the key frame at
    /// or just before the cut.
    fn join(parts: &[Arc<Part>], seconds: u32, out: &Path) -> std::result::Result<(), String> {
        let first = parts.first().ok_or_else(|| tr!("Nothing recorded yet.", "Nada gravado ainda.").to_string())?;
        let last = parts.last().expect("not empty");
        let cut = hns(last
            .end
            .checked_sub(Duration::from_secs(seconds as u64))
            .map_or(Duration::ZERO, |c| c.saturating_duration_since(first.start)));
        (|| -> Result<()> {
            // The key frame to start at: the last at or before the cut in the first part.
            let r = reader(&first.path)?;
            let kinds = unsafe { streams(&r) };
            let mut start = None;
            unsafe {
                each_sample(&r, kinds.len(), |i, ts, s| {
                    let video = kinds.iter().any(|(k, m, _)| *k == i && *m == MFMediaType_Video);
                    if video && s.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0 && (start.is_none() || ts <= cut) {
                        start = Some(ts);
                    }
                    Ok(())
                })?
            };
            let base = start.unwrap_or(0);
            let attrs = unsafe { attributes(&[(MF_SINK_WRITER_DISABLE_THROTTLING, 1)], true)? };
            let writer = unsafe { MFCreateSinkWriterFromURL(&HSTRING::from(out.as_os_str()), None, &attrs)? };
            // Output streams by kind, as the first part to have each wrote it: sound may begin a
            // part or two in.
            let mut outs: Vec<(GUID, u32)> = Vec::new();
            for p in parts {
                for (_, major, t) in unsafe { streams(&reader(&p.path)?) } {
                    if !outs.iter().any(|(m, _)| *m == major) {
                        let i = unsafe { writer.AddStream(&t)? };
                        unsafe { writer.SetInputMediaType(i, &t, None)? };
                        outs.push((major, i));
                    }
                }
            }
            unsafe { writer.BeginWriting()? };
            for p in parts {
                let offset = hns(p.start.saturating_duration_since(first.start));
                let r = reader(&p.path)?;
                let kinds = unsafe { streams(&r) };
                unsafe {
                    each_sample(&r, kinds.len(), |i, ts, s| {
                        let Some(major) = kinds.iter().find(|k| k.0 == i).map(|k| k.1) else { return Ok(()) };
                        let Some(&(_, o)) = outs.iter().find(|(m, _)| *m == major) else { return Ok(()) };
                        let t = offset + ts;
                        if t < base {
                            return Ok(());
                        }
                        s.SetSampleTime(t - base)?;
                        writer.WriteSample(o, &s)
                    })?
                };
            }
            unsafe { writer.Finalize() }
        })()
        .map_err(|e| e.message())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Records a moving picture with a tone for a while, saves a replay shorter than that,
        /// and reads it back.
        #[test]
        fn a_replay_holds_the_last_seconds() {
            let (tx, rx) = mpsc::channel();
            let backlog = Arc::new(AtomicUsize::new(0));
            let b = backlog.clone();
            let worker = std::thread::spawn(move || run(rx, b, 5));
            let (w, h, fps) = (640u32, 360u32, 30u32);
            let t0 = Instant::now() - Duration::from_secs(12);
            let mut tone = 0f32;
            for i in 0..(12 * fps) {
                let at = t0 + Duration::from_secs_f64(i as f64 / fps as f64);
                let mut nv12 = vec![128u8; (w * h * 3 / 2) as usize];
                let x = (i * 4 % w) as usize;
                for row in 0..h as usize {
                    nv12[row * w as usize + x..row * w as usize + x + 8.min(w as usize - x)].fill(235);
                }
                backlog.fetch_add(1, Ordering::Relaxed);
                tx.send(Msg::Video { w, h, fps, nv12, at }).unwrap();
                let samples: Vec<i16> = (0..1600)
                    .map(|_| {
                        tone += 440. * std::f32::consts::TAU / 48_000.;
                        (tone.sin() * 8000.) as i16
                    })
                    .flat_map(|s| [s, s])
                    .collect();
                tx.send(Msg::Audio { samples, at }).unwrap();
            }
            let (reply, got) = futures::channel::oneshot::channel();
            tx.send(Msg::Save(reply)).unwrap();
            let out = futures::executor::block_on(got).unwrap().expect("saved");
            drop(tx);
            worker.join().unwrap();

            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                MFStartup(MF_VERSION, MFSTARTUP_FULL).unwrap();
                let r = reader(&out).unwrap();
                let kinds = streams(&r);
                assert_eq!(kinds.len(), 2, "video and sound");
                let (mut first, mut last, mut frames) = (None, 0i64, 0);
                each_sample(&r, kinds.len(), |i, ts, _| {
                    if kinds.iter().any(|(k, m, _)| *k == i && *m == MFMediaType_Video) {
                        first.get_or_insert(ts);
                        last = last.max(ts);
                        frames += 1;
                    }
                    Ok(())
                })
                .unwrap();
                let secs = (last - first.unwrap()) as f64 / HNS as f64;
                // At zero, give or take what an encoder with reordered frames shifts the
                // pictures by (Microsoft's software one: about 15 ms).
                let start = first.unwrap() as f64 / HNS as f64;
                assert!(start.abs() < 0.1, "starts at {start} s");
                assert!((4.5..=9.5).contains(&secs), "{secs} s kept");
                assert!(frames as f64 >= secs * fps as f64 * 0.9, "{frames} frames");
                drop(r);
                let _ = MFShutdown();
            }
            let _ = std::fs::remove_file(&out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(start: u64, end: u64, size: (u32, u32), t0: Instant) -> Arc<Part> {
        let at = |s| t0 + Duration::from_secs(s);
        Arc::new(Part { path: PathBuf::new(), start: at(start), end: at(end), size })
    }

    #[test]
    fn a_replay_takes_the_newest_parts_of_one_size() {
        let t0 = Instant::now();
        let parts = [
            part(0, 4, (1280, 720), t0),
            part(4, 8, (1920, 1080), t0),
            part(8, 12, (1920, 1080), t0),
            part(12, 16, (1920, 1080), t0),
            part(16, 20, (1920, 1080), t0),
        ];
        let ends = |v: Vec<Arc<Part>>| v.iter().map(|p| p.end.duration_since(t0).as_secs()).collect::<Vec<_>>();
        assert_eq!(ends(chosen(&parts, 10)), vec![12, 16, 20]);
        assert_eq!(ends(chosen(&parts, 30)), vec![8, 12, 16, 20]);
    }

    #[test]
    fn a_replay_fits_in_the_chat() {
        for s in [10, 20, 30, 60] {
            let bytes = (bitrate(s) as u64 + AUDIO_BITRATE as u64) * s as u64 / 8;
            assert!(bytes <= 25 << 20, "{s} s: {bytes}");
        }
        assert_eq!(bitrate(5), 12_000_000);
    }
}
