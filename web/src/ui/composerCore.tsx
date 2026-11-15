import { Node, type JSONContent } from "@tiptap/core";
import { Document } from "@tiptap/extension-document";
import { Paragraph } from "@tiptap/extension-paragraph";
import { Text } from "@tiptap/extension-text";
import { HardBreak } from "@tiptap/extension-hard-break";
import { UndoRedo } from "@tiptap/extensions";
import { ReactNodeViewRenderer } from "@tiptap/react";
import {
  documentReadable,
  isReferenceDefinition,
  normalizeDocument,
  textDocument,
} from "../api/document.ts";
import type { ReferenceDefinition, UserContent } from "../api/types.ts";
import { ComposerChip } from "./ComposerChip.tsx";

/** What the clipboard's plain-text flavor spells for a definition. An
 * attachment has no textual capability, but its name stays readable. */
export const readableDefinition = (definition: ReferenceDefinition): string => {
  const target = definition.target;
  if (target.type === "filesystem") return target.original;
  if (target.type === "agent") return `@${target.session}`;
  return definition.label;
};

/** An atomic inline reference. It round-trips as a node, never as an HTML
 * identity: `parseHTML` accepts only the definition attribute that the
 * clipboard sanitizer re-adds after resolving a token, and `renderHTML`
 * emits the readable label alone. */
export const ComposerReference = Node.create({
  name: "reference",
  inline: true,
  group: "inline",
  atom: true,
  selectable: true,
  addAttributes() {
    return { id: { default: null }, definition: { default: null } };
  },
  parseHTML() {
    return [
      {
        tag: "span[data-efference-definition]",
        getAttrs: (element) => {
          const raw = (element as HTMLElement).getAttribute(
            "data-efference-definition",
          );
          if (!raw) return false;
          try {
            const definition: unknown = JSON.parse(raw);
            if (!isReferenceDefinition(definition)) return false;
            return { id: crypto.randomUUID(), definition };
          } catch {
            return false;
          }
        },
      },
    ];
  },
  renderHTML({ node }) {
    const definition = node.attrs.definition as ReferenceDefinition;
    // The node's attrs are not HTML: a copied reference carries its identity
    // in the clipboard token, and external HTML gets the readable label alone.
    return ["span", { class: "reference-clipboard-label" }, definition.label];
  },
  renderText({ node }) {
    return readableDefinition(node.attrs.definition as ReferenceDefinition);
  },
  addNodeView() {
    return ReactNodeViewRenderer(ComposerChip);
  },
});

/** The chat schema. Completion and other composer features add their own
 * extensions on top; no module here exports vendor JSON as a contract. */
export const composerCoreExtensions = [
  Document,
  Paragraph,
  Text,
  HardBreak,
  UndoRedo,
  ComposerReference,
];

/** `"a\n\nb"` loads as three paragraphs, preserving blank lines; a document's
 * newlines are the only textual boundary UserContent has. */
export function editorContentFromUserContent(
  content: UserContent,
): JSONContent {
  const paragraphs: JSONContent[] = [];
  let inline: JSONContent[] = [];
  const flush = () => {
    paragraphs.push(
      inline.length > 0
        ? { type: "paragraph", content: inline }
        : { type: "paragraph" },
    );
    inline = [];
  };
  for (const segment of content.content) {
    if ("Text" in segment) {
      segment.Text.split("\n").forEach((line, index) => {
        if (index > 0) flush();
        if (line !== "") inline.push({ type: "text", text: line });
      });
    } else {
      const definition = content.references[segment.Reference];
      if (definition === undefined) continue;
      inline.push({
        type: "reference",
        attrs: { id: segment.Reference, definition },
      });
    }
  }
  flush();
  return { type: "doc", content: paragraphs };
}

/** The editor is authoritative: this walks its current JSON, mints an id for
 * any node that lacks one, and normalizes attachment occurrences. */
export function userContentFromEditor(json: JSONContent): UserContent {
  const content: UserContent = { content: [], references: {} };
  const attachments = new Set<string>();

  const text = (value: string) => {
    if (value === "") return;
    const last = content.content.at(-1);
    if (last && "Text" in last) last.Text += value;
    else content.content.push({ Text: value });
  };

  const reference = (node: JSONContent) => {
    const definition = node.attrs?.definition as
      ReferenceDefinition | undefined;
    if (definition === undefined) return;
    if (definition.target.type === "attachment") {
      if (attachments.has(definition.target.name)) return;
      attachments.add(definition.target.name);
    }
    let id = node.attrs?.id as string | undefined;
    if (!id || id in content.references) id = crypto.randomUUID();
    content.content.push({ Reference: id });
    content.references[id] = definition;
  };

  const visit = (node: JSONContent) => {
    if (node.type === "text") text(node.text ?? "");
    else if (node.type === "hardBreak") text("\n");
    else if (node.type === "reference") reference(node);
    else if (node.type === "doc") {
      node.content?.forEach((child, index) => {
        if (index > 0) text("\n");
        visit(child);
      });
    } else {
      node.content?.forEach(visit);
    }
  };

  visit(json);
  return normalizeDocument(content);
}

/** The draft's text spelling, shared with the transcript's pending echo. */
export const textFromDocument = (content: UserContent): string =>
  documentReadable(content);

/** The store's plain text as the editor's initial document. */
export const initialDocument = (
  document: UserContent | null,
  text: string,
): UserContent => document ?? textDocument(text);
