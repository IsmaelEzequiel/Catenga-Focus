import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { ArrowPathIcon, FolderOpenIcon } from "@heroicons/react/20/solid";
import {
  loadConfig,
  notesDirInfo,
  type Config,
  type GoogleStatus,
  type NotesDirInfo,
  type SyncStatus,
} from "../api";
import type { ApplyResult } from "../lib/notesFolder";
import type { AccountState } from "../hooks/useAccount";
import type { useProjects } from "../hooks/useProjects";
import { useTags } from "../hooks/useTags";
import { SETTINGS_TABS, moveTab, type SettingsTab } from "../lib/settingsTabs";
import { notesLines, syncLine } from "../lib/sync";
import { CalendarsPane } from "./CalendarsPane";
import { ProjectsPane } from "./ProjectsPane";
import { ErrorBanner, Row, Section } from "./settingsParts";
import { TagsPane } from "./TagsPane";
import { Banner } from "./ui/Banner";
import { Button } from "./ui/Button";
import "../settings.css";

interface Props {
  account: AccountState;
  onSignIn: () => void;
  onCancel: () => void;
  onSignOut: () => void;
  sync: SyncStatus | null;
  onSyncNow: () => void;
  google: {
    status: GoogleStatus | null;
    connecting: boolean;
    error: string | null;
  };
  onGoogleConnect: () => void;
  onGoogleCancel: () => void;
  onGoogleDisconnect: (email: string) => void;
  onGoogleSetNewEventCalendar: (googleCalendarId: string) => void;
  /** Shared with the timeline and the Goals page. */
  projects: ReturnType<typeof useProjects>;
  /** The pane to open on; General when omitted. */
  initialTab?: SettingsTab;
  /** Flushes the notes, saves the folder (`null` = default) and restarts the app. */
  onApplyNotesFolder: (path: string | null) => Promise<ApplyResult>;
  onClose: () => void;
  onShowShortcuts: () => void;
}

export function Settings({
  account,
  onSignIn,
  onCancel,
  onSignOut,
  sync,
  onSyncNow,
  google,
  onGoogleConnect,
  onGoogleCancel,
  onGoogleDisconnect,
  onGoogleSetNewEventCalendar,
  projects,
  initialTab = "general",
  onApplyNotesFolder,
  onClose,
  onShowShortcuts,
}: Props) {
  const panel = useRef<HTMLDivElement>(null);
  const tabRefs = useRef(new Map<SettingsTab, HTMLButtonElement>());
  const [tab, setTab] = useState<SettingsTab>(initialTab);
  const tags = useTags();
  const { refresh } = projects;
  const canRead = !!google.status?.connected && !google.status.needsReconnect;

  useEffect(() => {
    const previous = document.activeElement;
    panel.current?.focus();
    return () => {
      if (previous instanceof HTMLElement) previous.focus();
    };
  }, []);

  // Opening Calendars or Projects re-reads the calendar list, at most every 10 minutes.
  useEffect(() => {
    if (canRead && (tab === "calendars" || tab === "projects")) void refresh(false);
  }, [canRead, tab, refresh]);

  const onKeyDown = (e: KeyboardEvent<HTMLElement>) => {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      onClose();
    } else if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
      // Day navigation behind the dialog must not fire.
      e.stopPropagation();
    }
  };

  const onTabKeyDown = (e: KeyboardEvent<HTMLButtonElement>) => {
    const next = moveTab(tab, e.key);
    if (!next) return;
    e.preventDefault();
    setTab(next);
    tabRefs.current.get(next)?.focus();
  };

  return (
    <div
      className="settings__backdrop"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        ref={panel}
        className="settings"
        role="dialog"
        aria-modal="true"
        aria-label="Settings"
        tabIndex={-1}
        onKeyDown={onKeyDown}
      >
        <header className="settings__head">
          <h2 className="settings__title">Settings</h2>
          <button
            type="button"
            className="settings__esc"
            onClick={onClose}
            aria-label="Close settings"
          >
            Esc
          </button>
        </header>
        <div className="settings__body">
          <div className="settings__nav" role="tablist" aria-label="Settings" aria-orientation="vertical">
            {SETTINGS_TABS.map((t) => (
              <button
                key={t.id}
                ref={(el) => {
                  if (el) tabRefs.current.set(t.id, el);
                  else tabRefs.current.delete(t.id);
                }}
                type="button"
                role="tab"
                id={`settings-tab-${t.id}`}
                aria-selected={tab === t.id}
                aria-controls={`settings-pane-${t.id}`}
                tabIndex={tab === t.id ? 0 : -1}
                className="settings__navitem"
                onClick={() => setTab(t.id)}
                onKeyDown={onTabKeyDown}
              >
                {t.label}
              </button>
            ))}
          </div>
          <div
            className="settings__pane"
            role="tabpanel"
            id={`settings-pane-${tab}`}
            aria-labelledby={`settings-tab-${tab}`}
          >
            {tab === "general" && (
              <>
                <Section id="settings-account" title="Account">
                  {account.kind === "signedOut" && (
                    <Row
                      label="Not signed in"
                      hint="Sign in to sync later. The app works fully without an account."
                    >
                      <Button size="sm" onClick={onSignIn}>
                        Sign in
                      </Button>
                    </Row>
                  )}
                  {account.kind === "signingIn" && (
                    <Row label="Signing in" hint="Check your browser and email…" status>
                      <Button size="sm" onClick={onCancel}>
                        Cancel
                      </Button>
                    </Row>
                  )}
                  {account.kind === "signedIn" && (
                    <>
                      <Row label={account.user.name} hint={account.user.email}>
                        <Button size="sm" onClick={onSignOut}>
                          Sign out
                        </Button>
                      </Row>
                      <SyncRows sync={sync} onSyncNow={onSyncNow} />
                    </>
                  )}
                  {account.kind === "signedInOffline" && (
                    <Row label="Signed in (offline)" hint="The app keeps working.">
                      <Button size="sm" onClick={onSignOut}>
                        Sign out
                      </Button>
                    </Row>
                  )}
                  {account.kind === "error" && (
                    <ErrorBanner
                      text="Couldn't sign in."
                      detail={account.message}
                      action={
                        <Button size="sm" onClick={onSignIn}>
                          Try again
                        </Button>
                      }
                    />
                  )}
                </Section>
                <Section id="settings-general" title="General">
                  <NotesFolder onApply={onApplyNotesFolder} />
                  <TimerDefaultsRow />
                  <Row label="Keyboard shortcuts" value="?">
                    <Button size="sm" onClick={onShowShortcuts}>
                      View
                    </Button>
                  </Row>
                </Section>
              </>
            )}
            {tab === "calendars" && (
              <CalendarsPane
                status={google.status}
                connecting={google.connecting}
                error={google.error}
                calendars={projects.calendars}
                onConnect={onGoogleConnect}
                onCancel={onGoogleCancel}
                onDisconnect={onGoogleDisconnect}
                onSetNewEventCalendar={onGoogleSetNewEventCalendar}
                onSetShown={(id, shown) => void projects.setShown(id, shown)}
              />
            )}
            {tab === "projects" && (
              <ProjectsPane
                projects={projects.list}
                error={projects.error}
                refreshing={projects.refreshing}
                google={google.status}
                onRefresh={() => void projects.refresh(true)}
                onConnect={onGoogleConnect}
                onSetArchived={(id, archived) => void projects.setArchived(id, archived)}
              />
            )}
            {tab === "tags" && (
              <TagsPane tags={tags.list} error={tags.error} onSave={tags.save} onDelete={tags.remove} />
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

/** Read-only: there is no command to save these values yet. */
function TimerDefaultsRow() {
  const [config, setConfig] = useState<Config | null>(null);
  useEffect(() => {
    loadConfig().then(setConfig, () => {});
  }, []);
  if (!config) return null;
  return (
    <Row
      label="Timer defaults"
      value={`Pomodoro · ${config.pomodoroCycles} × ${config.pomodoroFocusMinutes} + ${config.pomodoroBreakMinutes}`}
    />
  );
}

function SyncRows({
  sync,
  onSyncNow,
}: {
  sync: SyncStatus | null;
  onSyncNow: () => void;
}) {
  if (!sync) return null;
  const line = syncLine(sync, Date.now());
  const waiting = sync.files.pending;
  const syncButton = (
    <Button
      size="sm"
      icon={ArrowPathIcon}
      onClick={onSyncNow}
      disabled={sync.state === "syncing"}
    >
      {line.error ? "Try again" : "Sync now"}
    </Button>
  );
  const waitingText =
    waiting === 1
      ? "1 notes file is waiting and will sync next time."
      : `${waiting} notes files are waiting and will sync next time.`;
  return (
    <>
      {line.error ? (
        <Banner tone="error" variant="inline" actions={syncButton}>
          <span>
            {/* "offline" never reaches here: the server answered with this error. */}
            Couldn't sync: {line.text.replace(/\.$/, "")}.
            {waiting > 0 ? ` ${waitingText}` : ""}
          </span>
        </Banner>
      ) : (
        <Row label="Sync" value={line.text} status>
          {syncButton}
        </Row>
      )}
      {notesLines(sync)
        .filter(
          (l) =>
            line.error ||
            sync.files.pending === 0 ||
            !l.text.includes("waiting to sync"),
        )
        .map((l) => (
          <Row key={l.text} label="Notes" value={l.text} status />
        ))}
      <p className="settings__note">
        Notes are encrypted before they leave this computer.
      </p>
    </>
  );
}

/** The folder picker is native; the choice only takes effect after a restart, so it is confirmed first. */
function NotesFolder({
  onApply,
}: {
  onApply: (path: string | null) => Promise<ApplyResult>;
}) {
  const [info, setInfo] = useState<NotesDirInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  /** The folder waiting for confirmation; `null` inside means "use the default". */
  const [choice, setChoice] = useState<{
    path: string | null;
    label: string;
  } | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    notesDirInfo().then(setInfo, (e) => setError(String(e)));
  }, []);

  const pick = async () => {
    setError(null);
    try {
      const picked = await open({
        directory: true,
        multiple: false,
        defaultPath: info?.path,
      });
      if (typeof picked === "string")
        setChoice({ path: picked, label: picked });
    } catch (e) {
      setError(String(e));
    }
  };

  const apply = async () => {
    if (!choice) return;
    setBusy(true);
    const result = await onApply(choice.path);
    if (!result.ok) {
      setError(result.error);
      setChoice(null);
      setBusy(false);
    }
  };

  const errorBanner = error && (
    <ErrorBanner
      text="Couldn't change the notes folder."
      detail={error}
      action={
        <Button size="sm" onClick={() => void pick()} disabled={info?.fromEnv}>
          Try again
        </Button>
      }
    />
  );

  if (!info) {
    return error ? (
      <ErrorBanner
        text="Couldn't read the notes folder."
        detail={error}
        action={null}
      />
    ) : null;
  }
  return (
    <>
      <Row label="Notes folder" value={info.display}>
        {!choice && (
          <>
            <Button
              size="sm"
              icon={FolderOpenIcon}
              onClick={() => void pick()}
              disabled={info.fromEnv}
            >
              Change…
            </Button>
            {!info.isDefault && (
              <Button
                size="sm"
                onClick={() =>
                  setChoice({
                    path: null,
                    label: "the default folder (~/Catenga)",
                  })
                }
                disabled={info.fromEnv}
              >
                Use default
              </Button>
            )}
          </>
        )}
      </Row>
      {info.fromEnv && (
        <p className="settings__note">
          Set by CATENGA_NOTES_DIR (development).
        </p>
      )}
      {errorBanner}
      {choice && (
        <Row
          label="Switch folder?"
          hint={`Use ${choice.label}? Catenga Focus will restart to use this folder. Your notes aren't moved.`}
          status
        >
          <Button
            size="sm"
            variant="primary"
            onClick={() => void apply()}
            disabled={busy}
          >
            Use this folder
          </Button>
          <Button size="sm" onClick={() => setChoice(null)} disabled={busy}>
            Cancel
          </Button>
        </Row>
      )}
    </>
  );
}
