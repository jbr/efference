import { TriagePreview } from "./SideStreams.tsx";
import type { Streaming } from "../session/store.ts";
// The composer area (docs/web-client.md): where loop state is controlled.
// Gates arrive here, so the dials that decide how the loop moves — pace,
// hold, review level, interrupt — sit on the status row above the input
// rather than in a strip at the top of the page. Submission goes through the
// session action so an accepted turn queues its delivery echo
// (docs/control-api.md — submission is decoupled from inference; a 202 is
// success, the turn's effects arrive on the stream).

import {
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import { apiBase } from "../api/base.ts";
import {
  documentAttachmentNames,
  textDocument,
  withAttachments,
} from "../api/document.ts";
import { acquireDraft } from "./drafts.ts";
import {
  ApiError,
  continueLoop,
  compactLoop,
  interruptLoop,
  patchLoop,
} from "../api/http.ts";
import type {
  AskView,
  Arrival,
  Cold,
  CompactionFailure,
  CurrentOperations,
  LoopPhase,
  NewerVersion,
  PaceView,
  Stall,
  UserContent,
  Verbs,
} from "../api/types.ts";
import {
  ComposerToolbar,
  HoldButton,
  ComposerActionIcon,
  NewerVersionLine,
} from "./ComposerToolbar.tsx";
import { CompactNotice } from "./CompactNotice.tsx";
import { compactOffer } from "./compact.ts";
import { coldLine } from "./coldLine.ts";
import { replyHint } from "./askMark.ts";
import { turnState } from "./turnState.ts";
import {
  ComposerStatus,
  type ComposerStatusMessage,
} from "./ComposerStatus.tsx";
import { useDockOffset } from "./float.ts";
import { useNarrow } from "./useNarrow.ts";
import { useNow } from "./useNow.ts";
import {
  ComposerEditor,
  type ComposerEditorHandle,
} from "./ComposerEditor.tsx";
import { initialDocument } from "./composerCore.tsx";
import {
  appendDraft,
  dedupeDraftUploads,
  draftReadable,
  draftUploadKeys,
  hasDraftStructure,
  wireDocument,
} from "./draftDocument.ts";
import { useDragToDismiss } from "./useDragToDismiss.ts";

/** What a recall handed back: the text to prepend and the attachments to stage
 * again. `nonce` is what makes a repeat splice — recalling the same turn twice is
 * two events, not one value. */
export interface Recalled {
  text: string;
  attachments: Arrival[];
  /** The withdrawn turns' documents merged into one (plain turns as text),
   * or null when none carried references. */
  document: UserContent | null;
  nonce: number;
}

/** Whether a drag carries files (not text or a link being dragged about). */
const dragsFiles = (event: DragEvent) =>
  event.dataTransfer?.types.includes("Files") ?? false;

export function Composer({
  submit: send,
  recalled,
  pace,
  phase,
  verbs,
  stall,
  retrying,
  gatePending,
  ask,
  cold,
  compactionFailure,
  operations,
  model,
  status = null,
  triage = [],
  newerVersion = null,
}: {
  submit: (
    text: string,
    attachments: Arrival[],
    document?: UserContent | null,
  ) => Promise<void>;
  /** A turn a recall pulled back out of the daemon's queue, to prepend to the
   * draft (docs/web-client.md). */
  recalled: Recalled | null;
  pace: PaceView | null;
  /** The `loop_phase` topic (docs/control-api.md): where the loop is. */
  phase: LoopPhase | null;
  /** The `verbs` topic: what the continue and compact routes would answer now,
   * so a refusal is stated without a round trip. */
  verbs: Verbs | null;
  /** The trajectory gave up rather than finished (docs/transport.md): the
   * turn-state readout says so, since the idle phase alone cannot. */
  stall: Stall | null;
  /** A retry of the upstream is waiting (docs/transport.md): an empty send
   * retries now, and the readout says so. */
  retrying: boolean;
  /** A gate float may own the above-dock slot; the compose chip yields to
   * it (the float carries its own steering input). */
  gatePending: boolean;
  /** The pending `ask`: the placeholder says which question a send answers. */
  ask: Pick<AskView, "n" | "of"> | null;
  /** The `cold` and `compaction_failure` topics (docs/web-client.md): a cold session
   * states what a send reads and offers a compaction beside it, and a compaction's
   * failure shows. */
  cold: Cold | null;
  compactionFailure: CompactionFailure | null;
  /** The `current_operations` topic and the session's model: what the status
   * bar says the loop is waiting on, in place of a bare "running", and a
   * compaction in flight. */
  operations: CurrentOperations | null;
  model: string;
  /** Session startup, connection, or inspection condition occupying the readout. */
  status?: ComposerStatusMessage | null;
  triage?: Streaming["triage"];
  /** The `newer_version` topic (docs/update-check.md): a release newer than the
   * host, stated beside the turn readout. */
  newerVersion?: NewerVersion | null;
}) {
  const base = apiBase();
  const store = useMemo(() => acquireDraft(base), [base]);
  const snapshot = useSyncExternalStore(store.subscribe, store.snapshot);
  const draft = snapshot.text;
  const running = phase !== null && phase.phase !== "idle";
  const [error, setError] = useState<string | null>(null);
  const [holdPending, setHoldPending] = useState(false);
  const uploads = snapshot.uploads;
  // A send waits for the files the draft still shows; a pin is not visible
  // work (the send owns those bytes now) and a detached record has no chip.
  const liveUploads = new Set(draftUploadKeys(snapshot.document));
  const uploading = uploads.filter(
    (upload) => liveUploads.has(upload.key) && upload.status === "uploading",
  ).length;
  // A file dragged anywhere over the page is headed here: the composer is the
  // only drop target, so the whole window accepts it (docs/web-client.md).
  const [dragging, setDragging] = useState(false);
  const picker = useRef<HTMLInputElement>(null);
  const narrow = useNarrow();
  // On narrow the composer band collapses when not in use — the dock is a
  // third of a phone screen otherwise — and the floating compose chip
  // summons it (docs/web-client.md "The phone breakpoint"). The draft
  // survives collapse; only the band's visibility changes.
  const [expanded, setExpanded] = useState(false);
  /** Bumped after a send that cleared the draft: a new editor starts with a
   * fresh undo history, so the cleared content cannot be undone back into
   * uploads the send just released (setContent keeps history). */
  const [editorEpoch, setEditorEpoch] = useState(0);
  const editorSurface = useRef<ComposerEditorHandle>(null);
  const fabBottom = useDockOffset();
  const collapsed = narrow && !expanded;
  const { sheet: dragSheet, handlers: dragHandlers } =
    useDragToDismiss<HTMLFormElement>(() => setExpanded(false));
  useEffect(() => {
    if (narrow && expanded) editorSurface.current?.focus();
  }, [narrow, expanded]);
  // Splice a recall in ahead of whatever is already drafted, joined with a newline —
  // adjacent pending turns are separate blocks on the wire, so popping several into one
  // buffer has to invent a join (docs/web-client.md). Keyed on the nonce so recalling
  // the same text twice splices twice; the draft is uncommitted, so nothing here touches
  // the log or the cached prefix.
  const spliced = useRef<number | null>(null);
  useEffect(() => {
    if (!recalled || spliced.current === recalled.nonce) return;
    spliced.current = recalled.nonce;
    const before = store.snapshot();
    const retained = before.document;
    const recalledDocument = recalled.document ?? textDocument(recalled.text);
    // The bytes a recall names may be ones this draft already uploaded under
    // the same stored name. The recalled turn carries them from now on, so
    // their records become sent-owned and no teardown may delete them.
    const recalledNames = new Set([
      ...recalled.attachments.map((arrival) => arrival.name),
      ...documentAttachmentNames(recalledDocument),
    ]);
    store.retain(
      before.uploads.flatMap((upload) =>
        upload.status === "ready" &&
        upload.attachment !== undefined &&
        recalledNames.has(upload.attachment.name)
          ? [upload.key]
          : [],
      ),
    );
    // The withdrawn turns are one document: their documents merged, and the
    // arrivals that are not already an occurrence in it appended — after the
    // recalled text and before the retained draft. A recalled attachment is
    // the daemon's file, so its occurrence is an inline attachment reference,
    // not a draft-owned upload; the store owns no recalled arrival.
    const recalledContent = withAttachments(
      recalledDocument,
      recalled.attachments,
    );
    // A plain recalled turn is itself a document, so its text cannot fall out
    // of a structured merge; any references left in the merge stay the
    // editor's authoritative content — recall no longer refuses an edit. A
    // joined plain-text merge stays a plain draft (no document), and a
    // retained upload occurrence keeps its key through the append.
    const merged = dedupeDraftUploads(
      appendDraft(
        recalledContent,
        retained ?? textDocument(before.text),
        before.text !== "" ? "\n" : "",
      ),
      before.uploads,
    );
    const document = hasDraftStructure(merged) ? merged : null;
    store.setContent({ text: draftReadable(merged), document });
    editorSurface.current?.replace(
      document ?? textDocument(draftReadable(merged)),
    );
    setExpanded(true);
    editorSurface.current?.focus();
  }, [recalled, store]);
  useEffect(() => {
    // `dragleave` fires on every child boundary crossed, so presence is tracked
    // as a depth rather than a flag.
    let depth = 0;
    const enter = (event: DragEvent) => {
      if (!dragsFiles(event)) return;
      depth += 1;
      setDragging(true);
    };
    const leave = (event: DragEvent) => {
      if (!dragsFiles(event)) return;
      depth = Math.max(0, depth - 1);
      if (depth === 0) setDragging(false);
    };
    // Without a cancelled `dragover` the browser refuses the drop — and then
    // navigates to the file, which would leave the session.
    const over = (event: DragEvent) => {
      if (dragsFiles(event)) event.preventDefault();
    };
    const drop = (event: DragEvent) => {
      if (!dragsFiles(event)) return;
      // Every recognized file drop ends the page's drag state, including one
      // the editor already consumed at the pointer; otherwise the overlay
      // stays up after the files are in.
      depth = 0;
      setDragging(false);
      // A drop the editor placed at the pointer was cancelled there; this
      // listener only handles the rest of the page, where the retained caret
      // is the insertion point.
      if (event.defaultPrevented) return;
      event.preventDefault();
      const files = [...(event.dataTransfer?.files ?? [])];
      if (files.length === 0) return;
      editorSurface.current?.insertFiles(files);
      setExpanded(true);
      editorSurface.current?.focus();
    };
    window.addEventListener("dragenter", enter);
    window.addEventListener("dragleave", leave);
    window.addEventListener("dragover", over);
    window.addEventListener("drop", drop);
    return () => {
      window.removeEventListener("dragenter", enter);
      window.removeEventListener("dragleave", leave);
      window.removeEventListener("dragover", over);
      window.removeEventListener("drop", drop);
    };
  }, []);

  const act = (call: Promise<unknown>) =>
    void call.then(() => setError(null)).catch((e) => setError(`${e}`));
  async function changeHold(held: boolean) {
    if (holdPending) return;
    setHoldPending(true);
    try {
      await patchLoop({ held });
      setError(null);
    } catch (e) {
      setError(`${e}`);
    } finally {
      setHoldPending(false);
    }
  }

  // The cold line for the pending question: what answering reads uncached.
  const askColdLine = ask !== null ? coldLine(cold, "your answer") : null;

  async function submit() {
    const draft = store.snapshot();
    // A structured draft submits the document the editor shows; its readable
    // spelling is not trimmed, because a leading or trailing newline is part
    // of the structure.
    const wire = wireDocument(draft.document, draft.uploads);
    // A pending or failed occurrence blocks the send: dropping it would send
    // a document the user is not looking at, and a 400 at the final gate is a
    // worse answer than naming the file here.
    if (wire.blocked.length > 0) {
      setError(
        wire.blocked
          .map((upload) => {
            const reason =
              upload.error === undefined
                ? upload.label
                : `${upload.label}: ${upload.error}`;
            // Only a staged paste has text to insert; a file must be removed
            // and, if it is wanted, attached again.
            const recovery =
              upload.pastedText === undefined || upload.pastedText === ""
                ? "remove it, then send"
                : "remove it or insert it as text, then send";
            return `${reason} — ${recovery}`;
          })
          .join("; "),
      );
      return;
    }
    // An ordinary draft keeps the trimmed plain path; a structured one is not
    // trimmed, because a leading or trailing newline is part of the structure.
    const text = wire.document === null ? draft.text.trim() : draft.text;
    // An empty send hands control back with no input (docs/web-client.md): the
    // loop takes another turn on the wire it already has. A decline is a normal
    // answer — the moment had no continue in it — so its message renders on its
    // own rather than as a thrown-error string: from the `verbs` topic without a
    // round trip, or from the route when the topic had not moved yet.
    // An attachment-only document is structured with no readable text of its own.
    if (text === "" && wire.document === null) {
      const verb = verbs?.continue;
      if (verb !== undefined && !verb.accepted) {
        setError(verb.message ?? null);
        return;
      }
      try {
        await continueLoop();
        if (apiBase() === base) setError(null);
      } catch (e) {
        if (apiBase() === base) {
          setError(e instanceof ApiError ? e.message : `${e}`);
        }
      }
      return;
    }
    // Pin every upload this send names before the request leaves, so a store
    // that is torn down (or a session switched) mid-flight cannot clean up
    // bytes the daemon may be reading (docs/web-client.md).
    const submitted = store.beginSend(draft);
    try {
      await send(text, [], wire.document);
      store.sent(submitted);
      // The send outlived this view's session: its store is still the one that
      // submitted, but the editor ref and the expanded flag now belong to
      // whichever session is on screen. Clear the submitted store, then leave
      // the current view alone.
      if (apiBase() !== base) return;
      if (store.snapshot().text === "" && store.snapshot().document === null) {
        store.resetEditorHistory();
        setEditorEpoch((epoch) => epoch + 1);
        requestAnimationFrame(() => {
          if (apiBase() === base) editorSurface.current?.focus();
        });
      }
      setError(null);
      setExpanded(false);
    } catch (e) {
      // The store that submitted releases its pins even when the view has
      // moved on. A failed send may still have been admitted (the response can
      // be lost), so its uploads are retained and the draft is not cleared:
      // the user can retry, edit, or empty it themselves.
      store.sendFailed(submitted);
      if (apiBase() !== base) return;
      setError(e instanceof Error ? e.message : `${e}`);
    }
  }

  // The compaction is the alternative to the uncached read (docs/web-client.md); its
  // progress arrives on `current_operations`, so success changes nothing here.
  const offer = compactOffer(verbs, operations, cold);
  // The ask and the gate carry their own cold lines; the composer's notice is for
  // a send that opens a turn, or a compaction's progress or failure.
  const noticeShown =
    offer.kind === "writing" ||
    compactionFailure !== null ||
    (cold !== null && ask === null && !gatePending);
  function startCompaction() {
    compactLoop(offer.warm)
      .then(() => setError(null))
      .catch((e) => setError(e instanceof ApiError ? e.message : `${e}`));
  }

  const now = useNow(
    running && (operations !== null || phase?.phase === "delayed"),
  );
  const turn = turnState(phase, {
    stall,
    cold,
    retrying,
    view: { operations, model, now },
  });
  const statusMessage = error ? { text: error, error: true } : status;

  return (
    <>
      {collapsed && !gatePending && (
        <button
          className="compose-fab"
          style={{ bottom: fabBottom }}
          onClick={() => setExpanded(true)}
        >
          ✎ {draft.trim() ? "draft…" : "compose"}
        </button>
      )}
      <form
        ref={dragSheet}
        className={dragging ? "composer dragging" : "composer"}
        hidden={collapsed}
        onSubmit={(event) => {
          event.preventDefault();
          void submit();
        }}
      >
        <div className="col composer-inner">
          {narrow && (
            <div
              className="composer-drag-handle"
              {...dragHandlers}
              title="drag down to collapse"
              aria-label="drag down to collapse"
            >
              <span className="composer-grabber" aria-hidden />
            </div>
          )}
          <TriagePreview tails={triage} />
          {noticeShown && (
            <CompactNotice
              cold={cold}
              failure={compactionFailure}
              offer={offer}
              onCompact={startCompaction}
            />
          )}
          {askColdLine !== null && (
            <div className="cold-line" role="status">
              {askColdLine}
            </div>
          )}
          <div className="composer-input">
            <div className="composer-text-row">
              <ComposerEditor
                key={`${base}:${editorEpoch}`}
                ref={editorSurface}
                base={base}
                store={store}
                initial={initialDocument(snapshot.document, snapshot.text)}
                placeholder={ask !== null ? replyHint(ask) : "send a turn…"}
                narrow={narrow}
                onChange={(document, text) =>
                  store.setContent({ document, text })
                }
                onSubmit={() => void submit()}
                onEscape={() => setExpanded(false)}
              />
            </div>
            <input
              ref={picker}
              type="file"
              multiple
              hidden
              onChange={(event) => {
                const files = [...(event.target.files ?? [])];
                event.target.value = "";
                // The picker's files take the selection's place, and focus
                // returns to the editor without scrolling.
                editorSurface.current?.insertFiles(files, { replace: true });
                editorSurface.current?.focus();
              }}
            />
            <ComposerToolbar
              turn={turn}
              status={statusMessage}
              narrow={narrow}
              running={running}
              held={pace?.held ?? false}
              uploading={uploading > 0}
              onAttach={() => picker.current?.click()}
              onInterrupt={() => act(interruptLoop("stop-button"))}
              holdPending={holdPending}
              onHoldChange={(held) => void changeHold(held)}
              newerVersion={newerVersion}
            />
          </div>
        </div>
      </form>
      {collapsed && (
        <div className="status-bar">
          <ComposerStatus turn={turn} message={statusMessage} />
          <NewerVersionLine newer={newerVersion} />
          <span className="spacer" />
          {running && (
            <button
              type="button"
              className="interrupt-btn composer-icon-button"
              aria-label="Interrupt current operation"
              title="Interrupt current operation"
              onClick={() => act(interruptLoop("status-bar-stop"))}
            >
              <ComposerActionIcon kind="stop" />
            </button>
          )}
          <HoldButton
            held={pace?.held ?? false}
            pending={holdPending}
            onChange={(held) => void changeHold(held)}
          />
        </div>
      )}
    </>
  );
}
