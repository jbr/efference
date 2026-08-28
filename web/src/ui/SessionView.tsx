// One session, whole (docs/web-client.md): the transcript, the composer and
// dock, the gate surface, and the two overlays. Everything it fetches goes
// through the client's base (api/base.ts), so this is the same view whether the
// page was served by a session daemon at its root or by the host under
// `/sessions/<id>/`; `App.tsx` decides which of the two it is.

import { useCallback, useEffect, useMemo, useState } from "react";
import { apiBase, clientRoute } from "../api/base.ts";
import type { CallId } from "../api/types.ts";
import { spotFor } from "../session/store.ts";
import {
  anchorForCall,
  fileActivity,
  lastEdit,
} from "../session/touchedFiles.ts";
import { buildTree, type CallNode } from "../session/tree.ts";
import { useSession } from "../session/useSession.ts";
import { Composer } from "./Composer.tsx";
import { FileView } from "./FileView.tsx";
import { GateSurface } from "./Gate.tsx";
import { Inspector } from "./Inspector.tsx";
import { JumpToLatest } from "./JumpToLatest.tsx";
import { Login } from "./Login.tsx";
import { useOverlayHistory } from "./overlayHistory.ts";
import { Panel } from "./Panel.tsx";
import { TopBar } from "./TopBar.tsx";
import { Transcript } from "./Transcript.tsx";
import { useNarrow } from "./useNarrow.ts";

/** One request to show a file: what to show, which call asked (the anchor's
 * source), and a key that changes per request. */
interface OpenRequest {
  path: string | null;
  callId: CallId | null;
  key: number;
}

/** The client's routing, such as it is: the paths the server's SPA index
 * answers (`SPA_ROUTES`, src/daemon/tcp.rs), read with the base removed so
 * `/tree` and `/sessions/<id>/tree` are one route. `/tree` opens the viewer on
 * the working directory with no file chosen — the entry point for reaching a
 * file no call has named, and the one worth a link of its own. Read once at
 * mount; the overlays own history from there (ui/overlayHistory.ts). */
function bootRoute(): "session" | "tree" {
  return clientRoute() === "/tree" ? "tree" : "session";
}

export function SessionView() {
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

  // The file viewer (FileView.tsx): opened by a call's `open ⤢`, dismissed
  // like the inspector — `shownFile` holds the seed path through the exit
  // slide. Following moves the viewer's own path internally; this is only
  // where it starts.
  // Narrow chrome quieting: on a phone every non-transcript pixel is
  // clutter, so the dials and (empty) tab strip hide behind the status
  // row's ⋯ until revealed. Ephemeral by design — a reveal is a moment,
  // not a mode worth persisting.
  const narrow = useNarrow();
  const [chromeOpen, setChromeOpen] = useState(false);

  // One open request: the path, the call that asked (which is where the edit's
  // anchor comes from — session/editAnchor.ts), and a key that changes even
  // when the path does not, so a second call's `open ⤢` on the same file still
  // lands on *its* edit. A `null` path is the tree with nothing chosen, which
  // is what `/tree` opens.
  const [viewingFile, setViewingFile] = useState<OpenRequest | null>(
    bootRoute() === "tree" ? { path: null, callId: null, key: 0 } : null,
  );
  const [shownFile, setShownFile] = useState<OpenRequest | null>(viewingFile);
  if (viewingFile !== null && viewingFile !== shownFile)
    setShownFile(viewingFile);
  const fileClosing = viewingFile === null && shownFile !== null;
  const fileDone = useCallback(() => setShownFile(null), []);
  useEffect(() => {
    if (!fileClosing) return;
    const id = setTimeout(fileDone, 350);
    return () => clearTimeout(id);
  }, [fileClosing, fileDone]);
  const onOpenFile = useCallback(
    (path: string, callId: CallId | null = null) =>
      setViewingFile((prev) => ({ path, callId, key: (prev?.key ?? 0) + 1 })),
    [],
  );
  const closeFile = useCallback(() => setViewingFile(null), []);
  const openAnchor = useMemo(
    () => anchorForCall(session.entries, shownFile?.callId ?? null),
    [session.entries, shownFile],
  );
  const activity = useMemo(
    () => fileActivity(session.entries),
    [session.entries],
  );

  // Back/edge-swipe dismisses overlays (ui/overlayHistory.ts). The file
  // viewer closes first: it renders over the transcript the inspector's
  // sheet also occupies, and it is always the later-opened of the two here
  // (its only entry points are a call's open ⤢ and the inspector footer).
  useOverlayHistory(
    (inspecting !== null ? 1 : 0) + (viewingFile !== null ? 1 : 0),
    useCallback(() => {
      if (viewingFile !== null) setViewingFile(null);
      else setInspecting(null);
    }, [viewingFile]),
  );
  const newestEdit = useMemo(
    () => lastEdit(session.entries),
    [session.entries],
  );

  if (session.connection === "unauthenticated") {
    return <Login onAuthenticated={() => connector.retry()} />;
  }

  const gate = session.topics.gate;
  const quiet = narrow && !chromeOpen;
  return (
    <div className="app">
      <TopBar
        // Only under a host is there a list to go back to — at a session
        // daemon's root this view is the whole app (api/base.ts).
        listHref={apiBase() === "" ? null : "/"}
        running={session.topics.running ?? false}
        connection={session.connection}
        identity={session.topics.session_identity ?? null}
        usage={session.topics.usage ?? null}
        upstreamFailure={session.topics.upstream_failure ?? null}
        upstreamRetry={session.topics.upstream_retry ?? null}
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
        onOpenFile={onOpenFile}
      />
      {inspected !== null && (
        <Inspector
          node={inspected}
          closing={closing}
          onSlideOut={done}
          onClose={() => setInspecting(null)}
          onOpenFile={onOpenFile}
          // The three ways a call's own output can be reached when its
          // `ToolResult` is not it (docs/supervisor.md): the live tail while it
          // runs, and — through the call↔process mapping — the supervisor's
          // capture for a command that was backgrounded.
          liveOutput={session.streaming.toolOutput[inspected.callId]}
          processId={session.processCalls[inspected.callId]}
          processOutput={
            session.streaming.processOutput[
              session.processCalls[inspected.callId]
            ]
          }
          processLost={
            session.streaming.processLost[
              session.processCalls[inspected.callId]
            ]
          }
        />
      )}
      {shownFile !== null && (
        <FileView
          path={shownFile.path}
          openKey={shownFile.key}
          openAnchor={openAnchor}
          lastEdit={newestEdit}
          activity={activity}
          closing={fileClosing}
          onSlideOut={fileDone}
          onClose={closeFile}
        />
      )}
      {/* Hidden while a gate is pending (the gate float/cue owns the
          above-dock slot and carries its own jump) and while the file viewer
          covers the transcript the jump would land in. */}
      <JumpToLatest hidden={gate != null || shownFile !== null} />
      {gate && (
        <div className="col">
          <GateSurface
            gate={gate}
            spot={spotFor(session.entries, gate.call_id)}
            covered={shownFile !== null}
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
          processLost={session.streaming.processLost}
          quiet={quiet}
        />
        <Composer
          submit={actions.submitTurn}
          pace={session.topics.pace ?? null}
          reviewLevel={session.topics.review_level ?? null}
          running={session.topics.running ?? false}
          stall={session.topics.stall ?? null}
          gatePending={gate != null}
          quiet={quiet}
          onReveal={() => setChromeOpen((open) => !open)}
        />
      </div>
    </div>
  );
}
