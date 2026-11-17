// Attachment descriptions and paste rules (docs/inbox.md, docs/web-client.md).
// Upload ownership and staged state live in drafts.ts, outside the composer.

import type { Arrival, AttachmentKind } from "../api/types.ts";

/** How many lines of pasted text stay in the draft; past this a paste is staged
 * as a file instead, so a bulky blob never takes the decision about whether to
 * read it away from the model. The TUI's threshold
 * (`attachments::INLINE_LINE_LIMIT`), kept equal so a paste lands the same way
 * in either client. */
export const INLINE_LINE_LIMIT = 12;

/** Whether a pasted text is past the inline limit. Counts lines the way the
 * TUI does (`str::lines`): a trailing newline ends a line, it does not start
 * one. */
export function pastesAsFile(text: string): boolean {
  const lines = text.split("\n");
  if (lines.at(-1) === "") lines.pop();
  return lines.length > INLINE_LINE_LIMIT;
}

/** A magnitude, not an exact count — the same rounding the model's notice uses. */
export function humanBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/** The chip's one-line description: the triage signals the model reads, short. */
export function describeKind(kind: AttachmentKind): string {
  if ("Image" in kind) {
    const { format, width, height, bytes } = kind.Image;
    return `${format} · ${width}×${height} · ${humanBytes(bytes)}`;
  }
  if ("Text" in kind) {
    const { lines, bytes } = kind.Text;
    return lines > 0
      ? `${lines} lines · ${humanBytes(bytes)}`
      : humanBytes(kind.Text.bytes);
  }
  return humanBytes(kind.Binary.bytes);
}

export const isImage = (kind: AttachmentKind) => "Image" in kind;

/** One draft-local upload. `key` is the occurrence's stable identity in the
 * draft (a file can be attached twice); `attachment` is what the daemon stored
 * once the upload lands. A record is live while an `Upload` segment names it;
 * detached records are retained only while the editor's history can undo their
 * removal (ui/drafts.ts). */
export interface DraftUpload {
  key: string;
  label: string;
  /** A thumbnail source: an object URL for a local image, the stored bytes'
   * URL once uploaded, `null` for anything that is not an image (or a file
   * whose bytes are gone). */
  preview: string | null;
  status: "uploading" | "ready" | "failed";
  attachment?: Arrival;
  error?: string;
  /** A paste staged for its length keeps its text, so the chip can put it back
   * in the draft instead. */
  pastedText?: string;
  /** `draft` owns bytes this draft may delete; `sent` came from a recalled or
   * submitted turn and is never deleted by this store. */
  provenance: "draft" | "sent";
  /** A send names this upload and has not settled. Its bytes must survive
   * until it does; persisted so a reload during the request cannot delete
   * them. */
  pinned?: boolean;
}

/** A pasted blob has no name worth keeping (`image.png` every time); name it
 * for what it is and when, which the store then keeps unique. */
export function pastedName(file: File, now = new Date()): string {
  if (file.name && file.name !== "image.png") return file.name;
  const stamp = now.toISOString().slice(0, 19).replace(/[:T]/g, "-");
  const ext = file.type.startsWith("image/") ? file.type.slice(6) : "bin";
  return `paste-${stamp}.${ext}`;
}
