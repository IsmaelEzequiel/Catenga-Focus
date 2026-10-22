//! Window and app lifecycle (M12): closing the window hides it, the app keeps
//! running in the background, and quitting goes through the frontend first so
//! pending notes are flushed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, RunEvent, State};

/// How long the frontend has to flush before the app quits anyway.
const QUIT_GRACE: Duration = Duration::from_secs(3);

/// A quit was requested and the frontend has not answered yet.
#[derive(Default)]
pub struct QuitState(AtomicBool);

/// Shows, un-minimizes and focuses the main window.
pub fn show_main(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    for result in [window.show(), window.unminimize(), window.set_focus()] {
        if let Err(e) = result {
            eprintln!("showing the window: {e}");
        }
    }
}

/// Asks the frontend to flush its notes and call `app_exit`. If it does not
/// answer in time (window gone, page hung) the app exits anyway.
pub fn request_quit(app: &AppHandle) {
    let state = app.state::<QuitState>();
    if state.0.swap(true, Ordering::SeqCst) {
        // Already asked; the first request's fallback is still pending.
        return;
    }
    if let Err(e) = app.emit("app-quit-requested", ()) {
        eprintln!("emitting app-quit-requested: {e}");
    }
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(QUIT_GRACE);
        if app.state::<QuitState>().0.swap(false, Ordering::SeqCst) {
            app.exit(0);
        }
    });
}

/// Run-loop hook: Dock click reopens the window; a user-initiated exit (Cmd+Q,
/// app menu Quit) is held back until the frontend has flushed.
pub fn on_run_event(app: &AppHandle, event: &RunEvent) {
    match event {
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => show_main(app),
        RunEvent::ExitRequested { api, code, .. } if code.is_none() => {
            api.prevent_exit();
            request_quit(app);
        }
        _ => {}
    }
}

/// The frontend closed its window: hide it, keep running. Without a way back
/// (no tray item, and no Dock to reopen from) it quits instead; the frontend has
/// already flushed its notes.
#[tauri::command]
pub fn app_hide(app: AppHandle) {
    if !cfg!(target_os = "macos") && app.try_state::<crate::tray::TrayReady>().is_none() {
        app.exit(0);
        return;
    }
    if let Some(window) = app.get_webview_window("main")
        && let Err(e) = window.hide()
    {
        eprintln!("hiding the window: {e}");
    }
}

#[tauri::command]
pub fn app_show(app: AppHandle) {
    show_main(&app);
}

/// The frontend flushed; quit for real.
#[tauri::command]
pub fn app_exit(app: AppHandle, state: State<QuitState>) {
    state.0.store(false, Ordering::SeqCst);
    app.exit(0);
}

/// The frontend could not save its notes and stays open: drop the fallback exit.
#[tauri::command]
pub fn app_cancel_quit(state: State<QuitState>) {
    state.0.store(false, Ordering::SeqCst);
}
