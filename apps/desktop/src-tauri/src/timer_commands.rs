//! Tauri side of the timer (M6): commands, events and OS notifications. The
//! logic lives in `timer_service`.

use crate::sessions::Mode;
use crate::storage::DayChanged;
use crate::timer::{Notice, Plan};
use crate::timer_service::{BlockRef, Link, SessionDto, Sink, TimerService, TimerStatus};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_notification::NotificationExt;

#[derive(Clone, Serialize)]
struct SessionsChanged {
    date: String,
}

/// Emits the contract's events and shows OS notifications.
pub struct TauriSink(pub AppHandle);

impl TauriSink {
    fn emit<S: Serialize + Clone>(&self, event: &str, payload: S) {
        if let Err(e) = self.0.emit(event, payload) {
            eprintln!("emitting {event}: {e}");
        }
    }

    fn window_in_foreground(&self) -> bool {
        self.0
            .get_webview_window("main")
            .is_some_and(|w| w.is_focused().unwrap_or(false))
    }
}

impl Sink for TauriSink {
    fn timer_changed(&self, status: &TimerStatus) {
        self.emit("timer-changed", status.clone());
    }

    fn sessions_changed(&self, date: &str) {
        self.emit(
            "sessions-changed",
            SessionsChanged {
                date: date.to_string(),
            },
        );
    }

    fn day_changed(&self, changed: DayChanged) {
        self.emit("day-changed", changed);
    }

    /// Only when the window is in the background; in front, the UI shows it.
    fn notify(&self, notice: Notice) {
        if self.window_in_foreground() {
            return;
        }
        let (title, body) = notice_text(notice);
        if let Err(e) = self
            .0
            .notification()
            .builder()
            .title(title)
            .body(body)
            .show()
        {
            eprintln!("showing notification: {e}");
        }
    }
}

/// Title and body of the OS notification for a timer notice.
fn notice_text(notice: Notice) -> (String, String) {
    match notice {
        Notice::FocusDone { break_min } => (
            "Focus done".to_string(),
            format!("Take a {break_min} min break"),
        ),
        Notice::NextFocus { cycle, cycles } => (
            "Break over".to_string(),
            format!("Focus {cycle} of {cycles}"),
        ),
        Notice::RunDone { cycles } => (
            if cycles == 1 {
                "1 pomodoro done".to_string()
            } else {
                format!("{cycles} pomodoros done")
            },
            "Nice work".to_string(),
        ),
    }
}

#[tauri::command]
pub fn timer_status(timer: State<TimerService>) -> TimerStatus {
    timer.status()
}

#[tauri::command]
pub fn timer_start(
    timer: State<TimerService>,
    date: String,
    block: BlockRef,
    mode: Mode,
    plan: Option<Plan>,
) -> Result<TimerStatus, String> {
    timer.start(&date, block, mode, plan)
}

#[tauri::command]
pub fn timer_skip_break(timer: State<TimerService>) -> Result<TimerStatus, String> {
    timer.skip_break()
}

#[tauri::command]
pub fn timer_stop(timer: State<TimerService>) -> Result<TimerStatus, String> {
    timer.stop()
}

#[tauri::command]
pub fn timer_resolve_stale(
    timer: State<TimerService>,
    keep_running: bool,
) -> Result<TimerStatus, String> {
    timer.resolve_stale(keep_running)
}

#[tauri::command]
pub fn timer_sessions(timer: State<TimerService>, date: String) -> Result<Vec<SessionDto>, String> {
    timer.sessions(&date)
}

#[tauri::command]
pub fn timer_delete_session(
    timer: State<TimerService>,
    date: String,
    start: String,
) -> Result<Vec<SessionDto>, String> {
    timer.delete_session(&date, &start)
}

#[tauri::command]
pub fn timer_relink(
    timer: State<TimerService>,
    date: String,
    links: Vec<Link>,
) -> Result<(), String> {
    timer.relink(&date, links)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_texts() {
        assert_eq!(
            notice_text(Notice::FocusDone { break_min: 5 }),
            ("Focus done".to_string(), "Take a 5 min break".to_string())
        );
        assert_eq!(
            notice_text(Notice::NextFocus {
                cycle: 2,
                cycles: 4
            }),
            ("Break over".to_string(), "Focus 2 of 4".to_string())
        );
        assert_eq!(
            notice_text(Notice::RunDone { cycles: 4 }).0,
            "4 pomodoros done"
        );
        assert_eq!(
            notice_text(Notice::RunDone { cycles: 1 }).0,
            "1 pomodoro done"
        );
    }
}
