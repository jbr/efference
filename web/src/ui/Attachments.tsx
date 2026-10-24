// Attachment chips (docs/web-client.md): the composer's staged files, and the
// files a sent turn carried. An image shows as a thumbnail; anything else as its
// name and the triage signals the model reads about it.

import { attachmentUrl } from "../api/http.ts";
import type { Attachment } from "../api/types.ts";
import { openLightbox } from "./lightbox.ts";
import { describeKind, isImage, type Staged } from "./staging.ts";

/** The composer's staged row. Each chip can be removed; a paste staged for
 * its length can also be put back into the draft as text. */
export function StagedAttachments({
  staged,
  onRemove,
  onInline,
}: {
  staged: Staged[];
  onRemove: (key: string) => void;
  onInline: (key: string) => void;
}) {
  if (staged.length === 0) return null;
  return (
    <ul className="attachments staged">
      {staged.map((s) => (
        <li
          key={s.key}
          className={`attachment ${s.status}`}
          title={s.error ?? s.attachment?.path ?? s.label}
        >
          {s.preview !== null && <img src={s.preview} alt="" />}
          <span className="attachment-text">
            <span className="attachment-name">{s.label}</span>
            <span className="attachment-detail">
              {s.status === "uploading"
                ? "uploading…"
                : s.status === "failed"
                  ? (s.error ?? "upload failed")
                  : s.attachment && describeKind(s.attachment.kind)}
            </span>
          </span>
          {s.pastedText !== undefined && (
            <button
              type="button"
              className="linkish"
              title="put the pasted text into the draft instead"
              onClick={() => onInline(s.key)}
            >
              inline
            </button>
          )}
          <button
            type="button"
            className="attachment-remove"
            title="remove this attachment"
            aria-label={`remove ${s.label}`}
            onClick={() => onRemove(s.key)}
          >
            ×
          </button>
        </li>
      ))}
    </ul>
  );
}

/** The files a sent (or queued) turn carried. An image's thumbnail opens the
 * lightbox, so the full-size bytes are visible without leaving the app. */
export function SentAttachments({
  attachments,
}: {
  attachments: Attachment[];
}) {
  if (attachments.length === 0) return null;
  return (
    <ul className="attachments sent">
      {attachments.map((a) => (
        <li key={a.name} className="attachment" title={a.path}>
          {isImage(a.kind) && (
            <button
              type="button"
              className="attachment-image"
              aria-label={`open ${a.name} full screen`}
              onClick={() =>
                openLightbox({
                  src: attachmentUrl(a.name),
                  alt: a.name,
                  caption: `${a.name} · ${describeKind(a.kind)}`,
                })
              }
            >
              <img src={attachmentUrl(a.name)} alt={a.name} />
            </button>
          )}
          <span className="attachment-text">
            <span className="attachment-name">{a.name}</span>
            <span className="attachment-detail">{describeKind(a.kind)}</span>
          </span>
        </li>
      ))}
    </ul>
  );
}
