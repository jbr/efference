import { forwardRef, useEffect, useImperativeHandle, useRef } from "react";
import type { JSONContent } from "@tiptap/core";
import { Placeholder } from "@tiptap/extensions";
import { EditorContent, useEditor } from "@tiptap/react";
import type { UserContent } from "../api/types.ts";
import {
  composerCoreExtensions,
  editorContentFromUserContent,
  textFromDocument,
  userContentFromEditor,
} from "./composerCore.tsx";
import {
  PAYLOAD_TYPE,
  contentFromClipboardPayload,
  copySelection,
  cutSelection,
  sanitizePastedHtml,
} from "./referenceClipboard.ts";
import { pastesAsFile } from "./staging.ts";

export interface ComposerEditorHandle {
  focus: () => void;
  /** Replace the whole draft (a recall's merge); updates arrive through
   * `onChange` on the next edit, not from this call. */
  replace: (content: UserContent) => void;
  /** Put a staged long paste back at the caret, preserving its newlines. */
  insertText: (text: string) => void;
}

export interface ComposerEditorProps {
  /** The session's API base: clipboard tokens are scoped to it. */
  base: string;
  initial: UserContent;
  placeholder: string;
  narrow: boolean;
  onChange: (document: UserContent | null, text: string) => void;
  onSubmit: () => void;
  onEscape: () => void;
  onStageFile: (file: File, pastedText?: string) => void;
}

/** The composer's editable surface. The editor owns the structure; the store
 * receives its projection after every content edit (docs/web-client.md). */
export const ComposerEditor = forwardRef<
  ComposerEditorHandle,
  ComposerEditorProps
>(function ComposerEditor(
  {
    base,
    initial,
    placeholder,
    narrow,
    onChange,
    onSubmit,
    onEscape,
    onStageFile,
  },
  ref,
) {
  const handlers = useRef({
    base,
    narrow,
    onChange,
    onSubmit,
    onEscape,
    onStageFile,
  });
  handlers.current = {
    base,
    narrow,
    onChange,
    onSubmit,
    onEscape,
    onStageFile,
  };
  const placeholderText = useRef(placeholder);
  placeholderText.current = placeholder;

  const editor = useEditor(
    {
      extensions: [
        ...composerCoreExtensions,
        Placeholder.configure({
          placeholder: () => placeholderText.current,
          includeChildren: false,
        }),
      ],
      content: editorContentFromUserContent(initial),
      editorProps: {
        attributes: {
          role: "textbox",
          "aria-multiline": "true",
          "aria-label": "Send a turn",
          class: "composer-editor",
        },
        handleKeyDown(view, event) {
          const current = handlers.current;
          if (event.key === "Escape" && current.narrow) {
            current.onEscape();
            return true;
          }
          if (event.key !== "Enter") return false;
          // An IME's composing Enter commits a candidate, and a phone's Enter
          // is its only newline; both must reach the editor.
          if (event.isComposing || view.composing) return false;
          if (current.narrow || event.shiftKey) return false;
          event.preventDefault();
          current.onSubmit();
          return true;
        },
        handlePaste(_view, event) {
          const data = event.clipboardData;
          if (!data) return false;
          const files = [...data.files];
          if (files.length > 0) {
            event.preventDefault();
            files.forEach((file) => handlers.current.onStageFile(file));
            return true;
          }
          let payload: string | null;
          try {
            payload = data.getData(PAYLOAD_TYPE);
          } catch {
            payload = null;
          }
          if (payload) {
            const content = contentFromClipboardPayload(
              payload,
              handlers.current.base,
            );
            if (content !== null) {
              event.preventDefault();
              editor
                ?.chain()
                .focus()
                .insertContent(editorContentFromUserContent(content))
                .run();
              return true;
            }
          }
          const text = data.getData("text/plain");
          if (pastesAsFile(text)) {
            event.preventDefault();
            handlers.current.onStageFile(
              new File([text], "paste.txt", { type: "text/plain" }),
              text,
            );
            return true;
          }
          return false;
        },
        // ProseMirror runs this for dropped HTML as well as pasted HTML
        // (parseFromClipboard), so both paths reach the same sanitizer.
        transformPastedHTML: (html) => sanitizePastedHtml(html, base),
        handleDOMEvents: {
          copy: (view, event) =>
            copySelection(view, event as ClipboardEvent, base),
          cut: (view, event) =>
            cutSelection(view, event as ClipboardEvent, base),
        },
      },
      onUpdate: ({ editor }) => {
        const document = userContentFromEditor(editor.getJSON());
        handlers.current.onChange(
          Object.keys(document.references).length > 0 ? document : null,
          textFromDocument(document),
        );
      },
    },
    [base],
  );

  // The placeholder is a function of a ref, so an ask arriving while the
  // editor is empty needs a transaction to rebuild the decoration.
  useEffect(() => {
    if (!editor) return;
    editor.view.dispatch(editor.state.tr);
  }, [editor, placeholder]);

  useImperativeHandle(
    ref,
    () => ({
      focus: () => {
        editor?.commands.focus();
      },
      replace: (content) => {
        if (!editor) return;
        editor.commands.setContent(editorContentFromUserContent(content), {
          emitUpdate: false,
        });
        editor.commands.focus("end");
      },
      insertText: (value) => {
        if (!editor || value === "") return;
        const nodes: JSONContent[] = [];
        value.split("\n").forEach((line, index) => {
          if (index > 0) nodes.push({ type: "hardBreak" });
          if (line !== "") nodes.push({ type: "text", text: line });
        });
        if (nodes.length === 0) return;
        editor
          .chain()
          .focus()
          .insertContentAt(editor.state.selection.from, nodes)
          .run();
      },
    }),
    [editor],
  );

  return <EditorContent editor={editor} />;
});
