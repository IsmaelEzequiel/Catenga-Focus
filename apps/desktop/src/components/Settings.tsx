import { useEffect, useRef, type KeyboardEvent } from "react";
import { CalendarIcon } from "@heroicons/react/20/solid";
import type { GoogleStatus } from "../api";
import type { AccountState } from "../hooks/useAccount";
import { canReconnect, reconnectHint } from "../lib/projects";
import "../settings.css";

interface Props {
  account: AccountState;
  onSignIn: () => void;
  onCancel: () => void;
  onSignOut: () => void;
  google: { status: GoogleStatus | null; connecting: boolean; error: string | null };
  onGoogleConnect: () => void;
  onGoogleCancel: () => void;
  onGoogleDisconnect: () => void;
  onClose: () => void;
}

export function Settings({
  account,
  onSignIn,
  onCancel,
  onSignOut,
  google,
  onGoogleConnect,
  onGoogleCancel,
  onGoogleDisconnect,
  onClose,
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
      </div>
    </div>
  );
}
