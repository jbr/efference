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

/** Merge recalled documents into one, or `null` when nothing was recalled in
 * document form. Segments concatenate in order with a `Text` joiner between
 * documents; definitions union by id — two submissions mint ids
 * independently, so an id both documents use with different definitions
 * renames in the later document, occurrences included. */
export const mergeDocuments = (
  documents: (UserContent | null | undefined)[],
  joiner = "\n",
): UserContent | null => {
  const present = documents.filter((d): d is UserContent => d != null);
  if (present.length === 0) return null;
  const merged: UserContent = { content: [], references: {} };
  present.forEach((document, index) => {
    if (index > 0) merged.content.push({ Text: joiner });
    const renames = new Map<string, string>();
    for (const [id, definition] of Object.entries(document.references)) {
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
      } else {
        merged.content.push({
          Reference: renames.get(segment.Reference) ?? segment.Reference,
        });
      }
    }
  });
  const carries =
    merged.content.some((s) => "Reference" in s || s.Text.trim() !== "") ||
    Object.keys(merged.references).length > 0;
  return carries ? merged : null;
};

/** `n` floored to a code-unit boundary of `text`, so a slice never splits a
 * surrogate pair. */
const floorBoundary = (text: string, n: number): number => {
  let cut = Math.min(n, text.length);
  while (cut > 0) {
    const code = text.charCodeAt(cut - 1);
    if (code >= 0xd800 && code <= 0xdbff) cut -= 1;
    else break;
  }
  return cut;
};

const commonPrefix = (a: string, b: string): number => {
  let n = 0;
  while (n < a.length && n < b.length && a[n] === b[n]) n += 1;
  return floorBoundary(a, n);
};

const commonSuffix = (a: string, b: string): number => {
  let n = 0;
  while (
    n < a.length &&
    n < b.length &&
    a[a.length - 1 - n] === b[b.length - 1 - n]
  )
    n += 1;
  while (n > 0) {
    const cut = a.length - n;
    const code = a.charCodeAt(cut);
    if (code >= 0xdc00 && code <= 0xdfff) n -= 1;
    else break;
  }
  return n;
};

/** The document with the change from `oldText` to `newText` applied to its
 * readable form: prose around the edit survives, and an occurrence the edit
 * reaches is replaced by the typed text, its definition dropped when no
 * occurrence remains. `oldText` is the readable form the client last held; a
 * held recall whose draft moved uses this so a second recall keeps the
 * references the edit did not touch instead of flattening the document. */
export const applyTextEdit = (
  content: UserContent,
  oldText: string,
  newText: string,
): UserContent => {
  const prefix = commonPrefix(oldText, newText);
  const suffix = commonSuffix(oldText.slice(prefix), newText.slice(prefix));
  const oldEnd = oldText.length - suffix;
  const inserted = newText.slice(prefix, newText.length - suffix);
  const pieces: UserSegment[] = [];
  const references: Record<string, ReferenceDefinition> = {};
  let insertedDone = false;
  let pos = 0;
  const pushText = (text: string) => {
    if (text !== "") pieces.push({ Text: text });
  };
  for (const segment of content.content) {
    if ("Text" in segment) {
      const text = segment.Text;
      const end = pos + text.length;
      if (end <= prefix || (pos >= oldEnd && insertedDone)) {
        pushText(text);
      } else if (pos >= oldEnd) {
        pushText(inserted);
        insertedDone = true;
        pushText(text);
      } else {
        pushText(text.slice(0, floorBoundary(text, Math.max(0, prefix - pos))));
        if (!insertedDone) {
          pushText(inserted);
          insertedDone = true;
        }
        pushText(text.slice(floorBoundary(text, Math.max(0, oldEnd - pos))));
      }
      pos = end;
      continue;
    }
    const id = segment.Reference;
    const definition = content.references[id];
    if (definition === undefined) continue;
    const length = readableReference(content, id).length;
    const end = pos + length;
    // A zero-width occurrence (an attachment spells nothing) is untouched by
    // an edit that merely starts at it.
    const zeroWidthAtStart = length === 0 && pos === prefix;
    if (end <= prefix || pos >= oldEnd || zeroWidthAtStart) {
      if (pos >= oldEnd && !insertedDone) {
        pushText(inserted);
        insertedDone = true;
      }
      pieces.push({ Reference: id });
      references[id] = definition;
    } else {
      // The edit reaches this occurrence, so it is typed text now: the
      // spelling before and after the edit stays as prose and the occurrence
      // goes.
      const readable = readableReference(content, id);
      pushText(
        readable.slice(0, floorBoundary(readable, Math.max(0, prefix - pos))),
      );
      if (!insertedDone) {
        pushText(inserted);
        insertedDone = true;
      }
      pushText(
        readable.slice(floorBoundary(readable, Math.max(0, oldEnd - pos))),
      );
    }
    pos = end;
  }
  if (!insertedDone) pushText(inserted);
  return { content: pieces, references };
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
