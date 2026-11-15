import type { JSONContent } from "@tiptap/core";
import type { EditorView } from "@tiptap/pm/view";
import type { Slice } from "@tiptap/pm/model";
import { normalizeDocument, isUserContent } from "../api/document.ts";
import type { ReferenceDefinition, UserContent } from "../api/types.ts";
import { readableDefinition, userContentFromEditor } from "./composerCore.tsx";

/** The internal flavor. It carries structure and tokens, never definitions:
 * a definition is admitted only by resolving its token in this page's
 * session-scoped registry. */
export const PAYLOAD_TYPE = "application/x-efference-references+json";

const MAX_TOKENS = 256;
const registry = new Map<
  string,
  { base: string; definition: ReferenceDefinition }
>();

/** Tokens are minted on copy/cut only, never per keystroke. The cap keeps a
 * long-lived page bounded; an evicted token pastes as its readable label. */
export const mintReferenceToken = (
  base: string,
  definition: ReferenceDefinition,
): string => {
  const token = crypto.randomUUID();
  if (registry.size >= MAX_TOKENS) {
    const oldest = registry.keys().next().value;
    if (oldest !== undefined) registry.delete(oldest);
  }
  registry.set(token, { base, definition });
  return token;
};

/** Scope is deliberate: a reference asserts access in the session that
 * admitted it. Another session, or a reload, gets the readable spelling. */
export const resolveReferenceToken = (
  token: string,
  base: string,
): ReferenceDefinition | null => {
  const entry = registry.get(token);
  return entry && entry.base === base ? entry.definition : null;
};

type Part =
  | { kind: "text"; value: string }
  | { kind: "token"; token: string; label: string };

export interface ComposerClipboard {
  text: string;
  html: string;
  payload: string;
}

const escapeText = (value: string): string =>
  value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;");

const escapeAttribute = (value: string): string =>
  escapeText(value).replaceAll('"', "&quot;");

const partsForDocument = (
  document: UserContent,
  base: string,
): { text: string; parts: Part[] } => {
  let text = "";
  const parts: Part[] = [];
  for (const segment of document.content) {
    if ("Text" in segment) {
      text += segment.Text;
      parts.push({ kind: "text", value: segment.Text });
      continue;
    }
    const definition = document.references[segment.Reference];
    if (definition === undefined) continue;
    text += readableDefinition(definition);
    if (definition.target.type === "attachment") {
      // One occurrence per upload: a copied attachment is its readable name,
      // never a second reference (docs/control-api.md).
      parts.push({ kind: "text", value: definition.label });
    } else {
      parts.push({
        kind: "token",
        token: mintReferenceToken(base, definition),
        label: definition.label,
      });
    }
  }
  return { text, parts };
};

const linesOf = (parts: Part[]): Part[][] => {
  const lines: Part[][] = [[]];
  for (const part of parts) {
    if (part.kind === "token") {
      lines[lines.length - 1]!.push(part);
      continue;
    }
    part.value.split("\n").forEach((piece, index) => {
      if (index > 0) lines.push([]);
      if (piece !== "")
        lines[lines.length - 1]!.push({ kind: "text", value: piece });
    });
  }
  return lines;
};

const htmlOf = (parts: Part[]): string =>
  linesOf(parts)
    .map((line) => {
      const body = line
        .map((part) =>
          part.kind === "text"
            ? escapeText(part.value)
            : `<span data-efference-token="${escapeAttribute(part.token)}">${escapeText(part.label)}</span>`,
        )
        .join("");
      return `<p style="white-space: pre-wrap">${body}</p>`;
    })
    .join("");

const payloadOf = (parts: Part[]): string =>
  JSON.stringify({
    content: parts.map((part) =>
      part.kind === "text" ? { Text: part.value } : { Token: part.token },
    ),
    tokens: Object.fromEntries(
      parts.flatMap((part) =>
        part.kind === "token" ? [[part.token, part.label]] : [],
      ),
    ),
  });

/** A selection serialized once: exact readable text, token HTML for browsers
 * that drop the internal flavor, and the internal payload for this page. */
export const clipboardForSlice = (
  slice: Slice,
  base: string,
): ComposerClipboard => {
  const document = userContentFromEditor({
    type: "doc",
    content: slice.content.toJSON() as JSONContent[],
  });
  const { text, parts } = partsForDocument(document, base);
  return { text, html: htmlOf(parts), payload: payloadOf(parts) };
};

/** Resolve only registry tokens. Unknown or stale tokens become their label
 * as ordinary text; the payload's own definitions are never trusted. */
export const contentFromClipboardPayload = (
  payload: string,
  base: string,
): UserContent | null => {
  let parsed: unknown;
  try {
    parsed = JSON.parse(payload);
  } catch {
    return null;
  }
  if (
    typeof parsed !== "object" ||
    parsed === null ||
    !("content" in parsed) ||
    !Array.isArray(parsed.content) ||
    !("tokens" in parsed) ||
    typeof parsed.tokens !== "object" ||
    parsed.tokens === null
  )
    return null;
  const labels = parsed.tokens as Record<string, unknown>;
  const content: UserContent = { content: [], references: {} };
  for (const part of parsed.content) {
    if (typeof part !== "object" || part === null) return null;
    if ("Text" in part) {
      if (typeof part.Text !== "string") return null;
      content.content.push({ Text: part.Text });
    } else if ("Token" in part) {
      if (typeof part.Token !== "string") return null;
      const definition = resolveReferenceToken(part.Token, base);
      if (definition === null) {
        const label = labels[part.Token];
        content.content.push({ Text: typeof label === "string" ? label : "" });
      } else {
        // Fresh per pasted occurrence; the clipboard never supplies ids.
        const id = crypto.randomUUID();
        content.content.push({ Reference: id });
        content.references[id] = definition;
      }
    } else {
      return null;
    }
  }
  return normalizeDocument(content);
};

const BLOCK = new Set([
  "ADDRESS",
  "ARTICLE",
  "ASIDE",
  "BLOCKQUOTE",
  "DD",
  "DIV",
  "DL",
  "DT",
  "FIELDSET",
  "FIGCAPTION",
  "FIGURE",
  "FOOTER",
  "FORM",
  "H1",
  "H2",
  "H3",
  "H4",
  "H5",
  "H6",
  "HEADER",
  "HR",
  "LI",
  "MAIN",
  "NAV",
  "OL",
  "P",
  "PRE",
  "SECTION",
  "TABLE",
  "TBODY",
  "TD",
  "TFOOT",
  "TH",
  "THEAD",
  "TR",
  "UL",
]);

const SKIP = new Set(["HEAD", "NOSCRIPT", "SCRIPT", "STYLE", "TEMPLATE"]);

/** Flatten arbitrary pasted HTML to paragraphs. Every `data-efference-*`
 * attribute in the input is ignored; a token is re-added to the output only
 * if it resolves, and its definition is the registry's. */
export const sanitizePastedHtml = (html: string, base: string): string => {
  const parsed = new DOMParser().parseFromString(html, "text/html");
  const markers: { token: string; label: string }[] = [];

  const walk = (node: Node): string => {
    if (node.nodeType === 3) {
      return (node.textContent ?? "").replaceAll("\uE000", "");
    }
    if (node.nodeType !== 1) return "";
    const element = node as HTMLElement;
    const tag = element.tagName.toUpperCase();
    if (SKIP.has(tag)) return "";
    if (tag === "BR") return "\n";
    if (tag === "IMG") return element.getAttribute("alt") ?? "";
    if (tag === "SPAN") {
      const token = element.getAttribute("data-efference-token");
      if (token) {
        const index = markers.length;
        markers.push({ token, label: element.textContent ?? "" });
        return `\uE000EFFREF${index}\uE000`;
      }
    }
    let body = "";
    element.childNodes.forEach((child) => {
      body += walk(child);
    });
    return BLOCK.has(tag) ? `${body}\n` : body;
  };

  const raw = walk(parsed.body);
  const lines = raw.split("\n");
  while (lines.length > 0 && lines[0] === "") lines.shift();
  while (lines.length > 0 && lines[lines.length - 1] === "") lines.pop();
  if (lines.length === 0) return "";

  return lines
    .map((line) => {
      let output = "";
      let consumed = 0;
      const pattern = /\uE000EFFREF(\d+)\uE000/g;
      for (const match of line.matchAll(pattern)) {
        output += escapeText(line.slice(consumed, match.index));
        const marker = markers[Number(match[1])];
        const definition =
          marker === undefined
            ? null
            : resolveReferenceToken(marker.token, base);
        if (definition === null) {
          output += escapeText(marker?.label ?? "");
        } else {
          output += `<span data-efference-definition="${escapeAttribute(JSON.stringify(definition))}">${escapeText(definition.label)}</span>`;
        }
        consumed = match.index + match[0].length;
      }
      output += escapeText(line.slice(consumed));
      return `<p style="white-space: pre-wrap">${output}</p>`;
    })
    .join("");
};

/** Copy keeps the reference definitions in the page; cut is copy then delete,
 * so the token survives the removal and an immediate paste restores it. */
export const copySelection = (
  view: EditorView,
  event: ClipboardEvent,
  base: string,
): boolean => {
  if (view.state.selection.empty) return false;
  const clipboard = clipboardForSlice(view.state.selection.content(), base);
  const data = event.clipboardData;
  if (!data) return false;
  data.setData("text/plain", clipboard.text);
  data.setData("text/html", clipboard.html);
  try {
    data.setData(PAYLOAD_TYPE, clipboard.payload);
  } catch {
    // Some browsers refuse custom flavors; the token HTML remains.
  }
  event.preventDefault();
  return true;
};

export const cutSelection = (
  view: EditorView,
  event: ClipboardEvent,
  base: string,
): boolean => {
  if (!copySelection(view, event, base)) return false;
  view.dispatch(view.state.tr.deleteSelection());
  return true;
};

/** Re-exported so a caller that already has a document (a reload, a test)
 * gets the same shape check the store uses. */
export { isUserContent };
