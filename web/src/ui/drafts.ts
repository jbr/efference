// Per-session composer drafts (docs/web-client.md). The store outlives the
// view, including its uploads; localStorage keeps text and attachment metadata,
// not file bytes. A reload cannot resume an upload.
import {
  attachmentUrl,
  removeAttachment,
  uploadAttachment,
} from "../api/http.ts";
import type { Arrival } from "../api/types.ts";
import { isImage, pastedName, type Staged } from "./staging.ts";

export interface Draft {
  text: string;
  staged: Staged[];
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

function isAttachment(value: unknown): value is Arrival {
  if (typeof value !== "object" || value === null) return false;
  const a = value as Partial<Arrival>;
  if (typeof a.name !== "string" || typeof a.path !== "string") return false;
  if (typeof a.kind !== "object" || a.kind === null) return false;
  if ("Image" in a.kind) {
    const k = a.kind.Image;
    return (
      typeof k === "object" &&
      k !== null &&
      typeof k.format === "string" &&
      typeof k.width === "number" &&
      typeof k.height === "number" &&
      typeof k.bytes === "number"
    );
  }
  if ("Text" in a.kind) {
    const k = a.kind.Text;
    return (
      typeof k === "object" &&
      k !== null &&
      typeof k.bytes === "number" &&
      typeof k.lines === "number" &&
      typeof k.shape === "string" &&
      typeof k.first_line === "string"
    );
  }
  if ("Binary" in a.kind) {
    const k = a.kind.Binary;
    return typeof k === "object" && k !== null && typeof k.bytes === "number";
  }
  return false;
}

function load(storage: DraftStorage | null, base: string): Draft {
  try {
    const raw = storage?.getItem(storageKey(base));
    if (!raw) return { text: "", staged: [] };
    const saved: unknown = JSON.parse(raw);
    if (typeof saved !== "object" || saved === null)
      return { text: "", staged: [] };
    const draft = saved as { text?: unknown; staged?: unknown };
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
          s.status === "ready" && isAttachment(s.attachment)
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
    return { text: typeof draft.text === "string" ? draft.text : "", staged };
  } catch {
    return { text: "", staged: [] };
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
      if (value.text === "" && value.staged.length === 0) {
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
          }),
        );
      }
    } catch {
      // Storage can be denied or full; the live draft and uploads still work.
    }
    this.listeners.forEach((listener) => listener());
  }

  setText = (text: string | ((current: string) => string)) => {
    this.update({
      ...this.value,
      text: typeof text === "function" ? text(this.value.text) : text,
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

  restore = (attachments: Arrival[]) => {
    this.update({
      ...this.value,
      staged: [
        ...this.value.staged,
        ...attachments.map((attachment): Staged => ({
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
    this.update({
      ...this.value,
      staged: this.value.staged.filter((s) => s.key !== key),
    });
  };

  /** A successful send clears only what it sent: edits or attachments made
   * while its response was pending must survive (docs/web-client.md). */
  sent = (submitted: Draft) => {
    const keys = new Set(
      submitted.staged.filter((s) => s.status === "ready").map((s) => s.key),
    );
    this.value.staged.filter((s) => keys.has(s.key)).forEach(revoke);
    this.update({
      text: this.value.text === submitted.text ? "" : this.value.text,
      staged: this.value.staged.filter((s) => !keys.has(s.key)),
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
