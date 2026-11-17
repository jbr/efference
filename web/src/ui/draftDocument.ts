// The draft's content model (docs/web-client.md): a staged upload is an inline
// occurrence before the daemon has given it a name, so the draft has a segment
// the wire document does not. A wire `UserContent` is projected from it only
// when every occurrence is a ready upload; until then the occurrence is
// draft-local and cannot be sent.
//
// Tiptap JSON is never the durable form: the store persists this shape, and the
// editor projects to and from it.

import {
  isReferenceDefinition,
  mergeReferences,
  normalizeDocument,
} from "../api/document.ts";
import type {
  Arrival,
  ReferenceDefinition,
  UserContent,
  UserSegment,
} from "../api/types.ts";
import type { DraftUpload } from "./staging.ts";

/** A draft segment: the wire segments plus a staged upload's local key. */
export type DraftSegment = UserSegment | { Upload: string };

export interface DraftContent {
  content: DraftSegment[];
  references: Record<string, ReferenceDefinition>;
}

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);

/** The stored draft document's shape. Like `isUserContent`, an occurrence
 * naming a missing definition is dropped by normalization rather than
 * rejecting the whole draft — but an `Upload` occurrence is kept even when its
 * record is missing, so it stays visible and blocks sending. */
export const isDraftContent = (value: unknown): value is DraftContent => {
  if (
    !isRecord(value) ||
    !Array.isArray(value.content) ||
    !isRecord(value.references)
  )
    return false;
  for (const segment of value.content) {
    if (!isRecord(segment)) return false;
    if ("Text" in segment) {
      if (typeof segment.Text !== "string") return false;
    } else if ("Reference" in segment) {
      if (typeof segment.Reference !== "string") return false;
    } else if ("Upload" in segment) {
      if (typeof segment.Upload !== "string") return false;
    } else {
      return false;
    }
  }
  return Object.values(value.references).every(isReferenceDefinition);
};

/** Whether a draft needs the structured form: a reference, or an upload
 * occurrence with no wire definition yet. A draft with only text stays plain. */
export const hasDraftStructure = (
  content: DraftContent | null,
): content is DraftContent =>
  content !== null &&
  (content.content.some((segment) => "Upload" in segment) ||
    Object.keys(content.references).length > 0);

/** The local keys the document names, in occurrence order. A record not named
 * here is detached: it is retained only while the editor's history can undo
 * its removal. */
export const draftUploadKeys = (content: DraftContent | null): string[] =>
  content === null
    ? []
    : [
        ...new Set(
          content.content.flatMap((segment) =>
            "Upload" in segment ? [segment.Upload] : [],
          ),
        ),
      ];

/** Make stored or editor-produced content safe to load: drop an occurrence
 * whose definition is gone, keep at most one occurrence per upload key and per
 * attachment store name (the gate admits one of each), and prune definitions
 * no occurrence names. */
export const normalizeDraftContent = (content: DraftContent): DraftContent => {
  const segments: DraftSegment[] = [];
  const used = new Set<string>();
  const attachments = new Set<string>();
  const uploads = new Set<string>();
  for (const segment of content.content) {
    if ("Text" in segment) {
      segments.push({ Text: segment.Text });
      continue;
    }
    if ("Upload" in segment) {
      if (uploads.has(segment.Upload)) continue;
      uploads.add(segment.Upload);
      segments.push({ Upload: segment.Upload });
      continue;
    }
    const definition = content.references[segment.Reference];
    if (definition === undefined) continue;
    if (definition.target.type === "attachment") {
      if (attachments.has(definition.target.name)) continue;
      attachments.add(definition.target.name);
    }
    used.add(segment.Reference);
    segments.push({ Reference: segment.Reference });
  }
  const references: Record<string, ReferenceDefinition> = {};
  for (const [id, definition] of Object.entries(content.references)) {
    if (used.has(id)) references[id] = definition;
  }
  return { content: segments, references };
};

/** A definition's readable spelling in a document: prose verbatim, a
 * filesystem path or agent mention as typed, and a file — attached or still
 * uploading — as nothing. Its name is shown beside the readable form. */
const readableOccurrence = (
  definition: ReferenceDefinition | undefined,
): string => {
  if (definition === undefined) return "";
  const target = definition.target;
  if (target.type === "filesystem") return target.original;
  if (target.type === "agent") return `@${target.session}`;
  return "";
};

/** The draft's readable flat form: the text a send carries and a reload
 * derives. A staged upload spells nothing, exactly as its stored attachment
 * will. */
export const draftReadable = (content: DraftContent): string =>
  content.content
    .map((segment) =>
      "Text" in segment
        ? segment.Text
        : "Upload" in segment
          ? ""
          : readableOccurrence(content.references[segment.Reference]),
    )
    .join("");

/** What a stored arrival would look like as a definition. */
export const attachmentDefinition = (
  arrival: Arrival,
): ReferenceDefinition => ({
  label: arrival.name,
  target: { type: "attachment", ...arrival },
});

/** A record for an occurrence whose record is missing (corrupt storage, or a
 * draft written by a newer client that knew a field this one dropped). The
 * occurrence stays visible and keeps sending blocked. */
export const missingUpload = (key: string): DraftUpload => ({
  key,
  label: "attachment",
  preview: null,
  status: "failed",
  error:
    "This attachment is no longer available; remove it or attach the file again.",
  provenance: "sent",
});

/** The draft without an upload's occurrence (the chip's remove). The wire
 * document drops references no occurrence names, so nothing else changes. */
export const detachUpload = (
  content: DraftContent,
  key: string,
): DraftContent => ({
  ...content,
  content: content.content.filter(
    (segment) => !("Upload" in segment && segment.Upload === key),
  ),
});

/** Project the draft to the wire: upload occurrences become attachment
 * references under their stable local key, and an occurrence whose upload is
 * not ready is reported in `blocked` instead of being omitted. `document` is
 * null when the draft carries no references. */
/** The stored upload names the draft's ready upload occurrences already
 * resolve to. A reference definition for one of these names is the same file
 * shown twice. */
export const resolvedUploadNames = (
  content: DraftContent,
  uploads: readonly DraftUpload[],
): Set<string> => {
  const keys = new Set(draftUploadKeys(content));
  const names = new Set<string>();
  for (const upload of uploads) {
    if (
      keys.has(upload.key) &&
      upload.status === "ready" &&
      upload.attachment !== undefined
    )
      names.add(upload.attachment.name);
  }
  return names;
};

/** Drop reference occurrences that name an upload a ready upload occurrence
 * already resolves to, with their definitions: a recall can bring back the
 * same stored file, and the chip must show once. */
export const dedupeDraftUploads = (
  content: DraftContent,
  uploads: readonly DraftUpload[],
): DraftContent => {
  const names = resolvedUploadNames(content, uploads);
  if (names.size === 0) return content;
  const dropped = new Set<string>();
  for (const [id, definition] of Object.entries(content.references)) {
    if (
      definition.target.type === "attachment" &&
      names.has(definition.target.name)
    )
      dropped.add(id);
  }
  if (dropped.size === 0) return content;
  return normalizeDraftContent({
    content: content.content.filter(
      (segment) => !("Reference" in segment && dropped.has(segment.Reference)),
    ),
    references: content.references,
  });
};

export function wireDocument(
  content: DraftContent | null,
  uploads: readonly DraftUpload[],
): { document: UserContent | null; blocked: DraftUpload[] } {
  const blocked: DraftUpload[] = [];
  if (content === null) return { document: null, blocked };
  const byKey = new Map(uploads.map((upload) => [upload.key, upload]));
  const segments: UserSegment[] = [];
  const references: Record<string, ReferenceDefinition> = {};
  const attachments = new Set<string>();
  const reported = new Set<string>();
  const ids = new Set(Object.keys(content.references));
  const idFor = (key: string): string => {
    let id = key;
    let suffix = 2;
    while (ids.has(id)) id = `${key}-${suffix++}`;
    ids.add(id);
    return id;
  };
  const report = (upload: DraftUpload) => {
    if (reported.has(upload.key)) return;
    reported.add(upload.key);
    blocked.push(upload);
  };

  for (const segment of content.content) {
    if ("Text" in segment) {
      segments.push({ Text: segment.Text });
      continue;
    }
    if ("Upload" in segment) {
      const upload = byKey.get(segment.Upload);
      if (upload === undefined) {
        report(missingUpload(segment.Upload));
        continue;
      }
      if (upload.status !== "ready" || upload.attachment === undefined) {
        report(upload);
        continue;
      }
      if (attachments.has(upload.attachment.name)) continue;
      attachments.add(upload.attachment.name);
      const id = idFor(upload.key);
      segments.push({ Reference: id });
      references[id] = attachmentDefinition(upload.attachment);
      continue;
    }
    const definition = content.references[segment.Reference];
    if (definition === undefined) continue;
    if (definition.target.type === "attachment") {
      if (attachments.has(definition.target.name)) continue;
      attachments.add(definition.target.name);
    }
    segments.push({ Reference: segment.Reference });
    references[segment.Reference] = definition;
  }
  return { document: { content: segments, references }, blocked };
}

/** Append one document to the draft's merge (a recall splices ahead of the
 * retained draft). Reference identity follows `mergeReferences`; an upload
 * occurrence keeps its local key, because it has no wire definition to
 * rename and its record is the draft's own. */
export const appendDraft = (
  merged: DraftContent,
  document: DraftContent | UserContent | null | undefined,
  joiner = "\n",
): DraftContent => {
  if (document == null) return merged;
  if (merged.content.length > 0 && joiner !== "") {
    merged.content.push({ Text: joiner });
  }
  const { renames, dropped } = mergeReferences(
    merged.references,
    document.references,
  );
  for (const segment of document.content) {
    if ("Text" in segment) {
      merged.content.push({ Text: segment.Text });
    } else if ("Upload" in segment) {
      merged.content.push({ Upload: segment.Upload });
    } else if (!dropped.has(segment.Reference)) {
      merged.content.push({
        Reference: renames.get(segment.Reference) ?? segment.Reference,
      });
    }
  }
  return merged;
};

/** The wire shape of draft content that carries no upload occurrences (the
 * clipboard's flattened projection). */
export const asUserContent = (content: DraftContent): UserContent =>
  normalizeDocument({
    content: content.content.flatMap((segment) =>
      "Upload" in segment ? [] : [segment],
    ),
    references: content.references,
  });
