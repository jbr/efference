// One session, whole (docs/web-client.md): the transcript, the composer and
// dock, the gate surface, and the two overlays. Everything it fetches goes
// through the client's base (api/base.ts), so this is the same view whether the
// page was served by a session daemon at its root or by the host under
// `/sessions/<id>/`; `App.tsx` decides which of the two it is.

import { useCallback, useEffect, useMemo, useState } from "react";
import { apiBase, clientRoute, sessionHref, sessionId } from "../api/base.ts";
import type { CallId } from "../api/types.ts";
import { spotFor } from "../session/store.ts";
import {
  anchorForCall,
  fileActivity,
  lastEdit,
} from "../session/touchedFiles.ts";
import { buildTree, type CallNode } from "../session/tree.ts";
import { useSession } from "../session/useSession.ts";
import { Composer, type Recalled } from "./Composer.tsx";
import { sessionDocumentTitle, useDocumentTitle } from "./documentTitle.ts";
import { Drawer } from "./Drawer.tsx";
import { drawerAttention } from "./drawerAttention.ts";
import { FileView } from "./FileView.tsx";
import { GateSurface } from "./Gate.tsx";
import { sessionName, successorHref } from "./identity.ts";
import { Inspector } from "./Inspector.tsx";
import { JumpToLatest } from "./JumpToLatest.tsx";
import { LightboxHost } from "./Lightbox.tsx";
import { closeLightbox, useLightboxImage } from "./lightbox.ts";
import { Login } from "./Login.tsx";
import { useOverlayHistory } from "./overlayHistory.ts";
import { navigate } from "./route.ts";
import { TopBar } from "./TopBar.tsx";
import { Transcript } from "./Transcript.tsx";

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
  useDocumentTitle(
    sessionDocumentTitle(
      sessionName(session.topics.session_identity, sessionId()),
    ),
  );
  useEffect(() => {
    if (import.meta.env.DEV) {
      document.dispatchEvent(
        new CustomEvent("efference:stream-render", {
          detail: { base: apiBase(), seq: session.lastSeq },
        }),
      );
    }
  }, [session]);
  // One projection, shared: the transcript renders it, the inspector reads
  // the selected call out of it. The selection is a call id, not a node —
  // the node is re-found each render so a landing result or verdict shows
  // up in an open inspector.
  const nodes = useMemo(() => buildTree(session.entries), [session.entries]);
  // A session that handed off (docs/tools.md) is followed to its successor.
  // `replace`, so Back does not return to the archived predecessor.
  const follow = successorHref(session.topics.session_identity, sessionHref);
  useEffect(() => {
    if (follow !== null) navigate(follow, "replace");
    else if (session.connection === "missing" && apiBase() !== "") {
      navigate("/", "replace");
    }
  }, [follow, session.connection]);
  // The gated call's slot in the transcript, where the gate's decision
  // renders (ui/Gate.tsx); null until that call is on screen.
  const [gateSlot, setGateSlot] = useState<HTMLDivElement | null>(null);
  // The session drawer (ui/Drawer.tsx): open/shown/closing is the
  // inspector's slide pattern — `drawerShown` keeps it mounted through the
  // exit transition, with the same timeout fallback for a hidden tab.
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [drawerShown, setDrawerShown] = useState(false);
  if (drawerOpen && !drawerShown) setDrawerShown(true);
  const drawerClosing = !drawerOpen && drawerShown;
  const drawerDone = useCallback(() => setDrawerShown(false), []);
  useEffect(() => {
    if (!drawerClosing) return;
    const id = setTimeout(drawerDone, 350);
    return () => clearTimeout(id);
  }, [drawerClosing, drawerDone]);
  const [inspecting, setInspecting] = useState<CallId | null>(null);
  // Clicking the call being inspected closes the drawer: the call is a huge
  // central target where the ✕ is a distant corner one (Fitts), and a second
  // click on "show me this" reading as "stop showing me this" is the toggle
  // idiom. A different call switches without closing.
  const onInspect = useCallback((id: CallId) => {
    setDrawerOpen(false);
    setInspecting((prev) => (prev === id ? null : id));
  }, []);
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
  // What a recall handed back, for the composer to prepend (docs/web-client.md). The
  // counter is the trigger: recalling the same text twice must still splice twice, and
  // the draft store survives the view, so a splice is an event rather than a value.
  const [recalled, setRecalled] = useState<Recalled | null>(null);
  // Only what the daemon reports it actually popped reaches the composer — a turn the
  // loop already dequeued comes back as an empty list and the draft is left alone.
  const onRecall = useCallback(
    (id: string) => {
      void actions.recallTurn(id).then((turns) => {
        if (turns.length === 0) return;
        setRecalled((prev) => ({
          text: turns.map((t) => t.text).join("\n"),
          attachments: turns.flatMap((t) => t.attachments ?? []),
          nonce: (prev?.nonce ?? 0) + 1,
        }));
      });
    },
    [actions],
  );

  // The turns an `ask` handed back (session/store.ts) reach the composer the way a
  // manual recall does. The store's object is the trigger; the nonce continues the
  // counter above so the composer never sees one twice.
  // Adjusted during render, keyed on the store's object, rather than in an effect.
  const askRecall = session.askRecall;
  const [seenAskRecall, setSeenAskRecall] = useState(askRecall);
  if (askRecall !== seenAskRecall) {
    setSeenAskRecall(askRecall);
    if (askRecall !== null) {
      setRecalled((prev) => ({
        text: askRecall.turns.map((t) => t.text).join("\n"),
        attachments: askRecall.turns.flatMap((t) => t.attachments ?? []),
        nonce: (prev?.nonce ?? 0) + 1,
      }));
    }
  }

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

  // The lightbox (ui/Lightbox.tsx): its state is the module store, so back
  // and edge-swipe dismiss it through the same count as the other overlays.
  const lightboxImage = useLightboxImage();
  // Back/edge-swipe dismisses overlays (ui/overlayHistory.ts). The lightbox
  // closes first — it covers everything — then the file viewer, which renders
  // over the transcript the inspector's sheet also occupies and is always the
  // later-opened of the two here (its entry points are a call's open ⤢ and the
  // inspector footer).
  useOverlayHistory(
    (inspecting !== null ? 1 : 0) +
      (viewingFile !== null ? 1 : 0) +
      (drawerOpen ? 1 : 0) +
      (lightboxImage !== null ? 1 : 0),
    useCallback(() => {
      if (lightboxImage !== null) closeLightbox();
      else if (viewingFile !== null) setViewingFile(null);
      else if (drawerOpen) setDrawerOpen(false);
      else setInspecting(null);
    }, [lightboxImage, viewingFile, drawerOpen]),
  );
  const newestEdit = useMemo(
    () => lastEdit(session.entries),
    [session.entries],
  );

  if (session.connection === "unauthenticated") {
    return <Login onAuthenticated={() => connector.retry()} />;
  }

  const gate = session.topics.gate;
  const notifications = session.topics.notifications ?? [];
  const proposals = session.topics.proposals ?? [];
  const processes = session.topics.processes ?? [];
  return (
    <div className="app">
      <TopBar
        // Only under a host is there a list to go back to — at a session
        // daemon's root this view is the whole app (api/base.ts).
        listHref={apiBase() === "" ? null : "/"}
        identity={session.topics.session_identity ?? null}
        usage={session.topics.usage ?? null}
        upstreamFailure={session.topics.upstream_failure ?? null}
        upstreamRetry={session.topics.upstream_retry ?? null}
        drawerOpen={drawerOpen}
        attention={drawerAttention(notifications, proposals, processes)}
        onDrawer={() => {
          setInspecting(null);
          setDrawerOpen((open) => !open);
        }}
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
        ask={session.topics.ask ?? null}
        gatedCall={gate?.call_id ?? null}
        onGateSlot={setGateSlot}
        onRecall={onRecall}
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
            slot={gateSlot}
            cold={session.topics.cold ?? null}
            resolve={(decision) => actions.resolveGate(gate.call_id, decision)}
          />
        </div>
      )}
      {drawerShown && (
        <Drawer
          closing={drawerClosing}
          onSlideOut={drawerDone}
          onClose={() => setDrawerOpen(false)}
          identity={session.topics.session_identity ?? null}
          usage={session.topics.usage ?? null}
          pace={session.topics.pace ?? null}
          reviewLevel={session.topics.review_level ?? null}
          role={session.topics.role ?? null}
          proposals={proposals}
          processes={processes}
          notifications={notifications}
          cold={session.topics.cold ?? null}
          fold={session.topics.fold ?? null}
          running={session.topics.running ?? false}
          gatePending={gate != null}
          asking={(session.topics.ask ?? null) !== null}
          processOutput={session.streaming.processOutput}
          processLost={session.streaming.processLost}
        />
      )}
      <div className="dock">
        <Composer
          submit={actions.submitTurn}
          recalled={recalled}
          pace={session.topics.pace ?? null}
          running={session.topics.running ?? false}
          stall={session.topics.stall ?? null}
          retrying={(session.topics.upstream_retry ?? null) !== null}
          gatePending={gate != null}
          ask={session.topics.ask ?? null}
          cold={session.topics.cold ?? null}
          park={session.topics.park ?? null}
          fold={session.topics.fold ?? null}
          operations={session.topics.current_operations ?? null}
          model={session.topics.session_identity?.model ?? "the model"}
        />
      </div>
      <LightboxHost />
    </div>
  );
}
