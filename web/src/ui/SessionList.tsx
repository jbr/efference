// The host's root: an overview of every live session, grouped by working
// directory into workspaces, one card each (docs/web-client.md).
//
// The listing is a subscription (`api/sessions.ts`), so a session created or
// ended anywhere appears or leaves on its own. Each card's richer state — the
// gate's ask, usage, models, the latest intent and reply — comes from that
// session's own state topics and log tail, one connection per card, until the
// host serves an overview stream (backlog/host-overview-stream.md).

import { type RefObject, useEffect, useMemo, useRef, useState } from "react";
import { openSessionCard, openSessions } from "../api/sessions.ts";
import { archiveSession, type SessionEntry } from "../api/http.ts";
import type { StateSnapshot } from "../api/types.ts";
import { cardModel, foldRecent, type Recent, workspaces } from "./overview.ts";
import { SessionCard } from "./SessionCard.tsx";
import { NewSessionForm } from "./NewSessionForm.tsx";
import { archiveWarning } from "./sessionRow.ts";
import type { Draft } from "./sessionForm.ts";
import "./SessionList.css";

interface Feed {
  state: StateSnapshot;
  recent: Recent;
}

/** Each listed session's state and recent log, followed while it is listed. */
function useSessionFeeds(sessions: SessionEntry[]): Map<string, Feed> {
  const [feeds, setFeeds] = useState(() => new Map<string, Feed>());
  const ids = sessions.map((session) => session.id).join("\n");
  useEffect(() => {
    const handles = ids
      .split("\n")
      .filter((id) => id !== "")
      .map((id) =>
        openSessionCard(id, ({ state, entries }) =>
          setFeeds((prev) => {
            const recent = entries.reduce(
              foldRecent,
              prev.get(id)?.recent ?? {},
            );
            return new Map(prev).set(id, { state, recent });
          }),
        ),
      );
    return () => handles.forEach((handle) => handle.close());
  }, [ids]);
  return feeds;
}

/** The top bar's rendered height as `--top-bar-height` on the page, for the
 * workspace headings to dock beneath. Measured rather than assumed: the bar's
 * height moves with the safe-area inset and the wordmark's font. */
function useTopBarHeight() {
  const page = useRef<HTMLDivElement>(null);
  const bar = useRef<HTMLElement>(null);
  const [height, setHeight] = useState(0);
  useEffect(() => {
    if (!page.current || !bar.current) return;
    const target = page.current;
    const observer = new ResizeObserver(([entry]) => {
      const next = entry.borderBoxSize[0]?.blockSize ?? 0;
      target.style.setProperty("--top-bar-height", `${next}px`);
      setHeight(next);
    });
    observer.observe(bar.current);
    return () => observer.disconnect();
  }, []);
  return { page, bar, height };
}

/** Mark each docked workspace heading `data-stuck`, so it takes on the bar
 * material only while content scrolls beneath it. An observer rather than a
 * `scroll-state` container query, which Safari does not support: the root is
 * inset one pixel below the bar, so a docked heading is exactly the one cut
 * off at the top of that root. `key` re-observes when the headings change. */
function useStuckHeadings(
  page: RefObject<HTMLDivElement | null>,
  barHeight: number,
  key: string,
) {
  useEffect(() => {
    if (!page.current || barHeight === 0) return;
    const observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          const stuck =
            entry.isIntersecting &&
            entry.intersectionRatio < 1 &&
            entry.boundingClientRect.top < barHeight + 2;
          entry.target.toggleAttribute("data-stuck", stuck);
        }
      },
      { rootMargin: `-${barHeight + 1}px 0px 0px 0px`, threshold: [0, 1] },
    );
    page.current
      .querySelectorAll(".workspace-head")
      .forEach((heading) => observer.observe(heading));
    return () => observer.disconnect();
  }, [page, barHeight, key]);
}

export function SessionList({
  sessions: initial,
}: {
  sessions: SessionEntry[];
}) {
  const [sessions, setSessions] = useState(initial);
  const [archiving, setArchiving] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  // The new-session form's draft, held here rather than by the form: the draft
  // survives while the page is open, so cancelling and reopening returns to it
  // and only a reload discards it (backlog/new-session-form.md). `requested`
  // is the directory a workspace `+` asked for, applied once the form knows
  // which configuration that directory was last launched with.
  const [draft, setDraft] = useState<Draft | null>(null);
  const [requested, setRequested] = useState<string | null>(null);
  const [formOpen, setFormOpen] = useState(false);
  const feeds = useSessionFeeds(sessions);
  const { page, bar, height: barHeight } = useTopBarHeight();

  useEffect(() => {
    const handle = openSessions(
      (live) => {
        setSessions(live);
        setError(null);
      },
      () => setError("Lost the connection to the host — retrying"),
    );
    return () => handle.close();
  }, []);

  const groups = useMemo(() => {
    const cards = sessions.map((entry) => {
      const feed = feeds.get(entry.id);
      return cardModel(entry, sessions, feed?.state ?? null, feed?.recent);
    });
    return workspaces(cards);
  }, [sessions, feeds]);

  useStuckHeadings(
    page,
    barHeight,
    groups.map((group) => group.cwd).join("\n"),
  );

  async function end(id: string) {
    setArchiving(id);
    try {
      await archiveSession(id);
      // The card leaves on the next frame of the listing stream; the host
      // answers only once teardown has run, so that frame is already out.
    } catch (failure) {
      setError(`Archiving ${id}: ${String(failure)}`);
    } finally {
      setArchiving(null);
    }
  }

  return (
    <div className="app overview" ref={page}>
      <header className="top-bar" ref={bar}>
        <div className="top-bar-inner">
          <span className="wordmark">efference</span>
          <span className="spacer" />
          <button
            className="primary"
            onClick={() => {
              setRequested(null);
              setFormOpen(true);
            }}
          >
            New session
          </button>
        </div>
      </header>
      <main className="overview-body">
        {error !== null && <p className="overview-error">{error}</p>}
        {groups.length === 0 ? (
          // Liveness, not existence: a dead session's log stays on disk and
          // readable by the offline verbs, and is not listed here.
          <div className="overview-empty">
            <p className="overview-empty-title">No active sessions</p>
            <p className="dim">
              Start one with <code>efference tui</code>
            </p>
          </div>
        ) : (
          groups.map((group) => (
            <section className="workspace" key={group.cwd}>
              <header className="workspace-head">
                <h2 className="workspace-path" title={group.cwd}>
                  <span className="workspace-parent">{group.parent}</span>
                  <span className="workspace-name">{group.name}</span>
                </h2>
                <button
                  className="workspace-new"
                  aria-label={`New session in ${group.parent}${group.name}`}
                  onClick={() => {
                    setRequested(group.cwd);
                    setFormOpen(true);
                  }}
                >
                  +
                </button>
              </header>
              <div className="card-grid">
                {group.cards.map((card) => (
                  <SessionCard
                    key={card.id}
                    card={card}
                    warning={archiveWarning(
                      sessions.find((s) => s.id === card.id)!,
                    )}
                    archiving={archiving === card.id}
                    onArchive={() => void end(card.id)}
                  />
                ))}
              </div>
            </section>
          ))
        )}
      </main>
      {formOpen && (
        <NewSessionForm
          draft={draft}
          requested={requested}
          onDraft={setDraft}
          onClose={() => setFormOpen(false)}
        />
      )}
    </div>
  );
}
