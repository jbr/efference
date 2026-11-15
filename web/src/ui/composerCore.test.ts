import { expect, test } from "vitest";
import { textDocument } from "../api/document.ts";
import type { ReferenceDefinition, UserContent } from "../api/types.ts";
import {
  editorContentFromUserContent,
  textFromDocument,
  userContentFromEditor,
} from "./composerCore.tsx";

const roundTrip = (content: UserContent): UserContent =>
  userContentFromEditor(editorContentFromUserContent(content));

test("plain text keeps blank interior lines and trailing newlines", () => {
  for (const text of [
    "a\n\nb",
    "a\n\nb\n",
    "a\n\nb\n\n",
    "\nhello",
    "\n\nhello\n",
    "one line",
    "",
  ]) {
    const document = textDocument(text);
    expect(textFromDocument(roundTrip(document))).toBe(text);
  }
});

test("a reference between text keeps its position and definition", () => {
  const definition: ReferenceDefinition = {
    label: "main.tsx",
    target: {
      type: "filesystem",
      original: "./main.tsx",
      path: "/w/main.tsx",
      target: "file",
      access: { read: true, write: false },
    },
  };
  const input: UserContent = {
    content: [
      { Text: "look at " },
      { Reference: "r1" },
      { Text: " before editing\n\nthanks" },
    ],
    references: { r1: definition },
  };
  const output = roundTrip(input);
  expect(output.content).toEqual(input.content);
  expect(output.references.r1).toEqual(definition);
  expect(textFromDocument(output)).toBe(
    "look at ./main.tsx before editing\n\nthanks",
  );
});

test("a duplicated attachment occurrence is stored once", () => {
  const reference: ReferenceDefinition = {
    label: "notes.txt",
    target: {
      type: "attachment",
      kind: { Binary: { bytes: 5 } },
      name: "notes.txt",
      path: "/tmp/notes.txt",
    },
  };
  const json = editorContentFromUserContent({
    content: [{ Reference: "a" }, { Text: " and " }, { Reference: "b" }],
    references: { a: reference, b: reference },
  });
  const output = userContentFromEditor(json);
  expect(
    output.content.filter((segment) => "Reference" in segment),
  ).toHaveLength(1);
  expect(Object.values(output.references)).toEqual([reference]);
});
