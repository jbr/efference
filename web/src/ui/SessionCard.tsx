// One session on the overview (docs/web-client.md): what it is, where the
// operator's attention stands with it, and what it is doing. Cards in a row
// share one height (a minimum, grown by the tallest), so a workspace reads as a
// grid rather than a feed.
//
// The whole card opens the session: the title's link is stretched over the
// card, and the few controls that are not "open" (the supervisor link, the
// menu) sit above it. Archiving is behind the menu — it is the one destructive
// verb here, and a card's prominent action is opening it.

import { useEffect, useRef, useState } from "react";
import { sessionHref } from "../api/base.ts";
import type { CardModel } from "./overview.ts";
import { tokens } from "./overview.ts";

/** The predicate the card's top line completes after the session's id —
 * "amber-lattice needs approval" — or nothing: working is the default state and
 * needs no words, and a new session's title already says it. */
function statusPhrase(card: CardModel): string | null {
  if (card.status === "gated") return "needs approval";
  if (card.status === "asking") return "has a question";
  if (card.status === "stalled") return "has stalled";
  if (card.attention === "your_turn") return "is waiting for you";
  return null;
}

function Activity({ card }: { card: CardModel }) {
  const activity = card.activity;
  // Nothing known: a new session's title already says so.
  if (activity === null) return <p className="card-activity" />;
  switch (activity.kind) {
    case "gate":
      return (
        <p className="card-activity card-activity-gate">
          <span className="card-activity-tool">{activity.tool}</span>
          <code>{activity.detail}</code>
        </p>
      );
    case "ask":
      return (
        <p className="card-activity card-activity-ask">{activity.detail}</p>
      );
    case "stall":
      return (
        <p className="card-activity card-activity-stall">{activity.detail}</p>
      );
    case "intent":
      return <p className="card-activity">{activity.detail}</p>;
    case "reply":
      return (
        <p className="card-activity card-activity-reply">{activity.detail}</p>
      );
  }
}

function CardMenu({
  card,
  warning,
  onArchive,
}: {
  card: CardModel;
  warning: string | null;
  onArchive: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const close = (event: PointerEvent | KeyboardEvent) => {
      if (
        event instanceof KeyboardEvent
          ? event.key === "Escape"
          : !ref.current?.contains(event.target as Node)
      ) {
        setOpen(false);
        setConfirming(false);
      }
    };
    document.addEventListener("pointerdown", close);
    document.addEventListener("keydown", close);
    return () => {
      document.removeEventListener("pointerdown", close);
      document.removeEventListener("keydown", close);
    };
  }, [open]);

  return (
    <div className="card-menu" ref={ref}>
      <button
        className="card-menu-button"
        aria-label={`actions for ${card.id}`}
        aria-expanded={open}
        onClick={() => setOpen(!open)}
      >
        ⋯
      </button>
      {open && (
        <div className="card-menu-list" role="menu">
          {confirming ? (
            <>
              <p className="card-menu-warning">
                {card.id} {warning}.
              </p>
              <button role="menuitem" className="danger" onClick={onArchive}>
                Archive anyway
              </button>
              <button role="menuitem" onClick={() => setConfirming(false)}>
                Cancel
              </button>
            </>
          ) : (
            <button
              role="menuitem"
              className="danger"
              onClick={() =>
                warning === null ? onArchive() : setConfirming(true)
              }
            >
              Archive session
            </button>
          )}
        </div>
      )}
    </div>
  );
}

export function SessionCard({
  card,
  warning,
  archiving,
  onArchive,
}: {
  card: CardModel;
  warning: string | null;
  archiving: boolean;
  onArchive: () => void;
}) {
  const context = card.context;
  const used = context?.used ?? null;
  const phrase = archiving ? "is archiving…" : statusPhrase(card);
  return (
    <article
      className={`card attention-${card.attention}${card.status === "stalled" ? " status-stalled" : ""}${archiving ? " archiving" : ""}`}
    >
      <header className="card-head">
        <span className="card-status-line">
          <span className="card-id">{card.id}</span>
          {phrase !== null && <span className="card-status"> {phrase}</span>}
        </span>
        <span className="spacer" />
        <CardMenu card={card} warning={warning} onArchive={onArchive} />
      </header>

      <h3 className="card-title">
        <a className="card-link" href={sessionHref(card.id)}>
          {/* A title is written after the first turn, not with it, so a
              session can be under way with none yet. */}
          {card.title ?? (
            <span className="card-title-new">
              {card.attention === "new" ? "Ready to get started" : "Untitled"}
            </span>
          )}
        </a>
      </h3>

      <Activity card={card} />

      <footer className="card-meta">
        <dl className="card-facts">
          {card.models !== null && (
            <>
              <dt>model</dt>
              <dd>{card.models.main}</dd>
              <dt>spotter</dt>
              <dd>{card.models.spotter}</dd>
              {card.models.triage !== null && (
                <>
                  <dt>triage</dt>
                  <dd>{card.models.triage}</dd>
                </>
              )}
            </>
          )}
          {card.supervisor !== null && (
            <>
              <dt>supervisor</dt>
              <dd className="card-relation">
                <a href={sessionHref(card.supervisor)}>{card.supervisor}</a>
              </dd>
            </>
          )}
          {card.supervises.length > 0 && (
            <>
              <dt>supervises</dt>
              <dd className="card-relation">
                {card.supervises.map((id, i) => (
                  <span key={id}>
                    {i > 0 && ", "}
                    <a href={sessionHref(id)}>{id}</a>
                  </span>
                ))}
              </dd>
            </>
          )}
          {card.processes > 0 && (
            <>
              <dt>processes</dt>
              <dd>{card.processes} running</dd>
            </>
          )}
        </dl>
        <div className="card-line card-usage">
          {context !== null && (
            <span className="card-context">
              {context.limit !== null && (
                <span className="meter">
                  <span
                    className="meter-fill"
                    style={{
                      width:
                        used === null
                          ? 0
                          : `${Math.min(100, (used / context.limit) * 100)}%`,
                    }}
                  />
                </span>
              )}
              {used === null ? "–" : tokens(used)}
              {context.limit !== null && `/${tokens(context.limit)}`}
            </span>
          )}
          {card.costUsd !== null && <span>${card.costUsd.toFixed(2)}</span>}
        </div>
      </footer>
    </article>
  );
}
