import { useCallback, useEffect, useMemo, useState } from "react";
import type { CallId } from "../api/types.ts";
import { spotFor } from "../session/store.ts";
import { buildTree, type CallNode } from "../session/tree.ts";
import { useSession } from "../session/useSession.ts";
import { Composer } from "./Composer.tsx";
import { GateSurface } from "./Gate.tsx";
import { Inspector } from "./Inspector.tsx";
import { Login } from "./Login.tsx";
import { Panel } from "./Panel.tsx";
import { TopBar } from "./TopBar.tsx";
import { Transcript } from "./Transcript.tsx";

export function App() {
  const { session, connector, actions } = useSession();
  // One projection, shared: the transcript renders it, the inspector reads
  // the selected call out of it. The selection is a call id, not a node —
  // the node is re-found each render so a landing result or verdict shows
  // up in an open inspector.
  const nodes = useMemo(() => buildTree(session.entries), [session.entries]);
  const [inspecting, setInspecting] = useState<CallId | null>(null);
  // Clicking the call being inspected closes the drawer: the call is a huge
  // central target where the ✕ is a distant corner one (Fitts), and a second
  // click on "show me this" reading as "stop showing me this" is the toggle
  // idiom. A different call switches without closing.
  const onInspect = useCallback(
    (id: CallId) => setInspecting((prev) => (prev === id ? null : id)),
    [],
  );
  // The drawer's slide-out needs it mounted after `inspecting` clears —
  // React unmounts before CSS can animate — so `shown` holds the last call
  // id through the exit transition. `done` unmounts on `transitionend`, with
  // a timeout fallback (transitionend does not fire in a hidden tab);
  // reopening mid-slide cancels the pending unmount by making `closing`
  // false again. Entry needs no state: `@starting-style` animates the mount
  // frame (index.css).
  const [shown, setShown] = useState<CallId | null>(null);
  if (inspecting !== null && inspecting !== shown) setShown(inspecting);
  const closing = inspecting === null && shown !== null;
  const done = useCallback(() => setShown(null), []);
  useEffect(() => {
    if (!closing) return;
    const id = setTimeout(done, 350);
    return () => clearTimeout(id);
  }, [closing, done]);

  const inspected = useMemo<CallNode | null>(() => {
    if (shown === null) return null;
    for (const node of nodes) {
      if (node.kind !== "batch") continue;
      const call = node.calls.find((c) => c.callId === shown);
      if (call !== undefined) return call;
    }
    return null;
  }, [nodes, shown]);

  if (session.connection === "unauthenticated") {
    return <Login onAuthenticated={() => connector.retry()} />;
  }

  const gate = session.topics.gate;
  return (
    <div className="app">
      <TopBar
        running={session.topics.running ?? false}
        connection={session.connection}
        usage={session.topics.usage ?? null}
      />
      {session.connection !== "live" && (
        <div className="banner">
          <div className="col">{session.connection}…</div>
        </div>
      )}
      <Transcript
        nodes={nodes}
        streaming={session.streaming}
        pending={session.pendingTurns}
        onInspect={onInspect}
      />
      {inspected !== null && (
        <Inspector
          node={inspected}
          closing={closing}
          onSlideOut={done}
          onClose={() => setInspecting(null)}
        />
      )}
      {gate && (
        <div className="col">
          <GateSurface
            gate={gate}
            spot={spotFor(session.entries, gate.call_id)}
            resolve={(decision) => actions.resolveGate(gate.call_id, decision)}
          />
        </div>
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
