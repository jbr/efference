// The bottom panel (docs/web-client.md): tasks, inbox, policy — the docked
// home for what is neither transcript nor composer. Collapsed it is the tab
// strip; a tab opens its pane. The tasks pane is the rendering surface for
// background-process output, which previously folded into the store and
// never rendered anywhere.

import { useEffect, useRef, useState } from "react";
import {
  dismissNotification,
  killProcess,
  patchProcess,
  resolveProposal,
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
  act,
}: {
  processes: Process[];
  output: Record<number, string>;
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
        return (
          <div className="policy-row" key={i}>
            <span className="policy-section">{head ? row.section : ""}</span>
            <span className={row.proposal !== null ? "dim" : undefined}>
              {row.text}
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
    </div>
  );
}

export function Panel({
  processes,
  notifications,
  role,
  proposals,
  processOutput,
  quiet,
}: {
  processes: Process[];
  notifications: PendingNotification[];
  role: Role | null;
  proposals: Proposal[];
  processOutput: Record<number, string>;
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
          <Tasks processes={processes} output={processOutput} act={act} />
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
