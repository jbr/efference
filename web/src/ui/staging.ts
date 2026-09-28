// Attachment staging for the composer (docs/inbox.md, docs/web-client.md). A
// file is uploaded the moment it is staged — pasted, dropped, or picked — so the
// daemon's description of it is on the chip while it can still be removed, and
// the turn that sends it only names it. Framework-free apart from the hook at
// the bottom, so the rules are testable without a DOM.

import { useCallback, useEffect, useRef, useState } from "react";
import {
  removeAttachment,
  uploadAttachment,
  attachmentUrl,
} from "../api/http.ts";
import type { Attachment, AttachmentKind } from "../api/types.ts";

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

/** One staged file. `key` is local (a file can be staged twice); `attachment`
 * is what the daemon stored once the upload lands. */
export interface Staged {
  key: string;
  label: string;
  /** A thumbnail source: an object URL for a local image, the stored bytes'
   * URL for a recalled one, `null` for anything that is not an image. */
  preview: string | null;
  status: "uploading" | "ready" | "failed";
  attachment?: Attachment;
  error?: string;
  /** A paste staged for its length keeps its text, so the chip can put it back
   * in the draft instead. */
  pastedText?: string;
}

/** A pasted blob has no name worth keeping (`image.png` every time); name it
 * for what it is and when, which the store then keeps unique. */
export function pastedName(file: File, now = new Date()): string {
  if (file.name && file.name !== "image.png") return file.name;
  const stamp = now.toISOString().slice(0, 19).replace(/[:T]/g, "-");
  const ext = file.type.startsWith("image/") ? file.type.slice(6) : "bin";
  return `paste-${stamp}.${ext}`;
}

/** The composer's staged attachments. `stage` uploads at once; `remove` takes
 * one back (and deletes the upload); `ready` is what a send carries, and
 * `clear` empties the row after one without deleting anything, since the sent
 * turn names the files. */
export function useStagedAttachments() {
  const [staged, setStaged] = useState<Staged[]>([]);
  // Keys removed while their upload was in flight: the upload's answer deletes
  // what it stored rather than resurrecting the chip.
  const dropped = useRef(new Set<string>());
  const counter = useRef(0);
  const nextKey = () => `staged-${++counter.current}`;

  const stage = useCallback((file: File, pastedText?: string) => {
    const key = nextKey();
    const label = pastedName(file);
    const preview = file.type.startsWith("image/")
      ? URL.createObjectURL(file)
      : null;
    setStaged((all) => [
      ...all,
      { key, label, preview, status: "uploading", pastedText },
    ]);
    uploadAttachment(label, file).then(
      (attachment) => {
        if (dropped.current.delete(key)) {
          void removeAttachment(attachment.name).catch(() => {});
          return;
        }
        setStaged((all) =>
          all.map((s) =>
            s.key === key ? { ...s, status: "ready", attachment } : s,
          ),
        );
      },
      (e: unknown) => {
        dropped.current.delete(key);
        setStaged((all) =>
          all.map((s) =>
            s.key === key ? { ...s, status: "failed", error: `${e}` } : s,
          ),
        );
      },
    );
  }, []);

  /** Re-stage attachments a recall handed back: already stored, so ready. */
  const restore = useCallback((attachments: Attachment[]) => {
    setStaged((all) => [
      ...all,
      ...attachments.map((attachment): Staged => ({
        key: nextKey(),
        label: attachment.name,
        preview: isImage(attachment.kind)
          ? attachmentUrl(attachment.name)
          : null,
        status: "ready",
        attachment,
      })),
    ]);
  }, []);

  // The row as last rendered, for handlers whose side effects (a DELETE, a
  // revoke) must run once — not inside a state updater, which React may replay.
  const latest = useRef(staged);
  useEffect(() => {
    latest.current = staged;
  }, [staged]);

  const remove = useCallback((key: string) => {
    const gone = latest.current.find((s) => s.key === key);
    if (gone === undefined) return;
    if (gone.status === "uploading") dropped.current.add(key);
    if (gone.attachment)
      void removeAttachment(gone.attachment.name).catch(() => {});
    revoke(gone);
    setStaged((all) => all.filter((s) => s.key !== key));
  }, []);

  const clear = useCallback(() => {
    latest.current.forEach(revoke);
    setStaged([]);
  }, []);

  // Object URLs outlive their chips unless revoked; unmount revokes the rest.
  useEffect(() => () => latest.current.forEach(revoke), []);

  return {
    staged,
    stage,
    restore,
    remove,
    clear,
    ready: staged.flatMap((s) => (s.attachment ? [s.attachment] : [])),
    uploading: staged.some((s) => s.status === "uploading"),
  };
}

function revoke(staged: Staged | undefined) {
  if (staged?.preview?.startsWith("blob:")) URL.revokeObjectURL(staged.preview);
}
