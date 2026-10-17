//! Whether a newer Ciranda is out, asked of GitHub's API, and putting it in place of the running
//! copy. A release carries `ciranda-<version>-windows-x64.exe` and that file's SHA-256; the new
//! program is downloaded next to the running one, checked, and swapped in: Windows lets a running
//! program be renamed though not overwritten, so the running copy steps aside to `ciranda.exe.old`.
//!
//! That copy is the way back if the new version is broken, so it stays until the new one has
//! proved itself (see `Start`): a version that keeps dying as it starts is put back by its own
//! next start, which is the only code that runs by then. The proof is a file beside the program,
//! `ciranda.exe.trial`, counting the starts of the new version that ended without it: each start
//! adds one, and running a minute or quitting properly sets it back. Two starts in a row that
//! did neither are taken for a build that crashes, and the third puts `.old` back.

use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const RELEASES_API: &str = "https://api.github.com/repos/Brunovncs/ciranda/releases/latest";
pub const RELEASES_PAGE: &str = "https://github.com/Brunovncs/ciranda/releases/latest";

/// Where to ask instead of GitHub: a local server, to try an update end to end.
const URL_VAR: &str = "CIRANDA_UPDATE_URL";
/// The version to claim instead of this one, to try updating from an older one.
const PRETEND_VAR: &str = "CIRANDA_PRETEND_VERSION";
/// Set on the copy an update starts, while the one it replaces is still quitting.
pub const RELAUNCH_VAR: &str = "CIRANDA_RELAUNCHED";
/// What Harmony up to 4.1.1 sets instead, on the Ciranda it updates to.
const LEGACY_RELAUNCH_VAR: &str = "HARMONY_RELAUNCHED";

/// This copy was started by an update, while the one it replaces is still quitting.
pub fn relaunched() -> bool {
    std::env::var_os(RELAUNCH_VAR).is_some() || std::env::var_os(LEGACY_RELAUNCH_VAR).is_some()
}

/// The program for this build's processor: an Arm build must not update itself to the x64 one.
const ASSET_SUFFIX: &str = if cfg!(target_arch = "aarch64") { "-windows-arm64.exe" } else { "-windows-x64.exe" };

/// The latest release, as far as an update needs to know.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    /// Its page on GitHub, with the notes and every download.
    pub page: String,
    pub notes: String,
    /// The Windows program and the file holding its SHA-256, when the release has them.
    pub exe: Option<(String, String)>,
}

pub fn api_url() -> String {
    std::env::var(URL_VAR).unwrap_or_else(|_| RELEASES_API.to_string())
}

pub fn current() -> String {
    std::env::var(PRETEND_VAR).unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string())
}

fn numbers(v: &str) -> Vec<u64> {
    v.trim_start_matches('v').split(['.', '-', '+']).map_while(|p| p.parse().ok()).collect()
}

/// `candidate` is a later version than `current` (4.10.0 after 4.9.2; a pre-release suffix is
/// ignored).
pub fn is_newer(candidate: &str, current: &str) -> bool {
    let (a, b) = (numbers(candidate), numbers(current));
    !a.is_empty() && a > b
}

pub fn parse(json: &[u8]) -> Result<Release, String> {
    let v: serde_json::Value = serde_json::from_slice(json).map_err(|e| e.to_string())?;
    let tag = v["tag_name"].as_str().ok_or(tr!("the release has no tag", "a versão não tem tag"))?;
    let assets = v["assets"].as_array().cloned().unwrap_or_default();
    let url_of = |suffix: &str| {
        assets
            .iter()
            .find(|a| a["name"].as_str().is_some_and(|n| n.ends_with(suffix)))
            .and_then(|a| a["browser_download_url"].as_str())
            .map(String::from)
    };
    let exe = if cfg!(windows) { url_of(ASSET_SUFFIX).zip(url_of(&format!("{ASSET_SUFFIX}.sha256"))) } else { None };
    Ok(Release {
        version: tag.trim_start_matches('v').to_string(),
        page: v["html_url"].as_str().unwrap_or(RELEASES_PAGE).to_string(),
        notes: v["body"].as_str().unwrap_or_default().trim().to_string(),
        exe,
    })
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(concat!("Ciranda/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())
}

async fn fetch(http: &reqwest::Client, url: &str) -> Result<reqwest::Response, String> {
    let res = http.get(url).header("Accept", "application/vnd.github+json").send().await.map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(trf!("{} answered {}", "{} respondeu {}", res.url().host_str().unwrap_or("?"), res.status().as_u16()));
    }
    Ok(res)
}

/// The latest release, if it is newer than this one. GitHub's "latest" is never a draft or a
/// pre-release. Runs on the network runtime.
pub async fn check() -> Result<Option<Release>, String> {
    let http = client()?;
    let res = http.get(api_url()).header("Accept", "application/vnd.github+json").send().await.map_err(|e| e.to_string())?;
    // GitHub answers 404 while the repository has no release yet: nothing to update to.
    if res.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !res.status().is_success() {
        return Err(trf!("{} answered {}", "{} respondeu {}", res.url().host_str().unwrap_or("?"), res.status().as_u16()));
    }
    let body = res.bytes().await.map_err(|e| e.to_string())?;
    let r = parse(&body)?;
    Ok(is_newer(&r.version, &current()).then_some(r))
}

/// `ciranda.exe.<ext>`, beside the program.
pub fn beside(exe: &Path, ext: &str) -> PathBuf {
    let mut s = OsString::from(exe.as_os_str());
    s.push(".");
    s.push(ext);
    s.into()
}

/// Downloads the release's program to `to` and checks it against the SHA-256 published with it,
/// so a broken or swapped download is never run. `progress` hears the fraction done.
pub async fn download(r: &Release, to: &Path, mut progress: impl FnMut(f32)) -> Result<(), String> {
    let (url, sha_url) = r.exe.as_ref().ok_or(tr!("this release has no Windows program", "esta versão não tem o programa para Windows"))?;
    let http = client()?;
    let sums = fetch(&http, sha_url).await?.text().await.map_err(|e| e.to_string())?;
    let expected = sums.split_whitespace().next().unwrap_or_default().to_lowercase();
    if expected.len() != 64 {
        return Err(tr!("the checksum could not be read", "não foi possível ler o checksum").into());
    }
    let mut res = fetch(&http, url).await?;
    let total = res.content_length().unwrap_or(0);
    let mut file = std::fs::File::create(to).map_err(|e| format!("{}: {e}", to.display()))?;
    let mut hash = Sha256::new();
    let mut got = 0u64;
    let result = async {
        while let Some(chunk) = res.chunk().await.map_err(|e| e.to_string())? {
            file.write_all(&chunk).map_err(|e| e.to_string())?;
            hash.update(&chunk);
            got += chunk.len() as u64;
            if total > 0 {
                progress(got as f32 / total as f32);
            }
        }
        file.sync_all().map_err(|e| e.to_string())?;
        if hex::encode(hash.finalize()) != expected {
            return Err(tr!("the download does not match its checksum", "o download não confere com o checksum").to_string());
        }
        Ok(())
    }
    .await;
    if result.is_err() {
        drop(file);
        let _ = std::fs::remove_file(to);
    }
    result
}

/// Puts `new` where `exe` is, keeping the running copy as `.old` until the new version has
/// proved itself. Its trial starts over: the next start is its first.
pub fn swap(exe: &Path, new: &Path) -> Result<(), String> {
    let old = beside(exe, "old");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old).map_err(|e| format!("{}: {e}", exe.display()))?;
    if let Err(e) = std::fs::rename(new, exe) {
        let _ = std::fs::rename(&old, exe);
        return Err(format!("{}: {e}", exe.display()));
    }
    let _ = std::fs::remove_file(beside(exe, "trial"));
    Ok(())
}

/// Starts the program at `exe` again, with the arguments this one got. The new copy waits for
/// this one to quit instead of handing over to it as a second start would.
pub fn relaunch(exe: &Path) -> Result<(), String> {
    std::process::Command::new(exe)
        .args(std::env::args_os().skip(1))
        .env_remove(PRETEND_VAR)
        .env(RELAUNCH_VAR, "1")
        .spawn()
        .map(drop)
        .map_err(|e| e.to_string())
}

/// Starts of a new version in a row that may end without it running `HEALTHY_AFTER` or quitting
/// properly; the one after puts the previous version back. Two, so one crash of a working build
/// (or the power going) never undoes an update.
const FAILED_STARTS: u32 = 2;
/// How long a new version runs, its window up, before it is trusted and its previous copy goes.
/// A build that dies as it starts does so well within it.
pub const HEALTHY_AFTER: Duration = Duration::from_secs(60);

/// What this start makes of an update left beside the program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    /// No update on trial.
    Plain,
    /// The first start of a new version: its previous copy stays until it proves itself.
    Updated,
    /// A later start of a new version that has not proved itself yet.
    Trial,
    /// The new version started `FAILED_STARTS` times in a row and died each time: the previous
    /// one goes back.
    RollBack,
}

impl Start {
    /// The new version still has to prove itself during this run.
    pub fn on_trial(self) -> bool {
        matches!(self, Start::Updated | Start::Trial)
    }
}

/// The decision, from whether the previous copy is there and the trial file's count (none when
/// there is no file).
fn decide(old_exists: bool, failed: Option<u32>) -> Start {
    match (old_exists, failed) {
        (false, _) => Start::Plain,
        (true, None) => Start::Updated,
        (true, Some(n)) if n >= FAILED_STARTS => Start::RollBack,
        (true, Some(_)) => Start::Trial,
    }
}

/// Reads the trial and counts this start in it. Reading and writing a tiny file, it is meant
/// for the very start, before anything that could crash; `start` runs it once per process.
fn begin(exe: &Path) -> Start {
    let _ = std::fs::remove_file(beside(exe, "new"));
    let trial = beside(exe, "trial");
    // An unreadable count is taken as a start that went fine rather than as a reason to undo.
    let failed = std::fs::read_to_string(&trial).ok().map(|s| s.trim().parse::<u32>().unwrap_or(0));
    let start = decide(beside(exe, "old").exists(), failed);
    match start {
        Start::Plain => {
            let _ = std::fs::remove_file(&trial);
        }
        Start::Updated | Start::Trial => {
            if let Err(e) = std::fs::write(&trial, (failed.unwrap_or(0) + 1).to_string()) {
                log::warn!("the update's trial could not be counted: {e}");
            }
        }
        Start::RollBack => {}
    }
    start
}

/// What this start makes of an update, decided the first time it is asked and the same after;
/// so main can ask before anything else runs, and the updater again later.
pub fn start(exe: &Path) -> Start {
    static START: std::sync::OnceLock<Start> = std::sync::OnceLock::new();
    *START.get_or_init(|| begin(exe))
}

/// Puts the previous version back where `exe` is. The broken one goes to `.new`, which every
/// version deletes as it starts, the ones from before trials included.
fn put_back(exe: &Path) -> Result<(), String> {
    let (old, bad) = (beside(exe, "old"), beside(exe, "new"));
    let _ = std::fs::remove_file(&bad);
    std::fs::rename(exe, &bad).map_err(|e| format!("{}: {e}", exe.display()))?;
    if let Err(e) = std::fs::rename(&old, exe) {
        let _ = std::fs::rename(&bad, exe);
        return Err(format!("{}: {e}", old.display()));
    }
    let _ = std::fs::remove_file(beside(exe, "trial"));
    Ok(())
}

/// Puts the previous version back and starts it; the caller exits right after.
pub fn roll_back(exe: &Path) -> Result<(), String> {
    put_back(exe)?;
    relaunch(exe)
}

/// The new version quit properly before proving itself: whatever ends its starts, it is not a
/// crash, so the count starts over. Nothing to do when an update since has begun a new trial.
pub fn survived(exe: &Path) {
    let trial = beside(exe, "trial");
    if trial.exists() {
        let _ = std::fs::write(&trial, "0");
    }
}

/// The new version has run long enough to be trusted: deletes the previous copy and the trial.
/// Should the copy stay (a scanner holding it, say), the trial stays too, at nought, so the next
/// start tries again without taking itself for the first after an update.
pub fn settle(exe: &Path) {
    let (old, trial) = (beside(exe, "old"), beside(exe, "trial"));
    for _ in 0..20 {
        if !old.exists() || std::fs::remove_file(&old).is_ok() {
            let _ = std::fs::remove_file(&trial);
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    log::warn!("{} could not be deleted", old.display());
    survived(exe);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number() {
        assert!(is_newer("4.0.1", "4.0.0"));
        assert!(is_newer("v4.10.0", "4.9.9"));
        assert!(is_newer("5.0.0", "4.99.0"));
        assert!(!is_newer("4.0.0", "4.0.0"));
        assert!(!is_newer("3.9.9", "4.0.0"));
        assert!(!is_newer("garbage", "4.0.0"));
    }

    #[test]
    fn reads_a_release() {
        let json = br#"{"tag_name":"v4.1.0","html_url":"https://example/v4.1.0","body":" Faster joins. ","assets":[
            {"name":"ciranda-4.1.0-windows-x64.exe","browser_download_url":"https://example/exe"},
            {"name":"ciranda-4.1.0-windows-x64.exe.sha256","browser_download_url":"https://example/sha"}]}"#;
        let r = parse(json).unwrap();
        assert_eq!(r.version, "4.1.0");
        assert_eq!(r.page, "https://example/v4.1.0");
        assert_eq!(r.notes, "Faster joins.");
        if cfg!(all(windows, target_arch = "x86_64")) {
            assert_eq!(r.exe, Some(("https://example/exe".into(), "https://example/sha".into())));
        }
        // The installer sits beside the program in a release, listed first or not; the updater
        // still takes the program.
        let with_installer = br#"{"tag_name":"v4.2.1","assets":[
            {"name":"ciranda-4.2.1-windows-x64-setup.exe","browser_download_url":"https://example/setup"},
            {"name":"ciranda-4.2.1-windows-x64-setup.exe.sha256","browser_download_url":"https://example/setup-sha"},
            {"name":"ciranda-4.2.1-windows-x64.exe","browser_download_url":"https://example/exe"},
            {"name":"ciranda-4.2.1-windows-x64.exe.sha256","browser_download_url":"https://example/sha"}]}"#;
        if cfg!(all(windows, target_arch = "x86_64")) {
            assert_eq!(parse(with_installer).unwrap().exe, Some(("https://example/exe".into(), "https://example/sha".into())));
        }
        let without = parse(br#"{"tag_name":"v4.0.1","assets":[]}"#).unwrap();
        assert_eq!(without.exe, None);
        assert_eq!(without.page, RELEASES_PAGE);
    }

    /// A release has the program for each processor; a build takes its own.
    #[test]
    fn takes_the_program_for_this_processor() {
        let json = br#"{"tag_name":"v4.5.0","assets":[
            {"name":"ciranda-4.5.0-windows-arm64.exe","browser_download_url":"https://example/arm"},
            {"name":"ciranda-4.5.0-windows-arm64.exe.sha256","browser_download_url":"https://example/arm-sha"},
            {"name":"ciranda-4.5.0-windows-x64.exe","browser_download_url":"https://example/x64"},
            {"name":"ciranda-4.5.0-windows-x64.exe.sha256","browser_download_url":"https://example/x64-sha"}]}"#;
        let ours = |exe: &str, sha: &str| Some((format!("https://example/{exe}"), format!("https://example/{sha}")));
        let want = if !cfg!(windows) {
            None
        } else if cfg!(target_arch = "aarch64") {
            ours("arm", "arm-sha")
        } else {
            ours("x64", "x64-sha")
        };
        assert_eq!(parse(json).unwrap().exe, want);
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ciranda-update-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("ciranda.exe")
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn the_trial_decides() {
        assert_eq!(decide(false, None), Start::Plain);
        // A trial file left without the previous copy is no update at all.
        assert_eq!(decide(false, Some(5)), Start::Plain);
        assert_eq!(decide(true, None), Start::Updated);
        assert_eq!(decide(true, Some(0)), Start::Trial);
        assert_eq!(decide(true, Some(1)), Start::Trial);
        assert_eq!(decide(true, Some(FAILED_STARTS)), Start::RollBack);
        assert!(Start::Updated.on_trial() && Start::Trial.on_trial());
        assert!(!Start::Plain.on_trial() && !Start::RollBack.on_trial());
    }

    #[test]
    fn a_healthy_update_lets_go_of_the_old_copy() {
        let exe = scratch("healthy");
        std::fs::write(&exe, "old").unwrap();
        let new = beside(&exe, "new");
        std::fs::write(&new, "new").unwrap();
        swap(&exe, &new).unwrap();
        assert_eq!(read(&exe), "new");
        assert_eq!(read(&beside(&exe, "old")), "old");
        assert_eq!(begin(&exe), Start::Updated);
        assert_eq!(read(&beside(&exe, "trial")), "1");
        // Quitting properly is no crash: the count starts over, and the next start is no first.
        survived(&exe);
        assert_eq!(begin(&exe), Start::Trial);
        settle(&exe);
        assert!(!beside(&exe, "old").exists() && !beside(&exe, "trial").exists());
        assert_eq!(begin(&exe), Start::Plain);
        // Nothing to set back once settled.
        survived(&exe);
        assert!(!beside(&exe, "trial").exists());
        let _ = std::fs::remove_dir_all(exe.parent().unwrap());
    }

    #[test]
    fn a_version_that_keeps_crashing_is_rolled_back() {
        let exe = scratch("crash");
        std::fs::write(&exe, "old").unwrap();
        let new = beside(&exe, "new");
        std::fs::write(&new, "broken").unwrap();
        swap(&exe, &new).unwrap();
        // Each start dies before it proves itself.
        assert_eq!(begin(&exe), Start::Updated);
        assert_eq!(begin(&exe), Start::Trial);
        assert_eq!(begin(&exe), Start::RollBack);
        put_back(&exe).unwrap();
        assert_eq!(read(&exe), "old");
        assert_eq!(read(&beside(&exe, "new")), "broken");
        assert!(!beside(&exe, "old").exists() && !beside(&exe, "trial").exists());
        // The version put back starts as usual, and deletes the broken one.
        assert_eq!(begin(&exe), Start::Plain);
        assert!(!beside(&exe, "new").exists());
        let _ = std::fs::remove_dir_all(exe.parent().unwrap());
    }

    #[test]
    fn a_new_update_starts_a_new_trial() {
        let exe = scratch("again");
        std::fs::write(&exe, "a").unwrap();
        std::fs::write(beside(&exe, "new"), "b").unwrap();
        swap(&exe, &beside(&exe, "new")).unwrap();
        assert_eq!(begin(&exe), Start::Updated);
        assert_eq!(begin(&exe), Start::Trial);
        // Updated again before the trial ended: the count is the next version's, from nought.
        std::fs::write(beside(&exe, "new"), "c").unwrap();
        swap(&exe, &beside(&exe, "new")).unwrap();
        assert_eq!(read(&beside(&exe, "old")), "b");
        assert_eq!(begin(&exe), Start::Updated);
        let _ = std::fs::remove_dir_all(exe.parent().unwrap());
    }
}
