import { useState } from "react";
import type { JSONContent } from "@tiptap/core";
import { EditorContent, useEditor } from "@tiptap/react";
import type { ReferenceDefinition, UserContent } from "../api/types.ts";
import {
  composerCoreExtensions,
  editorContentFromUserContent,
  textFromDocument,
  userContentFromEditor,
} from "../ui/composerCore.tsx";
import "./InlineComposerSketch.css";

const fixture: UserContent = {
  content: [
    { Text: "Please check " },
    { Reference: "r1" },
    { Text: " before sending.\n**Markdown stays literal.**" },
  ],
  references: {
    r1: {
      label: "main.tsx",
      target: {
        type: "filesystem",
        original: "./web/src/main.tsx",
        path: "/fixture/web/src/main.tsx",
        target: "file",
        access: { read: true, write: false },
      },
    },
  },
};

const definitions: Record<"file" | "agent", ReferenceDefinition> = {
  file: {
    label: "main.tsx",
    target: {
      type: "filesystem",
      original: "./web/src/main.tsx",
      path: "/fixture/web/src/main.tsx",
      target: "file",
      access: { read: true, write: false },
    },
  },
  agent: {
    label: "@new-lilac",
    target: { type: "agent", session: "new-lilac" },
  },
};

export function InlineComposerSketch() {
  const [document, setDocument] = useState<UserContent>(fixture);
  const editor = useEditor({
    extensions: composerCoreExtensions,
    content: editorContentFromUserContent(fixture),
    editorProps: {
      attributes: {
        "aria-label": "Inline reference editor",
        role: "textbox",
        "aria-multiline": "true",
        class: "inline-composer-editor",
      },
    },
    onUpdate: ({ editor }) => {
      const next = userContentFromEditor(editor.getJSON());
      setDocument(next);
    },
  });

  function insert(kind: "file" | "agent") {
    if (!editor) return;
    const node: JSONContent = {
      type: "reference",
      attrs: { id: crypto.randomUUID(), definition: definitions[kind] },
    };
    editor.chain().focus().insertContent(node).run();
  }

  return (
    <div className="inline-composer-sketch">
      <p className="g-intro">
        The production editor schema; fixture identities only. Nothing is sent
        or authorized.
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
        Readable spelling: {JSON.stringify(textFromDocument(document))}
      </p>
      <p className="g-intro">
        Try typing on either side of a chip, selecting across it, deleting, and
        undoing. The production composer's clipboard boundary is not wired into
        this fixture; there, copies carry identity within the session.
      </p>
    </div>
  );
}
