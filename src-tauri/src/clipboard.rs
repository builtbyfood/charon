//! Clipboard text for the UI. Linux goes through GTK on the main thread: the
//! clipboard plugin's arboard needs wlr-data-control on Wayland, which GNOME's
//! compositor does not offer, and its X11 fallback cannot see the clipboard of a
//! native-Wayland window. GTK is the toolkit the webview already runs on, so it
//! works on Wayland and X11 alike. Elsewhere the plugin (arboard) is fine.

use anyhow::Result;
use tauri::AppHandle;

#[cfg(target_os = "linux")]
pub async fn read_text(app: &AppHandle) -> Result<String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let cb = gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD);
        cb.request_text(move |_, text| {
            let _ = tx.send(text.map(str::to_owned));
        });
    })?;
    Ok(rx.await?.unwrap_or_default())
}

#[cfg(target_os = "linux")]
pub async fn write_text(app: &AppHandle, text: String) -> Result<()> {
    app.run_on_main_thread(move || {
        let cb = gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD);
        cb.set_text(&text);
        // Hand it to the desktop's clipboard manager so it outlives Charon.
        cb.store();
    })?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub async fn read_text(app: &AppHandle) -> Result<String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    Ok(app.clipboard().read_text()?)
}

#[cfg(not(target_os = "linux"))]
pub async fn write_text(app: &AppHandle, text: String) -> Result<()> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    Ok(app.clipboard().write_text(text)?)
}
