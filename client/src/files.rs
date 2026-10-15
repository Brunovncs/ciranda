//! Picking a file to send, with the dialog showing only the kind asked for. GPUI's own dialog has
//! no filters, so on Windows this one is opened here; elsewhere it is GPUI's, showing everything.

use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// Pictures and videos.
    Media,
    /// Documents, with every other file one choice away.
    Document,
}

/// The file picked, or None if the dialog was closed.
pub async fn pick(kind: Kind, cx: &mut gpui::AsyncApp) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let _ = &cx;
        let (tx, rx) = futures::channel::oneshot::channel();
        // The owner is the window clicked a moment ago, which is still the one in front.
        let owner = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() }.0 as isize;
        std::thread::Builder::new()
            .name("ciranda-file-dialog".into())
            .spawn(move || {
                let _ = tx.send(windows_dialog(kind, owner).unwrap_or_else(|e| {
                    log::warn!("file dialog: {e}");
                    None
                }));
            })
            .ok()?;
        rx.await.ok().flatten()
    }
    #[cfg(not(windows))]
    {
        use gpui::{App, PathPromptOptions};
        let _ = kind;
        let paths = cx.update(|cx: &mut App| {
            cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: None })
        });
        paths.await.ok()?.ok()??.into_iter().next()
    }
}

#[cfg(windows)]
fn windows_dialog(kind: Kind, owner: isize) -> windows::core::Result<Option<PathBuf>> {
    use windows::Win32::Foundation::{ERROR_CANCELLED, HWND};
    use windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize,
    };
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH};
    use windows::core::{HRESULT, HSTRING, PCWSTR};

    let names: Vec<(HSTRING, HSTRING)> = match kind {
        Kind::Media => vec![
            (tr!("Pictures and videos", "Imagens e vídeos").into(), "*.png;*.jpg;*.jpeg;*.gif;*.webp;*.mp4;*.webm".into()),
            (tr!("All files", "Todos os arquivos").into(), "*.*".into()),
        ],
        Kind::Document => vec![
            (
                tr!("Documents", "Documentos").into(),
                "*.pdf;*.txt;*.md;*.doc;*.docx;*.odt;*.rtf;*.xls;*.xlsx;*.ods;*.csv;*.ppt;*.pptx;*.odp;*.zip;*.7z;*.rar".into(),
            ),
            (tr!("All files", "Todos os arquivos").into(), "*.*".into()),
        ],
    };
    let specs: Vec<COMDLG_FILTERSPEC> =
        names.iter().map(|(n, s)| COMDLG_FILTERSPEC { pszName: PCWSTR(n.as_ptr()), pszSpec: PCWSTR(s.as_ptr()) }).collect();
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let result = (|| {
            let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_ALL)?;
            dialog.SetOptions(FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM)?;
            dialog.SetFileTypes(&specs)?;
            dialog.SetFileTypeIndex(1)?;
            if let Err(e) = dialog.Show(Some(HWND(owner as _))) {
                if e.code() == HRESULT::from_win32(ERROR_CANCELLED.0) {
                    return Ok(None);
                }
                return Err(e);
            }
            let item = dialog.GetResult()?;
            let name = item.GetDisplayName(SIGDN_FILESYSPATH)?;
            let path = name.to_string().map(PathBuf::from).ok();
            CoTaskMemFree(Some(name.0 as _));
            Ok(path)
        })();
        CoUninitialize();
        result
    }
}
