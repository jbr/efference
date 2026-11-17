import { closeHistory } from "@tiptap/pm/history";
import type { JSONContent } from "@tiptap/core";
import { NodeViewWrapper, type NodeViewProps } from "@tiptap/react";
import { useSyncExternalStore } from "react";
import { describeKind } from "./staging.ts";
import { AttachmentThumbnail } from "./Attachments.tsx";
import type { DraftStore } from "./drafts.ts";

/** An upload occurrence's chip. Upload state is read through the store's
 * subscription: pending → ready re-renders this component and never replaces
 * editor content, so a landing upload cannot move the caret, add a history
 * step, or resurrect a removed occurrence. Removal and `inline` are ordinary
 * document edits, so undo restores the chip. */
export function ComposerUploadChip({
  node,
  editor,
  extension,
  getPos,
  deleteNode,
}: NodeViewProps) {
  const store = (extension.options as { store: DraftStore }).store;
  const snapshot = useSyncExternalStore(store.subscribe, store.snapshot);
  const key = node.attrs.key as string;
  const upload = snapshot.uploads.find((candidate) => candidate.key === key);
  const status = upload?.status ?? "failed";
  const label = upload?.label ?? "attachment";
  const detail =
    upload === undefined
      ? "no longer available"
      : status === "uploading"
        ? "uploading…"
        : status === "failed"
          ? (upload.error ?? "upload failed")
          : upload.attachment
            ? describeKind(upload.attachment.kind)
            : "attached";

  function inline() {
    const text = upload?.pastedText;
    if (text === undefined || text === "") return;
    const pos = getPos();
    if (pos === undefined) return;
    const nodes: JSONContent[] = [];
    text.split("\n").forEach((line, index) => {
      if (index > 0) nodes.push({ type: "hardBreak" });
      if (line !== "") nodes.push({ type: "text", text: line });
    });
    if (nodes.length === 0) return;
    // A plain transaction: the chip's action is a document edit like any
    // other, and it must not scroll or move focus on its own.
    editor.view.dispatch(
      closeHistory(
        editor.state.tr.replaceWith(
          pos,
          pos + node.nodeSize,
          nodes.map((node) => editor.schema.nodeFromJSON(node)),
        ),
      ),
    );
  }

  return (
    <NodeViewWrapper
      as="span"
      className="inline-reference-node"
      contentEditable={false}
    >
      <span
        className={`reference-chip upload-chip ${status}`}
        title={upload?.error ?? upload?.attachment?.path ?? label}
      >
        {upload?.preview != null && (
          <AttachmentThumbnail preview={upload.preview} />
        )}
        <span className="reference-kind" aria-hidden>
          {status === "failed" ? "!" : status === "uploading" ? "…" : "▤"}
        </span>
        <span className="reference-chip-label">{label}</span>
        <span className="upload-detail">{detail}</span>
        {upload?.pastedText !== undefined && (
          <button
            type="button"
            className="upload-inline"
            title="put the pasted text into the draft instead"
            onClick={inline}
          >
            inline
          </button>
        )}
        <button
          type="button"
          className="reference-chip-remove"
          title="remove this attachment"
          aria-label={`remove ${label}`}
          onClick={() => {
            editor.view.dispatch(closeHistory(editor.state.tr));
            deleteNode();
          }}
        >
          ×
        </button>
      </span>
    </NodeViewWrapper>
  );
}
