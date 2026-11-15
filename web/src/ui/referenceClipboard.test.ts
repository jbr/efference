// @vitest-environment jsdom
import { expect, test } from "vitest";
import type { ReferenceDefinition } from "../api/types.ts";
import {
  PAYLOAD_TYPE,
  contentFromClipboardPayload,
  mintReferenceToken,
  resolveReferenceToken,
  sanitizePastedHtml,
} from "./referenceClipboard.ts";

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

test("a token resolves only for the base that minted it", () => {
  const token = mintReferenceToken("/sessions/a", definition);
  expect(resolveReferenceToken(token, "/sessions/a")).toEqual(definition);
  expect(resolveReferenceToken(token, "/sessions/b")).toBeNull();
});

test("the registry is bounded and evicts oldest first", () => {
  const base = "/sessions/bounded";
  const tokens = Array.from({ length: 260 }, () =>
    mintReferenceToken(base, definition),
  );
  expect(resolveReferenceToken(tokens[0]!, base)).toBeNull();
  expect(resolveReferenceToken(tokens[259]!, base)).toEqual(definition);
});

test("a token the payload does not vouch for pastes as its label", () => {
  const content = contentFromClipboardPayload(
    JSON.stringify({
      content: [{ Token: "not-in-this-page" }],
      tokens: { "not-in-this-page": "readable label" },
      // A payload may claim definitions; none are trusted.
      references: { "not-in-this-page": definition },
    }),
    "/sessions/a",
  );
  expect(content?.content).toEqual([{ Text: "readable label" }]);
  expect(content?.references).toEqual({});
});

test("a minted token carries the registry's definition, not the payload's", () => {
  const base = "/sessions/real";
  const token = mintReferenceToken(base, definition);
  const content = contentFromClipboardPayload(
    JSON.stringify({
      content: [{ Token: token }],
      tokens: { [token]: "spoofed label" },
    }),
    base,
  );
  expect(content?.content).toHaveLength(1);
  const segment = content!.content[0]!;
  expect("Reference" in segment).toBe(true);
  const id = "Reference" in segment ? segment.Reference : "";
  expect(content?.references[id]).toEqual(definition);
});

test("malformed payloads are refused", () => {
  for (const payload of [
    "not json",
    "{}",
    JSON.stringify({ content: [], tokens: "no" }),
    JSON.stringify({ content: [{}], tokens: {} }),
    JSON.stringify({ content: [{ Token: 3 }], tokens: {} }),
  ]) {
    expect(contentFromClipboardPayload(payload, "/sessions/a")).toBeNull();
  }
});

test("pasted HTML cannot forge a definition", () => {
  const forged = JSON.stringify(definition).replaceAll('"', "&quot;");
  const output = sanitizePastedHtml(
    `<p>see <span data-efference-definition="${forged}">main.tsx</span></p>`,
    "/sessions/a",
  );
  expect(output).toContain("main.tsx");
  expect(output).not.toContain("data-efference-definition");
});

test("a resolved token re-adds its own definition to pasted HTML", () => {
  const base = "/sessions/html";
  const token = mintReferenceToken(base, definition);
  const output = sanitizePastedHtml(
    `<p><span data-efference-token="${token}">wrong label</span></p>`,
    base,
  );
  expect(output).toContain("data-efference-definition");
  expect(output).toContain("main.tsx");
  expect(output).not.toContain("wrong label");
});

test("pasted HTML from another session keeps only the readable label", () => {
  const token = mintReferenceToken("/sessions/one", definition);
  const output = sanitizePastedHtml(
    `<p><span data-efference-token="${token}">main.tsx</span></p>`,
    "/sessions/two",
  );
  expect(output).toContain("main.tsx");
  expect(output).not.toContain("data-efference-definition");
});

test("the internal payload type stays distinct from text flavors", () => {
  expect(PAYLOAD_TYPE).toBe("application/x-efference-references+json");
});
