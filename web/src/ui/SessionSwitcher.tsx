import { ContextBar } from "./ContextBar.tsx";
// The quick session switcher (docs/web-client.md): a slim second row under the
// top bar listing the *other* live sessions on the host, most recently visited
// first. At rest a tab is a status dot and a title; hovering or focusing one
// opens a card — the facts the overview card carries, less its settings —
// without leaving the page. Switching is a client-side navigation, as it is
// from the list (ui/route.ts): the tabs are plain links, so a modified click
// still opens another tab.
//
// Tabs that do not fit the row spill into a `⋯` menu, which carries a count
// when a session in it is waiting on the operator. At rest it costs one
// connection, the host's listing stream; a session's own state and log tail
// are opened only while its card is showing. With no other
// live session — or at a session daemon's root, where there is no host to
// list — it renders nothing and the row is absent.

import {
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { apiBase, sessionHref } from "../api/base.ts";
import { prefetchStore } from "../session/registry.ts";
import { lastListing, rememberListing } from "../session/listing.ts";
import type { SessionEntry } from "../api/http.ts";
import { openSessionCard, openSessions } from "../api/sessions.ts";
import type { StateSnapshot } from "../api/types.ts";
import {
  type CardModel,
  cardModel,
  foldRecent,
  type Recent,
  statusPhrase,
  tokens,
} from "./overview.ts";
import { Activity } from "./SessionCard.tsx";
import { orderByRecency, readVisits, recordVisit } from "./switcherOrder.ts";
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
 * quiet. A session parked at the cold check waits on the operator like a stall
 * and takes its colour. */
function dotKind(
  card: CardModel,
): "needs" | "turn" | "stalled" | "parked" | "working" | "idle" {
  if (card.status === "gated") return "needs";
  if (card.status === "stalled") return "stalled";
  if (card.status === "parked") return "parked";
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

const OPEN_DELAY_MS = 180;
const CLOSE_DELAY_MS = 120;

export function SessionSwitcher({ currentId }: { currentId: string }) {
  const sessions = useLiveSessions();
  // Read before this page's own visit is recorded; the current session is not
  // a tab, so its own entry would not matter anyway.
  const [visits] = useState(readVisits);
  useEffect(() => {
    if (apiBase() !== "") recordVisit(currentId);
  }, [currentId]);

  const cards = useMemo(() => {
    const all = sessions ?? [];
    const others = orderByRecency(
      all.filter((session) => session.id !== currentId),
      visits,
    );
    return others.map((entry) => cardModel(entry, all, null));
  }, [sessions, currentId, visits]);

  const entries = useMemo(
    () => new Map((sessions ?? []).map((session) => [session.id, session])),
    [sessions],
  );
  const [shown, setShown] = useState<string | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const feed = useCardFeed(shown);
  const { row, measure, fit } = useFit(cards.map((c) => c.title ?? c.id));

  if (sessions === null || cards.length === 0) return null;

  const schedule = (id: string | null, delay: number) => {
    clearTimeout(timer.current);
    timer.current = setTimeout(() => setShown(id), delay);
  };
  const visible = cards.slice(0, fit ?? cards.length);
  const spilled = cards.slice(fit ?? cards.length);

  return (
    <nav className="switcher" aria-label="Other sessions" ref={row}>
      {visible.map((card) => (
        <span
          key={card.id}
          className="switcher-slot"
          onPointerEnter={(event) => {
            if (event.pointerType !== "mouse") return;
            // Ahead of the card's own delay: the read has the whole hover to land in.
            prefetchStore(`/sessions/${card.id}`);
            schedule(card.id, OPEN_DELAY_MS);
          }}
          onPointerLeave={() => schedule(null, CLOSE_DELAY_MS)}
          onFocus={() => {
            prefetchStore(`/sessions/${card.id}`);
            schedule(card.id, 0);
          }}
          onBlur={() => schedule(null, CLOSE_DELAY_MS)}
          onKeyDown={(event) => {
            if (event.key === "Escape") schedule(null, 0);
          }}
        >
          <Tab card={card} />
          {shown === card.id && (
            <HoverCard
              card={cardModel(
                entries.get(card.id) ?? {
                  id: card.id,
                  status: card.status,
                  processes: card.processes,
                },
                sessions,
                feed?.state ?? null,
                feed?.recent,
              )}
            />
          )}
        </span>
      ))}
      {spilled.length > 0 && <MoreMenu cards={spilled} />}
      {/* Every tab and the `⋯` button at natural width, unseen, so the fit
          is read from layout rather than guessed. */}
      <div className="switcher-measure" aria-hidden="true" ref={measure}>
        {cards.map((card) => (
          <span key={card.id} className="switcher-slot">
            <Tab card={card} inert />
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
}: {
  card: CardModel;
  inert?: boolean;
}) {
  const label = card.title ?? card.id;
  const phrase = statusPhrase(card);
  const body = (
    <>
      <span className="switcher-dot" aria-hidden="true" />
      <span className="switcher-label">{label}</span>
      {!inert && (
        <span className="visually-hidden">
          {" "}
          {card.id}
          {phrase !== null ? ` ${phrase}` : ""}
        </span>
      )}
    </>
  );
  const className = `switcher-tab dot-${dotKind(card)}`;
  return inert ? (
    <span className={className}>{body}</span>
  ) : (
    <a className={className} href={sessionHref(card.id)}>
      {body}
    </a>
  );
}

/**
 * How many tabs fit the row, `null` until measured. The unseen measuring strip
 * holds every tab then the `⋯` button; tabs are taken in order while they and
 * the button fit, and all of them are taken when they all fit without it.
 */
function useFit(labels: string[]): {
  row: React.RefObject<HTMLElement | null>;
  measure: React.RefObject<HTMLDivElement | null>;
  fit: number | null;
} {
  const row = useRef<HTMLElement>(null);
  const measure = useRef<HTMLDivElement>(null);
  const [fit, setFit] = useState<number | null>(null);
  const key = labels.join("\n");
  useLayoutEffect(() => {
    const element = row.current;
    const strip = measure.current;
    if (element === null || strip === null) return;
    const compute = () => {
      const style = getComputedStyle(element);
      const gap = parseFloat(getComputedStyle(strip).columnGap) || 0;
      const room =
        element.clientWidth -
        parseFloat(style.paddingLeft) -
        parseFloat(style.paddingRight);
      const widths = [...strip.children].map(
        (child) => child.getBoundingClientRect().width,
      );
      const more = widths.pop() ?? 0;
      const total =
        widths.reduce((sum, w) => sum + w, 0) +
        gap * Math.max(0, widths.length - 1);
      if (total <= room) return setFit(widths.length);
      let used = more;
      let count = 0;
      for (const width of widths) {
        if (used + gap + width > room) break;
        used += gap + width;
        count += 1;
      }
      setFit(count);
    };
    compute();
    const observer = new ResizeObserver(compute);
    observer.observe(element);
    return () => observer.disconnect();
  }, [key]);
  return { row, measure, fit };
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
function MoreMenu({ cards }: { cards: CardModel[] }) {
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
            <MenuRow key={card.id} card={card} />
          ))}
          <a className="switcher-menu-row switcher-menu-all" href="/">
            All sessions
          </a>
        </div>
      )}
    </div>
  );
}

function MenuRow({ card }: { card: CardModel }): ReactNode {
  const phrase = statusPhrase(card);
  return (
    <a
      className={`switcher-menu-row dot-${dotKind(card)}`}
      href={sessionHref(card.id)}
      role="menuitem"
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

/** A floating panel opens at its anchor's left edge; one anchored near the
 * right edge of the window is shifted back until it is wholly visible. */
function useKeepOnScreen() {
  const ref = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const element = ref.current;
    if (element === null) return;
    element.style.transform = "";
    const over =
      element.getBoundingClientRect().right - (window.innerWidth - 8);
    if (over > 0) element.style.transform = `translateX(${-over}px)`;
  });
  return ref;
}

export function HoverCard({ card }: { card: CardModel }) {
  const phrase = statusPhrase(card);
  const context = card.context;
  const used = context?.used ?? null;
  const pop = useKeepOnScreen();
  return (
    <div className="switcher-pop" ref={pop}>
      <a
        className={`switcher-card attention-${card.attention}${card.status === "stalled" || card.status === "parked" ? ` status-${card.status}` : ""}`}
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
    </div>
  );
}
