import { Schema } from "@tiptap/pm/model";
import { EditorState, TextSelection } from "@tiptap/pm/state";
import { expect, test } from "vitest";
import { completionMatch } from "./completionMatch.ts";

const schema = new Schema({
  nodes: {
    doc: { content: "paragraph+" },
    paragraph: { content: "inline*" },
    hardBreak: { inline: true, group: "inline" },
    text: { group: "inline" },
  },
});
function state(text: string, from?: number, to?: number) {
  const doc = schema.node(
    "doc",
    null,
    text
      .split("\n")
      .map((line) =>
        schema.node("paragraph", null, line ? schema.text(line) : undefined),
      ),
  );
  return EditorState.create({
    doc,
    selection: TextSelection.create(doc, from ?? doc.content.size - 1, to),
  });
}

test.each(["./", "../src/", "~/notes", "/Users/jbr", "@", "@composer-uploads"])(
  "recognizes boundary prefix %s",
  (prefix) => {
    expect(completionMatch(state(`Please check ${prefix}`))).toMatchObject({
      prefix,
      kind: prefix.startsWith("@") ? "agents" : "paths",
      from: 14,
    });
  },
);
test.each([
  "https://example.com/path",
  "name@example",
  "word./src",
  "plain/path",
  "`./src",
  "``./src",
  "```\n./src",
  "~~~js\n./src",
  "text @agent.",
  "./src then text",
])("does not discover from %s", (text) => {
  expect(completionMatch(state(text))).toBeNull();
});
test("closed code does not disable later completion", () => {
  expect(completionMatch(state("```\ncode\n```\n./src"))?.prefix).toBe("./src");
  expect(completionMatch(state("`literal` ./src"))?.prefix).toBe("./src");
});
test("requires a collapsed caret at the end of the token", () => {
  expect(completionMatch(state("./src/file", 6))).toBeNull();
  expect(completionMatch(state("./src", 1, 6))).toBeNull();
  expect(completionMatch(state("./src rest", 6))?.prefix).toBe("./src");
});

test("hard breaks delimit completion tokens and fenced code lines", () => {
  for (const lines of [
    ["First line", "./src"],
    ["```", "code", "```", "@agent"],
  ]) {
    const content = lines.flatMap((line, index) => [
      ...(index ? [schema.node("hardBreak")] : []),
      schema.text(line),
    ]);
    const doc = schema.node(
      "doc",
      null,
      schema.node("paragraph", null, content),
    );
    const value = EditorState.create({
      doc,
      selection: TextSelection.create(doc, doc.content.size - 1),
    });
    expect(completionMatch(value)?.prefix).toBe(lines.at(-1));
  }
});
