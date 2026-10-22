import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { ArrowPathIcon, CalendarIcon, FolderOpenIcon } from "@heroicons/react/20/solid";
import { notesDirInfo, type GoogleStatus, type NotesDirInfo, type SyncStatus } from "../api";
import type { ApplyResult } from "../lib/notesFolder";
import type { AccountState } from "../hooks/useAccount";
import { canReconnect, reconnectHint } from "../lib/projects";
import { notesLines, syncLine } from "../lib/sync";
import "../settings.css";

interface Props {
  account: AccountState;
  onSignIn: () => void;
  onCancel: () => void;
  onSignOut: () => void;
  sync: SyncStatus | null;
  onSyncNow: () => void;
  google: { status: GoogleStatus | null; connecting: boolean; error: string | null };
  onGoogleConnect: () => void;
  onGoogleCancel: () => void;
  onGoogleDisconnect: () => void;
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
  onApplyNotesFolder,
  onClose,
  onShowShortcuts,
}: Props) {
  const panel = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const previous = document.activeElement;
    panel.current?.focus();
    return () => {
      if (previous instanceof HTMLElement) previous.focus();
    };
  }, []);

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
        <h2 className="settings__title">Settings</h2>
        <section className="settings__section" aria-labelledby="settings-account">
          <h3 id="settings-account" className="settings__heading">
            Account
          </h3>
          {account.kind === "signedOut" && (
            <>
              <p className="settings__text">Sign in to sync later. The app works fully without an account.</p>
              <button className="app__btn" onClick={onSignIn}>
                Sign in
              </button>
            </>
          )}
          {account.kind === "signingIn" && (
            <>
              <p className="settings__text" role="status">
                Check your browser and email…
              </p>
              <button className="app__btn" onClick={onCancel}>
                Cancel
              </button>
            </>
          )}
          {account.kind === "signedIn" && (
            <>
              <p className="settings__text">
                Signed in as <b>{account.user.name}</b> · {account.user.email}
              </p>
              <SyncLine sync={sync} onSyncNow={onSyncNow} />
              <button className="app__btn" onClick={onSignOut}>
                Sign out
              </button>
            </>
          )}
          {account.kind === "signedInOffline" && (
            <>
              <p className="settings__text">Signed in (offline). The app keeps working.</p>
              <button className="app__btn" onClick={onSignOut}>
                Sign out
              </button>
            </>
          )}
          {account.kind === "error" && (
            <>
              <p className="settings__text settings__text--error" role="alert">
                {account.message}
              </p>
              <button className="app__btn" onClick={onSignIn}>
                Sign in again
              </button>
            </>
          )}
        </section>
        {google.status && (
          <section className="settings__section settings__section--spaced" aria-labelledby="settings-google">
            <h3 id="settings-google" className="settings__heading">
              Google Calendar
            </h3>
            {!google.status.configured ? (
              <p className="settings__text">Google Calendar isn't configured in this build.</p>
            ) : google.connecting ? (
              <>
                <p className="settings__text" role="status">
                  Waiting for Google in your browser…
                </p>
                <button className="app__btn" onClick={onGoogleCancel}>
                  Cancel
                </button>
              </>
            ) : google.status.connected ? (
              <>
                <p className="settings__text">
                  Connected{google.status.email ? <> · {google.status.email}</> : null}
                </p>
                {canReconnect(google.status) && (
                  <p className="settings__text settings__text--error" role="status">
                    {reconnectHint(google.status)}
                  </p>
                )}
                {google.error && (
                  <p className="settings__text settings__text--error" role="alert">
                    {google.error}
                  </p>
                )}
                {canReconnect(google.status) && (
                  <button className="app__btn" onClick={onGoogleConnect}>
                    <CalendarIcon className="app__icon" aria-hidden="true" />
                    Reconnect
                  </button>
                )}
                <button className="app__btn" onClick={onGoogleDisconnect}>
                  Disconnect
                </button>
              </>
            ) : (
              <>
                {google.error ? (
                  <p className="settings__text settings__text--error" role="alert">
                    {google.error}
                  </p>
                ) : (
                  <p className="settings__text">Show your Google Calendar events on the timeline.</p>
                )}
                <button className="app__btn" onClick={onGoogleConnect}>
                  <CalendarIcon className="app__icon" aria-hidden="true" />
                  {google.error ? "Connect again" : "Connect Google Calendar"}
                </button>
              </>
            )}
          </section>
        )}
        <NotesFolder onApply={onApplyNotesFolder} />
        <section className="settings__section settings__section--spaced" aria-labelledby="settings-keys">
          <h3 id="settings-keys" className="settings__heading">
            Keyboard
          </h3>
          <button className="app__btn" onClick={onShowShortcuts}>
            Keyboard shortcuts
          </button>
        </section>
      </div>
    </div>
  );
}

function SyncLine({ sync, onSyncNow }: { sync: SyncStatus | null; onSyncNow: () => void }) {
  if (!sync) return null;
  const line = syncLine(sync, Date.now());
  return (
    <>
      <p
        className={line.error ? "settings__text settings__text--error" : "settings__text"}
        role={line.error ? "alert" : "status"}
      >
        {line.text}
      </p>
      {notesLines(sync).map((l) => (
        <p key={l.text} className="settings__text" role="status">
          {l.text}
        </p>
      ))}
      <p className="settings__text">
        Your local goals, tags, projects and notes are added to this account. Notes are encrypted before they leave this computer.
      </p>
      <button className="app__btn" onClick={onSyncNow} disabled={sync.state === "syncing"}>
        <ArrowPathIcon className="app__icon" aria-hidden="true" />
        Sync now
      </button>
    </>
  );
}

/** The folder picker is native; the choice only takes effect after a restart, so it is confirmed first. */
function NotesFolder({ onApply }: { onApply: (path: string | null) => Promise<ApplyResult> }) {
  const [info, setInfo] = useState<NotesDirInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  /** The folder waiting for confirmation; `null` inside means "use the default". */
  const [choice, setChoice] = useState<{ path: string | null; label: string } | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    notesDirInfo().then(setInfo, (e) => setError(String(e)));
  }, []);

  const pick = async () => {
    setError(null);
    try {
      const picked = await open({ directory: true, multiple: false, defaultPath: info?.path });
      if (typeof picked === "string") setChoice({ path: picked, label: picked });
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

  if (!info) {
    return error ? (
      <section className="settings__section settings__section--spaced">
        <p className="settings__text settings__text--error" role="alert">
          {error}
        </p>
      </section>
    ) : null;
  }
  return (
    <section className="settings__section settings__section--spaced" aria-labelledby="settings-folder">
      <h3 id="settings-folder" className="settings__heading">
        Notes folder
      </h3>
      <p className="settings__text settings__path">{info.display}</p>
      {info.fromEnv && <p className="settings__text">Set by CATENGA_NOTES_DIR (development).</p>}
      {error && (
        <p className="settings__text settings__text--error" role="alert">
          {error}
        </p>
      )}
      {choice ? (
        <>
          <p className="settings__text" role="status">
            Use <b>{choice.label}</b>? Catenga Focus will restart to use this folder. Your notes aren't moved.
          </p>
          <button className="app__btn" onClick={() => void apply()} disabled={busy}>
            Use this folder
          </button>
          <button className="app__btn" onClick={() => setChoice(null)} disabled={busy}>
            Cancel
          </button>
        </>
      ) : (
        <>
          <button className="app__btn" onClick={() => void pick()} disabled={info.fromEnv}>
            <FolderOpenIcon className="app__icon" aria-hidden="true" />
            Change…
          </button>
          {!info.isDefault && (
            <button className="app__btn" onClick={() => setChoice({ path: null, label: "the default folder (~/Catenga)" })} disabled={info.fromEnv}>
              Use default
            </button>
          )}
        </>
      )}
    </section>
  );
}
