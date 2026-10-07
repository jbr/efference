import { useEffect, useMemo, useRef, useState } from "react";
import { AppHeader } from "./AppHeader.tsx";
import { LocationRow } from "./LocationRow.tsx";
import { RelativeTime } from "./RelativeTime.tsx";
import {
  fetchArchivedSessions,
  resumeSession,
  type ArchivedSession,
} from "../api/http.ts";
import { fetchProjects, type ProjectEntry } from "../api/sessionForm.ts";
import { sessionHref } from "../api/base.ts";
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
  const [selectedIndex, setSelectedIndex] = useState(0);
  const [resumingId, setResumingId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const searchInputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

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
      window.location.assign(sessionHref(id));
    } catch (e) {
      setError(`Resuming session ${id}: ${String(e)}`);
      setResumingId(null);
    }
  };

  // Keyboard navigation inside the picker
  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (route.stage !== "picker") return;

    if (e.key === "ArrowDown") {
      e.preventDefault();
      setSelectedIndex((prev) =>
        filtered.length > 0 ? (prev + 1) % filtered.length : 0,
      );
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setSelectedIndex((prev) =>
        filtered.length > 0
          ? (prev - 1 + filtered.length) % filtered.length
          : 0,
      );
    } else if (e.key === "Enter") {
      e.preventDefault();
      const target = filtered[selectedIndex];
      if (target && !resumingId) {
        void handleResume(target.id, route.cwd);
      }
    } else if (e.key === "Escape") {
      e.preventDefault();
      if (query) {
        setQuery("");
      } else {
        onClose();
      }
    }
  };

  return (
    <div className="resume-page" onKeyDown={handleKeyDown}>
      <AppHeader onHome={onClose}>
        <button onClick={onClose} className="resume-header-cancel">
          Cancel
        </button>
      </AppHeader>

      <main className="resume-page-body">
        {error && <div className="resume-error">{error}</div>}

        {route.stage === "location" ? (
          <div className="resume-location-container">
            <h1 className="resume-title">Resume a session</h1>
            <p className="resume-subtitle">
              Choose a workspace to see its archived sessions.
            </p>

            <div className="resume-locations-list">
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
                  <span className="resume-count-badge">
                    {filtered.length} of{" "}
                    {sessions ? sessions.filter((s) => !s.problem).length : 0}
                  </span>
                </div>
              );
            })()}

            <div className="resume-search-bar">
              <input
                ref={searchInputRef}
                type="text"
                className="resume-search-input"
                placeholder="type to filter by title or id"
                value={query}
                onChange={(e) => {
                  setQuery(e.target.value);
                  setSelectedIndex(0);
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
              <div className="resume-results-list" ref={listRef} role="listbox">
                {filtered.map((item, idx) => {
                  const isSelected = idx === selectedIndex;
                  const isResuming = resumingId === item.id;

                  return (
                    <div
                      key={item.id}
                      role="option"
                      aria-selected={isSelected}
                      className={`resume-result-row${isSelected ? " selected" : ""}${isResuming ? " resuming" : ""}`}
                      onClick={() => void handleResume(item.id, route.cwd)}
                      onMouseEnter={() => setSelectedIndex(idx)}
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
                      <button
                        className="resume-result-btn"
                        onClick={(e) => {
                          e.stopPropagation();
                          void handleResume(item.id, route.cwd);
                        }}
                        disabled={isResuming}
                      >
                        {isResuming ? "Resuming…" : "Resume"}
                      </button>
                    </div>
                  );
                })}
              </div>
            )}

            <div className="resume-hints dim">
              <span>↑↓ move</span>
              <span>•</span>
              <span>enter resumes</span>
              <span>•</span>
              <span>esc goes back</span>
            </div>
          </div>
        )}
      </main>
    </div>
  );
}
