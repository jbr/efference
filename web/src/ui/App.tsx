import { useCallback, useMemo, useState } from "react";
import type { CallId } from "../api/types.ts";
import { spotFor } from "../session/store.ts";
import { buildTree, type CallNode } from "../session/tree.ts";
import { useSession } from "../session/useSession.ts";
import { Composer } from "./Composer.tsx";
import { Controls } from "./Controls.tsx";
import { GateSurface } from "./Gate.tsx";
import { Inspector } from "./Inspector.tsx";
import { Login } from "./Login.tsx";
import { Panel } from "./Panel.tsx";
import { setThemeChoice, themeChoice, type ThemeChoice } from "./theme.ts";
import { Transcript } from "./Transcript.tsx";
import {
  setViewSetting,
  useViewSettings,
  type ViewSettings,
} from "./viewSettings.ts";

const VIEW_LABELS: Record<keyof ViewSettings, string> = {
  thinking: "thinking",
  clearSpots: "clear spots",
  ambient: "ambient",
};

/** The transcript density switches (docs/web-client.md). Hiding is
 * display-only; the entries stay in the store. */
function ViewToggles() {
  const settings = useViewSettings();
  return (
    <div className="view-toggles">
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
  );
}

function ThemeSelect() {
  const [choice, setChoice] = useState(themeChoice);
  return (
    <select
      aria-label="theme"
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
  );
}

export function App() {
  const { session, connector, actions } = useSession();
  // One projection, shared: the transcript renders it, the inspector reads
  // the selected call out of it. The selection is a call id, not a node —
  // the node is re-found each render so a landing result or verdict shows
  // up in an open inspector.
  const nodes = useMemo(() => buildTree(session.entries), [session.entries]);
  const [inspecting, setInspecting] = useState<CallId | null>(null);
  const onInspect = useCallback((id: CallId) => setInspecting(id), []);
  const inspected = useMemo<CallNode | null>(() => {
    if (inspecting === null) return null;
    for (const node of nodes) {
      if (node.kind !== "batch") continue;
      const call = node.calls.find((c) => c.callId === inspecting);
      if (call !== undefined) return call;
    }
    return null;
  }, [nodes, inspecting]);

  if (session.connection === "unauthenticated") {
    return <Login onAuthenticated={() => connector.retry()} />;
  }

  const gate = session.topics.gate;
  return (
    <div className="app">
      <header>
        <span>efference</span>
        <span className="dim">
          {session.connection}
          {session.topics.running ? " · running" : ""}
          {gate ? " · gate pending" : ""}
        </span>
        <ViewToggles />
        <ThemeSelect />
      </header>
      {session.connection !== "live" && (
        <div className="banner">{session.connection}…</div>
      )}
      <Controls usage={session.topics.usage ?? null} />
      <Transcript
        nodes={nodes}
        streaming={session.streaming}
        pending={session.pendingTurns}
        onInspect={onInspect}
      />
      {inspected !== null && (
        <Inspector node={inspected} onClose={() => setInspecting(null)} />
      )}
      {gate && (
        <GateSurface
          gate={gate}
          spot={spotFor(session.entries, gate.call_id)}
          resolve={(decision) => actions.resolveGate(gate.call_id, decision)}
        />
      )}
      <div className="dock">
        <Panel
          processes={session.topics.processes ?? []}
          notifications={session.topics.notifications ?? []}
          role={session.topics.role ?? null}
          proposals={session.topics.proposals ?? []}
          processOutput={session.streaming.processOutput}
        />
        <Composer
          submit={actions.submitTurn}
          pace={session.topics.pace ?? null}
          reviewLevel={session.topics.review_level ?? null}
          running={session.topics.running ?? false}
        />
      </div>
    </div>
  );
}
