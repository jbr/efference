// The draft's own content model, independent of any editor: an upload
// occurrence projects to a wire reference only once its record is ready, and
// one that cannot be sent is reported rather than dropped.

import { expect, test } from "vitest";
import type { Arrival, ReferenceDefinition } from "../api/types.ts";
import {
  appendDraft,
  asUserContent,
  attachmentDefinition,
  detachUpload,
  draftReadable,
  draftUploadKeys,
  hasDraftStructure,
  normalizeDraftContent,
  wireDocument,
  type DraftContent,
} from "./draftDocument.ts";
import type { DraftUpload } from "./staging.ts";

const arrival: Arrival = {
  name: "note.txt",
  path: "/stored/note.txt",
  kind: { Binary: { bytes: 5 } },
};
const definition = attachmentDefinition(arrival);
const ready = (key: string): DraftUpload => ({
  key,
  label: "note.txt",
  preview: null,
  status: "ready",
  attachment: arrival,
  provenance: "draft",
});
const pending = (key: string): DraftUpload => ({
  key,
  label: "note.txt",
  preview: null,
  status: "uploading",
  provenance: "draft",
});
const failed = (key: string): DraftUpload => ({
  key,
  label: "note.txt",
  preview: null,
  status: "failed",
  error: "Upload failed (offline); attach this file again.",
  provenance: "draft",
});
const content = (
  segments: DraftContent["content"],
  references: Record<string, ReferenceDefinition> = {},
): DraftContent => ({ content: segments, references });

test("a ready upload projects to a wire reference under its local key", () => {
  const draft = content([{ Text: "look " }, { Upload: "u1" }]);
  const wire = wireDocument(draft, [ready("u1")]);
  expect(wire.blocked).toEqual([]);
  expect(wire.document).toEqual({
    content: [{ Text: "look " }, { Reference: "u1" }],
    references: { u1: definition },
  });
});

test("a pending or failed upload is reported instead of being omitted", () => {
  for (const record of [pending("u1"), failed("u1")]) {
    const wire = wireDocument(content([{ Text: "look " }, { Upload: "u1" }]), [
      record,
    ]);
    expect(wire.document).toEqual({
      content: [{ Text: "look " }],
      references: {},
    });
    expect(wire.blocked).toEqual([record]);
  }
});

test("an occurrence with no record stays blocked with a record of its own", () => {
  const wire = wireDocument(content([{ Upload: "gone" }]), []);
  expect(wire.blocked).toHaveLength(1);
  expect(wire.blocked[0]).toMatchObject({ key: "gone", status: "failed" });
  expect(wire.blocked[0]?.error).toContain("no longer available");
});

test("an upload's reference id is renamed around an existing definition", () => {
  const reference = {
    label: "@castback",
    target: { type: "agent" as const, session: "castback" },
  };
  const wire = wireDocument(
    content([{ Reference: "u1" }, { Upload: "u1" }], { u1: reference }),
    [ready("u1")],
  );
  expect(Object.keys(wire.document!.references).sort()).toEqual(["u1", "u1-2"]);
  expect(wire.document!.references["u1-2"]).toEqual(definition);
});

test("a repeated key projects one occurrence", () => {
  const wire = wireDocument(
    content([{ Upload: "u1" }, { Text: " again " }, { Upload: "u1" }]),
    [ready("u1")],
  );
  expect(wire.document).toEqual({
    content: [{ Reference: "u1" }, { Text: " again " }],
    references: { u1: definition },
  });
});

test("an upload whose file the draft already names is dropped, not blocked", () => {
  const wire = wireDocument(
    content([{ Reference: "r1" }, { Upload: "u1" }], { r1: definition }),
    [ready("u1")],
  );
  expect(wire.blocked).toEqual([]);
  expect(wire.document).toEqual({
    content: [{ Reference: "r1" }],
    references: { r1: definition },
  });
});

test("readable text spells an attachment or upload as nothing", () => {
  const draft = content(
    [
      { Text: "look at " },
      { Reference: "r1" },
      { Text: " and " },
      { Upload: "u1" },
    ],
    { r1: definition },
  );
  expect(draftReadable(draft)).toBe("look at  and ");
});

test("detaching an upload keeps the rest of the draft", () => {
  const draft = content([{ Text: "a " }, { Upload: "u1" }, { Text: " b" }]);
  expect(detachUpload(draft, "u1")).toEqual({
    content: [{ Text: "a " }, { Text: " b" }],
    references: {},
  });
  expect(detachUpload(draft, "other")).toEqual(draft);
});

test("appending preserves upload keys and unions definitions", () => {
  const uploaded = content([{ Text: "first " }, { Upload: "u1" }]);
  const second = content([{ Text: "second" }, { Reference: "r1" }], {
    r1: definition,
  });
  const merged = appendDraft(uploaded, second, " ");
  expect(merged.content).toEqual([
    { Text: "first " },
    { Upload: "u1" },
    { Text: " " },
    { Text: "second" },
    { Reference: "r1" },
  ]);
  expect(merged.references).toEqual({ r1: definition });
  expect(draftUploadKeys(merged)).toEqual(["u1"]);
});

test("a merge drops a later definition for an attachment the draft names", () => {
  const merged = appendDraft(
    content([{ Reference: "r1" }], { r1: definition }),
    content([{ Reference: "r9" }, { Text: " tail" }], { r9: definition }),
    "",
  );
  expect(merged.content).toEqual([{ Reference: "r1" }, { Text: " tail" }]);
  expect(Object.keys(merged.references)).toEqual(["r1"]);
});

test("normalization keeps one occurrence per key and prunes unused definitions", () => {
  const normalized = normalizeDraftContent(
    content(
      [
        { Upload: "u1" },
        { Upload: "u1" },
        { Reference: "gone" },
        { Upload: "u2" },
      ],
      { unused: definition },
    ),
  );
  expect(normalized).toEqual({
    content: [{ Upload: "u1" }, { Upload: "u2" }],
    references: {},
  });
});

test("structure means a reference or an upload occurrence", () => {
  expect(hasDraftStructure(null)).toBe(false);
  expect(hasDraftStructure(content([{ Text: "plain" }]))).toBe(false);
  expect(hasDraftStructure(content([{ Upload: "u1" }]))).toBe(true);
  expect(
    hasDraftStructure(content([{ Reference: "r1" }], { r1: definition })),
  ).toBe(true);
});

test("the clipboard's flat form carries no upload occurrences", () => {
  const flattened = asUserContent(
    content([{ Text: "look " }, { Upload: "u1" }, { Reference: "r1" }], {
      r1: definition,
    }),
  );
  expect(flattened).toEqual({
    content: [{ Text: "look " }, { Reference: "r1" }],
    references: { r1: definition },
  });
});
