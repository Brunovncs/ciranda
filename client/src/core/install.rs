//! The installed copy's entry in Windows' installed apps, written by the installer
//! (`packaging/ciranda.iss`). An update swaps the program in place without running the installer,
//! so the program itself keeps the version shown there up to date.

/// Inno Setup's key for the installer's `AppId`.
#[cfg(windows)]
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{A913E7F2-9D27-43F1-8B65-4F2E8D2DF207}_is1";

/// Sets the installed apps entry's version to this one, when this is the copy the installer put
/// there and the entry says otherwise. A copy run from anywhere else leaves it alone.
#[cfg(windows)]
pub fn note_version() {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegGetValueW, RegSetKeyValueW};
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let key = wide(UNINSTALL_KEY);
    let read = |value: &str| -> Option<String> {
        let mut buf = vec![0u16; 1024];
        let mut len = (buf.len() * 2) as u32;
        // SAFETY: `len` is the buffer's size in bytes; the value is written within it.
        let ok = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                wide(value).as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut len,
            )
        };
        (ok == 0).then(|| String::from_utf16_lossy(&buf[..(len as usize / 2)]).trim_end_matches('\0').to_string())
    };
    let Some(location) = read("InstallLocation") else { return };
    let Some(here) = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_string_lossy().into_owned())) else { return };
    let trim = |p: &str| p.trim_end_matches('\\').to_lowercase();
    let version = env!("CARGO_PKG_VERSION");
    if trim(&location) != trim(&here) || read("DisplayVersion").as_deref() == Some(version) {
        return;
    }
    let data = wide(version);
    // SAFETY: the value is a nul-terminated UTF-16 string of the length given in bytes.
    let ok = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            wide("DisplayVersion").as_ptr(),
            REG_SZ,
            data.as_ptr().cast(),
            (data.len() * 2) as u32,
        )
    };
    if ok != 0 {
        log::warn!("installed apps entry: the version could not be updated ({ok})");
    }
}

#[cfg(not(windows))]
pub fn note_version() {}
