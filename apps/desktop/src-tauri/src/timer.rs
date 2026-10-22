//! The timer as a pure state machine with an injected clock: no I/O, no Tauri.
//! It never writes anything itself; every transition returns `Effect`s for the
//! service to apply (sidecar writes, notifications).
//!
//! Pomodoro: only focus phases are sessions; a break is just a countdown.

use crate::sessions::{self, Mode, Session, SessionBlock};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};

/// A timer whose app was gone for longer than this is shown as stale.
pub const STALE_AFTER_SECONDS: i64 = 180;
pub const HEARTBEAT_SECONDS: i64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Focus,
    Break,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Running {
    /// `YYYY-MM-DD` of the session's start; names the sidecar.
    pub date: String,
    pub block: SessionBlock,
    pub mode: Mode,
    pub phase: Phase,
    pub session_start: Timestamp,
    pub phase_start: Timestamp,
    pub focus_min: u32,
    pub break_min: u32,
    /// 1-based index of the current pomodoro in the run.
    pub cycle: u32,
    /// Pomodoros in the run (1 for free timers).
    pub cycles: u32,
}

pub const MAX_FOCUS_MIN: u32 = 180;
pub const MAX_BREAK_MIN: u32 = 60;
pub const MAX_CYCLES: u32 = 12;

/// What a Pomodoro run is made of: `cycles` x (focus, break).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    #[serde(rename = "focusMinutes")]
    pub focus_min: u32,
    #[serde(rename = "breakMinutes")]
    pub break_min: u32,
    pub cycles: u32,
}

impl Plan {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=MAX_FOCUS_MIN).contains(&self.focus_min) {
            return Err(format!("Focus must be 1-{MAX_FOCUS_MIN} minutes"));
        }
        if !(1..=MAX_BREAK_MIN).contains(&self.break_min) {
            return Err(format!("Break must be 1-{MAX_BREAK_MIN} minutes"));
        }
        if !(1..=MAX_CYCLES).contains(&self.cycles) {
            return Err(format!("Pomodoros must be 1-{MAX_CYCLES}"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Idle,
    Running(Running),
    /// Found running at startup but the app was away too long; waits for the user.
    Stale {
        running: Running,
        last_seen: Timestamp,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    FocusDone {
        break_min: u32,
    },
    /// A break ended and the next focus started on its own.
    NextFocus {
        cycle: u32,
        cycles: u32,
    },
    /// The last break ended; the run is over.
    RunDone {
        cycles: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Insert or replace the session with this `start` (open or finished).
    Persist {
        date: String,
        session: Session,
    },
    /// Remove a session that turned out shorter than a minute.
    Discard {
        date: String,
        start: String,
    },
    /// Refresh `lastSeen` of the open session.
    Heartbeat {
        date: String,
        session: Session,
    },
    Notify(Notice),
}

/// `timer.json`: the running pointer, machine state outside the notes folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Persisted {
    pub date: String,
    pub start: String,
    pub mode: Mode,
    pub phase: Phase,
    pub phase_start: String,
    pub focus_min: u32,
    pub break_min: u32,
    /// Missing in `timer.json` files written before M6b.
    #[serde(default = "one")]
    pub cycle: u32,
    #[serde(default = "one")]
    pub cycles: u32,
    pub block: SessionBlock,
}

fn one() -> u32 {
    1
}

#[derive(Debug, Clone)]
pub struct Timer {
    tz: TimeZone,
    pub state: State,
}

/// Drops sub-second precision so in-memory and persisted timestamps agree.
pub fn whole_seconds(ts: Timestamp) -> Timestamp {
    Timestamp::from_second(ts.as_second()).unwrap_or(ts)
}

impl Timer {
    pub fn new(tz: TimeZone) -> Timer {
        Timer {
            tz,
            state: State::Idle,
        }
    }

    /// RFC 3339 with the local offset at that moment, second precision.
    pub fn fmt(&self, ts: Timestamp) -> String {
        ts.to_zoned(self.tz.clone())
            .strftime("%Y-%m-%dT%H:%M:%S%:z")
            .to_string()
    }

    pub fn start(
        &mut self,
        now: Timestamp,
        date: &str,
        block: SessionBlock,
        mode: Mode,
        plan: Plan,
    ) -> Vec<Effect> {
        let now = whole_seconds(now);
        let mut effects = self.stop(now);
        let running = Running {
            date: date.to_string(),
            block,
            mode,
            phase: Phase::Focus,
            session_start: now,
            phase_start: now,
            focus_min: plan.focus_min,
            break_min: plan.break_min,
            cycle: 1,
            cycles: if mode == Mode::Pomodoro {
                plan.cycles.max(1)
            } else {
                1
            },
        };
        effects.push(Effect::Persist {
            date: running.date.clone(),
            session: self.open_session(&running, Some(now)),
        });
        self.state = State::Running(running);
        effects
    }

    pub fn stop(&mut self, now: Timestamp) -> Vec<Effect> {
        let now = whole_seconds(now);
        match std::mem::replace(&mut self.state, State::Idle) {
            State::Idle => Vec::new(),
            State::Running(r) if r.phase == Phase::Focus => vec![self.close(&r, now)],
            State::Running(_) => Vec::new(),
            State::Stale { running, last_seen } => vec![self.close(&running, last_seen)],
        }
    }

    pub fn tick(&mut self, now: Timestamp) -> Vec<Effect> {
        let now = whole_seconds(now);
        let mut effects = Vec::new();
        // Phases follow the wall clock: a late tick catches up, one phase at a time,
        // and never past what the elapsed time allows.
        while let Some(deadline) = self.deadline() {
            if now < deadline {
                break;
            }
            self.advance(deadline, &mut effects);
        }
        effects
    }

    /// Ends the current phase at `at`: focus -> break, break -> next focus or idle.
    fn advance(&mut self, at: Timestamp, effects: &mut Vec<Effect>) {
        let State::Running(r) = &mut self.state else {
            return;
        };
        if r.phase == Phase::Focus {
            let finished = r.clone();
            r.phase = Phase::Break;
            r.phase_start = at;
            let break_min = r.break_min;
            effects.push(self.close(&finished, at));
            effects.push(Effect::Notify(Notice::FocusDone { break_min }));
            return;
        }
        if r.cycle < r.cycles {
            r.cycle += 1;
            r.phase = Phase::Focus;
            r.session_start = at;
            r.phase_start = at;
            let (cycle, cycles) = (r.cycle, r.cycles);
            let running = r.clone();
            effects.push(Effect::Persist {
                date: running.date.clone(),
                session: self.open_session(&running, Some(at)),
            });
            effects.push(Effect::Notify(Notice::NextFocus { cycle, cycles }));
        } else {
            let cycles = r.cycles;
            self.state = State::Idle;
            effects.push(Effect::Notify(Notice::RunDone { cycles }));
        }
    }

    /// During a break: start the next focus now (or finish the run after the
    /// last break). Anything else is a no-op.
    pub fn skip_break(&mut self, now: Timestamp) -> Vec<Effect> {
        let in_break = matches!(&self.state, State::Running(r) if r.phase == Phase::Break);
        if !in_break {
            return Vec::new();
        }
        let mut effects = Vec::new();
        self.advance(whole_seconds(now), &mut effects);
        effects.retain(|e| !matches!(e, Effect::Notify(_)));
        effects
    }

    pub fn heartbeat(&self, now: Timestamp) -> Option<Effect> {
        match &self.state {
            State::Running(r) if r.phase == Phase::Focus => Some(Effect::Heartbeat {
                date: r.date.clone(),
                session: self.open_session(r, Some(whole_seconds(now))),
            }),
            _ => None,
        }
    }

    /// When the current Pomodoro phase ends; `None` for free timers, idle and stale.
    pub fn deadline(&self) -> Option<Timestamp> {
        let State::Running(r) = &self.state else {
            return None;
        };
        if r.mode != Mode::Pomodoro {
            return None;
        }
        let minutes = match r.phase {
            Phase::Focus => r.focus_min,
            Phase::Break => r.break_min,
        };
        Some(r.phase_start + jiff::SignedDuration::from_secs(i64::from(minutes) * 60))
    }

    /// "Keep running" resumes the timer; otherwise the session is closed at the
    /// moment the app was last seen.
    pub fn resolve_stale(&mut self, keep_running: bool) -> Vec<Effect> {
        if !matches!(self.state, State::Stale { .. }) {
            return Vec::new();
        }
        if keep_running {
            if let State::Stale { running, .. } = std::mem::replace(&mut self.state, State::Idle) {
                self.state = State::Running(running);
            }
            return Vec::new();
        }
        self.stop(Timestamp::UNIX_EPOCH)
    }

    pub fn open_session(&self, r: &Running, last_seen: Option<Timestamp>) -> Session {
        Session {
            start: self.fmt(r.session_start),
            end: None,
            mode: r.mode,
            block: r.block.clone(),
            last_seen: last_seen.map(|t| self.fmt(t)),
        }
    }

    /// Closes the session at `end`, or discards it when it was too short.
    fn close(&self, r: &Running, end: Timestamp) -> Effect {
        let open = self.open_session(r, None);
        match sessions::finish(&open, &self.fmt(end)) {
            Some(session) => Effect::Persist {
                date: r.date.clone(),
                session,
            },
            None => Effect::Discard {
                date: r.date.clone(),
                start: open.start,
            },
        }
    }

    /// What `timer.json` should hold; `None` when idle.
    pub fn persisted(&self) -> Option<Persisted> {
        let (State::Running(r) | State::Stale { running: r, .. }) = &self.state else {
            return None;
        };
        Some(Persisted {
            date: r.date.clone(),
            start: self.fmt(r.session_start),
            mode: r.mode,
            phase: r.phase,
            phase_start: self.fmt(r.phase_start),
            focus_min: r.focus_min,
            break_min: r.break_min,
            cycle: r.cycle,
            cycles: r.cycles,
            block: r.block.clone(),
        })
    }
}

/// Rebuilds the timer from `timer.json` after a restart. `last_seen` is the open
/// session's last heartbeat.
pub fn recover(
    tz: TimeZone,
    saved: &Persisted,
    last_seen: Timestamp,
    now: Timestamp,
) -> Result<(Timer, Vec<Effect>), String> {
    let parse =
        |s: &str| sessions::parse_ts(s).ok_or_else(|| format!("bad timestamp {s:?} in timer.json"));
    let running = Running {
        date: saved.date.clone(),
        block: saved.block.clone(),
        mode: saved.mode,
        phase: saved.phase,
        session_start: parse(&saved.start)?,
        phase_start: parse(&saved.phase_start)?,
        focus_min: saved.focus_min,
        break_min: saved.break_min,
        cycle: saved.cycle.max(1),
        cycles: saved.cycles.max(1),
    };
    let (last_seen, now) = (whole_seconds(last_seen), whole_seconds(now));
    let mut timer = Timer::new(tz);
    if running.phase == Phase::Break {
        return Ok((timer, Vec::new()));
    }
    if running.mode == Mode::Pomodoro {
        let focus_end = running.phase_start
            + jiff::SignedDuration::from_secs(i64::from(running.focus_min) * 60);
        if now >= focus_end {
            let effect = timer.close(&running, focus_end.min(last_seen));
            return Ok((timer, vec![effect]));
        }
    }
    timer.state = if now.as_second() - last_seen.as_second() <= STALE_AFTER_SECONDS {
        State::Running(running)
    } else {
        State::Stale { running, last_seen }
    };
    Ok((timer, Vec::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tz() -> TimeZone {
        TimeZone::fixed(jiff::tz::offset(-3))
    }

    fn at(h: i64, m: i64, s: i64) -> Timestamp {
        format!("2026-10-20T{h:02}:{m:02}:{s:02}-03:00")
            .parse()
            .unwrap()
    }

    fn s(h: i64, m: i64, sec: i64) -> String {
        format!("2026-10-20T{h:02}:{m:02}:{sec:02}-03:00")
    }

    fn block() -> SessionBlock {
        SessionBlock {
            start: "09:00".into(),
            end: "10:30".into(),
            title: "Deep work".into(),
            tags: vec!["mestrado".into()],
            project: None,
        }
    }

    fn plan(focus_min: u32, break_min: u32, cycles: u32) -> Plan {
        Plan {
            focus_min,
            break_min,
            cycles,
        }
    }

    fn timer() -> Timer {
        Timer::new(tz())
    }

    fn started(mode: Mode) -> Timer {
        let mut t = timer();
        t.start(at(9, 0, 0), "2026-10-20", block(), mode, plan(25, 5, 1));
        t
    }

    fn persisted_closed(effects: &[Effect]) -> Vec<(String, Option<String>)> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Persist { session, .. } => {
                    Some((session.start.clone(), session.end.clone()))
                }
                _ => None,
            })
            .collect()
    }

    fn running(t: &Timer) -> &Running {
        match &t.state {
            State::Running(r) => r,
            other => panic!("not running: {other:?}"),
        }
    }

    #[test]
    fn fmt_uses_local_offset_and_drops_subseconds() {
        let t = timer();
        let ts: Timestamp = "2026-10-20T12:00:00.789Z".parse().unwrap();
        assert_eq!(t.fmt(ts), "2026-10-20T09:00:00-03:00");
    }

    #[test]
    fn free_start_opens_a_session() {
        let mut t = timer();
        let fx = t.start(
            at(9, 0, 0),
            "2026-10-20",
            block(),
            Mode::Free,
            plan(25, 5, 1),
        );
        assert_eq!(
            fx,
            vec![Effect::Persist {
                date: "2026-10-20".into(),
                session: Session {
                    start: s(9, 0, 0),
                    end: None,
                    mode: Mode::Free,
                    block: block(),
                    last_seen: Some(s(9, 0, 0)),
                }
            }]
        );
        assert_eq!(running(&t).phase, Phase::Focus);
        assert_eq!(t.deadline(), None);
    }

    #[test]
    fn free_stop_closes_at_now() {
        let mut t = started(Mode::Free);
        let fx = t.stop(at(9, 30, 0));
        assert_eq!(persisted_closed(&fx), vec![(s(9, 0, 0), Some(s(9, 30, 0)))]);
        assert_eq!(t.state, State::Idle);
    }

    #[test]
    fn stop_under_a_minute_discards() {
        let mut t = started(Mode::Free);
        let fx = t.stop(at(9, 0, 59));
        assert_eq!(
            fx,
            vec![Effect::Discard {
                date: "2026-10-20".into(),
                start: s(9, 0, 0)
            }]
        );
        assert_eq!(t.state, State::Idle);
    }

    #[test]
    fn stop_when_idle_does_nothing() {
        let mut t = timer();
        assert!(t.stop(at(9, 0, 0)).is_empty());
        assert!(t.tick(at(9, 0, 0)).is_empty());
        assert_eq!(t.heartbeat(at(9, 0, 0)), None);
        assert_eq!(t.deadline(), None);
    }

    #[test]
    fn free_tick_never_changes_anything() {
        let mut t = started(Mode::Free);
        assert!(t.tick(at(12, 0, 0)).is_empty());
        assert_eq!(running(&t).phase, Phase::Focus);
    }

    #[test]
    fn pomodoro_focus_end_closes_session_and_starts_break() {
        let mut t = started(Mode::Pomodoro);
        assert_eq!(t.deadline(), Some(at(9, 25, 0)));
        assert!(t.tick(at(9, 24, 59)).is_empty());
        let fx = t.tick(at(9, 25, 0));
        assert_eq!(persisted_closed(&fx), vec![(s(9, 0, 0), Some(s(9, 25, 0)))]);
        assert!(fx.contains(&Effect::Notify(Notice::FocusDone { break_min: 5 })));
        let r = running(&t);
        assert_eq!(r.phase, Phase::Break);
        assert_eq!(r.phase_start, at(9, 25, 0));
        assert_eq!(t.deadline(), Some(at(9, 30, 0)));
    }

    #[test]
    fn pomodoro_late_tick_closes_at_the_real_focus_end() {
        let mut t = started(Mode::Pomodoro);
        let fx = t.tick(at(9, 27, 13));
        assert_eq!(persisted_closed(&fx), vec![(s(9, 0, 0), Some(s(9, 25, 0)))]);
        assert_eq!(running(&t).phase, Phase::Break);
    }

    #[test]
    fn pomodoro_break_end_goes_idle_with_a_notice() {
        let mut t = started(Mode::Pomodoro);
        t.tick(at(9, 25, 0));
        assert!(t.tick(at(9, 29, 59)).is_empty());
        let fx = t.tick(at(9, 30, 0));
        assert_eq!(fx, vec![Effect::Notify(Notice::RunDone { cycles: 1 })]);
        assert_eq!(t.state, State::Idle);
    }

    #[test]
    fn pomodoro_very_late_tick_finishes_focus_and_break() {
        let mut t = started(Mode::Pomodoro);
        let fx = t.tick(at(11, 0, 0));
        assert_eq!(persisted_closed(&fx), vec![(s(9, 0, 0), Some(s(9, 25, 0)))]);
        assert!(fx.contains(&Effect::Notify(Notice::FocusDone { break_min: 5 })));
        assert_eq!(
            fx.last(),
            Some(&Effect::Notify(Notice::RunDone { cycles: 1 }))
        );
        assert_eq!(t.state, State::Idle);
    }

    #[test]
    fn stop_during_focus_closes_at_now() {
        let mut t = started(Mode::Pomodoro);
        let fx = t.stop(at(9, 10, 0));
        assert_eq!(persisted_closed(&fx), vec![(s(9, 0, 0), Some(s(9, 10, 0)))]);
        assert_eq!(t.state, State::Idle);
    }

    #[test]
    fn stop_during_break_records_nothing() {
        let mut t = started(Mode::Pomodoro);
        t.tick(at(9, 25, 0));
        assert!(t.stop(at(9, 27, 0)).is_empty());
        assert_eq!(t.state, State::Idle);
    }

    #[test]
    fn start_while_running_stops_the_previous_one_first() {
        let mut t = started(Mode::Free);
        let mut other = block();
        other.title = "Other".into();
        let fx = t.start(
            at(9, 40, 0),
            "2026-10-20",
            other.clone(),
            Mode::Pomodoro,
            plan(25, 5, 1),
        );
        assert_eq!(
            persisted_closed(&fx),
            vec![(s(9, 0, 0), Some(s(9, 40, 0))), (s(9, 40, 0), None)]
        );
        assert_eq!(running(&t).block, other);
        assert_eq!(running(&t).mode, Mode::Pomodoro);
    }

    #[test]
    fn start_during_break_records_nothing_extra() {
        let mut t = started(Mode::Pomodoro);
        t.tick(at(9, 25, 0));
        let fx = t.start(
            at(9, 27, 0),
            "2026-10-20",
            block(),
            Mode::Free,
            plan(25, 5, 1),
        );
        assert_eq!(persisted_closed(&fx), vec![(s(9, 27, 0), None)]);
        assert_eq!(running(&t).phase, Phase::Focus);
    }

    #[test]
    fn heartbeat_refreshes_last_seen_only_while_a_session_is_open() {
        let mut t = started(Mode::Pomodoro);
        match t.heartbeat(at(9, 1, 0)) {
            Some(Effect::Heartbeat { session, date }) => {
                assert_eq!(date, "2026-10-20");
                assert_eq!(session.end, None);
                assert_eq!(session.last_seen, Some(s(9, 1, 0)));
                assert_eq!(session.start, s(9, 0, 0));
            }
            other => panic!("{other:?}"),
        }
        t.tick(at(9, 25, 0));
        assert_eq!(t.heartbeat(at(9, 26, 0)), None);
    }

    // ---- persistence and recovery ----

    #[test]
    fn persisted_shape_is_camel_case() {
        let t = started(Mode::Pomodoro);
        let v = serde_json::to_value(t.persisted().unwrap()).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "date": "2026-10-20", "start": s(9, 0, 0), "mode": "pomodoro", "phase": "focus",
                "phaseStart": s(9, 0, 0), "focusMin": 25, "breakMin": 5, "cycle": 1, "cycles": 1,
                "block": {"start": "09:00", "end": "10:30", "title": "Deep work", "tags": ["mestrado"]}
            })
        );
        assert_eq!(timer().persisted(), None);
    }

    fn recovered(mode: Mode, last_seen: Timestamp, now: Timestamp) -> (Timer, Vec<Effect>) {
        let saved = started(mode).persisted().unwrap();
        recover(tz(), &saved, last_seen, now).unwrap()
    }

    #[test]
    fn recover_resumes_silently_within_three_minutes() {
        let (t, fx) = recovered(Mode::Free, at(9, 10, 0), at(9, 13, 0));
        assert!(fx.is_empty());
        assert_eq!(running(&t).session_start, at(9, 0, 0));
    }

    #[test]
    fn recover_after_more_than_three_minutes_is_stale() {
        let (t, fx) = recovered(Mode::Free, at(9, 10, 0), at(9, 13, 1));
        assert!(fx.is_empty());
        match t.state {
            State::Stale { last_seen, .. } => assert_eq!(last_seen, at(9, 10, 0)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn recover_pomodoro_still_in_focus_resumes() {
        let (t, _) = recovered(Mode::Pomodoro, at(9, 10, 0), at(9, 11, 0));
        assert_eq!(running(&t).phase, Phase::Focus);
    }

    #[test]
    fn recover_pomodoro_whose_focus_ended_closes_at_min_of_end_and_last_seen() {
        let (t, fx) = recovered(Mode::Pomodoro, at(9, 10, 0), at(9, 40, 0));
        assert_eq!(t.state, State::Idle);
        assert_eq!(persisted_closed(&fx), vec![(s(9, 0, 0), Some(s(9, 10, 0)))]);
        let (t, fx) = recovered(Mode::Pomodoro, at(9, 26, 0), at(9, 27, 0));
        assert_eq!(t.state, State::Idle);
        assert_eq!(persisted_closed(&fx), vec![(s(9, 0, 0), Some(s(9, 25, 0)))]);
    }

    #[test]
    fn recover_pomodoro_in_break_goes_idle() {
        let mut t = started(Mode::Pomodoro);
        t.tick(at(9, 25, 0));
        let saved = t.persisted().unwrap();
        assert_eq!(saved.phase, Phase::Break);
        let (t, fx) = recover(tz(), &saved, at(9, 26, 0), at(9, 27, 0)).unwrap();
        assert_eq!(t.state, State::Idle);
        assert!(fx.is_empty());
    }

    #[test]
    fn recover_rejects_unparseable_timestamps() {
        let mut saved = started(Mode::Free).persisted().unwrap();
        saved.start = "nope".into();
        assert!(recover(tz(), &saved, at(9, 0, 0), at(9, 1, 0)).is_err());
    }

    #[test]
    fn stale_keep_running_resumes_without_effects() {
        let (mut t, _) = recovered(Mode::Free, at(9, 10, 0), at(10, 0, 0));
        assert!(t.resolve_stale(true).is_empty());
        assert_eq!(running(&t).session_start, at(9, 0, 0));
    }

    #[test]
    fn stale_stop_closes_at_last_seen() {
        let (mut t, _) = recovered(Mode::Free, at(9, 10, 0), at(10, 0, 0));
        let fx = t.resolve_stale(false);
        assert_eq!(persisted_closed(&fx), vec![(s(9, 0, 0), Some(s(9, 10, 0)))]);
        assert_eq!(t.state, State::Idle);
    }

    #[test]
    fn stale_stop_with_short_session_discards() {
        let (mut t, _) = recovered(Mode::Free, at(9, 0, 30), at(10, 0, 0));
        let fx = t.resolve_stale(false);
        assert!(matches!(fx[..], [Effect::Discard { .. }]));
    }

    #[test]
    fn start_while_stale_closes_the_stale_session_at_last_seen() {
        let (mut t, _) = recovered(Mode::Free, at(9, 10, 0), at(10, 0, 0));
        let fx = t.start(
            at(10, 0, 0),
            "2026-10-20",
            block(),
            Mode::Free,
            plan(25, 5, 1),
        );
        assert_eq!(
            persisted_closed(&fx),
            vec![(s(9, 0, 0), Some(s(9, 10, 0))), (s(10, 0, 0), None)]
        );
    }

    #[test]
    fn stale_ignores_ticks_and_heartbeats() {
        let (mut t, _) = recovered(Mode::Pomodoro, at(9, 10, 0), at(10, 0, 0));
        assert!(t.tick(at(10, 0, 0)).is_empty());
        assert_eq!(t.heartbeat(at(10, 0, 0)), None);
        assert_eq!(t.deadline(), None);
    }

    // ---- M6b: cycles ----

    fn run_of(cycles: u32) -> Timer {
        let mut t = timer();
        t.start(
            at(9, 0, 0),
            "2026-10-20",
            block(),
            Mode::Pomodoro,
            plan(25, 5, cycles),
        );
        t
    }

    #[test]
    fn run_of_three_goes_focus_break_focus_break_focus_break_idle() {
        let mut t = run_of(3);
        assert_eq!((running(&t).cycle, running(&t).cycles), (1, 3));
        // focus 1 ends, break 1
        let fx = t.tick(at(9, 25, 0));
        assert_eq!(persisted_closed(&fx), vec![(s(9, 0, 0), Some(s(9, 25, 0)))]);
        assert_eq!(running(&t).phase, Phase::Break);
        // break 1 ends -> focus 2 as a new session starting at the break end
        let fx = t.tick(at(9, 30, 0));
        assert_eq!(persisted_closed(&fx), vec![(s(9, 30, 0), None)]);
        assert!(fx.contains(&Effect::Notify(Notice::NextFocus {
            cycle: 2,
            cycles: 3
        })));
        let r = running(&t);
        assert_eq!(
            (r.phase, r.cycle, r.session_start, r.phase_start),
            (Phase::Focus, 2, at(9, 30, 0), at(9, 30, 0))
        );
        assert_eq!(t.deadline(), Some(at(9, 55, 0)));
        t.tick(at(9, 55, 0));
        let fx = t.tick(at(10, 0, 0));
        assert_eq!(running(&t).cycle, 3);
        assert_eq!(persisted_closed(&fx), vec![(s(10, 0, 0), None)]);
        t.tick(at(10, 25, 0));
        assert_eq!(running(&t).phase, Phase::Break);
        let fx = t.tick(at(10, 30, 0));
        assert!(fx.contains(&Effect::Notify(Notice::RunDone { cycles: 3 })));
        assert_eq!(t.state, State::Idle);
    }

    #[test]
    fn very_late_tick_advances_only_as_far_as_the_wall_clock_allows() {
        let mut t = run_of(4);
        // 09:00 + 2 full cycles (60 min) + 10 min into focus 3
        let fx = t.tick(at(10, 10, 0));
        let r = running(&t);
        assert_eq!((r.phase, r.cycle), (Phase::Focus, 3));
        assert_eq!(r.phase_start, at(10, 0, 0));
        let closed: Vec<_> = persisted_closed(&fx);
        assert_eq!(
            closed,
            vec![
                (s(9, 0, 0), Some(s(9, 25, 0))),
                (s(9, 30, 0), None),
                (s(9, 30, 0), Some(s(9, 55, 0))),
                (s(10, 0, 0), None),
            ]
        );
    }

    #[test]
    fn very_late_tick_past_the_whole_run_ends_idle_without_extra_cycles() {
        let mut t = run_of(2);
        let fx = t.tick(at(23, 0, 0));
        assert_eq!(t.state, State::Idle);
        let starts = persisted_closed(&fx)
            .iter()
            .filter(|(_, e)| e.is_none())
            .count();
        assert_eq!(starts, 1);
        assert_eq!(
            fx.last(),
            Some(&Effect::Notify(Notice::RunDone { cycles: 2 }))
        );
    }

    #[test]
    fn skip_break_starts_the_next_focus_now() {
        let mut t = run_of(3);
        t.tick(at(9, 25, 0));
        let fx = t.skip_break(at(9, 27, 0));
        assert_eq!(persisted_closed(&fx), vec![(s(9, 27, 0), None)]);
        assert!(!fx.iter().any(|e| matches!(e, Effect::Notify(_))));
        let r = running(&t);
        assert_eq!(
            (r.phase, r.cycle, r.phase_start),
            (Phase::Focus, 2, at(9, 27, 0))
        );
        assert_eq!(t.deadline(), Some(at(9, 52, 0)));
    }

    #[test]
    fn skip_break_on_the_last_break_finishes_the_run() {
        let mut t = run_of(1);
        t.tick(at(9, 25, 0));
        let fx = t.skip_break(at(9, 26, 0));
        assert!(fx.is_empty());
        assert_eq!(t.state, State::Idle);
    }

    #[test]
    fn skip_break_outside_a_break_is_a_no_op() {
        let mut t = run_of(3);
        assert!(t.skip_break(at(9, 5, 0)).is_empty());
        assert_eq!(running(&t).phase, Phase::Focus);
        let mut idle = timer();
        assert!(idle.skip_break(at(9, 5, 0)).is_empty());
        let mut free = started(Mode::Free);
        assert!(free.skip_break(at(9, 5, 0)).is_empty());
        assert_eq!(running(&free).phase, Phase::Focus);
    }

    #[test]
    fn stop_during_focus_of_a_later_cycle_records_it_and_ends_the_run() {
        let mut t = run_of(3);
        t.tick(at(9, 25, 0));
        t.tick(at(9, 30, 0));
        let fx = t.stop(at(9, 40, 0));
        assert_eq!(
            persisted_closed(&fx),
            vec![(s(9, 30, 0), Some(s(9, 40, 0)))]
        );
        assert_eq!(t.state, State::Idle);
        assert_eq!(t.deadline(), None);
    }

    #[test]
    fn stop_during_a_later_break_ends_the_run_recording_nothing() {
        let mut t = run_of(3);
        t.tick(at(9, 25, 0));
        t.tick(at(9, 30, 0));
        t.tick(at(9, 55, 0));
        assert!(t.stop(at(9, 56, 0)).is_empty());
        assert_eq!(t.state, State::Idle);
    }

    #[test]
    fn free_timers_have_a_single_cycle() {
        let mut t = timer();
        t.start(
            at(9, 0, 0),
            "2026-10-20",
            block(),
            Mode::Free,
            plan(25, 5, 4),
        );
        assert_eq!(running(&t).cycles, 1);
    }

    #[test]
    fn plan_limits() {
        assert!(
            Plan {
                focus_min: 1,
                break_min: 1,
                cycles: 1
            }
            .validate()
            .is_ok()
        );
        assert!(
            Plan {
                focus_min: 180,
                break_min: 60,
                cycles: 12
            }
            .validate()
            .is_ok()
        );
        for (f, b, c) in [
            (0, 5, 4),
            (181, 5, 4),
            (25, 0, 4),
            (25, 61, 4),
            (25, 5, 0),
            (25, 5, 13),
        ] {
            assert!(
                Plan {
                    focus_min: f,
                    break_min: b,
                    cycles: c
                }
                .validate()
                .is_err(),
                "{f} {b} {c}"
            );
        }
    }

    #[test]
    fn persisted_carries_cycle_and_cycles() {
        let mut t = run_of(4);
        t.tick(at(9, 25, 0));
        t.tick(at(9, 30, 0));
        let v = serde_json::to_value(t.persisted().unwrap()).unwrap();
        assert_eq!(v["cycle"], 2);
        assert_eq!(v["cycles"], 4);
        assert_eq!(v["start"], s(9, 30, 0));
    }

    #[test]
    fn old_timer_json_without_cycles_defaults_to_one_of_one() {
        let old = r#"{"date":"2026-10-20","start":"2026-10-20T09:00:00-03:00","mode":"pomodoro",
            "phase":"focus","phaseStart":"2026-10-20T09:00:00-03:00","focusMin":25,"breakMin":5,
            "block":{"start":"09:00","end":"10:30","title":"Deep work","tags":[]}}"#;
        let saved: Persisted = serde_json::from_str(old).unwrap();
        assert_eq!((saved.cycle, saved.cycles), (1, 1));
    }

    #[test]
    fn recover_restores_the_cycle() {
        let mut t = run_of(4);
        t.tick(at(9, 25, 0));
        t.tick(at(9, 30, 0));
        let saved = t.persisted().unwrap();
        let (t, fx) = recover(tz(), &saved, at(9, 35, 0), at(9, 36, 0)).unwrap();
        assert!(fx.is_empty());
        assert_eq!((running(&t).cycle, running(&t).cycles), (2, 4));
    }
}
