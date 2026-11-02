// Routed launch pages (docs/web-session-creation.md). The list owns the draft;
// preview and creation both consume its raw overrides.
import { useCallback, useEffect, useRef, useState } from "react";
import { ApiError } from "../api/http.ts";
import {
  createSession,
  fetchProjects,
  forgetProject,
  previewSession,
} from "../api/sessionForm.ts";
import type { ProjectEntry } from "../api/types.ts";
import { sessionHref } from "../api/base.ts";
import { navigate } from "./route.ts";
import { launchedFrom, reopenedLaunch } from "./launchRetry.ts";
import {
  type Answered,
  type Draft,
  canStart,
  draftFor,
  emptyDraft,
  locationFor,
  nextRequest,
  orphanIssues,
  rowKey,
  sessionRequest,
  startRefusal,
  withDraftLocation,
} from "./sessionForm.ts";
import { Settings } from "./NewSessionSettings.tsx";
import { AppHeader } from "./AppHeader.tsx";
import { SelectionControl } from "./SelectionControl.tsx";
import { homeRelative } from "./identity.ts";
import { ModelPicker, type PickerBrowseState } from "./ModelPicker.tsx";
import type { Role } from "./pickerRows.ts";
import type { LaunchPage } from "./newSessionNavigation.ts";
import { LocationRow } from "./LocationRow.tsx";
import { DirectoryDisclosure } from "./DirectoryDisclosure.tsx";
import "./NewSessionForm.css";

export function NewSessionForm({
  draft,
  requested,
  onDraft,
  onClose,
  page,
  onNavigate,
  onBack,
}: {
  /** The draft so far, or `null` when nothing has been chosen yet. Held by the
   * list page, so it outlives this component and survives a cancel. */
  draft: Draft | null;
  /** The directory a workspace `+` asked for, or `null` for the top bar's
   * entry. Applied once the remembered locations are read, since which
   * configuration a directory was last launched with is what that answer
   * carries. */
  requested: string | null;
  onDraft: (draft: Draft) => void;
  onClose: () => void;
  page: LaunchPage;
  onNavigate: (page: LaunchPage) => void;
  onBack: (page: LaunchPage) => void;
}) {
  const [other, setOther] = useState(false);
  const [editing, setEditing] = useState(false);
  const [projects, setProjects] = useState<ProjectEntry[] | null>(null);
  const [typed, setTyped] = useState("");
  const [answered, setAnswered] = useState<Answered | null>(null);
  const [note, setNote] = useState<string | null>(null);
  // Why the last start from this draft failed, when the form reopened for a retry
  // (ui/launchRetry.ts). Apart from `note`, which a resolved preview clears.
  const [retrying, setRetrying] = useState(
    () => reopenedLaunch()?.reason ?? null,
  );
  const [creating, setCreating] = useState(false);
  const [elapsed, setElapsed] = useState(0);
  // The counter that pairs a reply with the draft it answers. State, because
  // the render judges the answer on screen against it; a ref is the authority
  // it is read from, because two committed edits in one render would otherwise
  // both name the same request and answer for one another.
  const counter = useRef(0);
  const [request, setRequest] = useState(0);
  const opened = useRef(false);
  const started = useRef(0);
  const [browsing, setBrowsing] = useState<
    Partial<Record<Role, PickerBrowseState>>
  >({});
  const rememberBrowse = useCallback((role: Role, state: PickerBrowseState) => {
    setBrowsing((current) => ({ ...current, [role]: state }));
  }, []);

  const preview = useCallback((next: Draft) => {
    const id = nextRequest(counter.current);
    counter.current = id;
    setRequest(id);
    previewSession(sessionRequest(next))
      .then((answer) => {
        // A reply for a draft that has moved on since this request went out
        // describes a draft nobody is looking at: it is dropped rather than
        // shown, and the current draft's own reply is the one that stands.
        if (id !== counter.current) return;
        // A draft that resolved clears the last failure's reason: it described
        // a draft that is no longer on screen.
        setNote(null);
        setAnswered({ request: id, answer });
      })
      .catch((failure: unknown) => {
        if (id !== counter.current) return;
        // A preview that could not be sent is the host declining to describe
        // the draft, not an issue with one field: the answer stays unanswered,
        // so Start stays refused, and the reason is shown with Start's own.
        setAnswered(null);
        setNote(`Could not resolve this draft: ${String(failure)}`);
      });
  }, []);

  useEffect(() => {
    fetchProjects()
      .then((answer) => setProjects(answer.projects))
      .catch(() => setProjects([]));
  }, []);

  // A workspace shortcut supplies its location. The general entry asks the
  // operator to choose, while a cancelled draft keeps its settings.
  useEffect(() => {
    if (projects === null || opened.current) return;
    opened.current = true;
    if (draft !== null && requested === null) return;
    const cwd = requested ?? undefined;
    if (cwd !== undefined) {
      onDraft({ ...(draft ?? emptyDraft(cwd)), ...locationFor(projects, cwd) });
    }
  }, [projects, draft, requested, onDraft]);

  // Preview on the location, and again whenever the draft moves: recomputed on
  // committed edits, never per keystroke (docs/tui.md).
  useEffect(() => {
    if (draft !== null) preview(draft);
  }, [draft, preview]);

  // A directory that cannot start a worktree takes the option away (the settings hide
  // it), and a draft still holding it on would send what creation refuses. Cleared
  // only from the answer to the current draft, so a stale reply never edits it.
  useEffect(() => {
    if (
      draft !== null &&
      draft.worktree &&
      answered !== null &&
      answered.request === request &&
      !answered.answer.worktree
    )
      onDraft({ ...draft, worktree: false });
  }, [draft, answered, request, onDraft]);

  // Measure the wait while creation is in flight; its progress is not reported
  // by the host (docs/web-session-creation.md).
  useEffect(() => {
    if (!creating) return;
    started.current = Date.now();
    const tick = window.setInterval(
      () => setElapsed(Math.floor((Date.now() - started.current) / 1000)),
      1000,
    );
    return () => window.clearInterval(tick);
  }, [creating]);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !event.defaultPrevented) {
        if (editing) {
          event.preventDefault();
          setEditing(false);
        } else if (other) {
          event.preventDefault();
          setOther(false);
        } else if (
          draft !== null &&
          page !== "location" &&
          page !== "settings"
        ) {
          onBack("settings");
        } else onClose();
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose, onBack, page, editing, other, draft]);

  function commitTyped() {
    const path = typed.trim();
    // Creation takes an absolute path, or one that starts with `~` for the
    // host's home, and refuses anything else; the form does not widen what it
    // accepts, so a relative path is not committed.
    if (path === "" || !(path.startsWith("/") || /^~(\/|$)/.test(path))) {
      setNote(
        "Enter a full directory path on the host, absolute or starting with ~.",
      );
      return;
    }
    setTyped("");
    setOther(false);
    setEditing(false);
    setNote(null);
    onDraft({ ...emptyDraft(path), ...rest() });
    onNavigate("settings");
  }

  /** The settings half of the draft, for a location change: picking where to
   * run does not undo what the operator chose about how. */
  const rest = () => ({
    main: draft?.main ?? null,
    spotter: draft?.spotter ?? null,
    triage: draft?.triage ?? { mode: "inherit" as const },
    effort: draft?.effort ?? null,
    temperature: draft?.temperature ?? null,
    worktree: draft?.worktree ?? false,
  });

  async function forget(entry: ProjectEntry) {
    if (projects === null) return;
    try {
      await forgetProject(entry);
    } catch (failure) {
      // A 404 means the entry is already gone, which is the state asked for, so
      // the row goes. Anything else did not forget it, and dropping the row
      // would claim a state the host is not in.
      if (!(failure instanceof ApiError) || failure.code !== "not_found") {
        setNote(`Could not forget ${entry.cwd}: ${String(failure)}`);
        return;
      }
    }
    setProjects((current) =>
      current === null
        ? null
        : current.filter((row) => rowKey(row) !== rowKey(entry)),
    );
  }

  async function start() {
    if (draft === null || creating) return;
    setElapsed(0);
    setCreating(true);
    setNote(null);
    setRetrying(null);
    try {
      const created = await createSession(sessionRequest(draft));
      // Kept by the id, so a start that fails can reopen the form on it.
      launchedFrom(created.id, draft);
      // A navigation, like opening a card: the base changes, so the page loads
      // again against the new session's control endpoint (api/base.ts). The
      // session opens starting, and its preparation shows there.
      navigate(sessionHref(created.id));
    } catch (failure) {
      // Nothing was minted, so every edit stays and only the reason is added.
      setCreating(false);
      setNote(
        failure instanceof ApiError
          ? failure.message
          : `Could not start a session: ${String(failure)}`,
      );
    }
  }

  const hint = startRefusal(draft, answered, request);
  const ready = canStart(draft, answered, request, creating);
  const rows = withDraftLocation(projects ?? [], draft);
  const currentLocation = rows.find((entry) => entry.config_key === "");
  const issues = answered?.answer.issues ?? [];

  const surface = draft === null ? "location" : page;
  const picking =
    surface !== "location" && surface !== "settings" ? surface : null;
  const title =
    surface === "location"
      ? "Where will you work?"
      : surface === "settings"
        ? "Session settings"
        : `Choose ${picking} model`;

  function chooseLocation(entry: ProjectEntry) {
    setOther(false);
    setEditing(false);
    onDraft({ ...draftFor(entry), ...rest() });
    onNavigate("settings");
  }

  function pick(role: Role, selection: { connection: string; model: string }) {
    if (draft === null) return;
    onDraft({
      ...draft,
      ...(role === "triage"
        ? { triage: { mode: "separate" as const, ...selection } }
        : { [role]: selection }),
    });
  }

  const rememberCurrentBrowse = useCallback(
    (state: PickerBrowseState) => {
      if (picking !== null) rememberBrowse(picking, state);
    },
    [picking, rememberBrowse],
  );

  return (
    <div className="new-session-page">
      <AppHeader
        className="new-session-bar"
        innerClassName="new-session-bar-inner"
        onHome={onClose}
      >
        <button onClick={onClose}>Cancel</button>
      </AppHeader>
      <main className="new-session" aria-label="New session">
        <header className="new-session-head">
          {picking !== null && (
            <button
              className="quiet new-session-back"
              onClick={() => onBack("settings")}
            >
              ← Back to settings
            </button>
          )}
          <h1>{title}</h1>
        </header>
        <div className="new-session-body">
          {surface === "location" && (
            <section className="new-session-where" aria-label="Location">
              <p className="new-session-description">
                Choose a recent location on the host, or use another directory.
              </p>
              {projects === null ? (
                <p className="dim">Reading recent locations…</p>
              ) : (
                <>
                  {currentLocation !== undefined && (
                    <section
                      className="new-session-current"
                      aria-label="Current location"
                    >
                      <h2 className="location-section-heading">
                        Current location
                      </h2>
                      <LocationRow
                        entry={currentLocation}
                        projects={rows}
                        selected
                        editing={false}
                        onSelect={() => chooseLocation(currentLocation)}
                      />
                    </section>
                  )}
                  <section aria-label="Recent locations">
                    {(projects.length > 0 || editing) && (
                      <div className="location-list-heading">
                        <h2 className="location-section-heading">
                          Recent locations
                        </h2>
                        <button
                          type="button"
                          className="location-edit"
                          aria-label={
                            editing
                              ? "Done editing recent locations"
                              : "Edit recent locations"
                          }
                          onClick={() => setEditing((current) => !current)}
                        >
                          {editing ? "Done" : "Edit"}
                        </button>
                      </div>
                    )}
                    {projects.length === 0 && (
                      <p className="new-session-empty">
                        No recent locations yet.
                      </p>
                    )}
                    {projects.map((entry) => (
                      <LocationRow
                        key={rowKey(entry)}
                        entry={entry}
                        projects={rows}
                        selected={
                          draft?.cwd === entry.cwd &&
                          draft?.config === entry.config
                        }
                        editing={editing}
                        onSelect={() => chooseLocation(entry)}
                        onForget={() => void forget(entry)}
                      />
                    ))}
                  </section>
                </>
              )}
              <DirectoryDisclosure
                open={other}
                value={typed}
                onToggle={() => {
                  setEditing(false);
                  setOther((current) => !current);
                }}
                onChange={setTyped}
                onCommit={commitTyped}
              />
            </section>
          )}
          {surface === "settings" && draft !== null && (
            <>
              <div className="new-session-location-summary">
                <div className="setting-heading">
                  <span className="setting-label">Location</span>
                </div>
                <SelectionControl
                  label={`Choose location: ${draft.cwd}`}
                  onClick={() => onNavigate("location")}
                >
                  {homeRelative(draft.cwd)}
                  {draft.config !== null && <code>{draft.config}</code>}
                </SelectionControl>
              </div>
              <Settings
                draft={draft}
                preview={
                  answered !== null && answered.request === request
                    ? answered.answer
                    : null
                }
                onDraft={onDraft}
                onPick={onNavigate}
              />
            </>
          )}
          {picking !== null && draft !== null && (
            <ModelPicker
              key={picking}
              role={picking}
              cwd={draft.cwd}
              config={draft.config}
              initialBrowse={browsing[picking]}
              onBrowse={rememberCurrentBrowse}
              onPick={(selection) => pick(picking, selection)}
              onClose={() => onBack("settings")}
            />
          )}
        </div>
        {picking === null && (
          <footer className="new-session-foot">
            {surface === "settings" &&
              orphanIssues(issues).map((issue) => (
                <p
                  className="new-session-issue"
                  key={`${issue.field}:${issue.message}`}
                >
                  {issue.message}
                </p>
              ))}
            {note !== null && (
              <p className="new-session-issue" role="alert">
                {note}
              </p>
            )}
            {retrying !== null && (
              <p className="new-session-issue" role="alert">
                The last start did not finish: {retrying}
              </p>
            )}
            {surface === "settings" && (
              <>
                {hint !== null && !creating && <p className="dim">{hint}</p>}
                <button
                  className="primary"
                  disabled={!ready}
                  onClick={() => void start()}
                >
                  {creating ? startingLabel(elapsed) : "Start session"}
                </button>
              </>
            )}
          </footer>
        )}
      </main>
    </div>
  );
}

/** Elapsed time makes a slow creation visible without guessing which step
 * of the host launch is taking time. */
function startingLabel(elapsed: number): string {
  return elapsed >= 5 ? `Starting… ${elapsed}s` : "Starting…";
}
