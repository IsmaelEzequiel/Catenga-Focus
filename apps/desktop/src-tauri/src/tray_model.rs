//! What the menu bar item shows (M12): the title next to the icon, the tooltip and
//! the menu contents. Pure: the Tauri glue lives in `tray`.

/// Longest block title shown in the menu bar text.
pub const MAX_TITLE_CHARS: usize = 24;
/// Upcoming items listed in the menu.
pub const MAX_NEXT: usize = 8;
const APP_NAME: &str = "Catenga Focus";

/// A block of today's day file (minutes since midnight).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockIn {
    pub start: u16,
    pub end: u16,
    pub title: String,
}

/// A calendar event of today (minutes since midnight).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventIn {
    pub id: String,
    pub start: u16,
    pub end: u16,
    pub title: String,
}

/// The timer as the tray sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimerIn {
    Idle,
    /// Free timer: seconds since the session started.
    Free {
        title: String,
        elapsed_secs: i64,
    },
    /// Pomodoro focus: seconds left in the phase; `cycle` is (current, of).
    Focus {
        title: String,
        remaining_secs: i64,
        cycle: Option<(u32, u32)>,
    },
    /// Pomodoro break: seconds left in the phase.
    Break {
        title: String,
        remaining_secs: i64,
        cycle: Option<(u32, u32)>,
    },
}

/// One row of the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub start: u16,
    pub end: u16,
    pub title: String,
    /// A calendar event with no block yet.
    pub ghost: bool,
}

impl Item {
    /// `HH:MM–HH:MM  Title`.
    pub fn label(&self) -> String {
        format!(
            "{}\u{2013}{}  {}",
            hhmm(self.start),
            hhmm(self.end),
            self.title
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayModel {
    /// Menu bar text; `None` (no timer) shows the icon only.
    pub title: Option<String>,
    pub tooltip: String,
    pub now_item: Option<Item>,
    pub next: Vec<Item>,
    pub timer_running: bool,
    /// The running timer's live state, the menu's first line: `Rust · Focus 1 of 4 · 24:56 left`.
    pub live: Option<String>,
    /// A Pomodoro break is running (the menu offers Skip break).
    pub on_break: bool,
}

impl TrayModel {
    /// The menu's first line: the live timer, else `Now: Title · until HH:MM`.
    pub fn header(&self) -> String {
        if let Some(live) = &self.live {
            return live.clone();
        }
        match &self.now_item {
            Some(i) => format!("Now: {} \u{b7} until {}", i.title, hhmm(i.end)),
            None => "Nothing scheduled now".to_string(),
        }
    }

    /// What "Start now" would start: the current item, else the next one; nothing while a timer runs.
    pub fn start_now(&self) -> Option<&Item> {
        if self.timer_running {
            return None;
        }
        self.now_item.as_ref().or_else(|| self.next.first())
    }

    /// The menu row for `start_now`, saying what it starts: `Start Rust`, or
    /// `Start Rust early (12:30)` when it hasn't begun yet.
    pub fn start_now_label(&self) -> Option<String> {
        let item = self.start_now()?;
        let title = truncate(&item.title);
        Some(if self.now_item.is_some() {
            format!("Start {title}")
        } else {
            format!("Start {title} early ({})", hhmm(item.start))
        })
    }

    /// Upcoming items to list, without the one "Start" already offers.
    pub fn listed_next(&self) -> &[Item] {
        let offered = self.now_item.is_none() && self.start_now().is_some();
        if offered { &self.next[1..] } else { &self.next }
    }

    /// Whether the menu's structure differs from `other`'s; the live line and the
    /// title change every second and are updated in place.
    pub fn same_menu(&self, other: &TrayModel) -> bool {
        self.now_item == other.now_item
            && self.next == other.next
            && self.timer_running == other.timer_running
            && self.on_break == other.on_break
            && self.live.is_some() == other.live.is_some()
    }
}

fn hhmm(minutes: u16) -> String {
    let m = minutes % 1440;
    format!("{:02}:{:02}", m / 60, m % 60)
}

fn title_or_untitled(title: &str) -> String {
    let t = title.trim();
    if t.is_empty() { "Untitled" } else { t }.to_string()
}

/// Cuts to `MAX_TITLE_CHARS` characters, the last one being `…` when cut.
pub fn truncate(title: &str) -> String {
    if title.chars().count() <= MAX_TITLE_CHARS {
        return title.to_string();
    }
    let mut cut: String = title.chars().take(MAX_TITLE_CHARS - 1).collect();
    cut.push('\u{2026}');
    cut
}

/// Always `MM:SS` (minutes keep counting past an hour), padded with figure spaces
/// (U+2007, as wide as a digit) so the menu bar text doesn't change width.
fn mmss(secs: i64) -> String {
    let s = secs.max(0);
    format!("{:\u{2007}>2}:{:02}", s / 60, s % 60)
}

/// `Focus 1 of 4 · ` (empty without a cycle).
fn cycle_part(phase: &str, cycle: Option<(u32, u32)>) -> String {
    match cycle {
        Some((n, of)) => format!("{phase} {n} of {of} \u{b7} "),
        None => format!("{phase} \u{b7} "),
    }
}

/// Today's items: every block, plus the events no block stands for (same slot
/// and trimmed title, one event per block, as the timeline matches them),
/// sorted by start.
pub fn items(blocks: &[BlockIn], events: &[EventIn]) -> Vec<Item> {
    let mut taken = vec![false; events.len()];
    let mut all: Vec<Item> = Vec::with_capacity(blocks.len() + events.len());
    for b in blocks {
        let hit = events.iter().enumerate().position(|(i, e)| {
            !taken[i] && e.start == b.start && e.end == b.end && e.title.trim() == b.title.trim()
        });
        if let Some(i) = hit {
            taken[i] = true;
        }
        all.push(Item {
            start: b.start,
            end: b.end,
            title: title_or_untitled(&b.title),
            ghost: false,
        });
    }
    for (e, taken) in events.iter().zip(&taken) {
        if !taken {
            all.push(Item {
                start: e.start,
                end: e.end,
                title: title_or_untitled(&e.title),
                ghost: true,
            });
        }
    }
    all.sort_by_key(|i| (i.start, i.end));
    all
}

/// `minute` is the local minute of the day.
pub fn build(minute: u16, blocks: &[BlockIn], events: &[EventIn], timer: &TimerIn) -> TrayModel {
    let all = items(blocks, events);
    let now_item = all
        .iter()
        .find(|i| i.start <= minute && minute < i.end)
        .cloned();
    let next: Vec<Item> = all
        .into_iter()
        .filter(|i| i.start > minute)
        .take(MAX_NEXT)
        .collect();
    let (title, live) = match timer {
        TimerIn::Idle => (None, None),
        TimerIn::Free {
            title,
            elapsed_secs,
        } => {
            let name = truncate(&title_or_untitled(title));
            let clock = mmss(*elapsed_secs);
            (
                Some(format!("{name} {clock}")),
                Some(format!("{name} \u{b7} {clock}")),
            )
        }
        TimerIn::Focus {
            title,
            remaining_secs,
            cycle,
        } => {
            let name = truncate(&title_or_untitled(title));
            let clock = mmss(*remaining_secs);
            (
                Some(format!("{name} {clock}")),
                Some(format!(
                    "{name} \u{b7} {}{clock} left",
                    cycle_part("Focus", *cycle)
                )),
            )
        }
        TimerIn::Break {
            title,
            remaining_secs,
            cycle,
        } => {
            let name = truncate(&title_or_untitled(title));
            let clock = mmss(*remaining_secs);
            (
                Some(format!("Break {clock}")),
                Some(format!(
                    "{name} \u{b7} {}{clock} left",
                    cycle_part("Break", *cycle)
                )),
            )
        }
    };
    TrayModel {
        tooltip: title.clone().unwrap_or_else(|| APP_NAME.to_string()),
        title,
        now_item,
        next,
        timer_running: *timer != TimerIn::Idle,
        live,
        on_break: matches!(timer, TimerIn::Break { .. }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(start: u16, end: u16, title: &str) -> BlockIn {
        BlockIn {
            start,
            end,
            title: title.to_string(),
        }
    }

    fn event(id: &str, start: u16, end: u16, title: &str) -> EventIn {
        EventIn {
            id: id.to_string(),
            start,
            end,
            title: title.to_string(),
        }
    }

    fn free(secs: i64) -> TimerIn {
        TimerIn::Free {
            title: "Rust".to_string(),
            elapsed_secs: secs,
        }
    }

    const FS: char = '\u{2007}';

    fn brk(remaining_secs: i64) -> TimerIn {
        TimerIn::Break {
            title: "Rust".to_string(),
            remaining_secs,
            cycle: Some((2, 4)),
        }
    }

    #[test]
    fn free_timer_title_is_elapsed_time() {
        let m = build(600, &[], &[], &free(754));
        assert_eq!(m.title.as_deref(), Some("Rust 12:34"));
        assert!(m.timer_running);
        assert_eq!(m.header(), "Rust \u{b7} 12:34");
    }

    #[test]
    fn free_timer_keeps_counting_minutes_past_an_hour() {
        let m = build(600, &[], &[], &free(3600 + 5 * 60 + 7));
        assert_eq!(m.title.as_deref(), Some("Rust 65:07"));
        let m = build(600, &[], &[], &free(3599));
        assert_eq!(m.title.as_deref(), Some("Rust 59:59"));
    }

    #[test]
    fn single_digit_minutes_are_padded_with_a_figure_space() {
        let m = build(600, &[], &[], &free(65));
        assert_eq!(m.title, Some(format!("Rust {FS}1:05")));
    }

    #[test]
    fn pomodoro_focus_shows_remaining_and_the_live_line_has_the_cycle() {
        let focus = TimerIn::Focus {
            title: "Rust".to_string(),
            remaining_secs: 24 * 60 + 56,
            cycle: Some((1, 4)),
        };
        let m = build(600, &[], &[], &focus);
        assert_eq!(m.title.as_deref(), Some("Rust 24:56"));
        assert_eq!(m.header(), "Rust \u{b7} Focus 1 of 4 \u{b7} 24:56 left");
        assert!(!m.on_break);
        let m = build(600, &[], &[], &brk(61));
        assert_eq!(m.title, Some(format!("Break {FS}1:01")));
        assert_eq!(
            m.header(),
            format!("Rust \u{b7} Break 2 of 4 \u{b7} {FS}1:01 left")
        );
        assert!(m.on_break);
    }

    #[test]
    fn negative_remaining_clamps_to_zero() {
        assert_eq!(
            build(600, &[], &[], &brk(-3)).title,
            Some(format!("Break {FS}0:00"))
        );
    }

    #[test]
    fn running_timer_wins_over_current_item() {
        let m = build(600, &[block(540, 660, "Other")], &[], &free(60));
        assert_eq!(m.title, Some(format!("Rust {FS}1:00")));
        assert_eq!(m.now_item.unwrap().title, "Other");
    }

    #[test]
    fn idle_shows_the_icon_only_even_during_a_block() {
        let m = build(600, &[block(540, 660, "Deep work")], &[], &TimerIn::Idle);
        assert_eq!(m.title, None);
        assert_eq!(m.tooltip, "Catenga Focus");
        assert_eq!(m.header(), "Now: Deep work \u{b7} until 11:00");
        assert!(!m.timer_running);
    }

    #[test]
    fn nothing_now_means_icon_only() {
        let m = build(700, &[block(540, 660, "Deep work")], &[], &TimerIn::Idle);
        assert_eq!(m.title, None);
        assert_eq!(m.tooltip, "Catenga Focus");
        assert_eq!(m.header(), "Nothing scheduled now");
    }

    #[test]
    fn range_is_start_inclusive_end_exclusive() {
        let b = [block(540, 660, "A")];
        assert!(build(540, &b, &[], &TimerIn::Idle).now_item.is_some());
        assert!(build(659, &b, &[], &TimerIn::Idle).now_item.is_some());
        assert!(build(660, &b, &[], &TimerIn::Idle).now_item.is_none());
        assert!(build(539, &b, &[], &TimerIn::Idle).now_item.is_none());
    }

    #[test]
    fn long_titles_are_truncated_with_an_ellipsis() {
        let long = "Teoria da Computacao e Linguagens Formais";
        let t = truncate(long);
        assert_eq!(t.chars().count(), 24);
        assert!(t.ends_with('\u{2026}'));
        assert_eq!(t, "Teoria da Computacao e \u{2026}");
    }

    #[test]
    fn exactly_24_chars_is_kept_and_timer_title_is_cut_before_the_time() {
        let t24 = "a".repeat(24);
        assert_eq!(truncate(&t24), t24);
        let timer = TimerIn::Free {
            title: "b".repeat(30),
            elapsed_secs: 5,
        };
        let title = build(0, &[], &[], &timer).title.unwrap();
        assert_eq!(title, format!("{}\u{2026} {FS}0:05", "b".repeat(23)));
    }

    #[test]
    fn truncation_counts_characters_not_bytes() {
        let t = "\u{e7}".repeat(30);
        let cut = truncate(&t);
        assert_eq!(cut.chars().count(), 24);
    }

    #[test]
    fn next_lists_upcoming_items_sorted_and_excludes_now_and_past() {
        let blocks = [
            block(1350, 1410, "Late"),
            block(480, 540, "Past"),
            block(600, 660, "Now"),
            block(700, 760, "Soon"),
        ];
        let m = build(610, &blocks, &[], &TimerIn::Idle);
        assert_eq!(m.now_item.as_ref().unwrap().title, "Now");
        let titles: Vec<_> = m.next.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles, ["Soon", "Late"]);
        assert_eq!(m.next[0].label(), "11:40\u{2013}12:40  Soon");
        assert_eq!(m.header(), "Now: Now \u{b7} until 11:00");
    }

    #[test]
    fn an_item_starting_exactly_now_is_current_not_next() {
        let m = build(600, &[block(600, 660, "A")], &[], &TimerIn::Idle);
        assert!(m.now_item.is_some());
        assert!(m.next.is_empty());
    }

    #[test]
    fn event_with_a_matching_block_is_not_listed_twice() {
        let blocks = [block(700, 760, "Ingles")];
        let events = [event("e1", 700, 760, " Ingles ")];
        let m = build(600, &blocks, &events, &TimerIn::Idle);
        assert_eq!(m.next.len(), 1);
        assert!(!m.next[0].ghost);
    }

    #[test]
    fn unmatched_event_is_a_ghost_and_one_event_covers_one_block() {
        let blocks = [block(700, 760, "A"), block(700, 760, "A")];
        let events = [event("e1", 700, 760, "A"), event("e2", 800, 860, "B")];
        let m = build(600, &blocks, &events, &TimerIn::Idle);
        let rows: Vec<_> = m.next.iter().map(|i| (i.title.as_str(), i.ghost)).collect();
        assert_eq!(rows, [("A", false), ("A", false), ("B", true)]);
        // Same slot but another title: the event is its own ghost.
        let m = build(
            600,
            &[block(700, 760, "A")],
            &[event("e1", 700, 760, "B")],
            &TimerIn::Idle,
        );
        assert_eq!(m.next.len(), 2);
        assert!(m.next.iter().any(|i| i.ghost));
    }

    #[test]
    fn a_ghost_can_be_the_current_item() {
        let m = build(600, &[], &[event("e1", 590, 650, "Aula")], &TimerIn::Idle);
        assert_eq!(m.header(), "Now: Aula \u{b7} until 10:50");
        assert!(m.now_item.unwrap().ghost);
    }

    #[test]
    fn next_is_capped_at_eight() {
        let blocks: Vec<_> = (0..12)
            .map(|i| block(700 + i * 10, 705 + i * 10, "X"))
            .collect();
        let m = build(600, &blocks, &[], &TimerIn::Idle);
        assert_eq!(m.next.len(), 8);
        assert_eq!(m.next[0].start, 700);
    }

    #[test]
    fn empty_titles_read_untitled() {
        let m = build(600, &[block(700, 760, "  ")], &[], &TimerIn::Idle);
        assert_eq!(m.next[0].title, "Untitled");
    }

    #[test]
    fn midnight_edges() {
        let b = [block(1380, 1440, "Night")];
        let at_2359 = build(1439, &b, &[], &TimerIn::Idle);
        assert_eq!(at_2359.now_item.as_ref().unwrap().title, "Night");
        assert_eq!(at_2359.header(), "Now: Night \u{b7} until 00:00");
        // 00:00 of the next day is minute 0 of a fresh model: nothing carries over.
        let at_0000 = build(0, &[], &[], &TimerIn::Idle);
        assert_eq!(at_0000.title, None);
        assert!(at_0000.next.is_empty());
    }

    #[test]
    fn same_menu_ignores_the_title_and_the_live_text() {
        let a = build(600, &[block(540, 660, "A")], &[], &free(1));
        let b = build(600, &[block(540, 660, "A")], &[], &free(2));
        assert!(a.same_menu(&b));
        assert_ne!(a.header(), b.header());
        let c = build(600, &[block(540, 660, "A")], &[], &TimerIn::Idle);
        assert!(!a.same_menu(&c));
        // Entering a break adds Skip break: a new menu.
        assert!(!build(600, &[], &[], &brk(60)).same_menu(&build(600, &[], &[], &free(60))));
    }

    #[test]
    fn start_now_is_the_current_item_else_the_next_and_hidden_while_a_timer_runs() {
        let blocks = [block(540, 600, "Rust"), block(660, 720, "Deep work")];
        let m = build(570, &blocks, &[], &TimerIn::Idle);
        assert_eq!(m.start_now().map(|i| i.title.as_str()), Some("Rust"));
        assert_eq!(m.start_now_label().as_deref(), Some("Start Rust"));
        assert_eq!(m.listed_next().len(), 1);
        let m = build(610, &blocks, &[], &TimerIn::Idle);
        assert_eq!(m.start_now().map(|i| i.title.as_str()), Some("Deep work"));
        assert_eq!(
            m.start_now_label().as_deref(),
            Some("Start Deep work early (11:00)")
        );
        // The block Start offers isn't listed again under "Next today".
        assert!(m.listed_next().is_empty());
        assert!(build(570, &blocks, &[], &free(10)).start_now().is_none());
        assert!(
            build(800, &blocks, &[], &TimerIn::Idle)
                .start_now()
                .is_none()
        );
        assert!(
            build(800, &blocks, &[], &TimerIn::Idle)
                .start_now_label()
                .is_none()
        );
    }

    #[test]
    fn start_now_counts_a_ghost_and_cuts_long_titles() {
        let m = build(
            500,
            &[],
            &[event(
                "e",
                540,
                600,
                "A very long calendar event title here",
            )],
            &TimerIn::Idle,
        );
        assert_eq!(
            m.start_now_label().as_deref(),
            Some("Start A very long calendar ev\u{2026} early (09:00)")
        );
    }
}
