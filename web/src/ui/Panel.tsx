// The bottom panel (docs/web-client.md): tasks, inbox, policy — the docked
// home for what is neither transcript nor composer. Collapsed it is the tab
// strip; a tab opens its pane. The tasks pane is the rendering surface for
// background-process output, which previously folded into the store and
// never rendered anywhere.

import { useEffect, useRef, useState } from "react";
import {
  dismissNotification,
  fetchPushKey,
  killProcess,
  patchProcess,
  resolveProposal,
  subscribePush,
  unsubscribePush,
} from "../api/http.ts";
import type {
  PendingNotification,
  Process,
  Proposal,
  Role,
} from "../api/types.ts";
import { policyRows } from "./policy.ts";
import { setThemeChoice, themeChoice, type ThemeChoice } from "./theme.ts";
import {
  setViewSetting,
  useViewSettings,
  type ViewSettings,
} from "./viewSettings.ts";

type Tab = "tasks" | "inbox" | "policy" | "interface";
const TABS: Tab[] = ["tasks", "inbox", "policy", "interface"];

const KEY = "panel";

/** Re-render once a second while `active`, for the elapsed clocks. */
function useTick(active: boolean) {
  const [, setTick] = useState(0);
  useEffect(() => {
    if (!active) return;
    const id = setInterval(() => setTick((t) => t + 1), 1000);
    return () => clearInterval(id);
  }, [active]);
}

const elapsed = (spawnedAt: number) => {
  const s = Math.max(0, Math.floor((Date.now() - spawnedAt) / 1000));
  const m = Math.floor(s / 60);
  return m >= 60
    ? `${Math.floor(m / 60)}h${(m % 60).toString().padStart(2, "0")}m`
    : `${m}:${(s % 60).toString().padStart(2, "0")}`;
};

/** The live tail, pinned to its bottom: it is a tail, so new output keeps
 * the newest line visible (the mock's tailRef behaviour). */
function Tail({ text }: { text: string }) {
  const ref = useRef<HTMLPreElement>(null);
  useEffect(() => {
    const el = ref.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [text]);
  return (
    <pre ref={ref} className="tail dim">
      {text}
    </pre>
  );
}

function Tasks({
  processes,
  output,
  lost,
  act,
}: {
  processes: Process[];
  output: Record<number, string>;
  lost: Record<number, number>;
  act: (call: Promise<unknown>) => void;
}) {
  const [picked, setPicked] = useState<number | null>(null);
  // One note draft suffices: only the single foreground process offers it.
  const [note, setNote] = useState("");
  useTick(processes.length > 0);
  const shown =
    processes.find((p) => p.id === picked) ??
    processes.find((p) => !p.background) ??
    processes.at(0) ??
    null;
  if (shown === null) {
    return <div className="panel-empty dim">no processes</div>;
  }
  return (
    <div className="panel-body tasks">
      <div className="proc-list">
        {processes.map((proc) => (
          <button
            key={proc.id}
            className={proc.id === shown.id ? "proc-row picked" : "proc-row"}
            onClick={() => setPicked(proc.id)}
          >
            <span className="proc-name">
              <span className={proc.background ? "dot idle" : "dot"} />
              {proc.command}
            </span>
            <span className="proc-meta dim">
              pid {proc.pid} · {elapsed(proc.spawned_at)}
              {proc.muted ? " · muted" : ""}
            </span>
          </button>
        ))}
      </div>
      <div className="proc-detail">
        <div className="proc-head">
          <span className="proc-command">
            <span className="prompt">$</span> {shown.command}
          </span>
          <span className="spacer" />
          {!shown.background && (
            <>
              <input
                value={note}
                onChange={(e) => setNote(e.target.value)}
                placeholder="note to ride the handoff"
              />
              <button
                onClick={() =>
                  act(
                    patchProcess(shown.id, {
                      background: true,
                      note: note.trim() || undefined,
                    }),
                  )
                }
              >
                background
              </button>
            </>
          )}
          <button
            onClick={() => act(patchProcess(shown.id, { muted: !shown.muted }))}
          >
            {shown.muted ? "unmute" : "mute"}
          </button>
          <button onClick={() => act(killProcess(shown.id))}>kill</button>
        </div>
        {/* Bytes the supervisor dropped at its retention cap before the seed
            read. Saying so is the point: a tail that silently starts partway
            through reads as the whole of it. */}
        {(lost[shown.id] ?? 0) > 0 && (
          <div className="tail-lost dim">
            ⋯ {lost[shown.id]} earlier bytes dropped at the retention cap
          </div>
        )}
        <Tail text={output[shown.id] ?? ""} />
      </div>
    </div>
  );
}

function Inbox({
  notifications,
  act,
}: {
  notifications: PendingNotification[];
  act: (call: Promise<unknown>) => void;
}) {
  if (notifications.length === 0) {
    // On an idle loop the inbox delivers immediately; rows only exist while
    // a tool is mid-run.
    return <div className="panel-empty dim">nothing pending</div>;
  }
  return (
    <div className="panel-body inbox">
      {notifications.map((n) => (
        <div className="inbox-row" key={n.id}>
          <span>
            <span className="ambient-source">{n.source}</span>
            {n.slot !== null && <span className="dim"> [{n.slot}]</span>} —{" "}
            {n.body}
          </span>
          <button onClick={() => act(dismissNotification(n.id))}>
            dismiss
          </button>
        </div>
      ))}
    </div>
  );
}

function Policy({
  role,
  proposals,
  act,
}: {
  role: Role | null;
  proposals: Proposal[];
  act: (call: Promise<unknown>) => void;
}) {
  const rows = policyRows(role, proposals);
  if (rows.length === 0) {
    return <div className="panel-empty dim">no role set</div>;
  }
  return (
    <div className="panel-body policy">
      {rows.map((row, i) => {
        const head = i === 0 || rows[i - 1].section !== row.section;
        // A `clear` stance reads as a grant, the way a bare row always has;
        // `watch`/`gate`/`deny` are carve-outs and must not be mistakable
        // for one, so they get the warn color plus their own word.
        const carveOut = row.stance !== null && row.stance !== "clear";
        return (
          <div className="policy-row" key={i}>
            <span className="policy-section">{head ? row.section : ""}</span>
            <span
              className={
                carveOut
                  ? "policy-carve-out"
                  : row.proposal !== null
                    ? "dim"
                    : undefined
              }
            >
              {row.text}
              {carveOut && ` [${row.stance}]`}
              {row.proposal !== null && " ⊕"}
            </span>
            {row.proposal !== null && (
              <span className="policy-verbs">
                <button
                  onClick={() =>
                    act(resolveProposal(row.proposal!, "committed"))
                  }
                >
                  commit
                </button>
                <button
                  onClick={() =>
                    act(resolveProposal(row.proposal!, "cancelled"))
                  }
                >
                  cancel
                </button>
              </span>
            )}
          </div>
        );
      })}
    </div>
  );
}

/** base64url → the raw bytes `applicationServerKey` takes. The daemon answers
 * the VAPID key in the encoding the RFC spells it in; `subscribe` wants the
 * point itself. */
function decodeKey(base64url: string): Uint8Array<ArrayBuffer> {
  const padded = base64url
    .replace(/-/g, "+")
    .replace(/_/g, "/")
    .padEnd(Math.ceil(base64url.length / 4) * 4, "=");
  const raw = atob(padded);
  const bytes = new Uint8Array(new ArrayBuffer(raw.length));
  for (let i = 0; i < raw.length; i += 1) bytes[i] = raw.charCodeAt(i);
  return bytes;
}

/** Why the notifications checkbox cannot be used, if it cannot. `null` means it
 * can. Rendered rather than hidden: a checkbox that silently does nothing is
 * worse than one that says why (docs/web-push.md). */
function pushObstacle(): string | null {
  if (Notification.permission === "denied") {
    // JS cannot re-prompt once denied; only the browser's own settings can
    // undo it, so the checkbox says where to go rather than offering a click
    // that would do nothing.
    return "blocked in browser settings";
  }
  // iOS delivers push only to the home-screen-installed app, so a subscribe
  // from a Safari tab fails at `requestPermission` with nothing to show for it.
  const ios = /iPad|iPhone|iPod/.test(navigator.userAgent);
  const standalone = window.matchMedia("(display-mode: standalone)").matches;
  if (ios && !standalone) {
    return "add to home screen first — iOS notifies only the installed app";
  }
  return null;
}

/** The "receive notifications" opt-in (docs/web-push.md).
 *
 * The checkbox reflects `pushManager.getSubscription()` rather than a stored
 * preference: the browser is the source of truth about whether this device is
 * subscribed, and a preference could disagree with it after a permission reset
 * or a cleared site. The prompt and the subscribe run only from the click,
 * which is what Safari requires and what Chrome quiet-prompts without. */
function Notifications() {
  const [subscribed, setSubscribed] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    void navigator.serviceWorker.ready
      .then((registration) => registration.pushManager.getSubscription())
      .then((subscription) => {
        if (live) setSubscribed(subscription !== null);
      })
      .catch(() => {
        if (live) setSubscribed(false);
      });
    return () => {
      live = false;
    };
  }, []);

  const obstacle = pushObstacle();

  const toggle = async (wanted: boolean) => {
    setBusy(true);
    setError(null);
    try {
      const registration = await navigator.serviceWorker.ready;
      if (wanted) {
        const permission = await Notification.requestPermission();
        if (permission !== "granted") {
          setError("permission was not granted");
          return;
        }
        const { key } = await fetchPushKey();
        const subscription = await registration.pushManager.subscribe({
          // Required by every platform: a push must produce a visible
          // notification, which is also all this harness sends push for.
          userVisibleOnly: true,
          applicationServerKey: decodeKey(key),
        });
        await subscribePush(subscription.toJSON());
        setSubscribed(true);
      } else {
        // Both sides, in the order that leaves nothing sending to a dead
        // endpoint: the daemon forgets it, then the browser drops it.
        const subscription = await registration.pushManager.getSubscription();
        if (subscription) {
          await unsubscribePush(subscription.endpoint);
          await subscription.unsubscribe();
        }
        setSubscribed(false);
      }
    } catch (e) {
      setError(`${e}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="push-toggle">
      <label>
        <input
          type="checkbox"
          checked={subscribed === true}
          disabled={busy || obstacle !== null || subscribed === null}
          onChange={(event) => void toggle(event.target.checked)}
        />
        receive notifications
      </label>
      {obstacle !== null && <span className="dim"> — {obstacle}</span>}
      {error !== null && <span className="error"> — {error}</span>}
    </div>
  );
}

const VIEW_LABELS: Record<keyof ViewSettings, string> = {
  thinking: "thinking",
  clearSpots: "clear spots",
  ambient: "ambient",
};

/** Interface preferences: the theme, and the transcript density switches.
 * Buried here on purpose — none of these is an affordance used *during* a
 * session, so they do not merit constant display (jbr, 2026-08-19). Hiding
 * via the switches is display-only; the entries stay in the store. */
function Interface() {
  const settings = useViewSettings();
  const [choice, setChoice] = useState(themeChoice);
  return (
    <div className="panel-body interface">
      <label className="dim">
        theme{" "}
        <select
          value={choice}
          onChange={(event) => {
            const next = event.target.value as ThemeChoice;
            setThemeChoice(next);
            setChoice(next);
          }}
        >
          <option value="system">system</option>
          <option value="after-dark">after dark</option>
          <option value="daylight">daylight</option>
        </select>
      </label>
      <div className="view-toggles">
        show:
        {(Object.keys(VIEW_LABELS) as (keyof ViewSettings)[]).map((key) => (
          <label key={key}>
            <input
              type="checkbox"
              checked={settings[key]}
              onChange={(event) => setViewSetting(key, event.target.checked)}
            />
            {VIEW_LABELS[key]}
          </label>
        ))}
      </div>
      {/* Hidden entirely where the browser has no push at all — there is
          nothing to explain, and an obstacle notice implies a path forward. */}
      {"PushManager" in window && <Notifications />}
    </div>
  );
}

export function Panel({
  processes,
  notifications,
  role,
  proposals,
  processOutput,
  processLost,
  quiet,
}: {
  processes: Process[];
  notifications: PendingNotification[];
  role: Role | null;
  proposals: Proposal[];
  processOutput: Record<number, string>;
  processLost: Record<number, number>;
  /** Narrow-viewport chrome quieting (docs/web-client.md "The phone
   * breakpoint"): while true and nothing demands the strip — no open pane,
   * no counts — the tab row hides entirely; the status row's ⋯ reveals it. */
  quiet: boolean;
}) {
  const [open, setOpen] = useState<Tab | null>(
    () => (localStorage.getItem(KEY) as Tab | null) || null,
  );
  const [error, setError] = useState<string | null>(null);
  const act = (call: Promise<unknown>) =>
    void call.then(() => setError(null)).catch((e) => setError(`${e}`));
  const pick = (tab: Tab) => {
    const next = open === tab ? null : tab;
    setOpen(next);
    if (next === null) localStorage.removeItem(KEY);
    else localStorage.setItem(KEY, next);
  };
  const counts: Record<Tab, number> = {
    tasks: processes.length,
    inbox: notifications.length,
    policy: proposals.length,
    interface: 0,
  };
  const anyCount = TABS.some((tab) => counts[tab] > 0);
  if (quiet && open === null && !anyCount) return null;
  return (
    <div className="panel">
      {/* The tab strip bleeds full width like the rest of the chrome; the
          tabs themselves sit in the content column. */}
      <div className="panel-tabs">
        <div className="panel-tabs-inner">
          {TABS.map((tab) => (
            <button
              key={tab}
              className={open === tab ? "panel-tab open" : "panel-tab"}
              onClick={() => pick(tab)}
            >
              {tab}
              {counts[tab] > 0 && <span className="chip">{counts[tab]}</span>}
            </button>
          ))}
          <span className="spacer" />
          {open !== null && (
            <button className="panel-tab" onClick={() => pick(open)}>
              collapse
            </button>
          )}
        </div>
      </div>
      <div className="col">
        {error && <div className="error">{error}</div>}
        {open === "tasks" && (
          <Tasks
            processes={processes}
            output={processOutput}
            lost={processLost}
            act={act}
          />
        )}
        {open === "inbox" && <Inbox notifications={notifications} act={act} />}
        {open === "policy" && (
          <Policy role={role} proposals={proposals} act={act} />
        )}
        {open === "interface" && <Interface />}
      </div>
    </div>
  );
}
