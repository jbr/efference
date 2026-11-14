import { describe, expect, it } from "vitest";
import {
  applyTextEdit,
  detachAttachment,
  documentReadable,
  mergeDocuments,
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

/** The shape the TUI's editable_document builds: prose, an agent occurrence,
 * more prose, then a zero-width attachment occurrence. */
const editableDocument = (): UserContent => ({
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

describe("applyTextEdit", () => {
  it("keeps an occurrence the edit does not reach", () => {
    const document = editableDocument();
    const edited = applyTextEdit(
      document,
      documentReadable(document),
      "look at @castback now",
    );
    expect(documentReadable(edited)).toBe("look at @castback now");
    expect(edited.references.a).toBeDefined();
    expect(edited.references.b).toBeDefined();
  });

  it("replaces an occurrence the edit reaches with its kept spelling", () => {
    const document = editableDocument();
    const edited = applyTextEdit(
      document,
      documentReadable(document),
      "look at @other please",
    );
    expect(documentReadable(edited)).toBe("look at @other please");
    expect(edited.references.a).toBeUndefined();
    expect(edited.references.b).toBeDefined();
  });

  it("keeps an attachment-only document when text is typed into it", () => {
    const document: UserContent = {
      content: [{ Reference: "b" }],
      references: {
        b: {
          label: "shot.png",
          target: { type: "attachment", ...arrival("shot.png") },
        },
      },
    };
    const edited = applyTextEdit(document, "", "describe this");
    expect(documentReadable(edited)).toBe("describe this");
    expect(edited.references.b).toBeDefined();
  });
});

describe("recallDocuments", () => {
  it("merges a documented turn and a plain one over the draft text", () => {
    const document = editableDocument();
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

  it("keeps an edited hold's references through a second recall", () => {
    const document = editableDocument();
    const spliced = documentReadable(document);
    const edited = applyTextEdit(
      document,
      spliced,
      "look at @castback thoroughly",
    );
    const merged = mergeDocuments([document, edited]);
    expect(documentReadable(merged!)).toBe(
      "look at @castback please\nlook at @castback thoroughly",
    );
    expect(merged!.references.a).toBeDefined();
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
