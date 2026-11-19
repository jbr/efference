import {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useRef,
} from "react";
import { isiOS, isMacOS, type Editor } from "@tiptap/core";
import { Placeholder } from "@tiptap/extensions";
import { EditorContent, useEditor } from "@tiptap/react";
import { TextSelection } from "@tiptap/pm/state";
import {
  ComposerUpload,
  composerCoreExtensions,
  draftContentFromEditor,
  editorContentFromDraft,
  textFromDraft,
} from "./composerCore.tsx";
import { hasDraftStructure, type DraftContent } from "./draftDocument.ts";
import {
  PAYLOAD_TYPE,
  contentFromClipboardPayload,
  copySelection,
  cutSelection,
  sanitizePastedHtml,
} from "./referenceClipboard.ts";
import { pastesAsFile } from "./staging.ts";
import type { DraftStore } from "./drafts.ts";
import { useId } from "react";
import { CompletionMenu, CompletionTray } from "./ComposerReferences.tsx";
import { useComposerCompletion } from "./useComposerCompletion.ts";

export interface ComposerEditorHandle {
  focus: () => void;
  /** Replace the whole draft (a recall's merge or a clear); updates arrive
   * through `onChange` on the next edit, not from this call. */
  replace: (content: DraftContent) => void;
  /** Insert files as inline upload occurrences — the picker and a drop
   * outside the editor's own surface. `replace` replaces the retained
   * selection, as a paste does; `at` inserts at a document position without
   * replacing anything (a pointer drop). */
  insertFiles: (
    files: File[],
    options?: { at?: number; replace?: boolean; pastedText?: string },
  ) => void;
}

export interface ComposerEditorProps {
  /** The session's API base: clipboard tokens are scoped to it. */
  base: string;
  /** The draft store the upload node's status is read from. */
  store: DraftStore;
  initial: DraftContent;
  placeholder: string;
  narrow: boolean;
  onChange: (document: DraftContent | null, text: string) => void;
  onSubmit: () => void;
  onEscape: () => void;
}

/** The composer's editable surface. The editor owns the structure; the store
 * receives its projection after every content edit (docs/web-client.md). An
 * upload's pending → ready transition is a store event, not an editor edit:
 * the node view re-renders and the document, selection and undo stack stay
 * untouched. */
export const ComposerEditor = forwardRef<
  ComposerEditorHandle,
  ComposerEditorProps
>(function ComposerEditor(
  { base, store, initial, placeholder, narrow, onChange, onSubmit, onEscape },
  ref,
) {
  const handlers = useRef({
    base,
    store,
    narrow,
    onChange,
    onSubmit,
    onEscape,
  });
  handlers.current = {
    base,
    store,
    narrow,
    onChange,
    onSubmit,
    onEscape,
  };
  const placeholderText = useRef(placeholder);
  placeholderText.current = placeholder;
  // `insertFiles` is handed to the editor's own paste/drop handlers once, at
  // creation, so it reads the instance through a ref rather than the render
  // that built those handlers.
  const editorRef = useRef<Editor | null>(null);
  const completionKeys = useRef<(event: KeyboardEvent) => boolean>(() => false);
  const completionId = useId();

  const editor = useEditor(
    {
      extensions: [
        ...composerCoreExtensions,
        ComposerUpload.configure({ store }),
        Placeholder.configure({
          placeholder: () => placeholderText.current,
          includeChildren: false,
        }),
      ],
      content: editorContentFromDraft(initial),
      editorProps: {
        attributes: {
          role: "textbox",
          "aria-multiline": "true",
          "aria-label": "Send a turn",
          class: "composer-editor",
        },
        handleKeyDown(view, event) {
          const current = handlers.current;
          if (event.isComposing || view.composing || event.keyCode === 229)
            return false;
          // Tiptap's Ctrl-A goes to the textblock start, skipping hard breaks.
          if (
            (isMacOS() || isiOS()) &&
            event.key === "a" &&
            event.ctrlKey &&
            !event.metaKey &&
            !event.altKey &&
            !event.shiftKey
          ) {
            const { $head } = view.state.selection;
            if ($head.parent.isTextblock) {
              let start = $head.start();
              $head.parent.forEach((node, offset) => {
                const end = offset + node.nodeSize;
                if (end <= $head.parentOffset && node.type.name === "hardBreak")
                  start = $head.start() + end;
              });
              view.dispatch(
                view.state.tr
                  .setSelection(TextSelection.create(view.state.doc, start))
                  .scrollIntoView(),
              );
              return true;
            }
          }
          if (completionKeys.current(event)) return true;
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
          if (!event.repeat) current.onSubmit();
          return true;
        },
        handlePaste(_view, event) {
          const data = event.clipboardData;
          if (!data) return false;
          const files = [...data.files];
          if (files.length > 0) {
            event.preventDefault();
            insertFiles(files, { replace: true });
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
              const instance = editorRef.current;
              if (instance !== null) {
                // No `focus()` here: its scroll-into-view is what a paste
                // does not need, and the caret already follows the insertion.
                instance.commands.insertContent(
                  editorContentFromDraft(content),
                );
              }
              return true;
            }
          }
          const text = data.getData("text/plain");
          if (pastesAsFile(text)) {
            // A paste staged for its length inserts where the user pasted,
            // not at the end of the draft.
            event.preventDefault();
            insertFiles(
              [new File([text], "paste.txt", { type: "text/plain" })],
              { replace: true, pastedText: text },
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
            copySelection(
              view,
              event as ClipboardEvent,
              handlers.current.base,
              (key) => handlers.current.store.uploadLabel(key),
            ),
          cut: (view, event) =>
            cutSelection(
              view,
              event as ClipboardEvent,
              handlers.current.base,
              (key) => handlers.current.store.uploadLabel(key),
            ),
          drop: (view, event) => {
            const drag = event as DragEvent;
            const files = [...(drag.dataTransfer?.files ?? [])];
            if (files.length === 0) return false;
            drag.preventDefault();
            const at = view.posAtCoords({
              left: drag.clientX,
              top: drag.clientY,
            })?.pos;
            insertFiles(files, { at });
            return true;
          },
        },
      },
      onCreate: () => handlers.current.store.beginEditorSession(),
      onDestroy: () => handlers.current.store.endEditorSession(),
      onUpdate: ({ editor }) => {
        const document = draftContentFromEditor(editor.getJSON());
        handlers.current.onChange(
          hasDraftStructure(document) ? document : null,
          textFromDraft(document),
        );
      },
    },
    [base],
  );

  const completion = useComposerCompletion(editor, base);
  completionKeys.current = completion.keyDown;
  const menu = completion.snapshot;
  useEffect(() => {
    const element = editor?.view.dom;
    if (!element) return;
    element.setAttribute("aria-autocomplete", "list");
    element.setAttribute("aria-expanded", String(menu !== null));
    if (menu) {
      element.setAttribute("aria-controls", completionId);
      if (menu.options.length)
        element.setAttribute(
          "aria-activedescendant",
          `${completionId}-${menu.selected}`,
        );
      else element.removeAttribute("aria-activedescendant");
    } else {
      element.removeAttribute("aria-controls");
      element.removeAttribute("aria-activedescendant");
    }
  }, [editor, menu, completionId]);
  useEffect(() => {
    if (!menu?.options.length || !editor) return;
    const shell = editor.view.dom.closest(".composer-editor-shell");
    const row = shell?.querySelector<HTMLElement>(
      '[role="option"][aria-selected="true"]',
    );
    const list = row?.parentElement;
    if (!row || !list) return;
    const bounds = list.getBoundingClientRect();
    const item = row.getBoundingClientRect();
    if (item.bottom > bounds.bottom)
      list.scrollTop += item.bottom - bounds.bottom;
    else if (item.top < bounds.top) list.scrollTop -= bounds.top - item.top;
  }, [editor, menu]);

  // The placeholder is a function of a ref, so an ask arriving while the
  // editor is empty needs a transaction to rebuild the decoration.
  useEffect(() => {
    if (!editor) return;
    editor.view.dispatch(editor.state.tr);
  }, [editor, placeholder]);

  const insertFiles = useCallback(
    (
      files: File[],
      options: { at?: number; replace?: boolean; pastedText?: string } = {},
    ): void => {
      const instance = editorRef.current;
      if (files.length === 0 || instance === null) return;
      const nodes = files.map((file) =>
        instance.schema.nodeFromJSON({
          type: "upload",
          attrs: {
            key: handlers.current.store.uploadFile(file, options.pastedText),
          },
        }),
      );
      // A plain transaction: inserting at the paste or drop position must not
      // scroll the editor, and the caret moves only where the insertion moves
      // it. Pasting or choosing files over a selection replaces it, as text
      // does; a pointer drop is placed where the drop happened.
      const state = instance.state;
      const from = options.replace
        ? state.selection.from
        : Math.min(options.at ?? state.selection.from, state.doc.content.size);
      const to = options.replace ? state.selection.to : from;
      instance.view.dispatch(state.tr.replaceWith(from, to, nodes));
    },
    [],
  );

  useImperativeHandle(
    ref,
    () => ({
      focus: () => {
        // The composer is already on screen: focus moves the caret, it does
        // not scroll a page the user is looking at.
        editor?.commands.focus(null, { scrollIntoView: false });
      },
      replace: (content: DraftContent) => {
        if (!editor) return;
        editor.commands.setContent(editorContentFromDraft(content), {
          emitUpdate: false,
        });
        editor.commands.focus("end", { scrollIntoView: false });
      },
      insertFiles: (files, options) => insertFiles(files, options),
    }),
    [editor, insertFiles],
  );

  editorRef.current = editor;
  return (
    <div className="composer-editor-shell">
      {menu && (
        <CompletionTray>
          <CompletionMenu
            id={completionId}
            options={menu.options}
            selected={menu.selected}
            state={menu.state}
            kind={menu.match.kind === "agents" ? "agent" : "file"}
            onHighlight={completion.highlight}
            onAccept={completion.accept}
          />
          {(menu.match.kind === "agents" || menu.truncated) && (
            <div className="composer-completion-disclosure">
              {menu.match.kind === "agents" &&
                "Reference a session · no notification is sent."}
              {menu.truncated &&
                `${menu.match.kind === "agents" ? " " : ""}More matches — type to narrow.`}
            </div>
          )}
        </CompletionTray>
      )}
      <EditorContent editor={editor} />
    </div>
  );
});
