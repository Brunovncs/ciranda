//! What the computer is playing, for a screen share's sound. On Windows, its process loopback
//! (Windows 10 2004 and later): everything except Ciranda's own process tree, so viewers don't
//! hear the call back, or only one application's, for a window share. On macOS 14.2 and later, a
//! Core Audio process tap of every process but Ciranda. On Linux, the monitor of PulseAudio's
//! (or PipeWire's) default output, which has no way to leave one program out: the call is in it.

use libwebrtc::audio_frame::AudioFrame;
use libwebrtc::audio_source::AudioSourceOptions;
use libwebrtc::audio_source::native::NativeAudioSource;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub const RATE: u32 = 48_000;
pub const CHANNELS: u32 = 2;
/// The start of the name of the device a share's sound is taken through on macOS, which is no
/// microphone to offer.
pub const TAP_DEVICE: &str = "app.ciranda.share-sound";

/// The sounds a share can carry on this computer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Support {
    /// What the whole computer plays.
    pub system: bool,
    /// Only the shared window's application.
    pub app: bool,
    /// The whole computer's sound leaves the call out.
    pub without_call: bool,
}

/// Linux takes the computer's sound from the output's monitor, which holds the call as well.
#[cfg(target_os = "linux")]
const CALL_LEFT_OUT_ON_LINUX: bool = false;

/// Worked out once: what the system offers doesn't change while Ciranda runs.
pub fn support() -> Support {
    static S: std::sync::OnceLock<Support> = std::sync::OnceLock::new();
    *S.get_or_init(|| {
        #[cfg(windows)]
        let s = Support { system: true, app: true, without_call: true };
        #[cfg(target_os = "macos")]
        let s = Support { system: tap::available(), app: false, without_call: true };
        // The monitor carries the call too, and played back to everyone in it each would hear
        // themselves a moment late. Offered again once a share can leave the call out.
        #[cfg(target_os = "linux")]
        let s = Support { system: monitor::available() && CALL_LEFT_OUT_ON_LINUX, app: false, without_call: false };
        #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
        let s = Support { system: false, app: false, without_call: false };
        s
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Everything the computer plays except this process tree.
    Excluding(u32),
    /// Only this process tree.
    Only(u32),
}

pub struct Loopback {
    stop: Arc<AtomicBool>,
    pub source: NativeAudioSource,
}

impl Loopback {
    pub fn start(mode: Mode) -> Loopback {
        let _rt = crate::core::runtime().enter();
        let source = NativeAudioSource::new(
            AudioSourceOptions { echo_cancellation: false, noise_suppression: false, auto_gain_control: false },
            RATE,
            CHANNELS,
            0,
        );
        Loopback::feeding(mode, source)
    }

    /// Captures into a source that already exists, so a share whose sound changes keeps the
    /// same track and needs no new negotiation.
    pub fn feeding(mode: Mode, source: NativeAudioSource) -> Loopback {
        let stop = Arc::new(AtomicBool::new(false));
        let (s, st) = (source.clone(), stop.clone());
        std::thread::Builder::new().name("ciranda-loopback".into()).spawn(move || imp::run(mode, &s, &st)).ok();
        Loopback { stop, source }
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Sends 10 ms frames of interleaved stereo i16 to the source, buffering the remainder.
struct Framer {
    pending: Vec<i16>,
    /// Zeros for the packets Windows marks silent, kept rather than made for each.
    silence: Vec<i16>,
}

impl Framer {
    fn push(&mut self, source: &NativeAudioSource, samples: &[i16]) {
        super::replay::audio(samples);
        self.pending.extend_from_slice(samples);
        let frame = (RATE / 100 * CHANNELS) as usize;
        let mut sent = 0;
        while self.pending.len() - sent >= frame {
            let f = AudioFrame {
                data: (&self.pending[sent..sent + frame]).into(),
                sample_rate: RATE,
                num_channels: CHANNELS,
                samples_per_channel: RATE / 100,
            };
            let _ = futures::executor::block_on(source.capture_frame(&f));
            sent += frame;
        }
        self.pending.drain(..sent);
    }

    fn push_silence(&mut self, source: &NativeAudioSource, n: usize) {
        let mut silence = std::mem::take(&mut self.silence);
        silence.resize(n, 0);
        self.push(source, &silence);
        self.silence = silence;
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use parking_lot::{Condvar, Mutex};
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::Media::Audio::*;
    use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0};
    use windows::Win32::System::Com::{BLOB, COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
    use windows::Win32::System::Variant::VT_BLOB;
    use windows::core::{IUnknown, Interface, implement};

    #[implement(IActivateAudioInterfaceCompletionHandler)]
    struct Done(Arc<(Mutex<bool>, Condvar)>);

    impl IActivateAudioInterfaceCompletionHandler_Impl for Done_Impl {
        fn ActivateCompleted(&self, _: windows::core::Ref<'_, IActivateAudioInterfaceAsyncOperation>) -> windows::core::Result<()> {
            let (m, cv) = &*self.0;
            *m.lock() = true;
            cv.notify_all();
            Ok(())
        }
    }

    /// Captures until `stop`. The loopback ends whenever Windows invalidates it (the output device
    /// changed or went to sleep, the audio service restarted); it is started again, sooner after
    /// one that ran a good while than after one that failed straight away, so the share does not
    /// go silent for the rest of it.
    pub fn run(mode: Mode, source: &NativeAudioSource, stop: &AtomicBool) {
        const RETRY_MIN: Duration = Duration::from_millis(500);
        const RETRY_MAX: Duration = Duration::from_secs(8);
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let mut framer = Framer { pending: Vec::new(), silence: Vec::new() };
        let mut wait = RETRY_MIN;
        let mut warned = false;
        while !stop.load(Ordering::Relaxed) {
            let started = Instant::now();
            let result = unsafe { capture(mode, source, stop, &mut framer) };
            if stop.load(Ordering::Relaxed) {
                break;
            }
            if started.elapsed() > Duration::from_secs(10) {
                wait = RETRY_MIN;
            }
            match result {
                Err(e) if !std::mem::replace(&mut warned, true) => {
                    log::warn!("system sound for the share stopped ({e}); starting it again")
                }
                Err(e) => log::debug!("system sound for the share: {e}"),
                Ok(()) => {}
            }
            let until = Instant::now() + wait;
            while Instant::now() < until && !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(50));
            }
            wait = (wait * 2).min(RETRY_MAX);
        }
        unsafe { CoUninitialize() };
    }

    /// An event handle, closed however `capture` returns.
    struct Event(HANDLE);

    impl Drop for Event {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    /// A started client, stopped however `capture` returns.
    struct Started<'a>(&'a IAudioClient);

    impl Drop for Started<'_> {
        fn drop(&mut self) {
            unsafe {
                let _ = self.0.Stop();
            }
        }
    }

    /// One run of the loopback: until `stop`, or an error.
    unsafe fn capture(mode: Mode, source: &NativeAudioSource, stop: &AtomicBool, framer: &mut Framer) -> anyhow::Result<()> {
        let (pid, loopback_mode) = match mode {
            Mode::Excluding(pid) => (pid, PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE),
            Mode::Only(pid) => (pid, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE),
        };
        // On the heap, and left there if Windows doesn't answer in time: whether the activation,
        // still going, reads them after that isn't documented, and a stack frame long gone would
        // hand it whatever was written there since. A few bytes leaked per timeout is the cost.
        let mut params = Box::new(AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS { TargetProcessId: pid, ProcessLoopbackMode: loopback_mode },
            },
        });
        // Never dropped: PROPVARIANT's drop is PropVariantClear, which hands a VT_BLOB's data to
        // CoTaskMemFree, and this blob points at `params`, which isn't CoTaskMem's. Freeing it
        // corrupted the heap, and Ciranda crashed as the share's sound stopped.
        let prop = Box::new(std::mem::ManuallyDrop::new(PROPVARIANT {
            Anonymous: PROPVARIANT_0 {
                Anonymous: std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                    vt: VT_BLOB,
                    Anonymous: PROPVARIANT_0_0_0 {
                        blob: BLOB {
                            cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                            pBlobData: &mut *params as *mut _ as *mut u8,
                        },
                    },
                    ..Default::default()
                }),
            },
        }));
        let signal = Arc::new((Mutex::new(false), Condvar::new()));
        let handler: IActivateAudioInterfaceCompletionHandler = Done(signal.clone()).into();
        let op =
            unsafe { ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, &IAudioClient::IID, Some(&**prop), &handler)? };
        {
            let (m, cv) = &*signal;
            let mut done = m.lock();
            while !*done {
                if cv.wait_for(&mut done, std::time::Duration::from_secs(5)).timed_out() {
                    std::mem::forget((params, prop));
                    anyhow::bail!("Windows did not start the loopback");
                }
            }
        }
        let mut hr = windows::core::HRESULT(0);
        let mut unknown: Option<IUnknown> = None;
        unsafe { op.GetActivateResult(&mut hr, &mut unknown)? };
        hr.ok()?;
        let client: IAudioClient = unknown.ok_or_else(|| anyhow::anyhow!("no audio client"))?.cast()?;
        let format = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: CHANNELS as u16,
            nSamplesPerSec: RATE,
            nAvgBytesPerSec: RATE * CHANNELS * 2,
            nBlockAlign: (CHANNELS * 2) as u16,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
                200_000,
                0,
                &format,
                None,
            )?;
        }
        let event = Event(unsafe { CreateEventW(None, false, false, None)? });
        unsafe { client.SetEventHandle(event.0)? };
        let capture: IAudioCaptureClient = unsafe { client.GetService()? };
        unsafe { client.Start()? };
        let _started = Started(&client);
        while !stop.load(Ordering::Relaxed) {
            if unsafe { WaitForSingleObject(event.0, 200) } != WAIT_OBJECT_0 {
                continue;
            }
            loop {
                let packet = unsafe { capture.GetNextPacketSize()? };
                if packet == 0 {
                    break;
                }
                let mut data = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                unsafe { capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)? };
                let n = (frames * CHANNELS) as usize;
                if flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || data.is_null() {
                    framer.push_silence(source, n);
                } else {
                    let samples = unsafe { std::slice::from_raw_parts(data as *const i16, n) };
                    framer.push(source, samples);
                }
                unsafe { capture.ReleaseBuffer(frames)? };
            }
        }
        Ok(())
    }
}

/// The same through cpal, from the device the system gives the computer's sound on: macOS's tap,
/// Linux's monitor. Only the whole computer's sound exists there, so `mode` is not looked at.
#[cfg(not(windows))]
mod imp {
    use super::*;
    use crate::media::audio;
    use cpal::traits::{DeviceTrait, StreamTrait};
    use rtrb::RingBuffer;
    use std::time::{Duration, Instant};

    /// Captures until `stop`, starting again (with the same backoff as Windows) whenever the
    /// device goes away: the output changed, the sound server restarted.
    pub fn run(_mode: Mode, source: &NativeAudioSource, stop: &AtomicBool) {
        const RETRY_MIN: Duration = Duration::from_millis(500);
        const RETRY_MAX: Duration = Duration::from_secs(8);
        let mut framer = Framer { pending: Vec::new(), silence: Vec::new() };
        let mut wait = RETRY_MIN;
        let mut warned = false;
        while !stop.load(Ordering::Relaxed) {
            let started = Instant::now();
            let result = capture(source, stop, &mut framer);
            if stop.load(Ordering::Relaxed) {
                break;
            }
            if started.elapsed() > Duration::from_secs(10) {
                wait = RETRY_MIN;
            }
            match result {
                Err(e) if !std::mem::replace(&mut warned, true) => {
                    log::warn!("system sound for the share stopped ({e}); starting it again")
                }
                Err(e) => log::debug!("system sound for the share: {e}"),
                Ok(()) => {}
            }
            let until = Instant::now() + wait;
            while Instant::now() < until && !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(50));
            }
            wait = (wait * 2).min(RETRY_MAX);
        }
    }

    /// One run: until `stop`, or the device fails. The device's callback only copies into a
    /// queue; converting, resampling and handing to WebRTC happen here, off its deadline.
    fn capture(source: &NativeAudioSource, stop: &AtomicBool, framer: &mut Framer) -> anyhow::Result<()> {
        #[cfg(target_os = "macos")]
        let tap = tap::Tap::open()?;
        #[cfg(target_os = "macos")]
        let device = tap.device()?;
        #[cfg(target_os = "linux")]
        let device = monitor::device()?;
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let device: cpal::Device = anyhow::bail!("no system sound here");
        let config = device.default_input_config()?;
        let (rate, channels) = (config.sample_rate(), (config.channels() as usize).max(1));
        // A second of room: far more than the 10 ms this thread takes between looks.
        let (mut queue, mut taken) = RingBuffer::<f32>::new(rate as usize * channels);
        let lost = Arc::new(AtomicBool::new(false));
        let gone = lost.clone();
        let stream = audio::build_input(
            &device,
            &config,
            move |data: &[f32]| {
                let n = (queue.slots().min(data.len()) / channels) * channels;
                if let Ok(chunk) = queue.write_chunk_uninit(n) {
                    chunk.fill_from_iter(data.iter().copied());
                }
            },
            move |e: cpal::Error| {
                audio::warn_stream("system sound", &e);
                if audio::gone(&e) {
                    gone.store(true, Ordering::Relaxed);
                }
            },
        )?;
        stream.play()?;
        let mut resampler = audio::Resampler::new(rate, RATE, CHANNELS as usize);
        let (mut stereo, mut resampled, mut pcm) = (Vec::new(), Vec::new(), Vec::new());
        while !stop.load(Ordering::Relaxed) {
            anyhow::ensure!(!lost.load(Ordering::Relaxed), "the sound device went away");
            std::thread::sleep(Duration::from_millis(10));
            let n = taken.slots() / channels * channels;
            let Ok(chunk) = taken.read_chunk(n) else { continue };
            stereo.clear();
            let mut samples = chunk.into_iter();
            for _ in 0..n / channels {
                let l = samples.next().unwrap_or(0.);
                let r = if channels > 1 { samples.next().unwrap_or(0.) } else { l };
                for _ in 2..channels {
                    samples.next();
                }
                stereo.extend([l, r]);
            }
            drop(samples);
            resampled.clear();
            resampler.process(&stereo, &mut resampled);
            pcm.clear();
            pcm.extend(resampled.iter().map(|s| (s.clamp(-1., 1.) * 32767.) as i16));
            if !pcm.is_empty() {
                framer.push(source, &pcm);
            }
        }
        drop(stream);
        Ok(())
    }
}

/// The macOS side: a private process tap of every process but this one (macOS 14.2 and later),
/// read through a private aggregate device made of it, as Apple's own sample does. Both are
/// destroyed with the `Tap`.
#[cfg(target_os = "macos")]
mod tap {
    use super::TAP_DEVICE;
    use crate::media::audio;
    use cpal::traits::HostTrait;
    use objc2::AnyThread;
    use objc2::rc::Retained;
    use objc2_core_audio::{
        AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap, AudioHardwareDestroyAggregateDevice,
        AudioHardwareDestroyProcessTap, AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress, CATapDescription,
        CATapMuteBehavior, kAudioAggregateDeviceNameKey, kAudioAggregateDeviceTapAutoStartKey, kAudioAggregateDeviceTapListKey,
        kAudioAggregateDeviceUIDKey, kAudioEndPointDeviceIsPrivateKey, kAudioHardwarePropertyTranslatePIDToProcessObject,
        kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject, kAudioSubTapDriftCompensationKey,
        kAudioSubTapUIDKey,
    };
    use objc2_core_foundation::{
        CFArray, CFDictionary, CFMutableDictionary, CFRetained, CFString, kCFAllocatorDefault, kCFTypeArrayCallBacks,
        kCFTypeDictionaryKeyCallBacks, kCFTypeDictionaryValueCallBacks,
    };
    use objc2_foundation::{NSArray, NSNumber, NSOperatingSystemVersion, NSProcessInfo, NSString};
    use std::ffi::{CStr, c_void};
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Process taps came with macOS 14.2.
    pub fn available() -> bool {
        let version = NSOperatingSystemVersion { majorVersion: 14, minorVersion: 2, patchVersion: 0 };
        NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(version)
    }

    pub struct Tap {
        tap: AudioObjectID,
        aggregate: AudioObjectID,
        uid: String,
    }

    impl Tap {
        pub fn open() -> anyhow::Result<Tap> {
            anyhow::ensure!(available(), "macOS before 14.2 has no process taps");
            // Ciranda as Core Audio knows it, once it has played anything: left out, so viewers
            // don't hear the call. Before that there is nothing of it to leave out.
            let me: Vec<Retained<NSNumber>> = own_process().map(NSNumber::new_u32).into_iter().collect();
            let exclude = NSArray::from_retained_slice(&me);
            // SAFETY: a fresh allocation, initialised once; the setters take plain values.
            let description = unsafe {
                let d = CATapDescription::initStereoGlobalTapButExcludeProcesses(CATapDescription::alloc(), &exclude);
                d.setMuteBehavior(CATapMuteBehavior::Unmuted);
                d.setPrivate(true);
                d.setName(&NSString::from_str("Ciranda share sound"));
                d
            };
            let mut tap: AudioObjectID = 0;
            // SAFETY: a valid description, and somewhere to write the new tap's id.
            let status = unsafe { AudioHardwareCreateProcessTap(Some(&*description), &mut tap) };
            anyhow::ensure!(status == 0 && tap != 0, "Core Audio would not tap the computer's sound ({status})");
            // SAFETY: the description lives; its UUID is what the aggregate device names the tap by.
            let tap_uid = unsafe { description.UUID().UUIDString() };
            static N: AtomicU32 = AtomicU32::new(0);
            let uid = format!("{TAP_DEVICE}.{}.{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed));
            let properties = aggregate_properties(tap_uid, &uid);
            let mut aggregate: AudioObjectID = 0;
            // SAFETY: a dictionary of the documented keys, and somewhere to write the device's id.
            let status = unsafe { AudioHardwareCreateAggregateDevice(&properties, NonNull::from(&mut aggregate)) };
            if status != 0 {
                // SAFETY: the tap made above, not used anywhere else.
                unsafe { AudioHardwareDestroyProcessTap(tap) };
                anyhow::bail!("Core Audio would not make a device of the tap ({status})");
            }
            Ok(Tap { tap, aggregate, uid })
        }

        /// The aggregate device as cpal sees it: found by its UID, once the system lists it.
        pub fn device(&self) -> anyhow::Result<cpal::Device> {
            let id = cpal::DeviceId::new(cpal::HostId::CoreAudio, &self.uid);
            for _ in 0..20 {
                if let Some(d) = audio::with_host(|h| h.device_by_id(&id)) {
                    return Ok(d);
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            anyhow::bail!("the tap's device never showed up")
        }
    }

    impl Drop for Tap {
        fn drop(&mut self) {
            // SAFETY: both made by `open` and used by nothing once the stream on them is gone.
            unsafe {
                AudioHardwareDestroyAggregateDevice(self.aggregate);
                AudioHardwareDestroyProcessTap(self.tap);
            }
        }
    }

    /// This process's Core Audio object, if it has one.
    fn own_process() -> Option<u32> {
        let address = AudioObjectPropertyAddress {
            mSelector: kAudioHardwarePropertyTranslatePIDToProcessObject,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        let pid = std::process::id() as i32;
        let mut object: AudioObjectID = 0;
        let mut size = std::mem::size_of::<AudioObjectID>() as u32;
        // SAFETY: the qualifier is the pid it is documented to take, and the out pointers are
        // valid for the sizes given.
        let status = unsafe {
            AudioObjectGetPropertyData(
                kAudioObjectSystemObject as AudioObjectID,
                NonNull::from(&address),
                std::mem::size_of::<i32>() as u32,
                (&pid as *const i32).cast::<c_void>(),
                NonNull::from(&mut size),
                NonNull::from(&mut object).cast(),
            )
        };
        (status == 0 && object != 0).then_some(object)
    }

    fn cf_key(key: &'static CStr) -> CFRetained<CFString> {
        // SAFETY: a static, NUL-terminated UTF-8 key.
        unsafe { CFString::with_c_string(kCFAllocatorDefault, key.as_ptr(), 0x0800_0100) }.expect("a Core Audio key")
    }

    /// `{ name, uid, taps: [{ uid: tap, drift: yes }], tapautostart: yes, private: yes }`, as cpal
    /// builds it for its own loopback.
    fn aggregate_properties(tap_uid: Retained<NSString>, uid: &str) -> CFRetained<CFDictionary> {
        // SAFETY: CoreFoundation containers with the standard callbacks, holding CF and
        // toll-free bridged Foundation objects that outlive the calls.
        unsafe {
            let yes = NSNumber::new_bool(true);
            let tap = CFMutableDictionary::new(kCFAllocatorDefault, 2, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks)
                .expect("a dictionary");
            CFMutableDictionary::set_value(
                Some(tap.as_ref()),
                &*cf_key(kAudioSubTapUIDKey) as *const _ as *const c_void,
                &*tap_uid as *const _ as *const c_void,
            );
            CFMutableDictionary::set_value(
                Some(tap.as_ref()),
                &*cf_key(kAudioSubTapDriftCompensationKey) as *const _ as *const c_void,
                &*yes as *const _ as *const c_void,
            );
            let list = [tap];
            let taps = CFArray::new(kCFAllocatorDefault, list.as_ptr() as *mut *const c_void, list.len() as _, &kCFTypeArrayCallBacks)
                .expect("an array");
            let d = CFMutableDictionary::new(kCFAllocatorDefault, 5, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks)
                .expect("a dictionary");
            let name = CFString::from_str("Ciranda share sound");
            let uid = CFString::from_str(uid);
            for (key, value) in [
                (kAudioAggregateDeviceNameKey, &*name as *const _ as *const c_void),
                (kAudioAggregateDeviceUIDKey, &*uid as *const _ as *const c_void),
                (kAudioAggregateDeviceTapListKey, &*taps as *const _ as *const c_void),
                (kAudioAggregateDeviceTapAutoStartKey, &*yes as *const _ as *const c_void),
                (kAudioEndPointDeviceIsPrivateKey, &*yes as *const _ as *const c_void),
            ] {
                CFMutableDictionary::set_value(Some(d.as_ref()), &*cf_key(key) as *const _ as *const c_void, value);
            }
            CFRetained::cast_unchecked::<CFDictionary>(d)
        }
    }
}

/// The Linux side: the monitor source PulseAudio keeps for every output, of the default one.
#[cfg(target_os = "linux")]
mod monitor {
    use crate::media::audio;
    use cpal::traits::{DeviceTrait, HostTrait};

    /// Only PulseAudio (or PipeWire's stand-in for it) has monitors; plain ALSA has none.
    pub fn available() -> bool {
        audio::with_host(|h| h.id() == cpal::HostId::PulseAudio)
    }

    pub fn device() -> anyhow::Result<cpal::Device> {
        audio::with_host(|h| {
            anyhow::ensure!(h.id() == cpal::HostId::PulseAudio, "the computer's sound is reachable only through PulseAudio");
            let output = h.default_output_device().ok_or_else(|| anyhow::anyhow!("no output to take the sound of"))?;
            let id = output.id()?;
            let monitor = cpal::DeviceId::new(cpal::HostId::PulseAudio, format!("{}.monitor", id.id()));
            h.device_by_id(&monitor).ok_or_else(|| anyhow::anyhow!("{} has no monitor", id.id()))
        })
    }
}

/// The process that owns a window, for capturing only that application's sound.
#[cfg(windows)]
pub fn window_process(hwnd: u64) -> Option<u32> {
    use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd as _, &mut pid) };
    (pid != 0).then_some(pid)
}

#[cfg(not(windows))]
pub fn window_process(_: u64) -> Option<u32> {
    None
}
