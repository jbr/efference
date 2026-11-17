// Attachment chips (docs/web-client.md): the files a sent turn carried. A
// draft's staged files are inline occurrences with their own chip
// (ui/ComposerUploadChip.tsx). An image shows as a thumbnail; anything else as
// its name and the triage signals the model reads about it.

import { useState } from "react";
import { attachmentUrl } from "../api/http.ts";
import type { Arrival } from "../api/types.ts";
import { openLightbox } from "./lightbox.ts";
import { describeKind, isImage } from "./staging.ts";
import { imageAttrs, useApiImage } from "./useApiImage.ts";

/** A draft upload's thumbnail: the local object URL of the file while it
 * uploads, and the stored bytes once the upload has landed (ui/drafts.ts). The
 * picture last drawn stays up while the stored bytes are fetched, so the
 * thumbnail does not blank between the two. */
export function AttachmentThumbnail({ preview }: { preview: string }) {
  const src = useApiImage(preview);
  const [drawn, setDrawn] = useState<string | null>(null);
  if (src !== null && src !== drawn) setDrawn(src);
  return <img src={src ?? drawn ?? undefined} alt="" />;
}

/** A sent image's thumbnail, which opens the lightbox on the image's URL. The
 * lightbox resolves that URL through its own hold on the bytes
 * (ui/useApiImage.ts), so this button leaving the page does not take the image
 * from under it. */
function SentThumbnail({ attachment }: { attachment: Arrival }) {
  const url = attachmentUrl(attachment.name);
  const src = useApiImage(url);
  return (
    <button
      type="button"
      className="attachment-image"
      aria-label={`open ${attachment.name} full screen`}
      onClick={() =>
        openLightbox({
          src: url,
          alt: attachment.name,
          caption: `${attachment.name} · ${describeKind(attachment.kind)}`,
        })
      }
    >
      <img {...imageAttrs(src, attachment.name)} />
    </button>
  );
}

/** The files a sent (or queued) turn carried. An image's thumbnail opens the
 * lightbox, so the full-size bytes are visible without leaving the app. */
export function SentAttachments({ attachments }: { attachments: Arrival[] }) {
  if (attachments.length === 0) return null;
  return (
    <ul className="attachments sent">
      {attachments.map((a) => (
        <li key={a.name} className="attachment" title={a.path}>
          {isImage(a.kind) && <SentThumbnail attachment={a} />}
          <span className="attachment-text">
            <span className="attachment-name">{a.name}</span>
            <span className="attachment-detail">{describeKind(a.kind)}</span>
          </span>
        </li>
      ))}
    </ul>
  );
}
