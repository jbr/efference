import { settingsHref } from "./settingsHref.ts";
import { useEffect, useRef } from "react";
import { Interface } from "./Panel.tsx";
import { AppHeader } from "./AppHeader.tsx";
import { navigate, useSearch } from "./route.ts";
import "./AppSettings.css";

export function AppSettingsLink() {
  return (
    <a
      className="app-settings-link"
      href={settingsHref(
        typeof location === "undefined" ? "/" : location.pathname,
        typeof location === "undefined" ? "" : location.search,
        true,
      )}
      title="App settings"
      aria-label="App settings"
    >
      <svg
        width="18"
        height="18"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
        aria-hidden="true"
      >
        <path d="M4 7h16M4 17h16" />
        <circle cx="9" cy="7" r="3" fill="var(--bg-surface)" />
        <circle cx="15" cy="17" r="3" fill="var(--bg-surface)" />
      </svg>
    </a>
  );
}

export function AppSettings() {
  const open = new URLSearchParams(useSearch()).has("settings");
  const dialog = useRef<HTMLDialogElement>(null);
  const close = () =>
    navigate(
      settingsHref(location.pathname, location.search, false),
      "replace",
    );
  useEffect(() => {
    if (!open) return;
    const element = dialog.current;
    element?.showModal();
    return () => element?.close();
  }, [open]);
  if (!open) return null;
  return (
    <dialog
      ref={dialog}
      className="app-settings-page overview"
      aria-labelledby="app-settings-title"
      onCancel={(event) => {
        event.preventDefault();
        close();
      }}
    >
      <AppHeader>
        <span className="spacer" />
        <button type="button" onClick={close}>
          Done
        </button>
      </AppHeader>
      <main className="app-settings-content">
        <h1 id="app-settings-title">App settings</h1>
        <p>Preferences for this browser, shared across sessions.</p>
        <Interface />
      </main>
    </dialog>
  );
}
