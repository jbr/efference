// What a chipped argument renders as (docs/web-client.md "An argument can carry
// a chip"). The table that decides which arguments these are is `chipTable.ts`;
// this half resolves one — the file link, the fetched process record — and
// falls back to the argument's own text whenever there is nothing to add.

import type { CallId, ProcessRecord, ToolCall } from "../api/types.ts";
import type { SigArg } from "../render/parse.ts";
import { argChip, chipCommand } from "./chipTable.ts";
import { useProcessRecord } from "./processRecord.ts";

/** One argument's value, chipped when the table has an entry for it and plain
 * text otherwise — so a signature reads the same either way and a chip is only
 * ever an upgrade in place. */
export function ArgValue({
  call,
  arg,
  onOpenFile,
}: {
  call: ToolCall;
  arg: SigArg;
  onOpenFile?: (path: string, callId: CallId) => void;
}) {
  // An elided argument renders as its *shape* (`505 lines`) — a description of
  // the value, which no chip has anything to say about.
  const input = call.input;
  if (arg.elided || typeof input !== "object" || input === null)
    return <>{arg.text}</>;
  const value = (input as Record<string, unknown>)[arg.key];
  const chip = argChip(call.name, arg.key, value);
  if (chip === null) return <>{arg.text}</>;
  if (chip.kind === "process")
    return <ResolvedProcessChip id={chip.id} text={arg.text} />;
  if (onOpenFile === undefined) return <>{arg.text}</>;
  return (
    <PathChip
      path={chip.path}
      text={arg.text}
      callId={call.id}
      onOpenFile={onOpenFile}
    />
  );
}

/** The path argument, as the link that opens the file — the affordance the
 * argument itself cannot carry. A button, so the call's open-the-inspector
 * click handler ignores it (Transcript.tsx); it opens the *current* file from
 * disk, never the file at the time of the call. */
export function PathChip({
  path,
  text,
  callId,
  onOpenFile,
}: {
  path: string;
  /** The argument as the signature renders it — quoted, and what the chip
   * keeps reading as. */
  text: string;
  /** The call the link belongs to: an edit's own result is where the viewer
   * gets the lines to land on (session/editAnchor.ts). */
  callId: CallId;
  onOpenFile: (path: string, callId: CallId) => void;
}) {
  return (
    <button
      type="button"
      className="linkish arg-chip path-chip"
      title={`view ${path} as it is now on disk`}
      onClick={() => onOpenFile(path, callId)}
    >
      {text}
      <span className="chip-glyph"> ⤢</span>
    </button>
  );
}

/** The handle, with the command it stood for. Presentational: a missing record
 * is the normal case (a foreground-only ordinal, a session recorded before the
 * records existed), and it degrades to the plain argument — the chip is an
 * annotation, so having nothing to annotate with costs the reader nothing. */
export function ProcessChip({
  record,
  text,
}: {
  record: ProcessRecord | null;
  text: string;
}) {
  if (record === null) return <>{text}</>;
  const exit = record.exit ?? null;
  const ended =
    exit === null
      ? "no exit recorded"
      : exit.outcome === "signal"
        ? "signalled"
        : exit.outcome === "unknown"
          ? "exited"
          : `exit ${exit.outcome.code}`;
  return (
    <span
      className={`arg-chip process-chip ${record.running ? "running" : "exited"}`}
      title={`p#${record.id} · ${record.command}${record.running ? "" : ` · ${ended}`}`}
    >
      ⟨p#{record.id} <span className="call-sep">·</span>{" "}
      {chipCommand(record.command)}⟩
    </span>
  );
}

/** [`ProcessChip`] with the fetch behind it (`processRecord.ts`). */
function ResolvedProcessChip({ id, text }: { id: number; text: string }) {
  const record = useProcessRecord(id);
  return <ProcessChip record={record} text={text} />;
}
