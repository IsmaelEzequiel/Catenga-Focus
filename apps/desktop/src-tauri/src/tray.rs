//! The menu bar (tray) item (M12): Tauri glue around `tray_model`. A ticker thread
//! recomputes the model every second while a timer runs and on the minute
//! otherwise; day, calendar and timer events wake it early.

use crate::google_commands;
use crate::lifecycle;
use crate::storage::DayStore;
use crate::timer_service::{TimerPhase, TimerService, TimerStatus};
use crate::tray_model::{self, BlockIn, EventIn, Item, TimerIn, TrayModel};
use jiff::{Timestamp, Zoned};
use serde::Serialize;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tauri::image::Image;
use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Emitter, Listener, Manager};

const TRAY_ID: &str = "main";
#[cfg(target_os = "macos")]
/// Raw RGBA of `icons/tray/tray@2x.png` (64x64), so no PNG decoder is needed.
const ICON_RGBA: &[u8] = include_bytes!("../icons/tray/tray@2x.rgba");
#[cfg(target_os = "macos")]
const ICON_SIDE: u32 = 64;

/// Managed once the tray item exists: only then is hiding the window safe on
/// Windows and Linux, where the tray is the way back.
pub struct TrayReady;

/// What the menu showed last: the click handler maps ids back to it.
#[derive(Default)]
struct Shown {
    date: String,
    next: Vec<Item>,
    model: Option<TrayModel>,
    /// The menu's first line, re-labelled every second while a timer runs.
    header: Option<MenuItem<tauri::Wry>>,
}

#[derive(Default)]
struct TrayState(Mutex<Shown>);

impl TrayState {
    fn lock(&self) -> MutexGuard<'_, Shown> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct OpenBlock {
    date: String,
    start: u16,
    end: u16,
    title: String,
    ghost: bool,
}

/// The timer as the model wants it; a stale (not running) timer counts as idle.
pub fn timer_in(status: &TimerStatus, now: Timestamp) -> TimerIn {
    let TimerStatus::Running {
        block,
        phase,
        session_start,
        phase_ends_at,
        mode,
        cycle,
        cycles,
        ..
    } = status
    else {
        return TimerIn::Idle;
    };
    let secs_to = |iso: &Option<String>| {
        iso.as_deref()
            .and_then(|s| s.parse::<Timestamp>().ok())
            .map_or(0, |t| t.as_second() - now.as_second())
    };
    let cycle = cycle.zip(*cycles);
    match (mode, phase) {
        (crate::sessions::Mode::Free, _) => TimerIn::Free {
            title: block.title.clone(),
            elapsed_secs: session_start
                .parse::<Timestamp>()
                .map_or(0, |t| now.as_second() - t.as_second()),
        },
        (_, TimerPhase::Focus) => TimerIn::Focus {
            title: block.title.clone(),
            remaining_secs: secs_to(phase_ends_at),
            cycle,
        },
        (_, TimerPhase::Break) => TimerIn::Break {
            title: block.title.clone(),
            remaining_secs: secs_to(phase_ends_at),
            cycle,
        },
    }
}

fn today() -> (String, u16, Zoned) {
    let now = Zoned::now();
    let date = format!("{:04}-{:02}-{:02}", now.year(), now.month(), now.day());
    let minute = u16::try_from(i32::from(now.hour()) * 60 + i32::from(now.minute())).unwrap_or(0);
    (date, minute, now)
}

fn collect(app: &AppHandle) -> (String, TrayModel) {
    let (date, minute, now) = today();
    let blocks: Vec<BlockIn> = match app.state::<Arc<DayStore>>().load(&date) {
        Ok(day) => day
            .blocks
            .iter()
            .map(|b| BlockIn {
                start: b.start,
                end: b.end,
                title: b.title.clone(),
            })
            .collect(),
        Err(e) => {
            eprintln!("tray: reading today: {e}");
            Vec::new()
        }
    };
    let events: Vec<EventIn> = google_commands::cached_events(app, &date)
        .into_iter()
        .map(|e| EventIn {
            id: e.id,
            start: u16::try_from(e.start).unwrap_or(0),
            end: u16::try_from(e.end).unwrap_or(1440),
            title: e.title,
        })
        .collect();
    let timer = timer_in(&app.state::<TimerService>().status(), now.timestamp());
    (date, tray_model::build(minute, &blocks, &events, &timer))
}

/// The menu and its first line (kept so the live timer text can change in place).
fn build_menu(
    app: &AppHandle,
    model: &TrayModel,
) -> tauri::Result<(Menu<tauri::Wry>, MenuItem<tauri::Wry>)> {
    let menu = Menu::new(app)?;
    let disabled = |id: &str, text: String| MenuItem::with_id(app, id, text, false, None::<&str>);
    let header = disabled("header", model.header())?;
    menu.append(&header)?;
    if let Some(label) = model.start_now_label() {
        menu.append(&MenuItem::with_id(
            app,
            "start-now",
            label,
            true,
            None::<&str>,
        )?)?;
    }
    if model.timer_running {
        menu.append(&MenuItem::with_id(
            app,
            "stop",
            "Stop timer",
            true,
            None::<&str>,
        )?)?;
        if model.on_break {
            menu.append(&MenuItem::with_id(
                app,
                "skip-break",
                "Skip break",
                true,
                None::<&str>,
            )?)?;
        }
    }
    let next = model.listed_next();
    if !next.is_empty() {
        menu.append(&PredefinedMenuItem::separator(app)?)?;
        menu.append(&disabled("next-label", "Next today".to_string())?)?;
        for (i, item) in next.iter().enumerate() {
            let id = format!("item:{i}");
            menu.append(&MenuItem::with_id(
                app,
                id,
                item.label(),
                true,
                None::<&str>,
            )?)?;
        }
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(
        app,
        "open",
        "Open Catenga Focus",
        true,
        Some("CmdOrCtrl+O"),
    )?)?;
    menu.append(&MenuItem::with_id(
        app,
        "quit",
        "Quit",
        true,
        Some("CmdOrCtrl+Q"),
    )?)?;
    Ok((menu, header))
}

/// Pushes `model` to the tray: title and tooltip when they changed, the menu when
/// its contents did.
fn apply(app: &AppHandle, tray: &TrayIcon, date: String, model: TrayModel) {
    let state = app.state::<TrayState>();
    let previous = state.lock().model.clone();
    if previous.as_ref().is_none_or(|p| p.title != model.title) {
        if let Err(e) = tray.set_title(model.title.clone()) {
            eprintln!("tray title: {e}");
        }
        if let Err(e) = tray.set_tooltip(Some(model.tooltip.clone())) {
            eprintln!("tray tooltip: {e}");
        }
    }
    let menu_changed = previous.as_ref().is_none_or(|p| !p.same_menu(&model));
    if menu_changed {
        // Record what the menu holds before showing it, so a click can't miss it.
        {
            let mut shown = state.lock();
            shown.date.clone_from(&date);
            shown.next = model.listed_next().to_vec();
        }
        match build_menu(app, &model) {
            Ok((menu, header)) => {
                if let Err(e) = tray.set_menu(Some(menu)) {
                    eprintln!("tray menu: {e}");
                }
                state.lock().header = Some(header);
            }
            Err(e) => eprintln!("tray menu: {e}"),
        }
    } else if previous
        .as_ref()
        .is_some_and(|p| p.header() != model.header())
    {
        let header = state.lock().header.clone();
        if let Some(header) = header
            && let Err(e) = header.set_text(model.header())
        {
            eprintln!("tray header: {e}");
        }
    }
    state.lock().model = Some(model);
}

fn on_menu(app: &AppHandle, event: MenuEvent) {
    let id = event.id().as_ref();
    match id {
        "open" => lifecycle::show_main(app),
        "quit" => lifecycle::request_quit(app),
        "start-now" => {
            lifecycle::show_main(app);
            if let Err(e) = app.emit("start-now", ()) {
                eprintln!("emitting start-now: {e}");
            }
        }
        "stop" => {
            if let Err(e) = app.state::<TimerService>().stop() {
                eprintln!("tray: stopping the timer: {e}");
            }
        }
        "skip-break" => {
            if let Err(e) = app.state::<TimerService>().skip_break() {
                eprintln!("tray: skipping the break: {e}");
            }
        }
        _ => {
            let Some(index) = id
                .strip_prefix("item:")
                .and_then(|i| i.parse::<usize>().ok())
            else {
                return;
            };
            let payload = {
                let shown = app.state::<TrayState>().lock().next.get(index).cloned();
                let date = app.state::<TrayState>().lock().date.clone();
                shown.map(|i| OpenBlock {
                    date,
                    start: i.start,
                    end: i.end,
                    title: i.title,
                    ghost: i.ghost,
                })
            };
            let Some(payload) = payload else { return };
            lifecycle::show_main(app);
            if let Err(e) = app.emit("open-block", payload) {
                eprintln!("emitting open-block: {e}");
            }
        }
    }
}

/// Time until the ticker should look again.
fn wait_for(running: bool, now: &Zoned) -> Duration {
    let sub = u64::try_from(now.subsec_nanosecond()).unwrap_or(0);
    let to_second = 1_000_000_000 - sub;
    if running {
        Duration::from_nanos(to_second)
    } else {
        // To the next minute boundary, plus a hair so the minute has changed.
        let secs = 59 - u64::try_from(now.second()).unwrap_or(0);
        Duration::from_nanos(to_second) + Duration::from_secs(secs) + Duration::from_millis(20)
    }
}

fn listen_for_changes(app: &AppHandle, wake: &Sender<()>) {
    for name in ["day-changed", "calendar-changed"] {
        let wake = wake.clone();
        app.listen(name, move |event| {
            // Only today's day/calendar matters to the menu bar.
            let date = serde_json::from_str::<serde_json::Value>(event.payload())
                .ok()
                .and_then(|v| v.get("date").and_then(|d| d.as_str().map(str::to_string)));
            if date.is_none_or(|d| d == today().0) {
                let _ = wake.send(());
            }
        });
    }
    let wake = wake.clone();
    app.listen("timer-changed", move |_| {
        let _ = wake.send(());
    });
}

/// Builds the tray item and starts its ticker. Call at the end of `setup`, once
/// the day store, timer, index and app database are managed.
pub fn start(app: &AppHandle) -> tauri::Result<()> {
    app.manage(TrayState::default());
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .show_menu_on_left_click(true)
        .on_menu_event(on_menu);
    #[cfg(target_os = "macos")]
    {
        builder = builder
            .icon(Image::new(ICON_RGBA, ICON_SIDE, ICON_SIDE))
            .icon_as_template(true);
    }
    // Windows and Linux trays are often dark: the colored app icon reads on both.
    #[cfg(not(target_os = "macos"))]
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(Image::from(icon.clone()));
    }
    let tray = builder.build(app)?;
    app.manage(TrayReady);

    let (wake, rx) = mpsc::channel::<()>();
    listen_for_changes(app, &wake);
    let app = app.clone();
    std::thread::spawn(move || {
        loop {
            let (date, model) = collect(&app);
            let running = model.timer_running;
            apply(&app, &tray, date, model);
            match rx.recv_timeout(wait_for(running, &Zoned::now())) {
                Ok(()) => {
                    // A burst of events is one refresh.
                    while rx.try_recv().is_ok() {}
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::Mode;
    use crate::timer_service::BlockRef;

    fn block() -> BlockRef {
        BlockRef {
            start: 540,
            end: 600,
            title: "Rust".to_string(),
            tags: vec![],
            project: None,
        }
    }

    fn running(mode: Mode, phase: TimerPhase, ends: Option<&str>) -> TimerStatus {
        TimerStatus::Running {
            date: "2026-10-22".to_string(),
            block: block(),
            mode,
            phase,
            session_start: "2026-10-22T09:00:00-03:00".to_string(),
            phase_ends_at: ends.map(str::to_string),
            phase_start: "2026-10-22T09:00:00-03:00".to_string(),
            cycle: None,
            cycles: None,
            focus_minutes: None,
            break_minutes: None,
        }
    }

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn idle_and_stale_are_idle() {
        let now = ts("2026-10-22T09:10:00-03:00");
        assert_eq!(timer_in(&TimerStatus::Idle, now), TimerIn::Idle);
        let stale = TimerStatus::Stale {
            date: "2026-10-22".to_string(),
            block: block(),
            mode: Mode::Free,
            session_start: "2026-10-22T09:00:00-03:00".to_string(),
            last_seen: "2026-10-22T09:05:00-03:00".to_string(),
        };
        assert_eq!(timer_in(&stale, now), TimerIn::Idle);
    }

    #[test]
    fn free_timer_counts_up_from_the_session_start() {
        let s = running(Mode::Free, TimerPhase::Focus, None);
        assert_eq!(
            timer_in(&s, ts("2026-10-22T09:12:34-03:00")),
            TimerIn::Free {
                title: "Rust".to_string(),
                elapsed_secs: 754
            }
        );
    }

    #[test]
    fn pomodoro_counts_down_to_the_phase_end() {
        let ends = Some("2026-10-22T09:25:00-03:00");
        let now = ts("2026-10-22T09:20:00-03:00");
        assert_eq!(
            timer_in(&running(Mode::Pomodoro, TimerPhase::Focus, ends), now),
            TimerIn::Focus {
                title: "Rust".to_string(),
                remaining_secs: 300,
                cycle: None
            }
        );
        assert_eq!(
            timer_in(&running(Mode::Pomodoro, TimerPhase::Break, ends), now),
            TimerIn::Break {
                title: "Rust".to_string(),
                remaining_secs: 300,
                cycle: None
            }
        );
    }

    #[test]
    fn ticker_waits_a_second_while_running_and_to_the_minute_otherwise() {
        let now: Zoned = "2026-10-22T09:10:30.250-03:00[America/Sao_Paulo]"
            .parse()
            .unwrap();
        assert_eq!(wait_for(true, &now), Duration::from_millis(750));
        assert_eq!(wait_for(false, &now), Duration::from_millis(29_770));
    }
}
