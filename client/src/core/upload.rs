//! A file on its way to the server: in pieces through an upload session where the server takes
//! them (`server/src/upload-sessions.js`), so a big one is never one request that has to survive
//! the whole upload, nor all in memory here; in one request to a server that doesn't.
//!
//! A piece that fails is sent again, a few times with a growing wait, from wherever the server
//! says the file has got to. Pieces start small and grow with the link, up to the server's size:
//! one that takes about two seconds keeps the bar moving on a slow uplink without making a fast
//! one pay a request per megabyte.

use super::api::{Api, ApiError, Result};
use super::types::*;
use serde_json::Value;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// The server's piece size when it doesn't say (the Electron line's and this line's both do).
const CHUNK_BYTES: u64 = 8 << 20;
/// The first piece, before there is anything to measure the link by, and the smallest after.
const FIRST_PIECE: u64 = 1 << 20;
/// How long a piece should take, as near as the last one says.
const PIECE_TIME: f64 = 2.0;
/// Failures in a row before giving up: about half a minute of waiting, in all.
const RETRIES: u32 = 5;

/// Where the bytes come from: already in memory (a pasted picture, a file small enough to show),
/// or a file on disk, read a piece at a time.
#[derive(Clone)]
pub enum Source {
    Memory(Arc<Vec<u8>>),
    Disk(PathBuf),
}

/// How far an upload has got, read by the window while it runs, and the way to stop it. A
/// download saved to disk (`Api::download_to`) is followed the same way.
#[derive(Default)]
pub struct Progress {
    sent: AtomicU64,
    total: AtomicU64,
    cancelled: AtomicBool,
    stop: tokio::sync::Notify,
}

impl Progress {
    /// Sent so far, from 0 to 1.
    pub fn fraction(&self) -> f32 {
        let total = self.total.load(Ordering::Relaxed);
        if total == 0 { 0. } else { (self.sent.load(Ordering::Relaxed) as f64 / total as f64).clamp(0., 1.) as f32 }
    }

    /// Stops it, mid-piece included: the request in flight is dropped.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
        // A permit, not only a wake: between pieces nothing is waiting yet.
        self.stop.notify_one();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    pub(crate) fn sent(&self, n: u64) {
        self.sent.store(n, Ordering::Relaxed);
    }

    pub(crate) fn total(&self, n: u64) {
        self.total.store(n, Ordering::Relaxed);
    }
}

/// Sends `size` bytes from `source`, in pieces when `pieces` (the server said it takes them).
pub async fn send(api: &Api, source: Source, size: u64, content_type: &str, pieces: bool, progress: Arc<Progress>) -> Result<Upload> {
    progress.total.store(size, Ordering::Relaxed);
    if !pieces {
        let bytes = match source {
            Source::Memory(b) => b.to_vec(),
            Source::Disk(path) => read_piece(Source::Disk(path), 0, size).await?,
        };
        let up = stoppable(&progress, api.upload(bytes, content_type)).await?;
        progress.sent(size);
        return Ok(up);
    }
    let session = stoppable(&progress, api.open_upload(size, content_type)).await?;
    let sent = send_pieces(api, &session, &source, size, &progress).await;
    if sent.is_err() {
        // Its room on the server is held until it's finished or forgotten: give it back now rather
        // than in ten minutes. Best effort; the server sweeps it anyway.
        api.cancel_upload(&session.id).await;
    }
    sent
}

async fn send_pieces(api: &Api, session: &UploadSession, source: &Source, size: u64, progress: &Progress) -> Result<Upload> {
    let most = if session.chunk_bytes > 0 { session.chunk_bytes } else { CHUNK_BYTES };
    let mut piece = FIRST_PIECE.min(most);
    let mut offset = 0;
    let mut failures = 0;
    while offset < size {
        if progress.is_cancelled() {
            return Err(cancelled());
        }
        let len = piece.min(size - offset);
        let bytes = read_piece(source.clone(), offset, len).await?;
        let started = Instant::now();
        match stoppable(progress, api.upload_chunk(&session.id, offset, bytes)).await {
            Ok(received) => {
                failures = 0;
                piece = next_piece(len, started.elapsed(), most);
                offset = received;
            }
            // Somewhere other than where we are: a piece that did arrive although its answer
            // never came, or a retry racing the first try still being read. Carry on from where
            // the server is; only standing still counts as a failure.
            Err(e) if e.code == ErrorCode::BadOffset => {
                let Some(received) = e.body.get("received").and_then(Value::as_u64) else { return Err(e) };
                if received == offset {
                    failures += 1;
                    if failures > RETRIES {
                        return Err(e);
                    }
                    wait(progress, backoff(failures)).await?;
                }
                offset = received;
            }
            // The server pacing uploads: as long as it asks, and it isn't a failure.
            Err(e) if e.code == ErrorCode::SlowDown => {
                let secs = e.body.get("retryAfterSec").and_then(Value::as_u64).unwrap_or(5).clamp(1, 60);
                wait(progress, Duration::from_secs(secs)).await?;
            }
            Err(e) if retryable(&e) && failures < RETRIES => {
                failures += 1;
                log::info!("upload piece at {offset} failed ({}), try {failures} of {RETRIES}", e.message);
                wait(progress, backoff(failures)).await?;
            }
            Err(e) => return Err(expired(e)),
        }
        progress.sent(offset.min(size));
    }
    stoppable(progress, api.finish_upload(&session.id)).await.map_err(expired)
}

/// A session the server forgot -- left idle too long, or the server restarted -- says
/// `no_such_upload`, which elsewhere means a file that is gone.
fn expired(e: ApiError) -> ApiError {
    if e.code != ErrorCode::NoSuchUpload {
        return e;
    }
    let message =
        tr!("The upload stopped for too long and expired. Send it again.", "O envio ficou parado tempo demais e expirou. Envie de novo.");
    ApiError { message: message.into(), ..e }
}

/// The next piece's size: what the last one says the link moves in `PIECE_TIME`, in whole 64 KB,
/// between `FIRST_PIECE` and the server's most.
fn next_piece(len: u64, took: Duration, most: u64) -> u64 {
    let rate = len as f64 / took.as_secs_f64().max(0.001);
    let want = ((rate * PIECE_TIME) as u64) & !0xffff;
    want.clamp(FIRST_PIECE.min(most), most)
}

fn backoff(failure: u32) -> Duration {
    Duration::from_secs(1 << failure.saturating_sub(1).min(4))
}

/// Worth sending again: the server not answering, a piece cut off on the way, or the server
/// failing on its side. A full disk or a refusal will say the same thing again.
fn retryable(e: &ApiError) -> bool {
    e.status == 0 || e.code == ErrorCode::Aborted || (e.status >= 500 && e.status != 507)
}

/// `len` bytes at `offset`. From disk on a blocking thread: a piece is up to 8 MB, from a disk
/// that may be slow or on the network.
async fn read_piece(source: Source, offset: u64, len: u64) -> Result<Vec<u8>> {
    match source {
        Source::Memory(b) => {
            let (from, to) = (offset as usize, (offset + len) as usize);
            b.get(from..to).map(<[u8]>::to_vec).ok_or_else(|| unreadable("the file changed while it was being sent".into()))
        }
        Source::Disk(path) => tokio::task::spawn_blocking(move || -> std::io::Result<Vec<u8>> {
            use std::io::{Read, Seek, SeekFrom};
            let mut file = std::fs::File::open(&path)?;
            file.seek(SeekFrom::Start(offset))?;
            let mut buf = vec![0; len as usize];
            file.read_exact(&mut buf)?;
            Ok(buf)
        })
        .await
        .map_err(|e| unreadable(e.to_string()))?
        .map_err(|e| unreadable(e.to_string())),
    }
}

/// `f`, unless the upload is cancelled first.
pub(crate) async fn stoppable<T>(progress: &Progress, f: impl Future<Output = Result<T>>) -> Result<T> {
    if progress.is_cancelled() {
        return Err(cancelled());
    }
    tokio::select! {
        r = f => r,
        _ = progress.stop.notified() => Err(cancelled()),
    }
}

async fn wait(progress: &Progress, d: Duration) -> Result<()> {
    stoppable(progress, async {
        tokio::time::sleep(d).await;
        Ok(())
    })
    .await
}

fn cancelled() -> ApiError {
    ApiError { status: 0, code: ErrorCode::Cancelled, message: tr!("Upload cancelled.", "Envio cancelado.").into(), body: Value::Null }
}

fn unreadable(why: String) -> ApiError {
    ApiError {
        status: 0,
        code: ErrorCode::Unknown,
        message: trf!("Could not read that file: {}", "Não foi possível ler esse arquivo: {}", why),
        body: Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pieces_grow_with_the_link_within_the_bounds() {
        let most = 8 << 20;
        // 1 MB in a second: two seconds' worth.
        assert_eq!(next_piece(1 << 20, Duration::from_secs(1), most), 2 << 20);
        // A very fast link stops at the server's size, a very slow one at the first piece's.
        assert_eq!(next_piece(1 << 20, Duration::from_millis(1), most), most);
        assert_eq!(next_piece(1 << 20, Duration::from_secs(60), most), FIRST_PIECE);
        // A server that takes smaller pieces than the first one is never sent bigger.
        assert_eq!(next_piece(1 << 19, Duration::from_secs(60), 1 << 19), 1 << 19);
    }

    #[test]
    fn waits_grow_and_stop_growing() {
        let waits: Vec<u64> = (1..=7).map(|n| backoff(n).as_secs()).collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 16, 16]);
    }

    #[test]
    fn a_piece_is_read_from_where_it_starts() {
        let path = std::env::temp_dir().join(format!("ciranda-piece-{}", std::process::id()));
        std::fs::write(&path, b"0123456789").unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        assert_eq!(rt.block_on(read_piece(Source::Disk(path.clone()), 3, 4)).unwrap(), b"3456");
        assert!(rt.block_on(read_piece(Source::Disk(path.clone()), 8, 4)).is_err(), "past the end is an error, not a short piece");
        let memory = Source::Memory(Arc::new(b"0123456789".to_vec()));
        assert_eq!(rt.block_on(read_piece(memory, 6, 4)).unwrap(), b"6789");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_cancel_before_anything_stops_everything() {
        let p = Progress::default();
        p.cancel();
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        let got = rt.block_on(stoppable(&p, async { Ok(()) }));
        assert_eq!(got.unwrap_err().code, ErrorCode::Cancelled);
    }
}
