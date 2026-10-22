//! The timer service: owns the `Timer` state machine, applies its effects
//! (sidecar, `timer.json`, day-file mirror lines) and runs one background thread
//! for Pomodoro phase ends and the 60 s heartbeat. Tauri-free: events and
//! notifications go through `Sink`.

use crate::atomic;
use crate::config;
use crate::day_file::Block;
use crate::sessions::{self, Mode, Session, SessionBlock};
use crate::storage::{self, DayChanged, DayStore};
pub use crate::timer::Phase as TimerPhase;
use crate::timer::{
    self, Effect, HEARTBEAT_SECONDS, MAX_BREAK_MIN, MAX_CYCLES, MAX_FOCUS_MIN, Notice, Persisted,
    Plan, State, Timer,
};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

/// Where the service reports to the outside world.
pub trait Sink: Send + Sync {
    fn timer_changed(&self, status: &TimerStatus);
    fn sessions_changed(&self, date: &str);
    fn day_changed(&self, changed: DayChanged);
    fn notify(&self, notice: Notice);
}

/// A block as the frontend knows it (minutes since midnight).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockRef {
    pub start: u16,
    pub end: u16,
    pub title: String,
    pub tags: Vec<String>,
    /// Google calendar id of the block's project; the frontend derives it
    /// from the calendar event the block matches.
    #[serde(default)]
    pub project: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Link {
    pub from: BlockRef,
    pub to: BlockRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionDto {
    pub start: String,
    pub end: Option<String>,
    pub mode: Mode,
    pub block: BlockRef,
    /// Whole minutes; for the running session, so far.
    pub minutes: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum TimerStatus {
    Idle,
    Running {
        date: String,
        block: BlockRef,
        mode: Mode,
        phase: TimerPhase,
        session_start: String,
        phase_ends_at: Option<String>,
        /// Start of the current phase, for progress rings.
        phase_start: String,
        /// `None` (null) for free timers.
        cycle: Option<u32>,
        cycles: Option<u32>,
        focus_minutes: Option<u32>,
        break_minutes: Option<u32>,
    },
    Stale {
        date: String,
        block: BlockRef,
        mode: Mode,
        session_start: String,
        last_seen: String,
    },
}

fn parse_hhmm(s: &str) -> Option<u16> {
    let (h, m) = s.split_once(':')?;
    let (h, m): (u16, u16) = (h.parse().ok()?, m.parse().ok()?);
    (m < 60 && h <= 24).then_some(h * 60 + m)
}

fn hhmm(minutes: u16) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

fn block_ref(b: &SessionBlock) -> BlockRef {
    BlockRef {
        start: parse_hhmm(&b.start).unwrap_or(0),
        end: parse_hhmm(&b.end).unwrap_or(0),
        title: b.title.clone(),
        tags: b.tags.clone(),
        project: b.project.clone(),
    }
}

/// Validates and normalises like a saved block, so snapshots match day-file blocks.
fn session_block(b: BlockRef) -> Result<SessionBlock, String> {
    let project = b
        .project
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty());
    let n = Block {
        start: b.start,
        end: b.end,
        title: b.title,
        tags: b.tags,
        body: String::new(),
    }
    .normalized()?;
    Ok(SessionBlock {
        start: hhmm(n.start),
        end: hhmm(n.end),
        title: n.title,
        tags: n.tags,
        project,
    })
}

fn whole(ts: Timestamp) -> Timestamp {
    timer::whole_seconds(ts)
}

struct Core {
    timer: Timer,
    last_heartbeat: Timestamp,
}

struct Inner {
    root: PathBuf,
    timer_json: PathBuf,
    store: Arc<DayStore>,
    sink: Arc<dyn Sink>,
    clock: Arc<dyn Fn() -> Timestamp + Send + Sync>,
    tz: TimeZone,
    core: Mutex<Core>,
    wake: Condvar,
    shutdown: AtomicBool,
}

#[derive(Clone)]
pub struct TimerService(Arc<Inner>);

impl TimerService {
    pub fn new(
        root: PathBuf,
        timer_json: PathBuf,
        store: Arc<DayStore>,
        sink: Arc<dyn Sink>,
        clock: Arc<dyn Fn() -> Timestamp + Send + Sync>,
        tz: TimeZone,
    ) -> TimerService {
        let now = whole(clock());
        TimerService(Arc::new(Inner {
            root,
            timer_json,
            store,
            sink,
            clock,
            core: Mutex::new(Core {
                timer: Timer::new(tz.clone()),
                last_heartbeat: now,
            }),
            tz,
            wake: Condvar::new(),
            shutdown: AtomicBool::new(false),
        }))
    }

    fn lock(&self) -> MutexGuard<'_, Core> {
        self.0.core.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn now(&self) -> Timestamp {
        whole((self.0.clock)())
    }

    pub fn status(&self) -> TimerStatus {
        status_of(&self.lock().timer)
    }

    /// Pomodoro defaults from `config.json` (clamped to the limits); built-in
    /// defaults when it can't be read.
    fn default_plan(&self) -> Plan {
        let config = config::load_or_create(&self.0.root).unwrap_or_else(|e| {
            eprintln!("config unreadable, using default Pomodoro lengths: {e}");
            config::Config::default()
        });
        Plan {
            focus_min: config.pomodoro_focus_minutes.clamp(1, MAX_FOCUS_MIN),
            break_min: config.pomodoro_break_minutes.clamp(1, MAX_BREAK_MIN),
            cycles: config.pomodoro_cycles.clamp(1, MAX_CYCLES),
        }
    }

    /// `plan` is ignored for free timers; `None` uses the config defaults.
    pub fn start(
        &self,
        date: &str,
        block: BlockRef,
        mode: Mode,
        plan: Option<Plan>,
    ) -> Result<TimerStatus, String> {
        storage::validate_date(date)?;
        let mut core = self.lock();
        let now = self.now();
        if date != now.to_zoned(self.0.tz.clone()).date().to_string() {
            return Err("Timers run on today's blocks".into());
        }
        let block = session_block(block)?;
        let plan = match (mode, plan) {
            (Mode::Pomodoro, Some(plan)) => {
                plan.validate()?;
                plan
            }
            _ => self.default_plan(),
        };
        // Refuse up front when the sidecar can't be written, before anything changes.
        sessions::read(&self.0.root, date)?;
        let effects = core.timer.start(now, date, block, mode, plan);
        core.last_heartbeat = now;
        let result = self.apply(&core, effects, true);
        self.0.wake.notify_all();
        result.map(|()| status_of(&core.timer))
    }

    pub fn stop(&self) -> Result<TimerStatus, String> {
        let mut core = self.lock();
        if matches!(core.timer.state, State::Idle) {
            return Ok(TimerStatus::Idle);
        }
        let effects = core.timer.stop(self.now());
        let result = self.apply(&core, effects, true);
        self.0.wake.notify_all();
        result.map(|()| status_of(&core.timer))
    }

    /// During a break, starts the next focus now (or ends the run after the
    /// last break). Otherwise returns the status unchanged.
    pub fn skip_break(&self) -> Result<TimerStatus, String> {
        let mut core = self.lock();
        let in_break =
            matches!(&core.timer.state, State::Running(r) if r.phase == TimerPhase::Break);
        if !in_break {
            return Ok(status_of(&core.timer));
        }
        let now = self.now();
        let effects = core.timer.skip_break(now);
        core.last_heartbeat = now;
        let result = self.apply(&core, effects, true);
        self.0.wake.notify_all();
        result.map(|()| status_of(&core.timer))
    }

    pub fn resolve_stale(&self, keep_running: bool) -> Result<TimerStatus, String> {
        let mut core = self.lock();
        if !matches!(core.timer.state, State::Stale { .. }) {
            return Ok(status_of(&core.timer));
        }
        let effects = core.timer.resolve_stale(keep_running);
        if keep_running {
            core.last_heartbeat = self.now();
        }
        let result = self.apply(&core, effects, true);
        self.0.wake.notify_all();
        result.map(|()| status_of(&core.timer))
    }

    /// That day's sessions with minutes (the running one: so far).
    pub fn sessions(&self, date: &str) -> Result<Vec<SessionDto>, String> {
        let core = self.lock();
        let all = sessions::read(&self.0.root, date)?;
        Ok(self.dtos(&core, &all))
    }

    fn dtos(&self, core: &Core, all: &[Session]) -> Vec<SessionDto> {
        let now = self.now();
        let live = match &core.timer.state {
            State::Running(r) if r.phase == TimerPhase::Focus => {
                Some(core.timer.fmt(r.session_start))
            }
            _ => None,
        };
        all.iter()
            .map(|s| {
                let until = match (&s.end, &live) {
                    (Some(end), _) => sessions::parse_ts(end),
                    (None, Some(l)) if *l == s.start => Some(now),
                    (None, _) => s
                        .last_seen
                        .as_deref()
                        .and_then(sessions::parse_ts)
                        .or_else(|| sessions::parse_ts(&s.start)),
                };
                let minutes = match (sessions::parse_ts(&s.start), until) {
                    (Some(a), Some(b)) => (b.as_second() - a.as_second()).max(0) / 60,
                    _ => 0,
                };
                SessionDto {
                    start: s.start.clone(),
                    end: s.end.clone(),
                    mode: s.mode,
                    block: block_ref(&s.block),
                    minutes,
                }
            })
            .collect()
    }

    pub fn delete_session(&self, date: &str, start: &str) -> Result<Vec<SessionDto>, String> {
        let core = self.lock();
        if let State::Running(r) | State::Stale { running: r, .. } = &core.timer.state
            && r.date == date
            && core.timer.fmt(r.session_start) == start
        {
            return Err("A running session can't be deleted".into());
        }
        let left = sessions::delete(&self.0.root, date, start)?;
        match self.0.store.rewrite_mirror(date) {
            Ok(Some(changed)) => self.0.sink.day_changed(changed),
            Ok(None) => {}
            Err(e) => eprintln!("updating mirror lines for {date}: {e}"),
        }
        self.0.sink.sessions_changed(date);
        Ok(self.dtos(&core, &left))
    }

    /// Follows blocks that changed in the app: rewrites the snapshots of their
    /// sessions (and of the running timer). The day file is not touched; the
    /// save that follows writes the mirror lines.
    pub fn relink(&self, date: &str, links: Vec<Link>) -> Result<(), String> {
        let mut core = self.lock();
        let pairs = links
            .into_iter()
            .map(|l| Ok((session_block(l.from)?, session_block(l.to)?)))
            .collect::<Result<Vec<_>, String>>()?;
        let changed = sessions::relink(&self.0.root, date, &pairs)?;
        let mut running_moved = false;
        if let State::Running(r) | State::Stale { running: r, .. } = &mut core.timer.state
            && r.date == date
            && let Some((_, to)) = pairs.iter().find(|(from, _)| from.same_block(&r.block))
        {
            let next = to.following(&r.block);
            if r.block != next {
                r.block = next;
                running_moved = true;
            }
        }
        if running_moved {
            self.write_pointer(&core.timer)?;
        }
        if changed {
            self.0.sink.sessions_changed(date);
        }
        if running_moved {
            self.0.sink.timer_changed(&status_of(&core.timer));
        }
        Ok(())
    }

    /// Startup: picks up a timer that was running when the app closed.
    pub fn recover(&self) {
        let text = match fs::read_to_string(&self.0.timer_json) {
            Ok(t) => t,
            Err(e) if e.kind() == ErrorKind::NotFound => return,
            Err(e) => {
                eprintln!("reading {}: {e}", self.0.timer_json.display());
                return;
            }
        };
        let Ok(saved) = serde_json::from_str::<Persisted>(&text) else {
            eprintln!("ignoring unreadable {}", self.0.timer_json.display());
            self.remove_pointer();
            return;
        };
        let all = match sessions::read(&self.0.root, &saved.date) {
            Ok(all) => all,
            Err(e) => {
                eprintln!("timer recovery postponed: {e}");
                return;
            }
        };
        // The pointer is only trusted while its session is still open.
        let Some(open) = all
            .iter()
            .find(|s| s.start == saved.start && s.end.is_none())
        else {
            self.remove_pointer();
            return;
        };
        let now = self.now();
        let last_seen = open
            .last_seen
            .as_deref()
            .and_then(sessions::parse_ts)
            .or_else(|| sessions::parse_ts(&open.start))
            .unwrap_or(now);
        match timer::recover(self.0.tz.clone(), &saved, last_seen, now) {
            Ok((timer, effects)) => {
                let mut core = self.lock();
                core.timer = timer;
                core.last_heartbeat = now;
                if let Err(e) = self.apply(&core, effects, true) {
                    eprintln!("recovering the timer: {e}");
                }
                self.0.wake.notify_all();
            }
            Err(e) => {
                eprintln!("ignoring {}: {e}", self.0.timer_json.display());
                self.remove_pointer();
            }
        }
    }

    /// One pass of the background loop: phase transitions, then the heartbeat.
    #[cfg(test)]
    pub fn step(&self, now: Timestamp) {
        let mut core = self.lock();
        self.step_locked(&mut core, now);
    }

    fn step_locked(&self, core: &mut Core, now: Timestamp) {
        let now = whole(now);
        let effects = core.timer.tick(now);
        if !effects.is_empty() {
            core.last_heartbeat = now;
            if let Err(e) = self.apply(core, effects, true) {
                eprintln!("timer: {e}");
            }
            return;
        }
        if now.as_second() - core.last_heartbeat.as_second() >= HEARTBEAT_SECONDS
            && let Some(beat) = core.timer.heartbeat(now)
        {
            core.last_heartbeat = now;
            if let Err(e) = self.apply(core, vec![beat], false) {
                eprintln!("timer heartbeat: {e}");
            }
        }
    }

    pub fn spawn(&self) -> JoinHandle<()> {
        let me = self.clone();
        std::thread::spawn(move || me.run())
    }

    /// Stops the background thread (tests; in the app the process exit ends it).
    #[cfg(test)]
    pub fn shutdown(&self) {
        let _core = self.lock();
        self.0.shutdown.store(true, Ordering::SeqCst);
        self.0.wake.notify_all();
    }

    fn run(&self) {
        let mut core = self.lock();
        while !self.0.shutdown.load(Ordering::SeqCst) {
            let now = self.now();
            self.step_locked(&mut core, now);
            core = match self.next_wait(&core, now) {
                Some(wait) => {
                    self.0
                        .wake
                        .wait_timeout(core, wait)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
                None => self
                    .0
                    .wake
                    .wait(core)
                    .unwrap_or_else(PoisonError::into_inner),
            };
        }
    }

    /// Time until the next phase end or heartbeat; `None` when nothing is pending.
    fn next_wait(&self, core: &Core, now: Timestamp) -> Option<Duration> {
        let beat = matches!(&core.timer.state, State::Running(r) if r.phase == TimerPhase::Focus)
            .then(|| core.last_heartbeat.as_second() + HEARTBEAT_SECONDS);
        let deadline = core.timer.deadline().map(Timestamp::as_second);
        let next = [beat, deadline].into_iter().flatten().min()?;
        Some(Duration::from_secs((next - now.as_second()).max(0) as u64))
    }

    /// Writes the effects to disk in a safe order (sidecar, then `timer.json`),
    /// refreshes the mirror lines and reports. Keeps going after an error and
    /// returns the first one.
    fn apply(&self, core: &Core, effects: Vec<Effect>, transition: bool) -> Result<(), String> {
        let mut first_error: Option<String> = None;
        let mut note = |r: Result<(), String>| {
            if let Err(e) = r {
                eprintln!("timer: {e}");
                first_error.get_or_insert(e);
            }
        };
        let mut dirty: Vec<String> = Vec::new();
        let mut touch = |date: &String| {
            if !dirty.contains(date) {
                dirty.push(date.clone());
            }
        };
        for effect in effects {
            match effect {
                Effect::Persist { date, session } => {
                    note(sessions::upsert(&self.0.root, &date, session));
                    touch(&date);
                }
                Effect::Discard { date, start } => {
                    note(sessions::remove(&self.0.root, &date, &start));
                    touch(&date);
                }
                Effect::Heartbeat { date, session } => {
                    note(sessions::upsert(&self.0.root, &date, session));
                }
                Effect::Notify(notice) => self.0.sink.notify(notice),
            }
        }
        if transition {
            note(self.write_pointer(&core.timer));
        }
        for date in &dirty {
            match self.0.store.rewrite_mirror(date) {
                Ok(Some(changed)) => self.0.sink.day_changed(changed),
                Ok(None) => {}
                Err(e) => note(Err(format!("updating mirror lines for {date}: {e}"))),
            }
            self.0.sink.sessions_changed(date);
        }
        if transition {
            self.0.sink.timer_changed(&status_of(&core.timer));
        }
        first_error.map_or(Ok(()), Err)
    }

    /// `timer.json` mirrors the running timer; it is removed when idle.
    fn write_pointer(&self, timer: &Timer) -> Result<(), String> {
        match timer.persisted() {
            Some(saved) => {
                let json = serde_json::to_string_pretty(&saved)
                    .map_err(|e| format!("serializing timer state: {e}"))?;
                atomic::write_atomic(&self.0.timer_json, (json + "\n").as_bytes())
            }
            None => remove_file_if_exists(&self.0.timer_json),
        }
    }

    fn remove_pointer(&self) {
        if let Err(e) = remove_file_if_exists(&self.0.timer_json) {
            eprintln!("{e}");
        }
    }
}

fn remove_file_if_exists(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("removing {}: {e}", path.display())),
    }
}

fn status_of(timer: &Timer) -> TimerStatus {
    match &timer.state {
        State::Idle => TimerStatus::Idle,
        State::Running(r) => {
            let pomodoro = r.mode == Mode::Pomodoro;
            TimerStatus::Running {
                date: r.date.clone(),
                block: block_ref(&r.block),
                mode: r.mode,
                phase: r.phase,
                session_start: timer.fmt(r.session_start),
                phase_ends_at: timer.deadline().map(|d| timer.fmt(d)),
                phase_start: timer.fmt(r.phase_start),
                cycle: pomodoro.then_some(r.cycle),
                cycles: pomodoro.then_some(r.cycles),
                focus_minutes: pomodoro.then_some(r.focus_min),
                break_minutes: pomodoro.then_some(r.break_min),
            }
        }
        State::Stale {
            running: r,
            last_seen,
        } => TimerStatus::Stale {
            date: r.date.clone(),
            block: block_ref(&r.block),
            mode: r.mode,
            session_start: timer.fmt(r.session_start),
            last_seen: timer.fmt(*last_seen),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_util::TempDir;
    use crate::day_file::Block;
    use crate::storage;
    use std::fs;

    const DATE: &str = "2026-10-20";

    #[derive(Debug, Clone, PartialEq)]
    enum Event {
        Timer(TimerStatus),
        Sessions(String),
        Day(DayChanged),
        Notify(Notice),
    }

    #[derive(Default)]
    struct FakeSink(Mutex<Vec<Event>>);

    impl FakeSink {
        fn take(&self) -> Vec<Event> {
            std::mem::take(&mut *self.0.lock().unwrap())
        }
    }

    impl Sink for FakeSink {
        fn timer_changed(&self, status: &TimerStatus) {
            self.0.lock().unwrap().push(Event::Timer(status.clone()));
        }
        fn sessions_changed(&self, date: &str) {
            self.0.lock().unwrap().push(Event::Sessions(date.into()));
        }
        fn day_changed(&self, changed: DayChanged) {
            self.0.lock().unwrap().push(Event::Day(changed));
        }
        fn notify(&self, notice: Notice) {
            self.0.lock().unwrap().push(Event::Notify(notice));
        }
    }

    fn ts(h: i64, m: i64, s: i64) -> Timestamp {
        format!("{DATE}T{h:02}:{m:02}:{s:02}-03:00")
            .parse()
            .unwrap()
    }

    fn st(h: i64, m: i64, s: i64) -> String {
        format!("{DATE}T{h:02}:{m:02}:{s:02}-03:00")
    }

    fn tz() -> TimeZone {
        TimeZone::fixed(jiff::tz::offset(-3))
    }

    struct Fx {
        _tmp: TempDir,
        root: PathBuf,
        timer_json: PathBuf,
        store: Arc<DayStore>,
        sink: Arc<FakeSink>,
        now: Arc<Mutex<Timestamp>>,
        svc: TimerService,
    }

    impl Fx {
        fn new() -> Fx {
            let tmp = TempDir::new();
            let root = fs::canonicalize(&tmp.0).unwrap().join("notes");
            fs::create_dir_all(&root).unwrap();
            let timer_json = tmp.0.join("data").join("timer.json");
            let store = Arc::new(DayStore::new(root.clone()));
            let sink = Arc::new(FakeSink::default());
            let now = Arc::new(Mutex::new(ts(9, 0, 0)));
            let svc = Fx::service(&root, &timer_json, &store, &sink, &now);
            Fx {
                _tmp: tmp,
                root,
                timer_json,
                store,
                sink,
                now,
                svc,
            }
        }

        fn service(
            root: &Path,
            timer_json: &Path,
            store: &Arc<DayStore>,
            sink: &Arc<FakeSink>,
            now: &Arc<Mutex<Timestamp>>,
        ) -> TimerService {
            let clock = now.clone();
            TimerService::new(
                root.to_path_buf(),
                timer_json.to_path_buf(),
                store.clone(),
                sink.clone(),
                Arc::new(move || *clock.lock().unwrap()),
                tz(),
            )
        }

        /// A second service over the same files, as after an app restart.
        fn restart(&self) -> TimerService {
            Fx::service(
                &self.root,
                &self.timer_json,
                &self.store,
                &self.sink,
                &self.now,
            )
        }

        fn set_now(&self, t: Timestamp) {
            *self.now.lock().unwrap() = t;
        }

        fn write_day(&self) {
            let block = Block {
                start: 540,
                end: 630,
                title: "Deep work".into(),
                tags: vec!["mestrado".into()],
                body: "note\n".into(),
            };
            storage::save_day(&self.root, DATE, &[block], "").unwrap();
        }

        fn day_text(&self) -> String {
            fs::read_to_string(storage::day_path(&self.root, DATE).unwrap()).unwrap()
        }

        fn sidecar(&self) -> Vec<Session> {
            sessions::read(&self.root, DATE).unwrap()
        }
    }

    fn br() -> BlockRef {
        BlockRef {
            start: 540,
            end: 630,
            title: "Deep work".into(),
            tags: vec!["mestrado".into()],
            project: None,
        }
    }

    fn running_of(status: &TimerStatus) -> (&BlockRef, Mode, TimerPhase, &str, Option<&str>) {
        match status {
            TimerStatus::Running {
                block,
                mode,
                phase,
                session_start,
                phase_ends_at,
                ..
            } => (
                block,
                *mode,
                *phase,
                session_start,
                phase_ends_at.as_deref(),
            ),
            other => panic!("not running: {other:?}"),
        }
    }

    #[test]
    fn idle_at_first() {
        let fx = Fx::new();
        assert_eq!(fx.svc.status(), TimerStatus::Idle);
        assert!(!fx.timer_json.exists());
    }

    #[test]
    fn start_only_on_todays_blocks() {
        let fx = Fx::new();
        for date in ["2026-10-19", "2026-10-21"] {
            let err = fx.svc.start(date, br(), Mode::Free, None).unwrap_err();
            assert_eq!(err, "Timers run on today's blocks");
        }
        assert_eq!(fx.svc.status(), TimerStatus::Idle);
        assert!(fx.svc.start("nope", br(), Mode::Free, None).is_err());
    }

    #[test]
    fn start_rejects_invalid_block_times() {
        let fx = Fx::new();
        let mut b = br();
        b.end = b.start;
        assert!(fx.svc.start(DATE, b, Mode::Free, None).is_err());
        assert_eq!(fx.svc.status(), TimerStatus::Idle);
    }

    #[test]
    fn start_free_persists_everything_and_emits() {
        let fx = Fx::new();
        let status = fx.svc.start(DATE, br(), Mode::Free, None).unwrap();
        let (block, mode, phase, start, ends) = running_of(&status);
        assert_eq!(block, &br());
        assert_eq!(
            (mode, phase, start, ends),
            (Mode::Free, TimerPhase::Focus, st(9, 0, 0).as_str(), None)
        );
        assert_eq!(fx.svc.status(), status);
        let open = fx.sidecar();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].end, None);
        assert_eq!(open[0].last_seen, Some(st(9, 0, 0)));
        let saved: timer::Persisted =
            serde_json::from_str(&fs::read_to_string(&fx.timer_json).unwrap()).unwrap();
        assert_eq!(saved.start, st(9, 0, 0));
        let events = fx.sink.take();
        assert!(events.contains(&Event::Sessions(DATE.into())));
        assert_eq!(events.last(), Some(&Event::Timer(status)));
    }

    #[test]
    fn stop_records_the_session_mirrors_it_and_goes_idle() {
        let fx = Fx::new();
        fx.write_day();
        fx.svc.start(DATE, br(), Mode::Free, None).unwrap();
        fx.sink.take();
        fx.set_now(ts(9, 45, 0));
        assert_eq!(fx.svc.stop().unwrap(), TimerStatus::Idle);
        let all = fx.sidecar();
        assert_eq!(all[0].end, Some(st(9, 45, 0)));
        assert_eq!(all[0].last_seen, None);
        assert_eq!(
            fx.day_text(),
            "## 09:00–10:30 · Deep work #mestrado\nnote\n\n⏱ 09:00–09:45 · 45 min · free\n"
        );
        assert!(!fx.timer_json.exists());
        let events = fx.sink.take();
        assert!(matches!(events[0], Event::Day(ref c) if c.date == DATE));
        assert!(events.contains(&Event::Sessions(DATE.into())));
        assert_eq!(events.last(), Some(&Event::Timer(TimerStatus::Idle)));
        // The frontend reloads on day-changed and sees no mirror lines.
        assert_eq!(fx.store.load(DATE).unwrap().blocks[0].body, "note\n");
    }

    #[test]
    fn stop_under_a_minute_leaves_no_session() {
        let fx = Fx::new();
        fx.write_day();
        fx.svc.start(DATE, br(), Mode::Free, None).unwrap();
        fx.set_now(ts(9, 0, 30));
        fx.svc.stop().unwrap();
        assert!(fx.sidecar().is_empty());
        assert!(fx.day_text().ends_with("note\n"));
    }

    #[test]
    fn stop_when_idle_is_fine() {
        let fx = Fx::new();
        assert_eq!(fx.svc.stop().unwrap(), TimerStatus::Idle);
    }

    #[test]
    fn start_while_running_records_the_previous_session() {
        let fx = Fx::new();
        fx.svc.start(DATE, br(), Mode::Free, None).unwrap();
        fx.set_now(ts(9, 30, 0));
        let mut other = br();
        other.title = "Other".into();
        fx.svc
            .start(DATE, other.clone(), Mode::Pomodoro, None)
            .unwrap();
        let all = fx.sidecar();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].end, Some(st(9, 30, 0)));
        assert_eq!(all[1].end, None);
        assert_eq!(all[1].block.title, "Other");
    }

    fn write_config(fx: &Fx, json: &str) {
        fs::write(fx.root.join("config.json"), json).unwrap();
    }

    #[test]
    fn pomodoro_runs_focus_then_break_then_idle() {
        let fx = Fx::new();
        write_config(&fx, r#"{"pomodoroCycles":1}"#);
        fx.write_day();
        let status = fx.svc.start(DATE, br(), Mode::Pomodoro, None).unwrap();
        assert_eq!(running_of(&status).4, Some(st(9, 25, 0).as_str()));
        fx.sink.take();

        fx.set_now(ts(9, 24, 59));
        fx.svc.step(ts(9, 24, 59));
        assert!(fx.sink.take().is_empty());

        fx.set_now(ts(9, 25, 0));
        fx.svc.step(ts(9, 25, 0));
        let events = fx.sink.take();
        assert!(events.contains(&Event::Notify(Notice::FocusDone { break_min: 5 })));
        let status = fx.svc.status();
        let (_, _, phase, _, ends) = running_of(&status);
        assert_eq!(
            (phase, ends),
            (TimerPhase::Break, Some(st(9, 30, 0).as_str()))
        );
        assert_eq!(events.last(), Some(&Event::Timer(status)));
        assert_eq!(fx.sidecar()[0].end, Some(st(9, 25, 0)));
        assert!(fx.day_text().contains("⏱ 09:00–09:25 · 25 min · pomodoro"));
        let saved: timer::Persisted =
            serde_json::from_str(&fs::read_to_string(&fx.timer_json).unwrap()).unwrap();
        assert_eq!(saved.phase, timer::Phase::Break);

        fx.set_now(ts(9, 30, 0));
        fx.svc.step(ts(9, 30, 0));
        let events = fx.sink.take();
        assert!(events.contains(&Event::Notify(Notice::RunDone { cycles: 1 })));
        assert_eq!(events.last(), Some(&Event::Timer(TimerStatus::Idle)));
        assert!(!fx.timer_json.exists());
        assert_eq!(fx.sidecar().len(), 1);
    }

    #[test]
    fn pomodoro_lengths_come_from_config() {
        let fx = Fx::new();
        write_config(
            &fx,
            r#"{"pomodoroFocusMinutes":1,"pomodoroBreakMinutes":2}"#,
        );
        let status = fx.svc.start(DATE, br(), Mode::Pomodoro, None).unwrap();
        assert_eq!(running_of(&status).4, Some(st(9, 1, 0).as_str()));
        fx.svc.step(ts(9, 1, 0));
        let status = fx.svc.status();
        assert_eq!(running_of(&status).4, Some(st(9, 3, 0).as_str()));
    }

    #[test]
    fn zero_lengths_in_config_are_treated_as_one_minute() {
        let fx = Fx::new();
        write_config(
            &fx,
            r#"{"pomodoroFocusMinutes":0,"pomodoroBreakMinutes":0}"#,
        );
        let status = fx.svc.start(DATE, br(), Mode::Pomodoro, None).unwrap();
        assert_eq!(running_of(&status).4, Some(st(9, 1, 0).as_str()));
    }

    #[test]
    fn broken_config_falls_back_to_defaults() {
        let fx = Fx::new();
        write_config(&fx, "{ nope");
        let status = fx.svc.start(DATE, br(), Mode::Pomodoro, None).unwrap();
        assert_eq!(running_of(&status).4, Some(st(9, 25, 0).as_str()));
    }

    #[test]
    fn stop_during_break_records_nothing_new() {
        let fx = Fx::new();
        fx.svc.start(DATE, br(), Mode::Pomodoro, None).unwrap();
        fx.svc.step(ts(9, 25, 0));
        fx.set_now(ts(9, 27, 0));
        fx.svc.stop().unwrap();
        assert_eq!(fx.sidecar().len(), 1);
        assert_eq!(fx.sidecar()[0].end, Some(st(9, 25, 0)));
    }

    #[test]
    fn heartbeat_refreshes_last_seen_every_minute_without_events() {
        let fx = Fx::new();
        fx.svc.start(DATE, br(), Mode::Free, None).unwrap();
        fx.sink.take();
        fx.svc.step(ts(9, 0, 59));
        assert_eq!(fx.sidecar()[0].last_seen, Some(st(9, 0, 0)));
        fx.svc.step(ts(9, 1, 0));
        assert_eq!(fx.sidecar()[0].last_seen, Some(st(9, 1, 0)));
        assert!(fx.sink.take().is_empty());
    }

    #[test]
    fn start_with_an_invalid_sidecar_fails_without_starting_or_touching_it() {
        let fx = Fx::new();
        let p = sessions::sidecar_path(&fx.root, DATE).unwrap();
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, "{ nope").unwrap();
        assert!(fx.svc.start(DATE, br(), Mode::Free, None).is_err());
        assert_eq!(fx.svc.status(), TimerStatus::Idle);
        assert!(!fx.timer_json.exists());
        assert_eq!(fs::read_to_string(&p).unwrap(), "{ nope");
        assert!(fx.svc.sessions(DATE).is_err());
    }

    // ---- sessions list, delete, relink ----

    #[test]
    fn sessions_report_whole_minutes_and_live_progress() {
        let fx = Fx::new();
        fx.svc.start(DATE, br(), Mode::Free, None).unwrap();
        fx.set_now(ts(9, 45, 59));
        fx.svc.stop().unwrap();
        fx.set_now(ts(10, 0, 0));
        fx.svc.start(DATE, br(), Mode::Free, None).unwrap();
        fx.set_now(ts(10, 7, 30));
        let list = fx.svc.sessions(DATE).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(
            (list[0].minutes, list[0].end.as_deref()),
            (45, Some(st(9, 45, 59).as_str()))
        );
        assert_eq!((list[1].minutes, list[1].end.as_deref()), (7, None));
        assert_eq!(list[1].block, br());
        assert!(fx.svc.sessions("2026-10-19").unwrap().is_empty());
    }

    #[test]
    fn session_json_shape() {
        let fx = Fx::new();
        fx.svc.start(DATE, br(), Mode::Pomodoro, None).unwrap();
        let v = serde_json::to_value(fx.svc.sessions(DATE).unwrap()).unwrap();
        assert_eq!(
            v,
            serde_json::json!([{
                "start": st(9, 0, 0), "end": null, "mode": "pomodoro", "minutes": 0,
                "block": {"start": 540, "end": 630, "title": "Deep work", "tags": ["mestrado"], "project": null}
            }])
        );
    }

    #[test]
    fn status_json_shapes_match_the_contract() {
        let fx = Fx::new();
        assert_eq!(
            serde_json::to_value(fx.svc.status()).unwrap(),
            serde_json::json!({"state":"idle"})
        );
        let status = fx.svc.start(DATE, br(), Mode::Pomodoro, None).unwrap();
        assert_eq!(
            serde_json::to_value(status).unwrap(),
            serde_json::json!({
                "state": "running", "date": DATE, "mode": "pomodoro", "phase": "focus",
                "sessionStart": st(9, 0, 0), "phaseEndsAt": st(9, 25, 0),
                "phaseStart": st(9, 0, 0), "cycle": 1, "cycles": 4,
                "focusMinutes": 25, "breakMinutes": 5,
                "block": {"start": 540, "end": 630, "title": "Deep work", "tags": ["mestrado"], "project": null}
            })
        );
        let status = fx.svc.start(DATE, br(), Mode::Free, None).unwrap();
        assert_eq!(
            serde_json::to_value(status).unwrap(),
            serde_json::json!({
                "state": "running", "date": DATE, "mode": "free", "phase": "focus",
                "sessionStart": st(9, 0, 0), "phaseEndsAt": null,
                "phaseStart": st(9, 0, 0), "cycle": null, "cycles": null,
                "focusMinutes": null, "breakMinutes": null,
                "block": {"start": 540, "end": 630, "title": "Deep work", "tags": ["mestrado"], "project": null}
            })
        );
    }

    #[test]
    fn delete_session_removes_it_and_its_mirror_line() {
        let fx = Fx::new();
        fx.write_day();
        fx.svc.start(DATE, br(), Mode::Free, None).unwrap();
        fx.set_now(ts(9, 30, 0));
        fx.svc.stop().unwrap();
        assert!(fx.day_text().contains('⏱'));
        fx.sink.take();
        let left = fx.svc.delete_session(DATE, &st(9, 0, 0)).unwrap();
        assert!(left.is_empty());
        assert!(fx.sidecar().is_empty());
        assert_eq!(
            fx.day_text(),
            "## 09:00–10:30 · Deep work #mestrado\nnote\n"
        );
        let events = fx.sink.take();
        assert!(events.iter().any(|e| matches!(e, Event::Day(_))));
        assert!(events.contains(&Event::Sessions(DATE.into())));
    }

    #[test]
    fn delete_session_refuses_the_running_one() {
        let fx = Fx::new();
        fx.svc.start(DATE, br(), Mode::Free, None).unwrap();
        assert!(fx.svc.delete_session(DATE, &st(9, 0, 0)).is_err());
        assert_eq!(fx.sidecar().len(), 1);
    }

    #[test]
    fn delete_session_removes_an_orphan_open_session() {
        // A crash between the sidecar write and timer.json leaves an open session nobody runs.
        let fx = Fx::new();
        let orphan = Session {
            start: st(8, 0, 0),
            end: None,
            mode: Mode::Free,
            block: session_block(br()).unwrap(),
            last_seen: None,
        };
        sessions::upsert(&fx.root, DATE, orphan).unwrap();
        assert!(
            fx.svc
                .delete_session(DATE, &st(8, 0, 0))
                .unwrap()
                .is_empty()
        );
        assert!(fx.sidecar().is_empty());
    }

    fn moved() -> BlockRef {
        BlockRef {
            start: 600,
            end: 660,
            title: "Deep work 2".into(),
            tags: vec![],
            project: None,
        }
    }

    #[test]
    fn relink_updates_sessions_and_the_running_block() {
        let fx = Fx::new();
        fx.svc.start(DATE, br(), Mode::Free, None).unwrap();
        fx.set_now(ts(9, 30, 0));
        fx.svc.stop().unwrap();
        fx.set_now(ts(10, 0, 0));
        fx.svc.start(DATE, br(), Mode::Free, None).unwrap();
        fx.sink.take();
        fx.svc
            .relink(
                DATE,
                vec![Link {
                    from: br(),
                    to: moved(),
                }],
            )
            .unwrap();
        let all = fx.sidecar();
        assert!(
            all.iter()
                .all(|s| s.block.title == "Deep work 2" && s.block.start == "10:00")
        );
        let status = fx.svc.status();
        assert_eq!(running_of(&status).0, &moved());
        let saved: timer::Persisted =
            serde_json::from_str(&fs::read_to_string(&fx.timer_json).unwrap()).unwrap();
        assert_eq!(saved.block.title, "Deep work 2");
        let events = fx.sink.take();
        assert!(events.contains(&Event::Sessions(DATE.into())));
        assert!(events.contains(&Event::Timer(status)));
    }

    #[test]
    fn start_records_the_blocks_project_in_the_sidecar_and_the_status() {
        let fx = Fx::new();
        let mut block = br();
        block.project = Some("mest@x".into());
        let status = fx.svc.start(DATE, block.clone(), Mode::Free, None).unwrap();
        assert_eq!(running_of(&status).0, &block);
        assert_eq!(fx.sidecar()[0].block.project.as_deref(), Some("mest@x"));
        // a blank project is no project
        let mut blank = moved();
        blank.project = Some("  ".into());
        fx.svc.start(DATE, blank, Mode::Free, None).unwrap();
        assert!(
            fx.sidecar()
                .iter()
                .all(|s| s.block.title != "Deep work 2" || s.block.project.is_none())
        );
    }

    #[test]
    fn missing_project_field_is_accepted_when_deserializing_a_block() {
        let b: BlockRef =
            serde_json::from_str(r#"{"start":540,"end":600,"title":"A","tags":[]}"#).unwrap();
        assert_eq!(b.project, None);
        let b: BlockRef =
            serde_json::from_str(r#"{"start":540,"end":600,"title":"A","tags":[],"project":null}"#)
                .unwrap();
        assert_eq!(b.project, None);
    }

    #[test]
    fn relink_with_null_projects_keeps_the_sessions_and_the_running_blocks_project() {
        let fx = Fx::new();
        let mut block = br();
        block.project = Some("mest@x".into());
        fx.svc.start(DATE, block.clone(), Mode::Free, None).unwrap();
        fx.set_now(ts(9, 30, 0));
        fx.svc.stop().unwrap();
        fx.set_now(ts(10, 0, 0));
        fx.svc.start(DATE, block, Mode::Free, None).unwrap();
        fx.svc
            .relink(
                DATE,
                vec![Link {
                    from: br(),
                    to: moved(),
                }],
            )
            .unwrap();
        assert!(fx.sidecar().iter().all(
            |s| s.block.title == "Deep work 2" && s.block.project.as_deref() == Some("mest@x")
        ));
        let status = fx.svc.status();
        let running = running_of(&status).0;
        assert_eq!(running.title, "Deep work 2");
        assert_eq!(running.project.as_deref(), Some("mest@x"));
    }

    #[test]
    fn relink_without_match_changes_nothing_and_emits_nothing() {
        let fx = Fx::new();
        fx.svc
            .relink(
                DATE,
                vec![Link {
                    from: br(),
                    to: moved(),
                }],
            )
            .unwrap();
        assert!(fx.sink.take().is_empty());
        assert!(!sessions::sidecar_path(&fx.root, DATE).unwrap().exists());
    }

    #[test]
    fn relink_then_save_writes_the_mirror_under_the_new_block() {
        let fx = Fx::new();
        fx.write_day();
        fx.svc.start(DATE, br(), Mode::Free, None).unwrap();
        fx.set_now(ts(9, 30, 0));
        fx.svc.stop().unwrap();
        fx.svc
            .relink(
                DATE,
                vec![Link {
                    from: br(),
                    to: moved(),
                }],
            )
            .unwrap();
        let loaded = fx.store.load(DATE).unwrap();
        let mut blocks = loaded.blocks.clone();
        blocks[0].start = 600;
        blocks[0].end = 660;
        blocks[0].title = "Deep work 2".into();
        fx.store.save(DATE, &blocks, &loaded.rev).unwrap();
        assert!(fx.day_text().ends_with("⏱ 09:00–09:30 · 30 min · free\n"));
    }

    // ---- recovery ----

    #[test]
    fn recover_with_nothing_saved_stays_idle() {
        let fx = Fx::new();
        fx.svc.recover();
        assert_eq!(fx.svc.status(), TimerStatus::Idle);
    }

    #[test]
    fn recover_within_three_minutes_resumes_silently() {
        let fx = Fx::new();
        fx.svc.start(DATE, br(), Mode::Free, None).unwrap();
        fx.svc.step(ts(9, 10, 0));
        fx.set_now(ts(9, 12, 0));
        let again = fx.restart();
        again.recover();
        let status = again.status();
        let (_, mode, phase, start, _) = running_of(&status);
        assert_eq!(
            (mode, phase, start),
            (Mode::Free, TimerPhase::Focus, st(9, 0, 0).as_str())
        );
        assert_eq!(fx.sidecar()[0].end, None);
    }

    #[test]
    fn recover_after_a_long_gap_is_stale_and_resolvable() {
        let fx = Fx::new();
        fx.svc.start(DATE, br(), Mode::Free, None).unwrap();
        fx.svc.step(ts(9, 10, 0));
        fx.set_now(ts(11, 32, 0));
        let again = fx.restart();
        again.recover();
        match again.status() {
            TimerStatus::Stale {
                last_seen,
                session_start,
                block,
                ..
            } => {
                assert_eq!(last_seen, st(9, 10, 0));
                assert_eq!(session_start, st(9, 0, 0));
                assert_eq!(block, br());
            }
            other => panic!("{other:?}"),
        }
        assert!(fx.timer_json.exists());
        assert_eq!(again.resolve_stale(false).unwrap(), TimerStatus::Idle);
        assert_eq!(fx.sidecar()[0].end, Some(st(9, 10, 0)));
        assert!(!fx.timer_json.exists());
    }

    #[test]
    fn stale_keep_running_continues_and_heartbeats_resume() {
        let fx = Fx::new();
        fx.svc.start(DATE, br(), Mode::Free, None).unwrap();
        fx.svc.step(ts(9, 10, 0));
        fx.set_now(ts(11, 32, 0));
        let again = fx.restart();
        again.recover();
        let status = again.resolve_stale(true).unwrap();
        assert_eq!(running_of(&status).3, st(9, 0, 0));
        again.step(ts(11, 33, 0));
        assert_eq!(fx.sidecar()[0].last_seen, Some(st(11, 33, 0)));
    }

    #[test]
    fn recover_stale_pomodoro_focus_that_ended_closes_it() {
        let fx = Fx::new();
        fx.svc.start(DATE, br(), Mode::Pomodoro, None).unwrap();
        fx.svc.step(ts(9, 10, 0));
        fx.set_now(ts(11, 0, 0));
        let again = fx.restart();
        again.recover();
        assert_eq!(again.status(), TimerStatus::Idle);
        assert_eq!(fx.sidecar()[0].end, Some(st(9, 10, 0)));
        assert!(!fx.timer_json.exists());
    }

    #[test]
    fn recover_drops_a_pointer_whose_session_is_gone_or_closed() {
        let fx = Fx::new();
        fx.svc.start(DATE, br(), Mode::Free, None).unwrap();
        fx.set_now(ts(9, 30, 0));
        // Simulate a crash between closing the session and deleting timer.json.
        let pointer = fs::read_to_string(&fx.timer_json).unwrap();
        fx.svc.stop().unwrap();
        fs::create_dir_all(fx.timer_json.parent().unwrap()).unwrap();
        fs::write(&fx.timer_json, &pointer).unwrap();
        let again = fx.restart();
        again.recover();
        assert_eq!(again.status(), TimerStatus::Idle);
        assert!(!fx.timer_json.exists());
        assert_eq!(fx.sidecar().len(), 1);
    }

    #[test]
    fn recover_ignores_a_corrupt_pointer() {
        let fx = Fx::new();
        fs::create_dir_all(fx.timer_json.parent().unwrap()).unwrap();
        fs::write(&fx.timer_json, "{ nope").unwrap();
        fx.svc.recover();
        assert_eq!(fx.svc.status(), TimerStatus::Idle);
        assert!(!fx.timer_json.exists());
    }

    #[test]
    fn start_while_stale_closes_the_stale_session_at_last_seen() {
        let fx = Fx::new();
        fx.svc.start(DATE, br(), Mode::Free, None).unwrap();
        fx.svc.step(ts(9, 10, 0));
        fx.set_now(ts(11, 0, 0));
        let again = fx.restart();
        again.recover();
        again.start(DATE, br(), Mode::Free, None).unwrap();
        let all = fx.sidecar();
        assert_eq!(all[0].end, Some(st(9, 10, 0)));
        assert_eq!(all[1].end, None);
    }

    #[test]
    fn thread_starts_and_shuts_down() {
        let fx = Fx::new();
        let handle = fx.svc.spawn();
        fx.svc.start(DATE, br(), Mode::Free, None).unwrap();
        fx.svc.shutdown();
        handle.join().unwrap();
    }

    fn plan(f: u32, b: u32, c: u32) -> Option<timer::Plan> {
        Some(timer::Plan {
            focus_min: f,
            break_min: b,
            cycles: c,
        })
    }

    #[test]
    fn explicit_plan_overrides_config_and_drives_the_whole_run() {
        let fx = Fx::new();
        fx.write_day();
        write_config(
            &fx,
            r#"{"pomodoroFocusMinutes":50,"pomodoroBreakMinutes":10}"#,
        );
        let status = fx
            .svc
            .start(DATE, br(), Mode::Pomodoro, plan(1, 1, 2))
            .unwrap();
        assert_eq!(running_of(&status).4, Some(st(9, 1, 0).as_str()));
        fx.sink.take();
        fx.set_now(ts(9, 1, 0));
        fx.svc.step(ts(9, 1, 0));
        fx.set_now(ts(9, 2, 0));
        fx.svc.step(ts(9, 2, 0));
        let events = fx.sink.take();
        assert!(events.contains(&Event::Notify(Notice::NextFocus {
            cycle: 2,
            cycles: 2
        })));
        let status = fx.svc.status();
        match &status {
            TimerStatus::Running {
                cycle,
                cycles,
                phase_start,
                phase,
                ..
            } => {
                assert_eq!(
                    (*cycle, *cycles, *phase),
                    (Some(2), Some(2), TimerPhase::Focus)
                );
                assert_eq!(phase_start, &st(9, 2, 0));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(fx.sidecar().len(), 2);
        assert_eq!(fx.sidecar()[1].start, st(9, 2, 0));
        assert_eq!(fx.sidecar()[1].end, None);
        let saved: timer::Persisted =
            serde_json::from_str(&fs::read_to_string(&fx.timer_json).unwrap()).unwrap();
        assert_eq!(
            (saved.cycle, saved.cycles, saved.start),
            (2, 2, st(9, 2, 0))
        );
        fx.set_now(ts(9, 3, 0));
        fx.svc.step(ts(9, 3, 0));
        fx.set_now(ts(9, 4, 0));
        fx.svc.step(ts(9, 4, 0));
        let events = fx.sink.take();
        assert!(events.contains(&Event::Notify(Notice::RunDone { cycles: 2 })));
        assert_eq!(events.last(), Some(&Event::Timer(TimerStatus::Idle)));
        assert_eq!(fx.sidecar().len(), 2);
        assert!(fx.sidecar().iter().all(|s| s.end.is_some()));
        assert!(fx.day_text().contains("⏱ 09:00–09:01 · 1 min · pomodoro"));
        assert!(fx.day_text().contains("⏱ 09:02–09:03 · 1 min · pomodoro"));
    }

    #[test]
    fn plan_out_of_range_is_an_error_and_changes_nothing() {
        let fx = Fx::new();
        for p in [
            plan(0, 5, 4),
            plan(181, 5, 4),
            plan(25, 0, 4),
            plan(25, 61, 4),
            plan(25, 5, 0),
            plan(25, 5, 13),
        ] {
            assert!(fx.svc.start(DATE, br(), Mode::Pomodoro, p).is_err());
        }
        assert_eq!(fx.svc.status(), TimerStatus::Idle);
        assert!(fx.sidecar().is_empty());
    }

    #[test]
    fn plan_is_ignored_for_free_timers() {
        let fx = Fx::new();
        let status = fx.svc.start(DATE, br(), Mode::Free, plan(0, 0, 0)).unwrap();
        assert!(matches!(status, TimerStatus::Running { cycles: None, .. }));
    }

    #[test]
    fn defaults_come_from_config_including_cycles() {
        let fx = Fx::new();
        write_config(&fx, r#"{"pomodoroCycles":3}"#);
        let status = fx.svc.start(DATE, br(), Mode::Pomodoro, None).unwrap();
        assert!(matches!(
            status,
            TimerStatus::Running {
                cycles: Some(3),
                focus_minutes: Some(25),
                break_minutes: Some(5),
                ..
            }
        ));
    }

    #[test]
    fn skip_break_starts_the_next_focus_and_finishes_after_the_last() {
        let fx = Fx::new();
        fx.svc
            .start(DATE, br(), Mode::Pomodoro, plan(1, 5, 2))
            .unwrap();
        fx.set_now(ts(9, 1, 0));
        fx.svc.step(ts(9, 1, 0));
        fx.set_now(ts(9, 2, 0));
        fx.sink.take();
        let status = fx.svc.skip_break().unwrap();
        match &status {
            TimerStatus::Running {
                cycle,
                phase,
                phase_start,
                ..
            } => {
                assert_eq!((*cycle, *phase), (Some(2), TimerPhase::Focus));
                assert_eq!(phase_start, &st(9, 2, 0));
            }
            other => panic!("{other:?}"),
        }
        assert!(fx.sink.take().contains(&Event::Timer(status)));
        assert_eq!(fx.sidecar().len(), 2);
        fx.set_now(ts(9, 3, 0));
        fx.svc.step(ts(9, 3, 0));
        fx.set_now(ts(9, 4, 0));
        assert_eq!(fx.svc.skip_break().unwrap(), TimerStatus::Idle);
        assert!(!fx.timer_json.exists());
    }

    #[test]
    fn skip_break_outside_a_break_just_returns_the_status() {
        let fx = Fx::new();
        assert_eq!(fx.svc.skip_break().unwrap(), TimerStatus::Idle);
        let started = fx
            .svc
            .start(DATE, br(), Mode::Pomodoro, plan(25, 5, 2))
            .unwrap();
        fx.sink.take();
        assert_eq!(fx.svc.skip_break().unwrap(), started);
        assert!(fx.sink.take().is_empty());
    }
}
