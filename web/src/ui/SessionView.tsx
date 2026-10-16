import { useSidebarOverlay } from "./inspector/useSidebarOverlay.ts";
import { anchorFromResult, type DiffAnchor } from "../session/editAnchor.ts";
import { useHistoricalCall } from "./inspector/useHistoricalCall.ts";
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
import { EarlierHistory } from "./EarlierHistory.tsx";
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
import { TaskStrip } from "./TaskStrip.tsx";

/** One request to show a file: what to show, which call asked (the anchor's
 * source), and a key that changes per request. */
interface OpenRequest {
  path: string | null;
  callId: CallId | null;
  key: number;
  anchor?: DiffAnchor | null;
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
  const { session, connector, actions, history } = useSession();
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
  const [inspecting, setInspecting] = useState<CallId | null>(null);
  // Clicking the call being inspected closes the drawer: the call is a huge
  // central target where the ✕ is a distant corner one (Fitts), and a second
  // click on "show me this" reading as "stop showing me this" is the toggle
  // idiom. A different call switches without closing.
  const [viewingFile, setViewingFile] = useState<OpenRequest | null>(
    bootRoute() === "tree" ? { path: null, callId: null, key: 0 } : null,
  );
  const onInspect = useCallback((id: CallId) => {
    setViewingFile(null);
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

  const loadedInspection = useMemo<CallNode | null>(() => {
    if (shown === null) return null;
    for (const node of nodes) {
      if (node.kind !== "batch") continue;
      const call = node.calls.find((c) => c.callId === shown);
      if (call !== undefined) return call;
    }
    return null;
  }, [nodes, shown]);
  const { node: inspected, error: inspectionError } = useHistoricalCall(
    shown,
    loadedInspection,
    session.entries,
    apiBase(),
  );

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
    (
      path: string,
      callId: CallId | null = null,
      anchor?: DiffAnchor | null,
    ) => {
      if (callId !== null) setInspecting(callId);
      else setInspecting(null);
      setViewingFile((prev) => ({
        path,
        callId,
        anchor,
        key: (prev?.key ?? 0) + 1,
      }));
    },
    [],
  );
  const closeFile = useCallback(() => setViewingFile(null), []);
  const openAnchor = useMemo(() => {
    if (shownFile?.anchor !== undefined) return shownFile.anchor;
    const anchor = anchorForCall(session.entries, shownFile?.callId ?? null);
    if (anchor !== null) return anchor;
    const payload =
      inspected?.callId === shownFile?.callId
        ? inspected?.result?.payload
        : null;
    return payload &&
      "ToolResult" in payload &&
      !payload.ToolResult.raw.is_error
      ? anchorFromResult(payload.ToolResult.raw.content)
      : null;
  }, [session.entries, shownFile, inspected]);
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
      (lightboxImage !== null ? 1 : 0),
    useCallback(() => {
      if (lightboxImage !== null) closeLightbox();
      else if (viewingFile !== null) setViewingFile(null);
      else setInspecting(null);
    }, [lightboxImage, viewingFile]),
  );
  const newestEdit = useMemo(
    () => lastEdit(session.entries),
    [session.entries],
  );

  const overlaySidebar = useSidebarOverlay();
  if (session.connection === "unauthenticated") {
    return <Login onAuthenticated={() => connector.retry()} />;
  }

  const gate = session.topics.gate;
  const processes = session.topics.processes ?? [];
  return (
    <div
      className={
        inspected !== null || shownFile !== null ? "app session-split" : "app"
      }
    >
      <div className="session-main">
        <TopBar
          // Only under a host is there a list to go back to — at a session
          // daemon's root this view is the whole app (api/base.ts).
          listHref={apiBase() === "" ? null : "/"}
          identity={session.topics.session_identity ?? null}
          usage={session.topics.usage ?? null}
          upstreamFailure={session.topics.upstream_failure ?? null}
          upstreamRetry={session.topics.upstream_retry ?? null}
          summary={{
            cold: session.topics.cold ?? null,
            verbs: session.topics.verbs ?? null,
            operations: session.topics.current_operations ?? null,
            failure: session.topics.compaction_failure ?? null,
          }}
        />
        {session.connection !== "live" && (
          <div className="banner">
            <div className="col">{session.connection}…</div>
          </div>
        )}
        {inspectionError && (
          <div className="banner" role="alert">
            Could not inspect task: {inspectionError}
          </div>
        )}
        <EarlierHistory history={history} entries={session.entries} />
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
        {/* Desktop file context is beside the transcript; only an overlay
            sheet hides the jump destination. */}
        <JumpToLatest
          hidden={gate != null || (overlaySidebar && shownFile !== null)}
        />
        {gate && (
          <div className="col">
            <GateSurface
              gate={gate}
              spot={spotFor(session.entries, gate.call_id)}
              covered={overlaySidebar && shownFile !== null}
              slot={gateSlot}
              cold={session.topics.cold ?? null}
              resolve={(decision) =>
                actions.resolveGate(gate.call_id, decision)
              }
            />
          </div>
        )}
        <div className="dock">
          <TaskStrip
            processes={processes}
            selected={inspecting}
            onInspect={(id) => {
              setViewingFile(null);
              setInspecting(id);
            }}
          />
          <Composer
            submit={actions.submitTurn}
            recalled={recalled}
            pace={session.topics.pace ?? null}
            phase={session.topics.loop_phase ?? null}
            verbs={session.topics.verbs ?? null}
            stall={session.topics.stall ?? null}
            retrying={(session.topics.upstream_retry ?? null) !== null}
            gatePending={gate != null}
            ask={session.topics.ask ?? null}
            cold={session.topics.cold ?? null}
            compactionFailure={session.topics.compaction_failure ?? null}
            operations={session.topics.current_operations ?? null}
            model={session.topics.session_identity?.model ?? "the model"}
          />
        </div>
        <LightboxHost />
      </div>
      {shownFile !== null && (
        <FileView
          path={shownFile.path}
          openKey={shownFile.key}
          openAnchor={openAnchor}
          lastEdit={newestEdit}
          activity={activity}
          closing={fileClosing}
          onSlideOut={fileDone}
          onBack={shownFile.callId !== null ? closeFile : undefined}
          onClose={() => {
            closeFile();
            setInspecting(null);
          }}
        />
      )}
      {inspected !== null && (
        <Inspector
          key={inspected.callId}
          hidden={shownFile !== null}
          node={inspected}
          process={processes.find((p) => p.call_id === inspected.callId)}
          closing={closing}
          onSlideOut={done}
          onClose={() => setInspecting(null)}
          onOpenFile={onOpenFile}
          // The three ways a call's own output can be reached when its
          // `ToolResult` is not it (docs/process-monitor.md): the live tail while it
          // runs, and — through the call↔process mapping — the process monitor's
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
    </div>
  );
}
