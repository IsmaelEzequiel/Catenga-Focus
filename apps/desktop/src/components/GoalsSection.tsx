import { useEffect, useMemo, useRef, useState, type FormEvent } from "react";
import { ArrowsPointingOutIcon, CheckIcon, EllipsisHorizontalIcon, PlayIcon, PlusIcon } from "@heroicons/react/20/solid";
import type { GoalDraft, GoalProgress, Project, Tag, TimerStatus } from "../api";
import { useGoals } from "../hooks/useGoals";
import { useLoggedToday } from "../hooks/useLoggedToday";
import { fmtDuration } from "../lib/dashboard";
import {
  applyLive,
  buildDraft,
  cellFill,
  fromMinutes,
  goalExtras,
  goalMetaLine,
  goalSummary,
  goalSubjectText,
  goalTitle,
  goalsSummary,
  stripCells,
  groupGoals,
  liveFocusMinutes,
  matchesGoal,
  ringFraction,
} from "../lib/goals";
import { projectStyle } from "../lib/projects";
import { toDateString } from "../lib/time";
import { ProjectDot, ProjectPicker } from "./ProjectPicker";
import { TagInput } from "./TagInput";
import { Banner } from "./ui/Banner";
import { Button } from "./ui/Button";
import { ListRow } from "./ui/ListRow";
import { StatTile } from "./ui/StatTile";
import "../card.css";
import "../field.css";
import "../goals.css";

interface Props {
  projects: Project[];
  /** Saved tags plus the ones seen in notes, for the tag input. */
  tags: Tag[];
  timerStatus: TimerStatus;
  nowMs: number;
  onPlay: (p: GoalProgress) => void;
}

const PRESETS = [30, 60, 120];
const RING_R = 28;
const RING_C = 2 * Math.PI * RING_R;

type FormState = { goal: GoalProgress | null } | null;

export function GoalsSection({ projects, tags, timerStatus, nowMs, onPlay }: Props) {
  const g = useGoals();
  const [form, setForm] = useState<FormState>(null);
  const [showArchived, setShowArchived] = useState(false);
  const [open, setOpen] = useState<{ upcoming: boolean; past: boolean }>({ upcoming: false, past: false });

  const today = toDateString(new Date(nowMs));
  const live = liveFocusMinutes(timerStatus, nowMs);
  const runningBlock = timerStatus.state === "running" && timerStatus.phase === "focus" ? timerStatus.block : null;
  const view = (p: GoalProgress) =>
    runningBlock && timerStatus.state === "running" && timerStatus.date === today && matchesGoal(p, runningBlock)
      ? applyLive(p, live, today)
      : p;
  const groups = useMemo(() => groupGoals(g.list ?? []), [g.list]);
  const total = (g.list ?? []).length;

  const loggedToday = useLoggedToday(today);
  const kpi = goalsSummary(groups.active.map(view), loggedToday + live);
  const timed = timerStatus.state === "running" ? timerStatus.block : null;

  const card = (p: GoalProgress) => (
    <GoalCard
      key={p.goal.id}
      p={view(p)}
      live={view(p) !== p}
      today={today}
      running={timed !== null && matchesGoal(p, timed)}
      onPlay={() => onPlay(p)}
      onEdit={() => setForm({ goal: p })}
      onArchive={() => void g.setArchived(p.goal.id, true)}
      onDelete={() => g.remove(p.goal.id)}
    />
  );

  return (
    <section className="goals" aria-label="Goals">
      <header className="goals__head">
        <h2 className="goals__title">Goals</h2>
        {!form && total > 0 && (
          <Button icon={PlusIcon} onClick={() => setForm({ goal: null })}>
            New goal
          </Button>
        )}
      </header>

      <div className="goals__stats" role="group" aria-label="Goals summary">
        <StatTile label="Today" value={kpi.today.value} sub={kpi.today.sub} />
        <StatTile label="Success rate" value={kpi.success.value} sub={kpi.success.sub} />
        <StatTile label="Best streak" value={kpi.streak.value} sub={kpi.streak.sub} />
        <StatTile label="Logged today" value={kpi.logged.value} sub={kpi.logged.sub} />
      </div>

      {g.error && (
        <Banner tone="error" variant="inline">
          {g.error}
        </Banner>
      )}

      {form && (
        <GoalForm
          key={form.goal?.goal.id ?? "new"}
          p={form.goal}
          projects={projects}
          tags={tags}
          today={today}
          onCancel={() => setForm(null)}
          onSave={async (draft) => {
            const err = await (form.goal ? g.update(form.goal.goal.id, draft) : g.create(draft));
            if (!err) setForm(null);
            return err;
          }}
        />
      )}

      {g.list !== null && total === 0 && !form && (
        <div className="goals__empty card">
          <p>Set a goal, for example Mestrado, 1h a day for 30 days.</p>
          <Button variant="primary" icon={PlusIcon} onClick={() => setForm({ goal: null })}>
            New goal
          </Button>
        </div>
      )}

      {groups.active.length > 0 && <ul className="goals__list">{groups.active.map((p) => <li key={p.goal.id}>{card(p)}</li>)}</ul>}

      {(["upcoming", "past"] as const).map((k) =>
        groups[k].length === 0 ? null : (
          <div key={k} className="goals__group">
            <button
              className="goals__toggle"
              aria-expanded={open[k]}
              onClick={() => setOpen((o) => ({ ...o, [k]: !o[k] }))}
            >
              {k === "upcoming" ? "Upcoming" : "Past goals"} <span>{groups[k].length}</span>
            </button>
            {open[k] && <ul className="goals__list">{groups[k].map((p) => <li key={p.goal.id}>{card(p)}</li>)}</ul>}
          </div>
        ),
      )}

      {g.archived.length > 0 && (
        <div className="goals__group">
          <button className="goals__toggle" aria-expanded={showArchived} onClick={() => setShowArchived((v) => !v)}>
            Archived <span>{g.archived.length}</span>
          </button>
          {showArchived && (
            <div className="card card--flush">
              {g.archived.map((p) => (
                <ArchivedRow
                  key={p.goal.id}
                  p={p}
                  onRestore={() => g.setArchived(p.goal.id, false)}
                  onDelete={() => g.remove(p.goal.id)}
                />
              ))}
            </div>
          )}
        </div>
      )}
    </section>
  );
}

function Ring({ minutes, target }: { minutes: number; target: number }) {
  const met = minutes >= target;
  const frac = ringFraction(minutes, target);
  return (
    <span className="goalring" data-met={met} role="img" aria-label={`${fmtDuration(minutes)} of ${fmtDuration(target)} today`}>
      <svg viewBox="0 0 64 64" width="64" height="64" aria-hidden="true">
        <circle className="goalring__track" cx="32" cy="32" r={RING_R} />
        <circle
          className="goalring__arc"
          cx="32"
          cy="32"
          r={RING_R}
          strokeDasharray={RING_C}
          strokeDashoffset={RING_C * (1 - frac)}
          transform="rotate(-90 32 32)"
        />
      </svg>
      <span className="goalring__center">
        {met ? <CheckIcon className="goalring__check" aria-hidden="true" /> : minutes > 0 ? fmtDuration(minutes) : `${Math.round(frac * 100)}%`}
      </span>
    </span>
  );
}

function GoalCard(props: {
  p: GoalProgress;
  live: boolean;
  today: string;
  running: boolean;
  onPlay: () => void;
  onEdit: () => void;
  onArchive: () => void;
  onDelete: () => Promise<string | null>;
}) {
  const { p, today } = props;
  const target = p.goal.minutesPerDay;
  const active = p.status === "active";
  const [confirm, setConfirm] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const extras = goalExtras(p);
  const cells = stripCells(p.last14, p.goal.start, today);
  const playHint = active ? undefined : p.status === "upcoming" ? "This goal hasn't started yet" : "This goal has ended";
  const meta = [goalMetaLine(p, today), extras.title, ...extras.tags.map((name) => `#${name}`)].filter(Boolean).join(" · ");
  return (
    <article className="goal card" data-status={p.status}>
      {active ? <Ring minutes={p.todayMinutes} target={target} /> : <span className="goalring goalring--idle" aria-hidden="true" />}
      <div className="goal__main">
        <h4 className="goal__name" title={goalSubjectText(p)} style={projectStyle(p.project?.color ?? null)}>
          {p.project && <ProjectDot color={p.project.color} />}
          <span>{goalTitle(p)}</span>
          {props.live && (
            <span className="goal__live" title="A timer is running for this goal">
              live
            </span>
          )}
        </h4>
        <p className="goal__meta">{meta}</p>
        <ol className="goalstrip" aria-label="Last 14 days">
          {cells.map((d) => {
            const c = cellFill(d, target);
            return (
              <li
                key={d.date}
                className={`goalstrip__cell${d.date === today ? " goalstrip__cell--today" : ""}`}
                data-state={d.placeholder ? "placeholder" : c.state}
                title={d.placeholder ? `${d.date} · before the start` : `${d.date} · ${fmtDuration(d.minutes)}`}
                aria-label={d.placeholder ? `${d.date}, before the start` : `${d.date}, ${fmtDuration(d.minutes)}${d.met ? ", met" : ""}`}
              >
                <i style={{ height: `${c.fill * 100}%` }} />
              </li>
            );
          })}
        </ol>
        <p className="goal__days">
          Met {p.metDays} of {p.elapsedDays} {p.elapsedDays === 1 ? "day" : "days"} · streak {p.currentStreak} · best {p.longestStreak}
        </p>
      </div>
      <div className="goal__side">
        <div className="goal__actions">
          <Button
            variant={props.running ? "secondary" : "primary"}
            icon={props.running ? ArrowsPointingOutIcon : PlayIcon}
            disabled={!active}
            title={playHint}
            aria-label={props.running ? `Open focus screen for ${goalTitle(p)}` : `Start ${goalTitle(p)}`}
            onClick={props.onPlay}
          >
            {props.running ? "Open" : "Start"}
          </Button>
          {!confirm && <GoalMenu onEdit={props.onEdit} onArchive={props.onArchive} onDelete={() => setConfirm(true)} />}
        </div>
        {active && (
          <p className="goal__today">
            {Math.round(p.todayMinutes)} / {Math.round(target)} min today
          </p>
        )}
      </div>
      {confirm && (
        <div className="goal__confirm" role="group" aria-label="Confirm delete">
          <span>Delete this goal?</span>
          <Button
            variant="danger"
            size="sm"
            onClick={async () => {
              const e = await props.onDelete();
              if (e) setErr(e);
            }}
          >
            Delete
          </Button>
          <Button size="sm" onClick={() => setConfirm(false)}>
            Keep
          </Button>
        </div>
      )}
      {err && (
        <div className="goal__err">
          <Banner tone="error" variant="inline">
            {err}
          </Banner>
        </div>
      )}
    </article>
  );
}

function GoalMenu({ onEdit, onArchive, onDelete }: { onEdit: () => void; onArchive: () => void; onDelete: () => void }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const away = (e: MouseEvent) => !ref.current?.contains(e.target as Node) && setOpen(false);
    const esc = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    document.addEventListener("mousedown", away);
    document.addEventListener("keydown", esc);
    return () => {
      document.removeEventListener("mousedown", away);
      document.removeEventListener("keydown", esc);
    };
  }, [open]);
  const item = (label: string, fn: () => void, danger = false) => (
    <button
      role="menuitem"
      className={`goal__item${danger ? " goal__item--danger" : ""}`}
      onClick={() => {
        setOpen(false);
        fn();
      }}
    >
      {label}
    </button>
  );
  return (
    <div className="goal__menu" ref={ref}>
      <Button
        variant="ghost"
        className="button--icon goal__kebab"
        icon={EllipsisHorizontalIcon}
        aria-label="Goal actions"
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      />
      {open && (
        <div className="goal__pop" role="menu">
          {item("Edit", onEdit)}
          {item("Archive", onArchive)}
          {item("Delete", onDelete, true)}
        </div>
      )}
    </div>
  );
}

function ArchivedRow(props: {
  p: GoalProgress;
  onRestore: () => Promise<string | null>;
  onDelete: () => Promise<string | null>;
}) {
  const [confirm, setConfirm] = useState(false);
  // e.g. "Pick a project, a tag or a title before restoring this goal" — never fail silently.
  const [err, setErr] = useState<string | null>(null);
  const run = (action: () => Promise<string | null>) => void action().then(setErr);
  const { p } = props;
  return (
    <div className="goal__archived">
      <ListRow
        lead={p.project ? <ProjectDot color={p.project.color} /> : undefined}
        title={goalTitle(p)}
        meta={goalSummary(p)}
        actions={
          confirm ? (
            <span className="goal__confirm" role="group" aria-label="Confirm delete">
              <span>Delete this goal?</span>
              <Button variant="danger" size="sm" onClick={() => run(props.onDelete)}>
                Delete
              </Button>
              <Button variant="ghost" size="sm" onClick={() => setConfirm(false)}>
                Keep
              </Button>
            </span>
          ) : (
            <>
              <Button variant="ghost" size="sm" onClick={() => run(props.onRestore)}>
                Restore
              </Button>
              <Button variant="ghost" size="sm" onClick={() => setConfirm(true)}>
                Delete
              </Button>
            </>
          )
        }
      />
      {err && (
        <div className="goal__err">
          <Banner tone="error" variant="inline">
            {err}
          </Banner>
        </div>
      )}
    </div>
  );
}

function GoalForm(props: {
  p: GoalProgress | null;
  projects: Project[];
  tags: Tag[];
  today: string;
  onCancel: () => void;
  onSave: (draft: GoalDraft) => Promise<string | null>;
}) {
  const goal = props.p?.goal ?? null;
  const init = fromMinutes(goal?.minutesPerDay ?? 60);
  const [projectId, setProjectId] = useState<number | null>(goal?.projectId ?? null);
  const [tagNames, setTagNames] = useState<string[]>(props.p?.tags.map((t) => t.name) ?? []);
  const [title, setTitle] = useState(goal?.title ?? "");
  const [more, setMore] = useState(!!goal?.title);
  const [h, setH] = useState(init.h);
  const [min, setMin] = useState(init.min);
  const [endless, setEndless] = useState(goal ? goal.days === null : false);
  const [days, setDays] = useState(goal?.days ?? 30);
  const [start, setStart] = useState(goal?.start ?? props.today);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const total = h * 60 + min;

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    const r = buildDraft({ projectId, tags: tagNames, title, h, min, start, endless, days });
    if ("error" in r) {
      setError(r.error);
      return;
    }
    setBusy(true);
    const err = await props.onSave(r.draft);
    setBusy(false);
    if (err) setError(err);
  };

  const num = (v: string) => (v === "" ? 0 : Number(v));

  return (
    <form className="goalform card" onSubmit={submit} aria-label={goal ? "Edit goal" : "New goal"}>
      <div className="goalform__line">
        <span className="goalform__word">Track</span>
        <ProjectPicker projects={props.projects} value={projectId} onChange={setProjectId} noneLabel="No project" label="Project" />
        <span className="goalform__word">and tags</span>
        <div className="goalform__grow">
          <TagInput value={tagNames} onChange={setTagNames} known={props.tags} label="Tags" placeholder="#mestrado or a tag…" />
        </div>
        <span className="goalform__word">for</span>
        <input className="field__input goalform__num" type="number" min={0} max={12} value={h} onChange={(e) => setH(num(e.target.value))} aria-label="Hours per day" />
        <span className="goalform__word">h</span>
        <input className="field__input goalform__num" type="number" min={0} max={59} value={min} onChange={(e) => setMin(num(e.target.value))} aria-label="Minutes per day" />
        <span className="goalform__word">min a day, for</span>
        <input
          className="field__input goalform__num"
          type="number"
          min={1}
          value={days}
          disabled={endless}
          onChange={(e) => setDays(num(e.target.value))}
          aria-label="Number of days"
        />
        <span className="goalform__word">days starting</span>
        <input className="field__input" type="date" value={start} onChange={(e) => setStart(e.target.value)} aria-label="Start date" />
      </div>
      <div className="goalform__presets">
        <span>Time:</span>
        {PRESETS.map((m) => (
          <button
            key={m}
            type="button"
            className="goalform__chip"
            aria-pressed={total === m}
            onClick={() => {
              const v = fromMinutes(m);
              setH(v.h);
              setMin(v.min);
            }}
          >
            {fmtDuration(m)}
          </button>
        ))}
        <span>Length:</span>
        <button
          type="button"
          className="goalform__chip"
          aria-pressed={!endless && days === 30}
          onClick={() => {
            setEndless(false);
            setDays(30);
          }}
        >
          30 days
        </button>
        <button type="button" className="goalform__chip" aria-pressed={endless} onClick={() => setEndless((v) => !v)}>
          No end
        </button>
      </div>
      {more && (
        <label className="field goalform__filter">
          <span className="field__label">Only count blocks titled</span>
          <input className="field__input" value={title} onChange={(e) => setTitle(e.target.value)} placeholder="Rust" aria-label="Title filter" />
        </label>
      )}
      {error && (
        <Banner tone="error" variant="inline">
          {error}
        </Banner>
      )}
      <div className="goalform__actions">
        <Button type="submit" variant="primary" disabled={busy}>
          {goal ? "Save" : "Add goal"}
        </Button>
        <Button variant="ghost" onClick={props.onCancel}>
          Cancel
        </Button>
        <button type="button" className="goalform__link" aria-expanded={more} onClick={() => setMore((v) => !v)}>
          More options
        </button>
      </div>
    </form>
  );
}
