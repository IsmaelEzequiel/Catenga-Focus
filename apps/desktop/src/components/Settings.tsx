import {
  useEffect,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactNode,
} from "react";
import { open } from "@tauri-apps/plugin-dialog";
import {
  ArrowPathIcon,
  CalendarIcon,
  FolderOpenIcon,
} from "@heroicons/react/20/solid";
import {
  loadConfig,
  notesDirInfo,
  type Config,
  GoogleStatus,
  type NotesDirInfo,
  type SyncStatus,
} from "../api";
import type { ApplyResult } from "../lib/notesFolder";
import type { AccountState } from "../hooks/useAccount";
import { canReconnect, reconnectHint } from "../lib/projects";
import { notesLines, syncLine } from "../lib/sync";
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
          {google.status && (
            <GoogleRows
              google={google}
              onConnect={onGoogleConnect}
              onCancel={onGoogleCancel}
              onDisconnect={onGoogleDisconnect}
            />
          )}
          <NotesFolder onApply={onApplyNotesFolder} />
          <TimerDefaultsRow />
          <Row label="Keyboard shortcuts" value="?">
            <Button size="sm" onClick={onShowShortcuts}>
              View
            </Button>
          </Row>
        </Section>
      </div>
    </div>
  );
}

function GoogleRows({
  google,
  onConnect,
  onCancel,
  onDisconnect,
}: {
  google: Props["google"];
  onConnect: () => void;
  onCancel: () => void;
  onDisconnect: () => void;
}) {
  const status = google.status;
  if (!status) return null;
  const retry = (
    <Button size="sm" onClick={onConnect}>
      Try again
    </Button>
  );
  if (!status.configured)
    return <Row label="Google Calendar" value="Not available in this build" />;
  if (google.connecting) {
    return (
      <Row
        label="Google Calendar"
        value="Waiting for Google in your browser…"
        status
      >
        <Button size="sm" onClick={onCancel}>
          Cancel
        </Button>
      </Row>
    );
  }
  return (
    <>
      <Row
        label="Google Calendar"
        value={
          status.connected
            ? `Connected${status.email ? ` · ${status.email}` : ""}`
            : "Not connected"
        }
      >
        {status.connected ? (
          <>
            {canReconnect(status) && (
              <Button size="sm" icon={CalendarIcon} onClick={onConnect}>
                Reconnect
              </Button>
            )}
            <Button size="sm" onClick={onDisconnect}>
              Disconnect
            </Button>
          </>
        ) : (
          <Button size="sm" icon={CalendarIcon} onClick={onConnect}>
            Connect
          </Button>
        )}
      </Row>
      {status.connected && canReconnect(status) && (
        <Banner tone="info" variant="inline">
          {reconnectHint(status)}
        </Banner>
      )}
      {google.error && (
        <ErrorBanner
          text={
            status.connected
              ? "Google Calendar had a problem."
              : "Couldn't connect to Google Calendar."
          }
          detail={google.error}
          action={retry}
        />
      )}
    </>
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

function Section({
  id,
  title,
  children,
}: {
  id: string;
  title: string;
  children: ReactNode;
}) {
  return (
    <section className="settings__section" aria-labelledby={id}>
      <h3 id={id} className="settings__heading">
        {title}
      </h3>
      <div className="settings__rows">{children}</div>
    </section>
  );
}

/** label on the left; value and small actions on the right. */
function Row({
  label,
  hint,
  value,
  status,
  children,
}: {
  label: string;
  hint?: string;
  value?: string;
  status?: boolean;
  children?: ReactNode;
}) {
  return (
    <div className="settings__row">
      <div className="settings__main">
        <span className="settings__label">{label}</span>
        {hint && (
          <span className="settings__hint" role={status ? "status" : undefined}>
            {hint}
          </span>
        )}
      </div>
      {(value || children) && (
        <div className="settings__side">
          {value && (
            <span
              className="settings__value"
              role={status ? "status" : undefined}
            >
              {value}
            </span>
          )}
          {children && <div className="settings__actions">{children}</div>}
        </div>
      )}
    </div>
  );
}

/** A friendly sentence with the raw error kept as a tooltip, plus a retry action. */
function ErrorBanner({
  text,
  detail,
  action,
}: {
  text: string;
  detail: string;
  action: ReactNode;
}) {
  return (
    <Banner tone="error" variant="inline" actions={action}>
      <span title={detail}>{text}</span>
    </Banner>
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
          <span title={line.text}>
            Couldn't reach the sync server.
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
