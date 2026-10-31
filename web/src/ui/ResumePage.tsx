import type { ArchivedSession } from "../api/types.ts";
import { useEffect, useMemo, useRef, useState } from "react";
import { AppHeader } from "./AppHeader.tsx";
import { LocationRow } from "./LocationRow.tsx";
import { RelativeTime } from "./RelativeTime.tsx";
import { fetchArchivedSessions, resumeSession } from "../api/http.ts";
import { fetchProjects, type ProjectEntry } from "../api/sessionForm.ts";
import { sessionHref } from "../api/base.ts";
import { navigate } from "./route.ts";
import { filterArchived } from "./resume.ts";
import type { ResumeRoute } from "./resumeNavigation.ts";
import { homeRelative } from "./identity.ts";
import { splitPath } from "./overview.ts";
import "./ResumePage.css";

export function ResumePage({
  route,
  onNavigate,
  onClose,
}: {
  route: ResumeRoute;
  onNavigate: (next: ResumeRoute) => void;
  onClose: () => void;
}) {
  // Location selection stage
  const [projects, setProjects] = useState<ProjectEntry[] | null>(null);

  // Picker stage
  const pickerCwd = route.stage === "picker" ? route.cwd : null;
  const [sessions, setSessions] = useState<ArchivedSession[] | null>(null);
  const [query, setQuery] = useState("");
  // None until the arrow keys or the pointer pick one; typing clears it again.
  const [selectedIndex, setSelectedIndex] = useState<number | null>(null);
  const [resumingId, setResumingId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const searchInputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  // Where the pointer last was. A list that changes under a still pointer makes
  // the browser re-dispatch hover to whatever row is now beneath it; only
  // actual movement selects, so typing in the filter keeps its reset.
  const pointer = useRef<{ x: number; y: number } | null>(null);

  // Load remembered projects for the location picker
  useEffect(() => {
    if (route.stage !== "location") return;
    fetchProjects()
      .then((res) => {
        setProjects(res.projects);
      })
      .catch((e) => {
        setError(`Loading projects: ${String(e)}`);
        setProjects([]);
      });
  }, [route.stage]);

  // Load archived sessions when in picker stage
  useEffect(() => {
    if (!pickerCwd) return;
    fetchArchivedSessions(pickerCwd)
      .then((res) => {
        setSessions(res);
      })
      .catch((e) => {
        setError(`Loading archived sessions: ${String(e)}`);
        setSessions([]);
      });
  }, [pickerCwd]);

  // Filter sessions (excluding unresumable problems by default)
  const filtered = useMemo(() => {
    if (!sessions) return [];
    const resumable = sessions.filter((s) => !s.problem);
    return filterArchived(resumable, query);
  }, [sessions, query]);

  // Autofocus search input when picker stage opens or finishes loading
  useEffect(() => {
    if (route.stage === "picker" && sessions !== null) {
      searchInputRef.current?.focus();
    }
  }, [route.stage, sessions]);

  const handleResume = async (id: string, cwd: string) => {
    setResumingId(id);
    setError(null);
    try {
      await resumeSession(id, cwd);
      // Navigate to the newly activated session
      navigate(sessionHref(id));
    } catch (e) {
      setError(`Resuming session ${id}: ${String(e)}`);
      setResumingId(null);
    }
  };

  // Arrow keys move the selection and Enter resumes it; nothing on the page
  // says so, since the rows are plain click targets first.
  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (route.stage !== "picker") return;

    if (e.key === "ArrowDown") {
      e.preventDefault();
      setSelectedIndex((prev) =>
        filtered.length === 0
          ? null
          : prev === null
            ? 0
            : (prev + 1) % filtered.length,
      );
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setSelectedIndex((prev) =>
        filtered.length === 0
          ? null
          : prev === null
            ? filtered.length - 1
            : (prev - 1 + filtered.length) % filtered.length,
      );
    } else if (e.key === "Enter") {
      e.preventDefault();
      const target =
        selectedIndex === null ? undefined : filtered[selectedIndex];
      if (target && !resumingId) {
        void handleResume(target.id, route.cwd);
      }
    }
  };

  return (
    <div className="resume-page" onKeyDown={handleKeyDown}>
      <AppHeader onHome={onClose} />

      <main className="resume-page-body">
        {error && <div className="resume-error">{error}</div>}

        {route.stage === "location" ? (
          <div className="resume-location-container">
            <h1 className="resume-title">Resume a session</h1>
            <p className="resume-subtitle">
              Choose a workspace to see its archived sessions.
            </p>

            <div className="resume-locations-list selection-list selection-panel">
              {projects === null ? (
                <div className="resume-loading">Reading workspaces…</div>
              ) : projects.length === 0 ? (
                <div className="resume-empty">
                  No remembered workspaces found.
                </div>
              ) : (
                projects.map((proj) => (
                  <LocationRow
                    key={`${proj.cwd}:${proj.config ?? ""}`}
                    entry={proj}
                    projects={projects}
                    selected={false}
                    editing={false}
                    onSelect={() => {
                      onNavigate({ stage: "picker", cwd: proj.cwd });
                    }}
                  />
                ))
              )}
            </div>
          </div>
        ) : (
          <div className="resume-picker-container">
            {(() => {
              const [parent, name] = splitPath(homeRelative(route.cwd));
              return (
                <div className="resume-picker-header">
                  <h1 className="resume-title">
                    Archived sessions in{" "}
                    <span className="resume-workspace-path">
                      <span className="dim">{parent}</span>
                      <span>{name}</span>
                    </span>
                  </h1>
                </div>
              );
            })()}

            <div className="resume-search-bar">
              <input
                ref={searchInputRef}
                type="text"
                className="resume-search-input"
                placeholder="Filter by title or id"
                value={query}
                onChange={(e) => {
                  setQuery(e.target.value);
                  setSelectedIndex(null);
                }}
                aria-label="Filter archived sessions"
              />
            </div>

            {sessions === null ? (
              <div className="resume-loading">Reading archived sessions…</div>
            ) : filtered.length === 0 ? (
              <div className="resume-empty">
                {query
                  ? "No sessions match your filter."
                  : "No archived sessions here."}
              </div>
            ) : (
              <div
                className="resume-results-list selection-list selection-panel"
                ref={listRef}
                role="listbox"
              >
                {filtered.map((item, idx) => {
                  const isSelected = idx === selectedIndex;
                  const isResuming = resumingId === item.id;

                  return (
                    <div
                      key={item.id}
                      role="option"
                      aria-selected={isSelected}
                      className={`resume-result-row selection-row${isSelected ? " selected" : ""}${isResuming ? " resuming" : ""}`}
                      onClick={() => void handleResume(item.id, route.cwd)}
                      onMouseMove={(e) => {
                        const last = pointer.current;
                        pointer.current = { x: e.clientX, y: e.clientY };
                        if (last?.x !== e.clientX || last.y !== e.clientY) {
                          setSelectedIndex(idx);
                        }
                      }}
                    >
                      <span
                        className="resume-result-title"
                        title={item.title ?? item.id}
                      >
                        {item.title ?? <span className="dim">Untitled</span>}
                      </span>
                      <span className="resume-result-id">{item.id}</span>
                      <span className="resume-result-age">
                        {item.last_active_at ? (
                          <RelativeTime date={item.last_active_at} />
                        ) : null}
                      </span>
                      <span className="resume-hint">
                        {isResuming ? "resuming…" : "resume ›"}
                      </span>
                    </div>
                  );
                })}
              </div>
            )}
          </div>
        )}
      </main>
    </div>
  );
}
