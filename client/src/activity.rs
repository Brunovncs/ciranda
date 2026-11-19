//! What you are playing, for "Playing VALORANT" beside your name: the programs running on this
//! computer, looked at every few seconds, matched against games known by their program's name and
//! against the folders where the stores install games (Steam's `steamapps\common\<Game>`, Epic,
//! Riot, the Xbox app, GOG, EA, Ubisoft), whose folder names the game.
//!
//! Only the name of the game ever leaves the computer, and only while `share_activity` is on; the
//! server keeps it while you are online and writes the hours down for the year in review.

use crate::prefs::prefs;
use gpui::{App, Global};
use std::time::Duration;

/// How often the running programs are looked at: a game is not started for less.
const EVERY: Duration = Duration::from_secs(15);

#[derive(Default)]
struct Playing(Option<String>);

impl Global for Playing {}

/// What you are playing, whether or not it is shared.
pub fn current(cx: &App) -> Option<String> {
    cx.try_global::<Playing>().and_then(|p| p.0.clone())
}

/// What the servers are told you are playing: nothing while sharing is off.
pub fn shared(cx: &App) -> Option<String> {
    if prefs(cx).share_activity { current(cx) } else { None }
}

/// Tells every server you are on in full what you play now, after a game came or went or the
/// setting changed.
pub fn announce(cx: &mut App) {
    for session in crate::elsewhere::sessions(cx) {
        session.update(cx, |s, cx| s.say_activity(cx));
    }
}

/// Starts looking. Nothing is looked at on systems this cannot read.
pub fn start(cx: &mut App) {
    cx.set_global(Playing::default());
    if !cfg!(windows) {
        return;
    }
    cx.spawn(async move |cx| {
        loop {
            let was = cx.update(|cx| current(cx));
            let now = cx.background_executor().spawn(async move { scan(was.as_deref()) }).await;
            let changed = cx.update(|cx| {
                if current(cx) == now {
                    return false;
                }
                log::info!("activity: {}", now.as_deref().unwrap_or("nothing"));
                cx.set_global(Playing(now));
                true
            });
            if changed {
                cx.update(announce);
            }
            cx.background_executor().timer(EVERY).await;
        }
    })
    .detach();
}

/// The game running now. The one already said keeps its place while it still runs, so two games
/// open at once don't take turns.
fn scan(was: Option<&str>) -> Option<String> {
    choose(running_games(), was)
}

fn choose(found: Vec<String>, was: Option<&str>) -> Option<String> {
    if let Some(was) = was
        && found.iter().any(|g| g == was)
    {
        return Some(was.to_string());
    }
    found.into_iter().next()
}

/// Every game running, the ones known by their program first.
#[cfg(windows)]
fn running_games() -> Vec<String> {
    let mut known = Vec::new();
    let mut by_folder = Vec::new();
    for (exe, path) in processes() {
        if let Some(name) = known_exe(&exe, &path) {
            if !known.contains(&name) {
                known.push(name);
            }
        } else if let Some(name) = store_folder(&exe, &path)
            && !by_folder.contains(&name)
        {
            by_folder.push(name);
        }
    }
    known.extend(by_folder);
    known
}

#[cfg(not(windows))]
fn running_games() -> Vec<String> {
    Vec::new()
}

/// Every process's program name and full path (empty when it can't be read: a process of
/// another user or a protected one, none of them games you are playing).
#[cfg(windows)]
fn processes() -> Vec<(String, String)> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
    let mut out = Vec::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snap, &mut entry) != 0;
        while more {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            let exe = String::from_utf16_lossy(&entry.szExeFile[..len]);
            let mut path = String::new();
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, entry.th32ProcessID);
            if !process.is_null() {
                let mut buf = [0u16; 1024];
                let mut size = buf.len() as u32;
                if QueryFullProcessImageNameW(process, 0, buf.as_mut_ptr(), &mut size) != 0 {
                    path = String::from_utf16_lossy(&buf[..size as usize]);
                }
                CloseHandle(process);
            }
            out.push((exe, path));
            more = Process32NextW(snap, &mut entry) != 0;
        }
        CloseHandle(snap);
    }
    out
}

/// Games known by their program, whatever folder they are in. Lowercase program names.
const KNOWN: &[(&str, &str)] = &[
    ("valorant-win64-shipping.exe", "VALORANT"),
    ("enshrouded.exe", "Enshrouded"),
    ("cs2.exe", "Counter-Strike 2"),
    ("league of legends.exe", "League of Legends"),
    ("fortniteclient-win64-shipping.exe", "Fortnite"),
    ("rocketleague.exe", "Rocket League"),
    ("gta5.exe", "Grand Theft Auto V"),
    ("gta5_enhanced.exe", "Grand Theft Auto V"),
    ("dota2.exe", "Dota 2"),
    ("r5apex.exe", "Apex Legends"),
    ("r5apex_dx12.exe", "Apex Legends"),
    ("overwatch.exe", "Overwatch 2"),
    ("eldenring.exe", "Elden Ring"),
    ("bg3.exe", "Baldur's Gate 3"),
    ("bg3_dx11.exe", "Baldur's Gate 3"),
    ("lethal company.exe", "Lethal Company"),
    ("terraria.exe", "Terraria"),
    ("stardew valley.exe", "Stardew Valley"),
    ("minecraft.windows.exe", "Minecraft"),
    ("rustclient.exe", "Rust"),
    ("tslgame.exe", "PUBG: BATTLEGROUNDS"),
    ("helldivers2.exe", "Helldivers 2"),
    ("palworld-win64-shipping.exe", "Palworld"),
    ("phasmophobia.exe", "Phasmophobia"),
    ("among us.exe", "Among Us"),
    ("rainbowsix.exe", "Rainbow Six Siege"),
    ("rainbowsix_vulkan.exe", "Rainbow Six Siege"),
    ("marvel-win64-shipping.exe", "Marvel Rivals"),
    ("repo.exe", "R.E.P.O."),
    ("deadlock.exe", "Deadlock"),
    ("project8.exe", "Deadlock"),
    ("sotgame.exe", "Sea of Thieves"),
    ("destiny2.exe", "Destiny 2"),
    ("warframe.x64.exe", "Warframe"),
    ("cod.exe", "Call of Duty"),
    ("hollow knight silksong.exe", "Hollow Knight: Silksong"),
];

fn known_exe(exe: &str, path: &str) -> Option<String> {
    let exe = exe.to_lowercase();
    if let Some((_, name)) = KNOWN.iter().find(|(e, _)| *e == exe) {
        return Some((*name).to_string());
    }
    // Java edition: java run by the Minecraft launcher, from its own runtime folder.
    if (exe == "javaw.exe" || exe == "java.exe") && path.to_lowercase().contains("minecraft") {
        return Some("Minecraft".into());
    }
    None
}

/// Where the stores put games, as the folder above the game's own (lowercase, `\`-ended), and
/// folders under them that are not games.
const STORES: &[&str] = &[
    "\\steamapps\\common\\",
    "\\epic games\\",
    "\\riot games\\",
    "\\xboxgames\\",
    "\\gog galaxy\\games\\",
    "\\ea games\\",
    "\\ubisoft game launcher\\games\\",
];
const NOT_GAMES: &[&str] = &[
    "steamworks shared",
    "steamvr",
    "steam controller configs",
    "wallpaper_engine",
    "wallpaper engine",
    "lossless scaling",
    "soundpad",
    "obs studio",
    "spacewar",
    "launcher",
    "directxredist",
    "epiconlineservices",
    "riot client",
    "gamesave",
];
/// Programs inside a game's folder that run without the game: crash reporters, anti-cheat,
/// launchers and installers.
const HELPERS: &[&str] =
    &["crash", "report", "anticheat", "easyanticheat", "be_service", "beservice", "launcher", "setup", "redist", "unins", "updater"];

fn store_folder(exe: &str, path: &str) -> Option<String> {
    let lower = path.to_lowercase();
    let exe = exe.to_lowercase();
    if HELPERS.iter().any(|h| exe.contains(h)) || lower.starts_with("c:\\windows\\") {
        return None;
    }
    // Proton and the runtimes Steam keeps under common are tools, whatever they are called.
    for store in STORES {
        let Some(at) = lower.find(store) else { continue };
        let rest = &path[at + store.len()..];
        let folder = rest.split('\\').next().filter(|f| !f.is_empty() && rest.len() > f.len())?;
        let key = folder.to_lowercase();
        if NOT_GAMES.iter().any(|n| key == *n) || key.starts_with("proton") || key.starts_with("steamlinuxruntime") {
            return None;
        }
        return Some(pretty(folder));
    }
    None
}

/// A folder name as a game's: underscores to spaces, nothing else touched.
fn pretty(folder: &str) -> String {
    folder.replace('_', " ").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_programs_name_their_game() {
        assert_eq!(known_exe("VALORANT-Win64-Shipping.exe", "").as_deref(), Some("VALORANT"));
        assert_eq!(
            known_exe("enshrouded.exe", "D:\\SteamLibrary\\steamapps\\common\\Enshrouded\\enshrouded.exe").as_deref(),
            Some("Enshrouded")
        );
        assert_eq!(
            known_exe("javaw.exe", "C:\\Users\\x\\AppData\\Roaming\\.minecraft\\runtime\\bin\\javaw.exe").as_deref(),
            Some("Minecraft")
        );
        assert_eq!(known_exe("javaw.exe", "C:\\Program Files\\Java\\bin\\javaw.exe"), None);
        assert_eq!(known_exe("notepad.exe", ""), None);
    }

    #[test]
    fn a_store_folder_names_a_game_it_does_not_know() {
        let game = |p: &str| store_folder(p.rsplit('\\').next().unwrap(), p);
        assert_eq!(
            game("D:\\SteamLibrary\\steamapps\\common\\Deep Rock Galactic\\FSD\\Binaries\\Win64\\FSD-Win64-Shipping.exe").as_deref(),
            Some("Deep Rock Galactic")
        );
        assert_eq!(game("C:\\Program Files\\Epic Games\\RocketLeague\\Binaries\\Win64\\RocketLeague.exe").as_deref(), Some("RocketLeague"));
        assert_eq!(game("C:\\XboxGames\\Forza_Horizon_5\\Content\\ForzaHorizon5.exe").as_deref(), Some("Forza Horizon 5"));
        assert_eq!(game("C:\\Riot Games\\Riot Client\\RiotClientServices.exe"), None);
        assert_eq!(game("C:\\Steam\\steamapps\\common\\wallpaper_engine\\wallpaper64.exe"), None);
        assert_eq!(game("C:\\Steam\\steamapps\\common\\Proton 9.0\\proton.exe"), None);
        assert_eq!(game("C:\\Steam\\steamapps\\common\\Some Game\\UnityCrashHandler64.exe"), None);
        assert_eq!(game("C:\\Program Files\\Ciranda\\ciranda.exe"), None);
    }

    #[test]
    fn the_game_already_said_keeps_its_place() {
        let found = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(choose(found(&["A", "B"]), Some("B")).as_deref(), Some("B"));
        assert_eq!(choose(found(&["A"]), Some("B")).as_deref(), Some("A"));
        assert_eq!(choose(found(&[]), Some("B")), None);
    }
}
