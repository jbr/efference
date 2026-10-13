import { sessionHref } from "../api/base.ts";
import { ArchiveAction } from "./ArchiveAction.tsx";
import { ContextBar } from "./ContextBar.tsx";
import { homeRelative } from "./identity.ts";
import { type CardModel, statusPhrase, tokens } from "./overview.ts";
import { Activity } from "./SessionCard.tsx";
import { archiveWarning } from "./sessionRow.ts";
import { useKeepOnScreen } from "./useKeepOnScreen.ts";

export function HoverCard({
  card,
  id,
  onArchive,
  onInteract,
}: {
  card: CardModel;
  id?: string;
  onArchive: () => Promise<unknown>;
  onInteract?: () => void;
}) {
  const phrase = statusPhrase(card);
  const context = card.context;
  const used = context?.used ?? null;
  const pop = useKeepOnScreen();
  return (
    <div className="switcher-pop hover-popup-position" ref={pop}>
      <div
        id={id}
        role="dialog"
        aria-label={`${card.id} session details`}
        className={`switcher-card hover-popup-surface attention-${card.attention}${card.status === "stalled" || card.status === "parked" ? ` status-${card.status}` : ""}`}
      >
        <a
          className="switcher-card-link"
          href={sessionHref(card.id)}
          tabIndex={-1}
        >
          <span className="switcher-card-head">
            <span className="card-id">{card.id}</span>
            {phrase !== null && <span className="card-status"> {phrase}</span>}
          </span>
          <span className="switcher-card-title">
            {card.title ?? <em>Untitled</em>}
          </span>
          <Activity card={card} />
          <span className="switcher-card-facts">
            {card.cwd && (
              <span>
                <span>directory</span>
                <span title={card.cwd}>{homeRelative(card.cwd)}</span>
              </span>
            )}
            {card.models && (
              <>
                <span>
                  <span>model</span>
                  <span>{card.models.main}</span>
                </span>
                <span>
                  <span>spotter</span>
                  <span>{card.models.spotter}</span>
                </span>
                {card.models.triage && (
                  <span>
                    <span>triage</span>
                    <span>{card.models.triage}</span>
                  </span>
                )}
              </>
            )}
          </span>
          {context !== null && (
            <span className="switcher-card-usage">
              {context.limit !== null && (
                <ContextBar used={used} limit={context.limit} />
              )}
              {used === null ? "–" : tokens(used)}
              {context.limit !== null && `/${tokens(context.limit)}`}
              {card.costUsd !== null && <span>${card.costUsd.toFixed(2)}</span>}
            </span>
          )}
        </a>
        <ArchiveAction
          id={card.id}
          warning={archiveWarning({
            id: card.id,
            status: card.status,
            processes: card.processes,
          })}
          onArchive={onArchive}
          onInteract={onInteract}
        />
      </div>
    </div>
  );
}
