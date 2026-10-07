// The composer area (docs/web-client.md): where loop state is controlled.
// Gates arrive here, so the dials that decide how the loop moves — pace,
// hold, review level, interrupt — sit on the status row above the input
// rather than in a strip at the top of the page. Submission goes through the
// session action so an accepted turn queues its delivery echo
// (docs/control-api.md — submission is decoupled from inference; a 202 is
// success, the turn's effects arrive on the stream).

import { useEffect, useRef, useState } from "react";
import {
  ApiError,
  continueLoop,
  interruptLoop,
  patchLoop,
} from "../api/http.ts";
import type { Ask, Attachment, Cold, Pace, Stall } from "../api/types.ts";
import { replyHint } from "./askMark.ts";
import { turnState } from "./turnState.ts";
import { useDockOffset } from "./float.ts";
import { useNarrow } from "./useNarrow.ts";
import { StagedAttachments } from "./Attachments.tsx";
import { pastesAsFile, useStagedAttachments } from "./staging.ts";
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
  running,
  stall,
  retrying,
  gatePending,
  ask,
}: {
  submit: (
    text: string,
    attachments: Attachment[],
    acceptColdRead: boolean,
  ) => Promise<void>;
  /** A turn a recall pulled back out of the daemon's queue, to prepend to the
   * draft (docs/web-client.md). */
  recalled: Recalled | null;
  pace: Pace | null;
  running: boolean;
  /** The trajectory gave up rather than finished (docs/transport.md): the
   * turn-state readout says so, since `running: false` alone cannot. */
  stall: Stall | null;
  /** A retry of the upstream is waiting (docs/transport.md): an empty send
   * retries now, and the readout says so. */
  retrying: boolean;
  /** A gate float may own the above-dock slot; the compose chip yields to
   * it (the float carries its own steering input). */
  gatePending: boolean;
  /** The pending `ask`: the placeholder says which question a send answers. */
  ask: Pick<Ask, "n" | "of"> | null;
}) {
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string | null>(null);
  // A `409 cold` came back (docs/control-api.md): the next send carries the
  // consent it asked for, so sending again is the operator's yes.
  const [coldConsent, setColdConsent] = useState(false);
  const attachments = useStagedAttachments();
  const { stage, restore } = attachments;
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
  }, [recalled, restore]);
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
    // own rather than as a thrown-error string. Attachments alone are a turn.
    const acceptColdRead = coldConsent;
    setColdConsent(false);
    if (!text && attachments.ready.length === 0) {
      try {
        await continueLoop(acceptColdRead);
        setError(null);
      } catch (e) {
        if (!refusedCold(e))
          setError(e instanceof ApiError ? e.message : `${e}`);
      }
      return;
    }
    try {
      await send(text, attachments.ready, acceptColdRead);
      attachments.clear();
      setDraft("");
      setError(null);
      setExpanded(false);
    } catch (e) {
      if (!refusedCold(e)) setError(`${e}`);
    }
  }

  // Nothing was queued and the draft is untouched: say what sending again costs.
  function refusedCold(e: unknown): boolean {
    if (!(e instanceof ApiError) || e.code !== "cold") return false;
    const tokens = (e.detail as Cold | null)?.context_tokens;
    setColdConsent(true);
    setError(
      tokens
        ? `cache believed cold — send again to read ${Math.round(tokens / 1000)}k tokens uncached`
        : "cache believed cold — send again to read it uncached",
    );
    return true;
  }

  const turn = turnState(running, stall, retrying);
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
            <div className="composer-toolbar">
              <div className="composer-toolbar-left">
                {/* The picker is the phone's way in: there is no drag, and a
                    paste carries no file. */}
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
                <button
                  type="button"
                  className="attach-btn"
                  title="attach files (or paste, or drop them anywhere)"
                  aria-label="attach files"
                  onClick={() => picker.current?.click()}
                >
                  <svg
                    width="16"
                    height="16"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="1.5"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    aria-hidden="true"
                  >
                    <path d="m21.44 11.05-9.19 9.19a6 6 0 0 1-8.49-8.49l8.57-8.57A4 4 0 1 1 18 8.84l-8.59 8.57a2 2 0 0 1-2.83-2.83l8.49-8.48" />
                  </svg>
                  <span>attach</span>
                </button>
                <span
                  className={
                    turn.kind === "running"
                      ? "turn-state dim"
                      : turn.kind === "stalled"
                        ? "turn-state stalled"
                        : "turn-state your-turn"
                  }
                  title={turn.title}
                >
                  {turn.text}
                </span>
              </div>
              <div className="composer-toolbar-right">
                {running && (
                  <button
                    type="button"
                    className="interrupt-btn"
                    onClick={() => act(interruptLoop())}
                  >
                    interrupt
                  </button>
                )}
                {(pace?.held ?? false) && (
                  <button
                    type="button"
                    className="resume-btn"
                    onClick={() => act(patchLoop({ held: false }))}
                  >
                    resume
                  </button>
                )}
                {!narrow && (
                  <span className="composer-hint">shift-enter for newline</span>
                )}
                <button
                  disabled={attachments.uploading}
                  className="send-btn primary"
                >
                  <span>send</span>
                  {!narrow && <kbd className="send-kbd">↵</kbd>}
                </button>
              </div>
            </div>
          </div>
        </div>
      </form>
      {collapsed && (
        <div className="status-bar">
          <span
            className={
              turn.kind === "running"
                ? "turn-state dim"
                : turn.kind === "stalled"
                  ? "turn-state stalled"
                  : "turn-state your-turn"
            }
            title={turn.title}
          >
            {turn.text}
          </span>
          <span className="spacer" />
          {running && (
            <button type="button" onClick={() => act(interruptLoop())}>
              interrupt
            </button>
          )}
          {(pace?.held ?? false) && (
            <button
              type="button"
              onClick={() => act(patchLoop({ held: false }))}
            >
              resume
            </button>
          )}
        </div>
      )}
    </>
  );
}
