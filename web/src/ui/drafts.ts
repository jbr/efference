// Per-session composer drafts (docs/web-client.md). The store outlives the
// view, including its uploads; localStorage keeps text, document and
// attachment metadata, not file bytes. A reload cannot resume an upload.
import {
  detachAttachment,
  documentAttachmentNames,
  documentReadable,
  isArrival,
  isUserContent,
  normalizeDocument,
} from "../api/document.ts";
import {
  attachmentUrl,
  removeAttachment,
  uploadAttachment,
} from "../api/http.ts";
import type { Arrival, UserContent } from "../api/types.ts";
import { isImage, pastedName, type Staged } from "./staging.ts";

/** The draft's authoritative content: `text` is the readable spelling, the
 * document carries reference identity and position, and `revision` advances on
 * every content edit so a send can tell whether it submitted what it sent. */
export interface Draft {
  text: string;
  staged: Staged[];
  document: UserContent | null;
  revision: number;
}

type DraftStorage = Pick<Storage, "getItem" | "setItem" | "removeItem">;
interface Dependencies {
  storage: DraftStorage | null;
  upload: (name: string, file: File) => Promise<Arrival>;
  remove: (name: string) => Promise<void>;
}

const storageKey = (base: string) => `efference.composer.v1:${base}`;
const interrupted = "Upload interrupted by a reload; attach this file again.";

/** A failed upload's text: the daemon's reason when it answered (an
 * `ApiError`'s message is its envelope's), the browser's when the request
 * itself failed (Safari's is `Load failed`). Shown on the chip, not only in its
 * title, because a touch screen has no hover to read a title with. */
const failure = (error: unknown) =>
  `Upload failed (${error instanceof Error ? error.message : String(error)}); attach this file again.`;

function browserStorage(): DraftStorage | null {
  try {
    return localStorage;
  } catch {
    return null;
  }
}

const empty = (): Draft => ({
  text: "",
  staged: [],
  document: null,
  revision: 0,
});

function load(storage: DraftStorage | null, base: string): Draft {
  try {
    const raw = storage?.getItem(storageKey(base));
    if (!raw) return empty();
    const saved: unknown = JSON.parse(raw);
    if (typeof saved !== "object" || saved === null) return empty();
    const draft = saved as {
      text?: unknown;
      staged?: unknown;
      document?: unknown;
    };
    const staged: Staged[] = [];
    if (Array.isArray(draft.staged)) {
      for (const value of draft.staged as unknown[]) {
        if (typeof value !== "object" || value === null) continue;
        const s = value as Partial<Staged>;
        if (typeof s.label !== "string") continue;
        if (
          s.status !== "uploading" &&
          s.status !== "ready" &&
          s.status !== "failed"
        )
          continue;
        const attachment =
          s.status === "ready" && isArrival(s.attachment)
            ? s.attachment
            : undefined;
        staged.push({
          key: crypto.randomUUID(),
          label: s.label,
          status: attachment ? "ready" : "failed",
          attachment,
          preview:
            attachment && isImage(attachment.kind)
              ? attachmentUrl(attachment.name, base)
              : null,
          error: attachment
            ? undefined
            : s.status === "uploading"
              ? interrupted
              : typeof s.error === "string"
                ? s.error
                : "Upload failed; attach this file again.",
          pastedText:
            typeof s.pastedText === "string" ? s.pastedText : undefined,
        });
      }
    }
    const loaded = isUserContent(draft.document)
      ? normalizeDocument(draft.document)
      : null;
    const document =
      loaded !== null && Object.keys(loaded.references).length > 0
        ? loaded
        : null;
    // The document is authoritative when present; its readable projection is
    // the text the composer stores, so the two cannot drift apart on reload.
    return {
      text: document
        ? documentReadable(document)
        : typeof draft.text === "string"
          ? draft.text
          : "",
      staged,
      document,
      revision: 0,
    };
  } catch {
    return empty();
  }
}

export class DraftStore {
  private value: Draft;
  private listeners = new Set<() => void>();
  private dropped = new Set<string>();
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
  }

  snapshot = (): Draft => this.value;
  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  private update(value: Draft) {
    this.value = value;
    try {
      if (
        value.text === "" &&
        value.staged.length === 0 &&
        value.document === null
      ) {
        this.deps.storage?.removeItem(storageKey(this.base));
      } else {
        this.deps.storage?.setItem(
          storageKey(this.base),
          JSON.stringify({
            text: value.text,
            staged: value.staged.map(
              ({ label, status, attachment, error, pastedText }) => ({
                label,
                status,
                attachment,
                error,
                pastedText,
              }),
            ),
            document: value.document,
          }),
        );
      }
    } catch {
      // Storage can be denied or full; the live draft and uploads still work.
    }
    this.listeners.forEach((listener) => listener());
  }

  /** The editor's authoritative content. `revision` is deliberately not
   * persisted: after a reload nothing is in flight to compare against. */
  setContent = (content: { text: string; document: UserContent | null }) => {
    const document =
      content.document !== null &&
      Object.keys(content.document.references).length > 0
        ? content.document
        : null;
    this.update({
      ...this.value,
      text: content.text,
      document,
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

  stage = (file: File, pastedText?: string) => {
    const key = crypto.randomUUID();
    const label = pastedName(file);
    const image =
      file.type.startsWith("image/") ||
      /\.(png|jpe?g|gif|webp|svg|bmp|ico)$/i.test(file.name);
    const preview = image ? URL.createObjectURL(file) : null;
    this.update({
      ...this.value,
      staged: [
        ...this.value.staged,
        { key, label, preview, status: "uploading", pastedText },
      ],
    });
    this.deps.upload(label, file).then(
      (attachment) => {
        if (this.dropped.delete(key)) {
          void this.deps.remove(attachment.name).catch(() => {});
          return;
        }
        this.update({
          ...this.value,
          staged: this.value.staged.map((s) => {
            if (s.key !== key) return s;
            revoke(s);
            return {
              ...s,
              status: "ready",
              attachment,
              preview: isImage(attachment.kind)
                ? attachmentUrl(attachment.name, this.base)
                : null,
            };
          }),
        });
      },
      (error: unknown) => {
        this.dropped.delete(key);
        this.update({
          ...this.value,
          staged: this.value.staged.map((s) =>
            s.key === key
              ? { ...s, status: "failed", error: failure(error) }
              : s,
          ),
        });
      },
    );
  };

  /** Recalled arrivals that the document does not already represent inline.
   * A documented attachment has one inline occurrence, so staging it again
   * would show it twice. */
  restore = (attachments: Arrival[]) => {
    const present = new Set([
      ...(this.value.document === null
        ? []
        : documentAttachmentNames(this.value.document)),
      ...this.value.staged.flatMap((s) =>
        s.attachment ? [s.attachment.name] : [],
      ),
    ]);
    const additions = attachments.filter((a) => !present.has(a.name));
    if (additions.length === 0) return;
    this.update({
      ...this.value,
      staged: [
        ...this.value.staged,
        ...additions.map((attachment): Staged => ({
          key: crypto.randomUUID(),
          label: attachment.name,
          status: "ready",
          attachment,
          preview: isImage(attachment.kind)
            ? attachmentUrl(attachment.name, this.base)
            : null,
        })),
      ],
    });
  };

  remove = (key: string) => {
    const gone = this.value.staged.find((s) => s.key === key);
    if (!gone) return;
    if (gone.status === "uploading") this.dropped.add(key);
    if (gone.attachment)
      void this.deps.remove(gone.attachment.name).catch(() => {});
    revoke(gone);
    // Defensive: if a staged chip shares a name with a document attachment,
    // removing the chip detaches that definition and its occurrences.
    let document = this.value.document;
    if (document !== null && gone.attachment) {
      document = detachAttachment(document, gone.attachment.name);
      if (Object.keys(document.references).length === 0) document = null;
    }
    this.update({
      ...this.value,
      staged: this.value.staged.filter((s) => s.key !== key),
      document,
      revision: this.value.revision + 1,
    });
  };

  /** A successful send clears only what it sent: edits made while its
   * response was pending must survive, including a same-label access toggle
   * that changed no readable text (docs/web-client.md). */
  sent = (submitted: Draft) => {
    const keys = new Set(
      submitted.staged.filter((s) => s.status === "ready").map((s) => s.key),
    );
    this.value.staged.filter((s) => keys.has(s.key)).forEach(revoke);
    const unchanged = this.value.revision === submitted.revision;
    this.update({
      text: unchanged ? "" : this.value.text,
      document: unchanged ? null : this.value.document,
      staged: this.value.staged.filter((s) => !keys.has(s.key)),
      revision: this.value.revision,
    });
  };
}

function revoke(staged: Staged) {
  if (staged.preview?.startsWith("blob:")) URL.revokeObjectURL(staged.preview);
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
