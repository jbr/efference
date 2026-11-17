// Persistence for composer drafts (docs/web-client.md): the v2 storage shape,
// loading it, and the one-shot v1 migration. Text, the draft document and
// upload metadata persist — never file bytes or a vendor editor's JSON.
//
// A migrated v1 ready row becomes an inline attachment reference with no
// record, so this store can never delete a file a send may already have used.

import {
  isArrival,
  isUserContent,
  normalizeDocument,
} from "../../api/document.ts";
import type { ReferenceDefinition } from "../../api/types.ts";
import {
  attachmentDefinition,
  draftReadable,
  draftUploadKeys,
  hasDraftStructure,
  isDraftContent,
  missingUpload,
  normalizeDraftContent,
} from "../draftDocument.ts";
import type { DraftContent } from "../draftDocument.ts";
import type { DraftUpload } from "../staging.ts";
import { interrupted, previewOf } from "./records.ts";

/** The draft's authoritative content: `text` is the readable spelling, the
 * document carries reference identity and position (including upload
 * occurrences), and `revision` advances on every content edit so a send can
 * tell whether it submitted what it sent. */
export interface Draft {
  text: string;
  uploads: DraftUpload[];
  document: DraftContent | null;
  revision: number;
}

/** The storage surface a draft needs; a denied or full `localStorage` shows up
 * as `null` or a throw from `write`. */
export type DraftStorage = Pick<Storage, "getItem" | "setItem" | "removeItem">;

const v1Key = (base: string) => `efference.composer.v1:${base}`;
const v2Key = (base: string) => `efference.composer.v2:${base}`;

const empty = (): Draft => ({
  text: "",
  uploads: [],
  document: null,
  revision: 0,
});

const serialized = (draft: Draft) => ({
  text: draft.text,
  document: draft.document,
  uploads: draft.uploads.map(
    ({
      key,
      label,
      status,
      attachment,
      error,
      pastedText,
      provenance,
      pinned,
    }) => ({
      key,
      label,
      status,
      attachment,
      error,
      pastedText,
      provenance,
      pinned,
    }),
  ),
});

/** Write the draft, reporting whether storage accepted it. A caller that is
 * about to remove older data (a migration) must wait for `true`. */
export function write(
  storage: DraftStorage | null,
  base: string,
  draft: Draft,
): boolean {
  try {
    if (
      draft.text === "" &&
      draft.uploads.length === 0 &&
      draft.document === null
    ) {
      storage?.removeItem(v2Key(base));
    } else {
      storage?.setItem(v2Key(base), JSON.stringify(serialized(draft)));
    }
    return true;
  } catch {
    return false;
  }
}

export function load(storage: DraftStorage | null, base: string): Draft {
  try {
    const raw = storage?.getItem(v2Key(base));
    if (raw) return parseDraft(raw, base);
    const legacy = storage?.getItem(v1Key(base));
    if (!legacy) return empty();
    const migrated = migrateLegacy(legacy);
    // v1 is removed only after v2 was written: a failed save must not lose the
    // only copy of a user's draft.
    if (write(storage, base, migrated)) storage?.removeItem(v1Key(base));
    return migrated;
  } catch {
    return empty();
  }
}

function parseUploads(value: unknown, base: string): DraftUpload[] {
  if (!Array.isArray(value)) return [];
  const uploads: DraftUpload[] = [];
  const keys = new Set<string>();
  for (const item of value) {
    if (typeof item !== "object" || item === null) continue;
    const upload = item as Record<string, unknown>;
    if (typeof upload.key !== "string" || typeof upload.label !== "string")
      continue;
    if (keys.has(upload.key)) continue;
    if (
      upload.status !== "uploading" &&
      upload.status !== "ready" &&
      upload.status !== "failed"
    )
      continue;
    keys.add(upload.key);
    const attachment =
      upload.status === "ready" && isArrival(upload.attachment)
        ? upload.attachment
        : undefined;
    const pinned = upload.pinned === true;
    uploads.push({
      key: upload.key,
      label: upload.label,
      status: attachment ? "ready" : "failed",
      attachment,
      preview: previewOf(attachment, base),
      error: attachment
        ? undefined
        : upload.status === "uploading" || pinned
          ? interrupted
          : typeof upload.error === "string"
            ? upload.error
            : "Upload failed; attach this file again.",
      pastedText:
        typeof upload.pastedText === "string" ? upload.pastedText : undefined,
      // A send that was in flight when the page went away may have been
      // admitted; its bytes are never deleted by a later load.
      provenance: pinned || upload.provenance === "sent" ? "sent" : "draft",
      pinned: false,
    });
  }
  return uploads;
}

function parseDraft(raw: string, base: string): Draft {
  const saved: unknown = JSON.parse(raw);
  if (typeof saved !== "object" || saved === null) return empty();
  const draft = saved as {
    text?: unknown;
    document?: unknown;
    uploads?: unknown;
  };
  const uploads = parseUploads(draft.uploads, base);
  let document = isDraftContent(draft.document)
    ? normalizeDraftContent(draft.document)
    : null;
  if (document !== null) {
    // An occurrence without a record is visible and blocks sending; it must
    // not vanish just because the record was lost.
    const known = new Set(uploads.map((upload) => upload.key));
    for (const key of draftUploadKeys(document)) {
      if (known.has(key)) continue;
      const missing = missingUpload(key);
      uploads.push(missing);
      known.add(key);
    }
  }
  // The document is authoritative when present; its readable projection is
  // the text the composer stores, so the two cannot drift apart on reload.
  const text =
    document !== null
      ? draftReadable(document)
      : typeof draft.text === "string"
        ? draft.text
        : "";
  if (!hasDraftStructure(document)) document = null;
  return { text, uploads, document, revision: 0 };
}

/** Read a v1 draft: a `UserContent` document plus staged rows with no caret.
 * A ready row becomes an inline attachment reference (no record: a sent
 * upload is never deleted by this store). A pending or failed row becomes an
 * upload occurrence at the end of the draft, keeping its pasted text. A ready
 * row whose store name the document already carries is skipped, not shown
 * twice. */
function migrateLegacy(raw: string): Draft {
  const saved: unknown = JSON.parse(raw);
  if (typeof saved !== "object" || saved === null) return empty();
  const legacy = saved as {
    text?: unknown;
    staged?: unknown;
    document?: unknown;
  };
  const document: DraftContent | null = isUserContent(legacy.document)
    ? normalizeDocument(legacy.document)
    : null;
  const content: DraftContent = document ?? { content: [], references: {} };
  // A v1 draft with staged rows and no document keeps its text only in the
  // `text` field; seed it as leading content so the editor shows it ahead of
  // the occurrences appended below.
  if (
    document === null &&
    typeof legacy.text === "string" &&
    legacy.text !== ""
  ) {
    content.content.push({ Text: legacy.text });
  }
  const names = new Set(
    Object.values(content.references).flatMap((definition) =>
      definition.target.type === "attachment" ? [definition.target.name] : [],
    ),
  );
  const uploads: DraftUpload[] = [];
  if (Array.isArray(legacy.staged)) {
    for (const value of legacy.staged) {
      if (typeof value !== "object" || value === null) continue;
      const row = value as Partial<DraftUpload> & { status?: unknown };
      if (typeof row.label !== "string") continue;
      if (
        row.status !== "uploading" &&
        row.status !== "ready" &&
        row.status !== "failed"
      )
        continue;
      if (row.status === "ready" && isArrival(row.attachment)) {
        if (names.has(row.attachment.name)) continue;
        names.add(row.attachment.name);
        const id = nextReferenceId(content.references);
        content.content.push({ Reference: id });
        content.references[id] = attachmentDefinition(row.attachment);
        continue;
      }
      const key = crypto.randomUUID();
      uploads.push({
        key,
        label: row.label,
        preview: null,
        status: "failed",
        error:
          row.status === "uploading"
            ? interrupted
            : typeof row.error === "string"
              ? row.error
              : "Upload failed; attach this file again.",
        pastedText:
          typeof row.pastedText === "string" ? row.pastedText : undefined,
        provenance: "sent",
      });
      content.content.push({ Upload: key });
    }
  }
  const text =
    document !== null
      ? draftReadable(content)
      : typeof legacy.text === "string"
        ? legacy.text
        : "";
  return {
    text,
    uploads,
    document: hasDraftStructure(content) ? content : null,
    revision: 0,
  };
}

function nextReferenceId(references: Record<string, ReferenceDefinition>) {
  let n = Object.keys(references).length + 1;
  while (references[`r${n}`] !== undefined) n += 1;
  return `r${n}`;
}
