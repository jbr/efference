// The quick session switcher (docs/web-client.md): a slim second row under the
// top bar listing live sessions in stable slots, including the selected session.
// At rest a tab is a status dot and a session id; hovering or focusing one
// opens a card — the facts the overview card carries, less its settings —
// without leaving the page. Switching is a client-side navigation, as it is
// from the list (ui/route.ts): the tabs are plain links, so a modified click
// still opens another tab.
//
// Tabs that do not fit the row spill into a `+N` menu; the selected tab stays
// visible so its card's archive action remains reachable. At rest it costs one
// connection, the host's listing stream; a session's own state and log tail
// are opened only while its card is showing. An empty listing — or a session
// daemon's root, where there is no host to list — leaves the row absent.

import {
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { HOVER_OPEN_DELAY_MS, HOVER_CLOSE_DELAY_MS } from "./HoverPopup.tsx";
import { apiBase, sessionHref, sessionId } from "../api/base.ts";
import { prefetchStore } from "../session/registry.ts";
import { lastListing, rememberListing } from "../session/listing.ts";
import { archiveSession, type SessionEntry } from "../api/http.ts";
import { openSessionCard, openSessions } from "../api/sessions.ts";
import type { StateSnapshot } from "../api/types.ts";
import {
  type CardModel,
  cardModel,
  foldRecent,
  type Recent,
  statusPhrase,
} from "./overview.ts";
import {
  orderByRecency,
  orderByTabs,
  readVisits,
  recordVisit,
} from "./switcherOrder.ts";
import { navigate } from "./route.ts";
import { HoverCard } from "./SessionHoverCard.tsx";
import { useKeepOnScreen } from "./useKeepOnScreen.ts";
import { useSwitcherFit } from "./useSwitcherFit.ts";
import "./SessionSwitcher.css";

/** The host's live sessions, followed; `null` until the first listing, and
 * always `null` where the page has no host behind it. */
function useLiveSessions(): SessionEntry[] | null {
  const [sessions, setSessions] = useState<SessionEntry[] | null>(
    lastListing(),
  );
  useEffect(() => {
    if (apiBase() === "") return;
    const handle = openSessions((listing) => {
      rememberListing(listing);
      setSessions(listing);
    });
    return () => handle.close();
  }, []);
  return sessions;
}

interface Feed {
  state: StateSnapshot;
  recent: Recent;
}

/** One session's state and recent log while `id` is non-null; the last reading
 * stays up while a reopened card catches up. */
function useCardFeed(id: string | null): Feed | null {
  const [feeds, setFeeds] = useState(() => new Map<string, Feed>());
  useEffect(() => {
    if (id === null) return;
    const handle = openSessionCard(id, ({ state, entries }) =>
      setFeeds((prev) => {
        const recent = entries.reduce(foldRecent, prev.get(id)?.recent ?? {});
        return new Map(prev).set(id, { state, recent });
      }),
    );
    return () => handle.close();
  }, [id]);
  return id === null ? null : (feeds.get(id) ?? null);
}

/** What the dot says, from what the listing alone knows. A gate blocks the
 * session, so it is loudest; a session whose turn ended — idle, or asking —
 * is waiting on the operator until archived, so it carries the same colour
 * without the halo; running is a faint pulse; a session never started is
 * quiet. A session parked at the cold check, or blocked, waits on the operator
 * like a stall and takes the parked colour. */
function dotKind(
  card: CardModel,
): "needs" | "turn" | "stalled" | "parked" | "working" | "idle" {
  if (card.status === "gated") return "needs";
  if (card.status === "stalled") return "stalled";
  if (card.status === "parked" || card.status === "blocked") return "parked";
  if (card.status === "running") return "working";
  return card.attention === "your_turn" ? "turn" : "idle";
}

/** Whether a session is asking for the operator, for the `⋯` badge. */
const wantsOperator = (card: CardModel) => {
  const kind = dotKind(card);
  return (
    kind === "needs" ||
    kind === "turn" ||
    kind === "stalled" ||
    kind === "parked"
  );
};

let tabOrder: string[] | null = null;

export function SessionSwitcher({ currentId }: { currentId: string }) {
  const sessions = useLiveSessions();
  // Recency seeds the first row; selecting a tab does not change its slot.
  const [visits] = useState(readVisits);
  useEffect(() => {
    if (apiBase() !== "") recordVisit(currentId);
  }, [currentId]);

  const cards = useMemo(() => {
    const all = sessions ?? [];
    const ordered =
      tabOrder === null
        ? orderByRecency(all, visits)
        : orderByTabs(all, tabOrder);
    return ordered.map((entry) => cardModel(entry, all, null));
  }, [sessions, visits]);
  useEffect(() => {
    if (cards.length > 0) tabOrder = cards.map((entry) => entry.id);
  }, [cards]);

  const entries = useMemo(
    () => new Map((sessions ?? []).map((session) => [session.id, session])),
    [sessions],
  );
  const [requestedShown, setShown] = useState<string | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const pinned = useRef(false);
  const keyboard = useRef(true);
  const requests = useRef(new Map<string, Promise<unknown>>());
  const popupId = useId();
  const { row, measure, fit } = useSwitcherFit(
    cards.map((c) => c.id),
    currentId,
  );
  const shown =
    requestedShown !== null &&
    entries.has(requestedShown) &&
    (fit === null || fit.has(requestedShown))
      ? requestedShown
      : null;
  const feed = useCardFeed(shown);

  useEffect(() => () => clearTimeout(timer.current), []);
  useEffect(() => {
    const key = () => {
      keyboard.current = true;
    };
    const pointer = (event: PointerEvent) => {
      keyboard.current = false;
      const slot = row.current?.querySelector('[data-popup-open="true"]');
      if (slot && !slot.contains(event.target as Node)) {
        clearTimeout(timer.current);
        pinned.current = false;
        setShown(null);
      }
    };
    document.addEventListener("keydown", key, true);
    document.addEventListener("pointerdown", pointer, true);
    return () => {
      document.removeEventListener("keydown", key, true);
      document.removeEventListener("pointerdown", pointer, true);
    };
  }, [row]);

  const schedule = (id: string | null, delay: number) => {
    clearTimeout(timer.current);
    timer.current = setTimeout(() => {
      pinned.current = false;
      setShown(id);
    }, delay);
  };
  const pin = (id: string) => {
    clearTimeout(timer.current);
    pinned.current = true;
    setShown(id);
  };
  function archive(id: string) {
    const existing = requests.current.get(id);
    if (existing) return existing;
    const request = archiveSession(id)
      .then(() => {
        // A request can outlive navigation: do not leave a different session.
        if (sessionId() === id) navigate("/", "replace");
      })
      .finally(() => requests.current.delete(id));
    requests.current.set(id, request);
    return request;
  }

  if (sessions === null || cards.length === 0) return null;
  const visible = cards.filter((card) => fit === null || fit.has(card.id));
  const spilled = cards.filter((card) => fit !== null && !fit.has(card.id));

  return (
    <nav className="switcher" aria-label="Sessions" ref={row}>
      {visible.map((card) => (
        <span
          key={card.id}
          className="switcher-slot"
          data-popup-open={shown === card.id}
          onClickCapture={() => clearTimeout(timer.current)}
          onPointerEnter={(event) => {
            if (event.pointerType !== "mouse") return;
            // Ahead of the card's own delay: the read has the whole hover to land in.
            prefetchStore(`/sessions/${card.id}`);
            if (shown === card.id) clearTimeout(timer.current);
            else if (!pinned.current || shown === null)
              schedule(card.id, HOVER_OPEN_DELAY_MS);
          }}
          onPointerLeave={(event) => {
            if (event.pointerType !== "mouse" || pinned.current) return;
            if (
              keyboard.current &&
              event.currentTarget.contains(document.activeElement)
            )
              return;
            schedule(null, HOVER_CLOSE_DELAY_MS);
          }}
          onFocus={(event) => {
            if (
              !keyboard.current ||
              event.currentTarget.contains(event.relatedTarget as Node | null)
            )
              return;
            prefetchStore(`/sessions/${card.id}`);
            pin(card.id);
          }}
          onBlur={(event) => {
            if (
              !event.currentTarget.contains(event.relatedTarget as Node | null)
            )
              schedule(null, HOVER_CLOSE_DELAY_MS);
          }}
          onKeyDown={(event) => {
            if (event.key !== "Escape") return;
            event.currentTarget
              .querySelector<HTMLAnchorElement>(".switcher-tab")
              ?.focus();
            clearTimeout(timer.current);
            pinned.current = false;
            setShown(null);
            event.stopPropagation();
          }}
        >
          <Tab
            card={card}
            current={card.id === currentId}
            popupId={`${popupId}-${card.id}`}
            open={shown === card.id}
            onCurrentClick={() => {
              if (shown === card.id && pinned.current) {
                clearTimeout(timer.current);
                pinned.current = false;
                setShown(null);
              } else pin(card.id);
            }}
          />
          {shown === card.id && (
            <HoverCard
              id={`${popupId}-${card.id}`}
              card={cardModel(
                entries.get(card.id)!,
                sessions,
                feed?.state ?? null,
                feed?.recent,
              )}
              onArchive={() => archive(card.id)}
              onInteract={() => pin(card.id)}
            />
          )}
        </span>
      ))}
      {spilled.length > 0 && <MoreMenu cards={spilled} currentId={currentId} />}
      {/* Every tab and the `⋯` button at natural width, unseen, so the fit
          is read from layout rather than guessed. */}
      <div className="switcher-measure" aria-hidden="true" ref={measure}>
        {cards.map((card) => (
          <span key={card.id} className="switcher-slot">
            <Tab card={card} inert current={card.id === currentId} />
          </span>
        ))}
        <MoreButton total={99} waiting={1} />
      </div>
    </nav>
  );
}

export function Tab({
  card,
  inert = false,
  current = false,
  onCurrentClick,
  popupId,
  open,
}: {
  card: CardModel;
  inert?: boolean;
  current?: boolean;
  onCurrentClick?: () => void;
  popupId?: string;
  open?: boolean;
}) {
  const label = card.id;
  const phrase = statusPhrase(card);
  const body = (
    <>
      <span className="switcher-dot" aria-hidden="true" />
      <span className="switcher-label">{label}</span>
      {!inert && <span className="visually-hidden"> {phrase ?? ""}</span>}
    </>
  );
  const className = `switcher-tab dot-${dotKind(card)}${current ? " is-current" : ""}`;
  return inert ? (
    <span className={className}>{body}</span>
  ) : (
    <a
      className={className}
      href={sessionHref(card.id)}
      aria-current={current ? "page" : undefined}
      aria-haspopup={popupId ? "dialog" : undefined}
      aria-controls={open ? popupId : undefined}
      aria-expanded={open}
      onClick={
        current
          ? (event) => {
              if (
                event.button !== 0 ||
                event.metaKey ||
                event.ctrlKey ||
                event.shiftKey ||
                event.altKey
              )
                return;
              event.preventDefault();
              onCurrentClick?.();
            }
          : undefined
      }
    >
      {body}
    </a>
  );
}

export function MoreButton({
  total,
  waiting,
  open = false,
  onClick,
}: {
  /** How many sessions are in the menu. */
  total: number;
  /** How many of them are waiting on the operator. */
  waiting: number;
  open?: boolean;
  onClick?: () => void;
}) {
  return (
    <button
      className={open ? "switcher-more open" : "switcher-more"}
      aria-expanded={onClick === undefined ? undefined : open}
      aria-label={
        waiting > 0
          ? `${total} more sessions, ${waiting} waiting`
          : `${total} more sessions`
      }
      onClick={onClick}
      tabIndex={onClick === undefined ? -1 : 0}
    >
      {waiting > 0 && (
        <span className="switcher-dot more-dot" aria-hidden="true" />
      )}
      +{total}
    </button>
  );
}

/** The tabs that did not fit, as a list: dot, title, and what it is doing. */
function MoreMenu({
  cards,
  currentId,
}: {
  cards: CardModel[];
  currentId: string;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (event: Event) => {
      if (!ref.current?.contains(event.target as Node)) setOpen(false);
    };
    const key = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    document.addEventListener("pointerdown", close);
    document.addEventListener("keydown", key);
    return () => {
      document.removeEventListener("pointerdown", close);
      document.removeEventListener("keydown", key);
    };
  }, [open]);
  const menu = useKeepOnScreen();
  const waiting = cards.filter(wantsOperator).length;
  return (
    <div className="switcher-more-wrap" ref={ref}>
      <MoreButton
        total={cards.length}
        waiting={waiting}
        open={open}
        onClick={() => setOpen(!open)}
      />
      {open && (
        <div className="switcher-menu" role="menu" ref={menu}>
          {cards.map((card) => (
            <MenuRow
              key={card.id}
              card={card}
              current={card.id === currentId}
            />
          ))}
          <a className="switcher-menu-row switcher-menu-all" href="/">
            All sessions
          </a>
        </div>
      )}
    </div>
  );
}

function MenuRow({
  card,
  current,
}: {
  card: CardModel;
  current: boolean;
}): ReactNode {
  const phrase = statusPhrase(card);
  return (
    <a
      className={`switcher-menu-row dot-${dotKind(card)}`}
      href={sessionHref(card.id)}
      role="menuitem"
      aria-current={current ? "page" : undefined}
    >
      <span className="switcher-dot" aria-hidden="true" />
      <span className="switcher-label">{card.title ?? card.id}</span>
      <span className="switcher-menu-status">
        {card.title !== null && `${card.id} `}
        {phrase}
      </span>
    </a>
  );
}
