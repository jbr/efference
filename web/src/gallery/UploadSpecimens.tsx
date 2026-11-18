import { EditorContent, useEditor } from "@tiptap/react";
import type { Arrival } from "../api/types.ts";
import {
  ComposerUpload,
  composerCoreExtensions,
  editorContentFromDraft,
  textFromDraft,
} from "../ui/composerCore.tsx";
import type { DraftContent } from "../ui/draftDocument.ts";
import { DraftStore } from "../ui/drafts.ts";
import { Specimen } from "./Specimens.tsx";

const stored = (name: string): Arrival => ({
  name,
  path: `/gallery/attachments/${name}`,
  kind: { Binary: { bytes: 2048 } },
});

/** One fixture store for the page. Uploads are answered by name: the pending
 * file never lands, the failed one is refused, and the ready ones resolve on a
 * microtask. No request leaves the page. */
let fixture: { store: DraftStore; content: DraftContent } | null = null;
function uploadFixture() {
  if (fixture !== null) return fixture;
  const store = new DraftStore("gallery://uploads", {
    storage: null,
    upload: (name) => {
      if (name.startsWith("pending")) return new Promise<Arrival>(() => {});
      if (name.startsWith("failed"))
        return Promise.reject(new Error("the daemon refused this file"));
      return Promise.resolve(stored(name));
    },
    remove: () => Promise.resolve(),
  });
  const upload = (file: File, pastedText?: string) =>
    store.uploadFile(file, pastedText);
  const pending = upload(new File([""], "pending-scan.pdf"));
  const ready = upload(new File([""], "ready-notes.txt"));
  const image = upload(
    new File(
      [
        '<svg xmlns="http://www.w3.org/2000/svg" width="1179" height="2556"><rect width="1179" height="2556" fill="#8973ac"/><circle cx="590" cy="900" r="400" fill="#e8dff3"/></svg>',
      ],
      "pending-portrait-preview.svg",
      { type: "image/svg+xml" },
    ),
  );
  const failed = upload(new File([""], "failed-drawing.dwg"));
  const pasted = upload(
    new File([""], "paste.txt", { type: "text/plain" }),
    "line one\nline two\nline three",
  );
  const longName = upload(
    new File(
      [""],
      "a-very-long-file-name-that-should-not-push-the-controls-off-a-phone-screen.pdf",
    ),
  );
  const content: DraftContent = {
    content: [
      { Text: "Landing: " },
      { Upload: pending },
      { Text: " · ready: " },
      { Upload: ready },
      { Text: " · image: " },
      { Upload: image },
      { Text: " · failed: " },
      { Upload: failed },
      { Text: " · long paste: " },
      { Upload: pasted },
      { Text: " · long name: " },
      { Upload: longName },
    ],
    references: {},
  };
  fixture = { store, content };
  return fixture;
}

function FixtureEditor({
  store,
  content,
  label,
}: {
  store: DraftStore;
  content: DraftContent;
  label: string;
}) {
  const editor = useEditor({
    extensions: [
      ...composerCoreExtensions,
      ComposerUpload.configure({ store }),
    ],
    content: editorContentFromDraft(content),
    editorProps: {
      attributes: {
        role: "textbox",
        "aria-multiline": "true",
        "aria-label": label,
        class: "composer-editor",
      },
    },
  });
  return <EditorContent editor={editor} />;
}

/** The upload chip in every state the draft can hold, at full and phone
 * widths. The controls are live against the fixture records: removing one
 * detaches it, and the long paste's `inline` action replaces it with its
 * text. */
export function UploadSpecimens() {
  const { store, content } = uploadFixture();
  return (
    <Specimen name="Upload chip — landing, ready, failed, pasted text, long name">
      <div className="composer-input">
        <FixtureEditor
          store={store}
          content={content}
          label="Upload chip states"
        />
      </div>
      <p className="g-intro">
        Fixture store and upload answers; nothing leaves the page. The failed
        chip carries a reason, the long paste keeps its insert-as-text action,
        and the long name ellipsizes inside the chip.
      </p>
      <div style={{ width: 320 }}>
        <div className="composer-input">
          <FixtureEditor
            store={store}
            content={content}
            label="Upload chip states at phone width"
          />
        </div>
      </div>
      <p className="g-intro">
        Readable spelling: {JSON.stringify(textFromDraft(content))}
      </p>
    </Specimen>
  );
}
