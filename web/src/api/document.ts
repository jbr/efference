// The structured user document's client-side helpers (docs/control-api.md):
// the readable flat form a draft or a pending echo shows, the merge a
// multi-turn recall holds for an unedited resubmission, and the attachment
// bookkeeping a held document shares with the staged chips. Pure data — no
// framework, no fetch.

import type {
  Arrival,
  QueuedTurn,
  ReferenceDefinition,
  UserContent,
  UserSegment,
} from "./types.ts";

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);

/** A stored upload's shape, as the daemon described it. Shared by draft
 * persistence and reference validation so neither invents a second wire
 * spelling (docs/control-api.md). */
export const isArrival = (value: unknown): value is Arrival => {
  if (!isRecord(value)) return false;
  if (typeof value.name !== "string" || typeof value.path !== "string")
    return false;
  const kind = value.kind;
  if (!isRecord(kind)) return false;
  if ("Image" in kind) {
    const image = kind.Image;
    return (
      isRecord(image) &&
      typeof image.format === "string" &&
      typeof image.width === "number" &&
      typeof image.height === "number" &&
      typeof image.bytes === "number"
    );
  }
  if ("Text" in kind) {
    const text = kind.Text;
    return (
      isRecord(text) &&
      typeof text.bytes === "number" &&
      typeof text.lines === "number" &&
      typeof text.shape === "string" &&
      typeof text.first_line === "string"
    );
  }
  if ("Binary" in kind) {
    const binary = kind.Binary;
    return isRecord(binary) && typeof binary.bytes === "number";
  }
  return false;
};

export const isReferenceTarget = (value: unknown): boolean => {
  if (!isRecord(value)) return false;
  if (value.type === "filesystem") {
    return (
      typeof value.original === "string" &&
      typeof value.path === "string" &&
      (value.target === "file" || value.target === "directory") &&
      isRecord(value.access) &&
      typeof value.access.read === "boolean" &&
      typeof value.access.write === "boolean"
    );
  }
  if (value.type === "agent") return typeof value.session === "string";
  if (value.type === "attachment") return isArrival(value);
  return false;
};

export const isReferenceDefinition = (
  value: unknown,
): value is ReferenceDefinition =>
  isRecord(value) &&
  typeof value.label === "string" &&
  isReferenceTarget(value.target);

/** The stored structured draft's shape. Content segments are checked
 * structurally; an occurrence naming a missing definition is pruned by
 * `normalizeDocument` rather than rejecting the whole draft. */
export const isUserContent = (value: unknown): value is UserContent => {
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
    } else {
      return false;
    }
  }
  return Object.values(value.references).every(isReferenceDefinition);
};

/** Make a document safe to load into an editor: drop occurrences whose
 * definition is absent, keep at most one occurrence per upload (an attachment
 * has one reference; docs/control-api.md), and prune definitions no
 * occurrence names. */
export const normalizeDocument = (content: UserContent): UserContent => {
  const segments: UserSegment[] = [];
  const used = new Set<string>();
  const attachments = new Set<string>();
  for (const segment of content.content) {
    if ("Text" in segment) {
      segments.push({ Text: segment.Text });
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

/** A plain draft as a document, for loading into the editor and for merges. */
export const textDocument = (text: string): UserContent => ({
  content: text === "" ? [] : [{ Text: text }],
  references: {},
});

/** The stored upload names a document already carries, so a staged chip that
 * is also inline is not shown twice. */
export const documentAttachmentNames = (content: UserContent): Set<string> =>
  new Set(
    Object.values(content.references).flatMap((definition) =>
      definition.target.type === "attachment" ? [definition.target.name] : [],
    ),
  );

/** A definition's readable flat spelling: what re-typing the chip as text
 * grants nothing by. An attachment spells nothing — its name is shown beside
 * the readable form (the transcript's definition list, a staged chip). */
const readableReference = (content: UserContent, id: string): string => {
  const definition = content.references[id];
  if (definition === undefined) return "";
  const target = definition.target;
  if (target.type === "filesystem") return target.original;
  if (target.type === "agent") return `@${target.session}`;
  return "";
};

/** The document's readable flat form (docs/control-api.md): prose verbatim, a
 * chip flattened to its readable spelling, an attachment to nothing. */
export const documentReadable = (content: UserContent): string =>
  content.content
    .map((segment: UserSegment) =>
      "Text" in segment
        ? segment.Text
        : readableReference(content, segment.Reference),
    )
    .join("");

/** The document a recall covers: every withdrawn turn contributes its own
 * document when it carried references, or its text otherwise, so the merged
 * readable form is exactly the recalled text. `null` when no turn carried
 * references — a plain recall stays editable text. */
export const recallDocuments = (turns: QueuedTurn[]): UserContent | null => {
  if (!turns.some((turn) => turn.document != null)) return null;
  return mergeDocuments(
    turns.map(
      (turn): UserContent =>
        turn.document ?? {
          content: [{ Text: turn.text }],
          references: {},
        },
    ),
  );
};

/** Append one document to a merge, ahead of it with `joiner` when the merge
 * already has content and some text separates the two pieces: definitions
 * union by id and occurrences follow their definition. Two submissions mint
 * ids independently, so an id both documents use with different definitions
 * renames in the later document, occurrences included. A later document's
 * attachment definition for an upload the merge already names is dropped with
 * its occurrences — the gate admits one definition per upload, and an
 * attachment spells nothing, so the readable form is unchanged. */
export const appendDocument = (
  merged: UserContent,
  document: UserContent | null | undefined,
  joiner = "\n",
): UserContent => {
  if (document == null) return merged;
  if (merged.content.length > 0 && joiner !== "") {
    merged.content.push({ Text: joiner });
  }
  const uploads = new Set(
    Object.values(merged.references).flatMap((definition) =>
      definition.target.type === "attachment" ? [definition.target.name] : [],
    ),
  );
  const dropped = new Set<string>();
  for (const [id, definition] of Object.entries(document.references)) {
    if (definition.target.type === "attachment") {
      if (uploads.has(definition.target.name)) dropped.add(id);
      else uploads.add(definition.target.name);
    }
  }
  const renames = new Map<string, string>();
  for (const [id, definition] of Object.entries(document.references)) {
    if (dropped.has(id)) continue;
    const existing = merged.references[id];
    let target = id;
    if (
      existing !== undefined &&
      JSON.stringify(existing) !== JSON.stringify(definition)
    ) {
      let suffix = 2;
      while (
        merged.references[`${id}-${suffix}`] !== undefined ||
        document.references[`${id}-${suffix}`] !== undefined
      ) {
        suffix += 1;
      }
      target = `${id}-${suffix}`;
    }
    renames.set(id, target);
    merged.references[target] = definition;
  }
  for (const segment of document.content) {
    if ("Text" in segment) {
      merged.content.push({ Text: segment.Text });
    } else if (!dropped.has(segment.Reference)) {
      merged.content.push({
        Reference: renames.get(segment.Reference) ?? segment.Reference,
      });
    }
  }
  return merged;
};

/** Merge recalled documents into one, or `null` when nothing was recalled in
 * document form. Segments concatenate in order with a `Text` joiner between
 * documents. */
const mergeDocuments = (
  documents: (UserContent | null | undefined)[],
  joiner = "\n",
): UserContent | null => {
  const present = documents.filter((d): d is UserContent => d != null);
  if (present.length === 0) return null;
  let merged: UserContent = { content: [], references: {} };
  for (const document of present) {
    merged = appendDocument(merged, document, joiner);
  }
  const carries =
    merged.content.some((s) => "Reference" in s || s.Text.trim() !== "") ||
    Object.keys(merged.references).length > 0;
  return carries ? merged : null;
};

/** Append resolved uploads as attachment references after the prose, minting
 * `r1…` while skipping ids the document already carries — the client-side
 * mirror of the server's `with_attachments`. An upload the document already
 * references by store name is skipped: the gate admits one definition per
 * upload, so a recalled documented turn's restored chips are sent once. */
export const withAttachments = (
  content: UserContent,
  arrivals: Arrival[],
): UserContent => {
  const merged: UserContent = {
    content: [...content.content],
    references: { ...content.references },
  };
  const names = new Set(
    Object.values(merged.references).flatMap((definition) =>
      definition.target.type === "attachment" ? [definition.target.name] : [],
    ),
  );
  let n = Object.keys(merged.references).length;
  for (const arrival of arrivals) {
    if (names.has(arrival.name)) continue;
    let id = `r${n + 1}`;
    while (merged.references[id] !== undefined) {
      n += 1;
      id = `r${n + 1}`;
    }
    merged.content.push({ Reference: id });
    merged.references[id] = {
      label: arrival.name,
      target: { type: "attachment", ...arrival },
    };
    names.add(arrival.name);
    n += 1;
  }
  return merged;
};

/** Remove the attachment definition for store upload `name` with every
 * occurrence that pointed at it — what a chip removal does to a held
 * document, so it cannot resubmit an upload that was taken back. */
export const detachAttachment = (
  content: UserContent,
  name: string,
): UserContent => {
  const references: Record<string, ReferenceDefinition> = {};
  for (const [id, definition] of Object.entries(content.references)) {
    if (
      definition.target.type === "attachment" &&
      definition.target.name === name
    )
      continue;
    references[id] = definition;
  }
  return {
    content: content.content.filter(
      (segment) =>
        !("Reference" in segment) ||
        references[segment.Reference] !== undefined,
    ),
    references,
  };
};
