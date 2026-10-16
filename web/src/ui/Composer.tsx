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
import { acquireDraft } from "./drafts.ts";
import {
  ApiError,
  continueLoop,
  compactLoop,
  interruptLoop,
  patchLoop,
} from "../api/http.ts";
import type {
  Ask,
  Attachment,
  Cold,
  CompactionFailure,
  CurrentOperations,
  LoopPhase,
  Pace,
  Stall,
  Verbs,
} from "../api/types.ts";
import {
  ComposerToolbar,
  HoldButton,
  ComposerActionIcon,
} from "./ComposerToolbar.tsx";
import { CompactNotice } from "./CompactNotice.tsx";
import { compactOffer } from "./compact.ts";
import { coldLine } from "./coldLine.ts";
import { replyHint } from "./askMark.ts";
import { turnState, turnClass } from "./turnState.ts";
import { useDockOffset } from "./float.ts";
import { useNarrow } from "./useNarrow.ts";
import { useNow } from "./useNow.ts";
import { StagedAttachments } from "./Attachments.tsx";
import { pastesAsFile } from "./staging.ts";
import { useDragToDismiss } from "./useDragToDismiss.ts";

/** What a recall handed back: the text to prepend and the attachments to stage
 * again. `nonce` is what makes a repeat splice — recalling the same turn twice is
 * two events, not one value. */
export interface Recalled {
  text: string;
  attachments: Attachment[];
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
}: {
  submit: (text: string, attachments: Attachment[]) => Promise<void>;
  /** A turn a recall pulled back out of the daemon's queue, to prepend to the
   * draft (docs/web-client.md). */
  recalled: Recalled | null;
  pace: Pace | null;
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
  ask: Pick<Ask, "n" | "of"> | null;
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
}) {
  const store = useMemo(() => acquireDraft(apiBase()), []);
  const snapshot = useSyncExternalStore(store.subscribe, store.snapshot);
  const draft = snapshot.text;
  const running = phase !== null && phase.phase !== "idle";
  const setDraft = store.setText;
  const [error, setError] = useState<string | null>(null);
  const [holdPending, setHoldPending] = useState(false);
  const attachments = {
    staged: snapshot.staged,
    ready: snapshot.staged.flatMap((s) => (s.attachment ? [s.attachment] : [])),
    uploading: snapshot.staged.some((s) => s.status === "uploading"),
    remove: store.remove,
  };
  const { stage, restore } = store;
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
  const field = useRef<HTMLTextAreaElement>(null);
  const fabBottom = useDockOffset();
  const collapsed = narrow && !expanded;
  const { sheet: dragSheet, handlers: dragHandlers } =
    useDragToDismiss<HTMLFormElement>(() => setExpanded(false));
  useEffect(() => {
    if (narrow && expanded) field.current?.focus();
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
    setDraft((current) =>
      current ? `${recalled.text}\n${current}` : recalled.text,
    );
    restore(recalled.attachments);
    setExpanded(true);
    field.current?.focus();
  }, [recalled, restore, setDraft]);
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
      event.preventDefault();
      depth = 0;
      setDragging(false);
      for (const file of event.dataTransfer?.files ?? []) stage(file);
      setExpanded(true);
      field.current?.focus();
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
  }, [stage]);

  // A pasted file or image is staged; so is text past the inline limit, kept
  // as a file the model can grep rather than a wall in the draft. Anything else
  // pastes as it always has.
  function onPaste(event: React.ClipboardEvent<HTMLTextAreaElement>) {
    const files = [...event.clipboardData.files];
    if (files.length > 0) {
      event.preventDefault();
      files.forEach((file) => stage(file));
      return;
    }
    const text = event.clipboardData.getData("text/plain");
    if (pastesAsFile(text)) {
      event.preventDefault();
      stage(new File([text], "paste.txt", { type: "text/plain" }), text);
    }
  }

  // Put a paste staged for its length back into the draft, at the caret.
  function inline(key: string) {
    const staged = attachments.staged.find((s) => s.key === key);
    if (staged?.pastedText === undefined) return;
    const text = staged.pastedText;
    attachments.remove(key);
    const at = field.current?.selectionStart ?? draft.length;
    setDraft((current) => current.slice(0, at) + text + current.slice(at));
  }
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
    const text = draft.trim();
    // A send waits for its files: a chip still uploading has no name to send.
    if (attachments.uploading) {
      setError("an attachment is still uploading");
      return;
    }
    // An empty send hands control back with no input (docs/web-client.md): the
    // loop takes another turn on the wire it already has. A decline is a normal
    // answer — the moment had no continue in it — so its message renders on its
    // own rather than as a thrown-error string: from the `verbs` topic without a
    // round trip, or from the route when the topic had not moved yet.
    // Attachments alone are a turn.
    if (!text && attachments.ready.length === 0) {
      const verb = verbs?.continue;
      if (verb !== undefined && !verb.accepted) {
        setError(verb.message);
        return;
      }
      try {
        await continueLoop();
        setError(null);
      } catch (e) {
        setError(e instanceof ApiError ? e.message : `${e}`);
      }
      return;
    }
    try {
      await send(text, attachments.ready);
      store.sent(snapshot);
      setError(null);
      setExpanded(false);
    } catch (e) {
      setError(`${e}`);
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
          {error && <div className="error">{error}</div>}
          <StagedAttachments
            staged={attachments.staged}
            onRemove={attachments.remove}
            onInline={inline}
          />
          <div className="composer-input">
            <div className="composer-text-row">
              <textarea
                ref={field}
                value={draft}
                onChange={(event) => setDraft(event.target.value)}
                onPaste={onPaste}
                onKeyDown={(event) => {
                  // On a phone there is no shift-enter, so intercepting Enter
                  // would make multi-line drafts impossible: return inserts a
                  // newline and the send button sends.
                  if (!narrow && event.key === "Enter" && !event.shiftKey) {
                    event.preventDefault();
                    void submit();
                  }
                  if (narrow && event.key === "Escape") setExpanded(false);
                }}
                placeholder={ask !== null ? replyHint(ask) : "send a turn…"}
                rows={3}
              />
            </div>
            <input
              ref={picker}
              type="file"
              multiple
              hidden
              onChange={(event) => {
                for (const file of event.target.files ?? []) stage(file);
                event.target.value = "";
              }}
            />
            <ComposerToolbar
              turn={turn}
              narrow={narrow}
              running={running}
              held={pace?.held ?? false}
              uploading={attachments.uploading}
              onAttach={() => picker.current?.click()}
              onInterrupt={() => act(interruptLoop("stop-button"))}
              holdPending={holdPending}
              onHoldChange={(held) => void changeHold(held)}
            />
          </div>
        </div>
      </form>
      {collapsed && (
        <div className="status-bar">
          <span className={turnClass(turn.kind)} title={turn.title}>
            {turn.text}
          </span>
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
