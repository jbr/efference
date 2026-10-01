// The new-session form (docs/web-client.md): where a session would run, and
// the button that starts it. A centered modal over the overview on a wide
// window, a full-screen sheet with a close button on a phone.
//
// Step 1 of the form (backlog/new-session-form.md) is the Where section and
// creation with no overrides; the Settings rows and the model picker land on
// top of this, which is why the draft, its request and the preview rules live
// in `sessionForm.ts` rather than here.
//
// Two rules are worth stating where they are implemented. The draft survives
// while the page is open — cancelling and reopening returns to it, a reload
// discards it — so it is held by the list page and passed in, not owned by
// this component. And a refused creation keeps the form open with every edit,
// because a refusal mints nothing and so leaves nothing to reconcile.

import { useCallback, useEffect, useRef, useState } from "react";
import { ApiError } from "../api/http.ts";
import {
  createSession,
  fetchProjects,
  forgetProject,
  type ProjectEntry,
  previewSession,
} from "../api/sessionForm.ts";
import { sessionHref } from "../api/base.ts";
import {
  type Answered,
  type Draft,
  canStart,
  draftFor,
  emptyDraft,
  locationFor,
  mostRecent,
  nextRequest,
  orphanIssues,
  rowKey,
  sessionRequest,
  showsConfig,
  startRefusal,
  withTypedLocation,
} from "./sessionForm.ts";
import { Settings } from "./NewSessionSettings.tsx";
import { homeRelative } from "./identity.ts";
import { splitPath } from "./overview.ts";
import { useNarrow } from "./useNarrow.ts";
import "./NewSessionForm.css";

export function NewSessionForm({
  draft,
  requested,
  onDraft,
  onClose,
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
}) {
  const narrow = useNarrow();
  const [projects, setProjects] = useState<ProjectEntry[] | null>(null);
  const [typed, setTyped] = useState("");
  const [answered, setAnswered] = useState<Answered | null>(null);
  const [note, setNote] = useState<string | null>(null);
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

  // Opening resolves a location, so the form shows a real one rather than a
  // blank: a `+` opens on its own workspace's directory, the top bar on the
  // most recently used one. Applied once, and only when nothing has been chosen
  // — a draft that survived a cancel is kept. With nothing remembered there is
  // no location, so Start stays refused and the Other directory input is the
  // way out of the empty state.
  useEffect(() => {
    if (projects === null || opened.current) return;
    opened.current = true;
    if (draft !== null || projects.length === 0) return;
    const cwd = requested ?? mostRecent(projects)?.cwd;
    if (cwd !== undefined) {
      onDraft(
        emptyDraft(cwd, locationFor(projects, cwd)?.config ?? null) as Draft,
      );
    }
  }, [projects, draft, requested, onDraft]);

  // Preview on the location, and again whenever the draft moves: recomputed on
  // committed edits, never per keystroke (docs/tui.md).
  useEffect(() => {
    if (draft !== null) preview(draft);
  }, [draft, preview]);

  // While a creation is in flight the elapsed time is shown past five seconds,
  // because establishing the environment runs the host's hook, which has a 30s
  // deadline of its own (docs/control-api.md).
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
      if (event.key === "Escape") onClose();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose]);

  function commitTyped() {
    const path = typed.trim();
    // Creation takes an absolute path or refuses; the form does not widen what
    // it accepts, so a relative path is not committed.
    if (path === "" || !path.startsWith("/")) return;
    setTyped("");
    setNote(null);
    onDraft({ ...emptyDraft(path), ...rest() });
  }

  /** The settings half of the draft, for a location change: picking where to
   * run does not undo what the operator chose about how. */
  const rest = () => ({
    main: draft?.main ?? null,
    spotter: draft?.spotter ?? null,
    triage: draft?.triage ?? { mode: "inherit" as const },
    effort: draft?.effort ?? null,
    temperature: draft?.temperature ?? null,
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
    setProjects(projects.filter((row) => row !== entry));
  }

  async function start() {
    if (draft === null || creating) return;
    setElapsed(0);
    setCreating(true);
    setNote(null);
    try {
      const created = await createSession(sessionRequest(draft));
      // A navigation, like opening a card: the base changes, so the page loads
      // again against the new session's control endpoint (api/base.ts).
      window.location.assign(sessionHref(created.id));
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
  const rows = withTypedLocation(projects ?? [], draft);
  const issues = answered?.answer.issues ?? [];

  return (
    <div className="new-session-scrim" onClick={onClose}>
      <div
        className="new-session"
        role="dialog"
        aria-modal="true"
        aria-label="New session"
        onClick={(event) => event.stopPropagation()}
      >
        <header className="new-session-head">
          <h2>New session</h2>
          {narrow && (
            <button className="new-session-close" onClick={onClose}>
              Close
            </button>
          )}
        </header>

        <div className="new-session-body">
          <section className="new-session-where">
            <h3>Where</h3>
            {projects === null ? (
              <p className="dim">Reading remembered locations…</p>
            ) : (
              <>
                {projects.length === 0 && (
                  <p className="new-session-empty">
                    No remembered locations yet. Sessions you start, here or in
                    a terminal, appear here.
                  </p>
                )}
                {rows.map((entry) => (
                  <LocationRow
                    key={rowKey(entry)}
                    entry={entry}
                    projects={projects}
                    selected={
                      draft?.cwd === entry.cwd && draft?.config === entry.config
                    }
                    onSelect={() => onDraft({ ...draftFor(entry), ...rest() })}
                    onForget={() => void forget(entry)}
                  />
                ))}
              </>
            )}

            <label className="new-session-other">
              <span>Other directory…</span>
              <input
                type="text"
                value={typed}
                placeholder="/Users/you/code/project"
                spellCheck={false}
                autoCapitalize="off"
                autoCorrect="off"
                onChange={(event) => setTyped(event.target.value)}
                onBlur={commitTyped}
                onKeyDown={(event) => {
                  if (event.key === "Enter") commitTyped();
                }}
              />
            </label>

            <p className="new-session-note dim">
              The session's environment comes from your <code>env_hook</code>,
              or the host's own environment if none is configured.
            </p>
          </section>

          {draft !== null && (
            <Settings
              draft={draft}
              // Only an answer about the draft on screen describes it; while a
              // newer one is in flight the older is not shown, since its sources
              // are the ones the operator just changed.
              preview={
                answered !== null && answered.request === request
                  ? answered.answer
                  : null
              }
              onDraft={onDraft}
            />
          )}
        </div>

        <footer className="new-session-foot">
          {orphanIssues(issues).map((issue) => (
            <p
              className="new-session-issue"
              key={`${issue.field}:${issue.message}`}
            >
              {issue.message}
            </p>
          ))}
          {note !== null && <p className="new-session-issue">{note}</p>}
          {hint !== null && !creating && <p className="dim">{hint}</p>}
          <button
            className="primary"
            disabled={!ready}
            onClick={() => void start()}
          >
            {creating ? startingLabel(elapsed) : "Start session"}
          </button>
        </footer>
      </div>
    </div>
  );
}

/** While creation is in flight the button says so, and past five seconds it
 * also says what is taking the time: establishing the environment runs the
 * host's hook, which has a 30s deadline of its own. */
function startingLabel(elapsed: number): string {
  return elapsed >= 5
    ? `Starting… ${elapsed}s · running your environment hook`
    : "Starting…";
}

/** One remembered location: the directory's own name in the serif, its parent
 * small and faint the way a workspace heading shows it, and the configuration
 * it would launch with when saying so is what tells two rows of one directory
 * apart. */
function LocationRow({
  entry,
  projects,
  selected,
  onSelect,
  onForget,
}: {
  entry: ProjectEntry;
  projects: ProjectEntry[];
  selected: boolean;
  onSelect: () => void;
  onForget: () => void;
}) {
  const [parent, name] = splitPath(homeRelative(entry.cwd));
  return (
    <div className={selected ? "location selected" : "location"}>
      <button className="location-pick" onClick={onSelect}>
        {/* The cursor colours the chosen row rather than filling it, the
            terminal form's own idiom (docs/tui.md). */}
        <span className="location-line">
          <span className="location-cursor">❯</span>
          <span className="location-name">{entry.label || name}</span>
        </span>
        {parent !== "" && <span className="location-parent">{parent}</span>}
        {showsConfig(projects, entry) && (
          <code className="location-config">
            {entry.config ?? "project config"}
          </code>
        )}
      </button>
      {/* Nothing to forget about a path the index does not carry — it is
          remembered by the first session launched there. No confirmation
          either: forgetting deletes nothing on disk. */}
      {entry.config_key !== "" && (
        <button
          className="location-forget"
          aria-label={`Forget ${entry.cwd}`}
          title="Forget this location"
          onClick={onForget}
        >
          ×
        </button>
      )}
    </div>
  );
}
