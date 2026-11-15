import { useState } from "react";
import { Node, mergeAttributes, type JSONContent } from "@tiptap/core";
import { Document } from "@tiptap/extension-document";
import { Paragraph } from "@tiptap/extension-paragraph";
import { Text } from "@tiptap/extension-text";
import { HardBreak } from "@tiptap/extension-hard-break";
import { UndoRedo } from "@tiptap/extensions";
import { closeHistory } from "@tiptap/pm/history";
import {
  EditorContent,
  NodeViewWrapper,
  ReactNodeViewRenderer,
  useEditor,
  type NodeViewProps,
} from "@tiptap/react";
import type { ReferenceDefinition, UserContent } from "../api/types.ts";
import { ReferenceChip } from "../ui/ComposerReferences.tsx";
import "./InlineComposerSketch.css";

function ChipView({
  node,
  editor,
  updateAttributes,
  deleteNode,
}: NodeViewProps) {
  const definition = node.attrs.definition as ReferenceDefinition;
  const target = definition.target;
  return (
    <NodeViewWrapper
      as="span"
      className="inline-reference-node"
      contentEditable={false}
    >
      <ReferenceChip
        label={definition.label}
        detail={
          target.type === "filesystem"
            ? target.path
            : target.type === "agent"
              ? `Exact session: ${target.session}`
              : target.name
        }
        kind={target.type === "filesystem" ? target.target : target.type}
        write={target.type === "filesystem" && target.access.write}
        onWriteChange={(write) => {
          if (target.type === "filesystem") {
            updateAttributes({
              definition: {
                ...definition,
                target: { ...target, access: { read: true, write } },
              },
            });
          }
        }}
        onRemove={() => {
          editor.view.dispatch(closeHistory(editor.state.tr));
          deleteNode();
        }}
      />
    </NodeViewWrapper>
  );
}

// Fixture-only schema (docs/web-client.md). No HTML reference import: pasted
// labels must not become authoritative identities merely by resembling chips.
const SketchReference = Node.create({
  name: "reference",
  inline: true,
  group: "inline",
  atom: true,
  selectable: true,
  addAttributes() {
    return { id: { default: null }, definition: { default: null } };
  },
  renderHTML({ node, HTMLAttributes }) {
    return [
      "span",
      mergeAttributes(HTMLAttributes, { class: "reference-clipboard-label" }),
      (node.attrs.definition as ReferenceDefinition).label,
    ];
  },
  renderText({ node }) {
    const definition = node.attrs.definition as ReferenceDefinition;
    const target = definition.target;
    return target.type === "filesystem"
      ? target.original
      : target.type === "agent"
        ? `@${target.session}`
        : definition.label;
  },
  addNodeView() {
    return ReactNodeViewRenderer(ChipView);
  },
});

function sketchDocument(json: JSONContent): UserContent {
  const document: UserContent = { content: [], references: {} };
  function text(value: string) {
    const last = document.content.at(-1);
    if (last && "Text" in last) last.Text += value;
    else if (value) document.content.push({ Text: value });
  }
  function visit(node: JSONContent) {
    if (node.type === "text") text(node.text ?? "");
    else if (node.type === "hardBreak") text("\n");
    else if (node.type === "reference") {
      const id = node.attrs!.id as string;
      document.content.push({ Reference: id });
      document.references[id] = node.attrs!.definition as ReferenceDefinition;
    } else {
      node.content?.forEach((child, index) => {
        if (node.type === "doc" && index > 0) text("\n");
        visit(child);
      });
    }
  }
  visit(json);
  return document;
}

export function InlineComposerSketch() {
  const [document, setDocument] = useState<UserContent>({
    content: [
      { Text: "Please check  before sending.\n**Markdown stays literal.**" },
    ],
    references: {},
  });
  const editor = useEditor({
    extensions: [
      Document,
      Paragraph,
      Text,
      HardBreak,
      UndoRedo,
      SketchReference,
    ],
    content: {
      type: "doc",
      content: [
        {
          type: "paragraph",
          content: [
            { type: "text", text: "Please check " },
            { type: "text", text: " before sending." },
          ],
        },
        {
          type: "paragraph",
          content: [{ type: "text", text: "**Markdown stays literal.**" }],
        },
      ],
    },
    editorProps: {
      attributes: {
        "aria-label": "Inline reference editor",
        role: "textbox",
        "aria-multiline": "true",
        class: "inline-composer-editor",
      },
    },
    onUpdate: ({ editor }) => setDocument(sketchDocument(editor.getJSON())),
  });
  function insert(kind: "file" | "agent") {
    if (!editor) return;
    const definition: ReferenceDefinition =
      kind === "file"
        ? {
            label: "main.tsx",
            target: {
              type: "filesystem",
              original: "./web/src/main.tsx",
              path: "/fixture/web/src/main.tsx",
              target: "file",
              access: { read: true, write: false },
            },
          }
        : {
            label: "@new-lilac",
            target: { type: "agent", session: "new-lilac" },
          };
    editor
      .chain()
      .focus()
      .insertContent({
        type: "reference",
        attrs: { id: crypto.randomUUID(), definition },
      })
      .run();
  }
  return (
    <div className="inline-composer-sketch">
      <p className="g-intro">
        Real inline editing; fixture identities only. Nothing is sent or
        authorized.
      </p>
      <div className="composer-input">
        <EditorContent editor={editor} />
      </div>
      <div className="inline-composer-actions">
        <button className="quiet" onClick={() => insert("file")}>
          Insert file at caret
        </button>
        <button className="quiet" onClick={() => insert("agent")}>
          Insert agent at caret
        </button>
        <button
          className="quiet"
          onClick={() => editor?.chain().focus().undo().run()}
        >
          Undo
        </button>
        <button
          className="quiet"
          onClick={() => editor?.chain().focus().redo().run()}
        >
          Redo
        </button>
        <button
          className="quiet"
          onClick={() => editor?.commands.clearContent()}
        >
          Clear
        </button>
      </div>
      <details>
        <summary>Structured draft</summary>
        <pre className="inline-composer-output">
          {JSON.stringify(document, null, 2)}
        </pre>
      </details>
      <p className="g-intro">
        Try typing on either side of a chip, selecting across it, deleting, and
        undoing. Clipboard currently exports readable labels; pasted labels are
        ordinary text, not restored references.
      </p>
    </div>
  );
}
