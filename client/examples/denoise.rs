//! Runs a recording through both noise suppressors the way a call does, to hear them side by side:
//! `<name>.classic.wav` has the processing module's suppressor, `<name>.neural.wav` DeepFilterNet
//! after the same module with its suppressor off. The input is a 48 kHz WAV, 16-bit or float,
//! mixed down to mono.
//!
//!     cargo run --release --example denoise -- recording.wav [--limit dB] [--raw]
//!
//! `--limit` is the most taken off the noise, 30 dB as in a call; `--raw` leaves the processing
//! module out of the neural path, to compare with libDF's own `deep-filter`.

#[allow(dead_code)]
#[path = "../src/media/deepfilter.rs"]
mod deepfilter;

use deepfilter::{DeepFilter, HOP};
use libwebrtc::native::apm::AudioProcessingModule;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn main() -> anyhow::Result<()> {
    let (mut input, mut limit, mut raw) = (None, 30f32, false);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--limit" => limit = args.next().ok_or_else(|| anyhow::anyhow!("--limit needs dB"))?.parse()?,
            "--raw" => raw = true,
            _ => input = Some(PathBuf::from(arg)),
        }
    }
    let input = input.ok_or_else(|| anyhow::anyhow!("usage: denoise <file.wav> [--limit dB] [--raw]"))?;
    let samples = read_wav(&input)?;
    let frames = samples.len() / HOP;

    let mut classic = AudioProcessingModule::new(false, false, true, true);
    let mut out = Vec::with_capacity(frames * HOP);
    for chunk in samples.chunks_exact(HOP) {
        let mut pcm: Vec<i16> = chunk.iter().map(|s| (s.clamp(-1., 1.) * 32767.) as i16).collect();
        let _ = classic.process_stream(&mut pcm, 48_000, 1);
        out.extend(pcm.iter().map(|&s| s as f32 / 32768.));
    }
    write_wav(&with_suffix(&input, "classic"), &out)?;

    let started = Instant::now();
    let mut df = DeepFilter::load()?;
    df.set_attenuation_limit(limit);
    println!("DeepFilterNet built in {} ms", started.elapsed().as_millis());
    let mut module = AudioProcessingModule::new(false, false, true, false);
    let (mut out, mut clean) = (Vec::with_capacity(frames * HOP), vec![0f32; HOP]);
    let mut times = Vec::with_capacity(frames);
    for chunk in samples.chunks_exact(HOP) {
        let mut pcm: Vec<i16> = chunk.iter().map(|s| (s.clamp(-1., 1.) * 32767.) as i16).collect();
        if !raw {
            let _ = module.process_stream(&mut pcm, 48_000, 1);
        }
        let frame: Vec<f32> = pcm.iter().map(|&s| s as f32 / 32768.).collect();
        let t = Instant::now();
        df.process(&frame, &mut clean)?;
        times.push(t.elapsed());
        out.extend_from_slice(&clean);
    }
    write_wav(&with_suffix(&input, "neural"), &out)?;

    times.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1000.;
    let mean = times.iter().map(|&d| ms(d)).sum::<f64>() / times.len().max(1) as f64;
    println!(
        "{frames} frames of 10 ms: DeepFilterNet {mean:.2} ms each on average, {:.2} ms at the 99th percentile, {:.2} ms at worst",
        ms(times[times.len() * 99 / 100]),
        ms(times[times.len() - 1])
    );
    Ok(())
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!("{stem}.{suffix}.wav"))
}

fn read_wav(path: &Path) -> anyhow::Result<Vec<f32>> {
    let bytes = std::fs::read(path)?;
    anyhow::ensure!(bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE", "not a WAV file");
    let (mut format, mut data) = (None, None);
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into()?) as usize;
        let body = &bytes[at + 8..(at + 8 + len).min(bytes.len())];
        match &bytes[at..at + 4] {
            b"fmt " => format = Some(body),
            b"data" => data = Some(body),
            _ => {}
        }
        at += 8 + len + len % 2;
    }
    let (format, data) = (format.ok_or_else(|| anyhow::anyhow!("no fmt chunk"))?, data.ok_or_else(|| anyhow::anyhow!("no data chunk"))?);
    let kind = u16::from_le_bytes([format[0], format[1]]);
    let channels = u16::from_le_bytes([format[2], format[3]]) as usize;
    let rate = u32::from_le_bytes(format[4..8].try_into()?);
    let bits = u16::from_le_bytes([format[14], format[15]]);
    anyhow::ensure!(rate == 48_000, "{rate} Hz: only 48 kHz recordings, as the call runs at");
    let samples: Vec<f32> = match (kind, bits) {
        (1, 16) => data.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.).collect(),
        (3, 32) => data.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect(),
        _ => anyhow::bail!("format {kind} at {bits} bits: only 16-bit PCM or 32-bit float"),
    };
    Ok(samples.chunks_exact(channels).map(|c| c.iter().sum::<f32>() / channels as f32).collect())
}

fn write_wav(path: &Path, samples: &[f32]) -> anyhow::Result<()> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&48_000u32.to_le_bytes());
    out.extend_from_slice(&96_000u32.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&((s.clamp(-1., 1.) * 32767.) as i16).to_le_bytes());
    }
    std::fs::write(path, out)?;
    println!("wrote {}", path.display());
    Ok(())
}
