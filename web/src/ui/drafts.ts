// Per-session composer drafts (docs/web-client.md). The store outlives the
// view, including its uploads; localStorage keeps text, the draft document and
// upload metadata, not file bytes. A reload cannot resume an upload.
//
// Ownership: the store owns only uploads this draft created. A record is live
// while an `Upload` occurrence names it and detached otherwise; a detached
// record is retained only while the editor whose history could undo the removal
// is alive. Bytes are deleted when that history is gone, never while a send
// that names them is in flight. Recalled and sent uploads are `sent`-owned and
// are never deleted here.

import { removeAttachment, uploadAttachment } from "../api/http.ts";
import type { Arrival } from "../api/types.ts";
import {
  detachUpload,
  draftReadable,
  draftUploadKeys,
  hasDraftStructure,
  normalizeDraftContent,
} from "./draftDocument.ts";
import type { DraftContent } from "./draftDocument.ts";
import { failure, isImageFile, previewOf, revoke } from "./drafts/records.ts";
import { load, write } from "./drafts/persist.ts";
import type { Draft, DraftStorage } from "./drafts/persist.ts";
import { pastedName, type DraftUpload } from "./staging.ts";

export type { Draft } from "./drafts/persist.ts";

interface Dependencies {
  storage: DraftStorage | null;
  upload: (name: string, file: File) => Promise<Arrival>;
  remove: (name: string) => Promise<void>;
}

function browserStorage(): DraftStorage | null {
  try {
    return localStorage;
  } catch {
    return null;
  }
}

export class DraftStore {
  private value: Draft;
  private listeners = new Set<() => void>();
  /** Bumped on every editor mount and unmount, so a teardown's deferred prune
   * stands down when a new editor mounted in the same tick (StrictMode runs
   * the pair twice in development). */
  private session = 0;
  private historyGone = true;
  /** A cleared send replaces the editor; the history that could undo into the
   * cleared draft is deliberately discarded, so the records it retained may
   * be dropped at the next mount. */
  private historyReset = false;
  private deps: Dependencies;

  constructor(
    readonly base: string,
    deps?: Dependencies,
  ) {
    this.deps = deps ?? {
      storage: browserStorage(),
      upload: (name, file) => uploadAttachment(name, file, base),
      remove: (name) => removeAttachment(name, base),
    };
    this.value = load(this.deps.storage, base);
    // A previous page left no live editor history, so detached records it
    // retained cannot be undone into. Their bytes are cleaned here, except a
    // sent-owned or pinned record (the send may have been admitted).
    this.pruneDetached();
  }

  snapshot = (): Draft => this.value;
  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };
  /** The readable file name a node's key stands for; the clipboard projects an
   * upload occurrence to this. */
  uploadLabel = (key: string): string | undefined =>
    this.value.uploads.find((upload) => upload.key === key)?.label;

  private update(value: Draft) {
    this.value = value;
    write(this.deps.storage, this.base, value);
    this.listeners.forEach((listener) => listener());
  }

  /** The editor's authoritative content. `revision` is deliberately not
   * persisted: after a reload nothing is in flight to compare against. */
  setContent = (content: { text: string; document: DraftContent | null }) => {
    const document =
      content.document !== null
        ? normalizeDraftContent(content.document)
        : null;
    this.update({
      ...this.value,
      text: content.text,
      document: hasDraftStructure(document) ? document : null,
      revision: this.value.revision + 1,
    });
  };

  /** Programmatic plain-text replacement. Structured edits go through
   * `setContent`, so a document cannot be left carrying stale text. */
  setText = (text: string | ((current: string) => string)) => {
    const next = typeof text === "function" ? text(this.value.text) : text;
    this.update({
      ...this.value,
      text: next,
      document: null,
      revision: this.value.revision + 1,
    });
  };

  /** Detach an upload's occurrence. Nothing is deleted here: undo can restore
   * the chip, and it must find its record, bytes and preview intact. A
   * detached ready record this draft owns is deleted once the editor's
   * history is gone (pruneDetached).
   *
   * Removal works while a send names the upload: a pin protects its bytes from
   * cleanup, not the draft from editing, and a cut or backspace detaches the
   * same occurrence without consulting the pin. */
  remove = (key: string) => {
    if (!this.value.uploads.some((record) => record.key === key)) return;
    if (this.value.document === null) return;
    const document = detachUpload(this.value.document, key);
    this.update({
      ...this.value,
      document: hasDraftStructure(document) ? document : null,
      text: draftReadable(document),
      revision: this.value.revision + 1,
    });
  };

  /** Create a draft-local upload and start it. The returned key is what the
   * editor's `Upload` occurrence names; the caller inserts the node. */
  uploadFile = (file: File, pastedText?: string): string => {
    const key = crypto.randomUUID();
    const label = pastedName(file);
    const preview = isImageFile(file) ? URL.createObjectURL(file) : null;
    this.update({
      ...this.value,
      uploads: [
        ...this.value.uploads,
        {
          key,
          label,
          preview,
          status: "uploading",
          pastedText,
          provenance: "draft",
        },
      ],
    });
    this.deps.upload(label, file).then(
      (attachment) => {
        if (!this.value.uploads.some((upload) => upload.key === key)) {
          // The record was pruned at teardown while the request was in
          // flight: no editor can restore its chip, so the bytes are unused.
          void this.deps.remove(attachment.name).catch(() => {});
          return;
        }
        // A detached occurrence that lands stays detached — an undo may
        // still put its chip back, so the record and preview are kept and
        // the chip is not reinserted (nothing here edits the editor).
        this.update({
          ...this.value,
          uploads: this.value.uploads.map((upload) => {
            if (upload.key !== key) return upload;
            revoke(upload);
            return {
              ...upload,
              status: "ready",
              attachment,
              preview: previewOf(attachment, this.base),
              error: undefined,
            };
          }),
        });
      },
      (error: unknown) => {
        if (!this.value.uploads.some((upload) => upload.key === key)) return;
        this.update({
          ...this.value,
          uploads: this.value.uploads.map((upload) =>
            upload.key === key
              ? { ...upload, status: "failed", error: failure(error) }
              : upload,
          ),
        });
      },
    );
    return key;
  };

  /** Retain uploads a recalled turn already holds by name: the same stored
   * bytes are now the recalled turn's too, so no teardown may delete them. */
  retain = (keys: readonly string[]) => {
    const named = new Set(keys);
    if (
      !this.value.uploads.some(
        (upload) => named.has(upload.key) && upload.provenance !== "sent",
      )
    )
      return;
    this.update({
      ...this.value,
      uploads: this.value.uploads.map((upload) =>
        named.has(upload.key) ? { ...upload, provenance: "sent" } : upload,
      ),
    });
  };

  /** The editor mounted: a removal is undoable again. When a send cleared the
   * draft, the editor it replaces was discarded along with the history that
   * could restore the send, so the records it retained are pruned now rather
   * than accumulating until the composer unmounts. */
  beginEditorSession = () => {
    this.session += 1;
    this.historyGone = false;
    if (this.historyReset) {
      this.historyReset = false;
      this.pruneDetached(true);
    }
  };

  /** The composer is about to replace the editor after clearing the draft: no
   * undo of the submitted content survives that replacement. */
  resetEditorHistory = () => {
    this.historyReset = true;
  };

  /** The editor unmounted: its history (and its undo of a removal) is gone.
   * The prune waits a microtask, which a same-tick mount cancels. */
  endEditorSession = () => {
    const session = ++this.session;
    this.historyGone = true;
    queueMicrotask(() => {
      if (this.session === session) this.pruneDetached();
    });
  };

  /** Pin every upload the submitted document names. Pinning is persisted, so a
   * page that goes away mid-request cannot later decide the bytes are unused.
   * It does not touch the revision: the draft's content is unchanged. */
  beginSend = (submitted: Draft): Draft => {
    const keys = new Set(draftUploadKeys(submitted.document));
    if (keys.size === 0) return this.value;
    this.update({
      ...this.value,
      uploads: this.value.uploads.map((upload) =>
        keys.has(upload.key) ? { ...upload, pinned: true } : upload,
      ),
    });
    return this.value;
  };

  /** A successful send keeps every upload it named, even one the draft no
   * longer shows: undo may need the chip, and the sent turn may still read the
   * bytes. Only what the send actually submitted is cleared. */
  sent = (submitted: Draft): Draft => {
    const keys = new Set(draftUploadKeys(submitted.document));
    const unchanged = this.value.revision === submitted.revision;
    this.update({
      text: unchanged ? "" : this.value.text,
      document: unchanged ? null : this.value.document,
      uploads: this.value.uploads.map((upload) =>
        keys.has(upload.key)
          ? { ...upload, provenance: "sent", pinned: false }
          : upload,
      ),
      revision: this.value.revision,
    });
    // If the view moved on during the send, no editor will be torn down to
    // prune the records this send released; the store still has no history to
    // undo into. A mounted editor defers to its teardown.
    this.pruneDetached();
    return this.value;
  };

  /** A send that did not report success may still have been admitted: the
   * request can reach the daemon and its response be lost. Every upload the
   * send named is therefore retained (marked `sent`, pinned released), never
   * deleted — a failed POST is not proof the file is unused. Only uploads no
   * send ever named are cleaned up, and only once their history is gone. */
  sendFailed = (submitted: Draft): Draft => {
    const keys = new Set(draftUploadKeys(submitted.document));
    if (keys.size > 0) {
      this.update({
        ...this.value,
        uploads: this.value.uploads.map((upload) =>
          keys.has(upload.key)
            ? { ...upload, provenance: "sent", pinned: false }
            : upload,
        ),
      });
    }
    this.pruneDetached();
    return this.value;
  };

  /** Delete what no edit can restore and no send ever named. A record stays
   * while an occurrence names it, while a send is in flight, and (while an
   * editor is mounted) while undo can put its chip back. Only a `draft`-owned
   * ready upload — one this draft created and no send ever submitted — is
   * deleted. */
  private pruneDetached(forced = false) {
    if (!this.historyGone && !forced) return;
    const live = new Set(draftUploadKeys(this.value.document));
    const kept: DraftUpload[] = [];
    let changed = false;
    for (const upload of this.value.uploads) {
      if (live.has(upload.key) || upload.pinned) {
        kept.push(upload);
        continue;
      }
      changed = true;
      // A record still uploading when its history ends is dropped; its landing
      // response finds no record and deletes the bytes, and no chip revives.
      if (
        upload.status === "ready" &&
        upload.attachment !== undefined &&
        upload.provenance === "draft"
      ) {
        void this.deps.remove(upload.attachment.name).catch(() => {});
      }
      revoke(upload);
    }
    if (changed) this.update({ ...this.value, uploads: kept });
  }
}

const drafts = new Map<string, DraftStore>();
/** Browser storage is origin-scoped; the session API base scopes drafts within
 * that host. No transcript or watching connection is retained by this store. */
export function acquireDraft(base: string): DraftStore {
  let store = drafts.get(base);
  if (!store) {
    store = new DraftStore(base);
    drafts.set(base, store);
  }
  return store;
}
