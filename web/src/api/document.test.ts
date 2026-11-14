import { describe, expect, it } from "vitest";
import {
  appendDocument,
  detachAttachment,
  documentReadable,
  recallDocuments,
  withAttachments,
} from "./document.ts";
import type { Arrival, QueuedTurn, UserContent } from "./types.ts";

const arrival = (name: string): Arrival => ({
  name,
  path: `att/composer/${name}`,
  kind: { Binary: { bytes: 3 } },
});

const agent = (session: string) => ({
  label: "reviewer",
  target: { type: "agent" as const, session },
});

/** Prose, an agent occurrence, more prose, then a zero-width attachment
 * occurrence — a recalled turn that carries both kinds of reference. */
const reviewDocument = (): UserContent => ({
  content: [
    { Text: "look at " },
    { Reference: "a" },
    { Text: " please" },
    { Reference: "b" },
  ],
  references: {
    a: agent("castback"),
    b: {
      label: "shot.png",
      target: { type: "attachment", ...arrival("shot.png") },
    },
  },
});

const queued = (document: UserContent, id: string): QueuedTurn => ({
  submission_id: id,
  text: documentReadable(document),
  document,
});

/** An attachment-only document: its readable form is empty, so a hold of it
 * has no text to splice and must still carry its definitions. */
const attachmentOnly = (name: string): UserContent => ({
  content: [{ Reference: "r1" }],
  references: {
    r1: {
      label: name,
      target: { type: "attachment", ...arrival(name) },
    },
  },
});

describe("recallDocuments", () => {
  it("merges a documented turn and a plain one over the draft text", () => {
    const document = reviewDocument();
    const merged = recallDocuments([
      queued(document, "s1"),
      {
        submission_id: "s2",
        text: "second",
        attachments: [],
      },
    ]);
    expect(documentReadable(merged!)).toBe("look at @castback please\nsecond");
    expect(merged!.references.a).toBeDefined();
  });

  it("keeps an attachment-only hold through a second recall", () => {
    const held = attachmentOnly("shot.png");
    expect(documentReadable(held)).toBe("");
    // The recall has no document of its own (a plain turn), so the composer
    // starts from an empty document and appends the hold with no separator.
    const merged = appendDocument({ content: [], references: {} }, held, "");
    expect(documentReadable(merged)).toBe("");
    expect(Object.keys(merged.references)).toHaveLength(1);
  });

  it("references an upload once when two recalled turns carry it", () => {
    const merged = recallDocuments([
      queued(attachmentOnly("shot.png"), "s1"),
      queued(attachmentOnly("shot.png"), "s2"),
    ]);
    expect(Object.keys(merged!.references)).toHaveLength(1);
    expect(
      merged!.content.filter((segment) => "Reference" in segment),
    ).toHaveLength(1);
  });
});

describe("attachment bookkeeping", () => {
  const proseDocument = (): UserContent => ({
    content: [{ Text: "look " }, { Reference: "a" }],
    references: { a: agent("castback") },
  });

  it("appends an upload once and skips one the document already names", () => {
    const document = proseDocument();
    const withOne = withAttachments(document, [arrival("shot.png")]);
    expect(documentReadable(withOne)).toBe("look @castback");
    const attachments = Object.values(withOne.references).filter(
      (definition) => definition.target.type === "attachment",
    );
    expect(attachments).toHaveLength(1);
    const twice = withAttachments(withOne, [arrival("shot.png")]);
    expect(Object.keys(twice.references)).toHaveLength(2);
  });

  it("detaches the occurrence and definition a chip removal names", () => {
    const document = withAttachments(proseDocument(), [arrival("shot.png")]);
    const id = Object.entries(document.references).find(
      ([, definition]) => definition.target.type === "attachment",
    )![0];
    const detached = detachAttachment(document, "shot.png");
    expect(detached.references[id]).toBeUndefined();
    expect(documentReadable(detached)).toBe("look @castback");
  });
});
