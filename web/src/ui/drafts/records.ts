// Upload-record helpers shared by the draft store and its persistence: the
// reason text a failed upload shows, local and stored previews, and revocation.

import { attachmentUrl } from "../../api/http.ts";
import type { Arrival } from "../../api/types.ts";
import { isImage, type DraftUpload } from "../staging.ts";

export const interrupted =
  "Upload interrupted by a reload; attach this file again.";

/** A failed upload's text: the daemon's reason when it answered (an
 * `ApiError`'s message is its envelope's), the browser's when the request
 * itself failed (Safari's is `Load failed`). Shown on the chip, not only in its
 * title, because a touch screen has no hover to read a title with. */
export const failure = (error: unknown) =>
  `Upload failed (${error instanceof Error ? error.message : String(error)}); attach this file again.`;

/** Whether a local file gets a thumbnail before it lands. */
export const isImageFile = (file: File) =>
  file.type.startsWith("image/") ||
  /\.(png|jpe?g|gif|webp|svg|bmp|ico)$/i.test(file.name);

/** The preview a stored arrival's URL provides once the upload lands. */
export function previewOf(attachment: Arrival | undefined, base: string) {
  return attachment && isImage(attachment.kind)
    ? attachmentUrl(attachment.name, base)
    : null;
}

export function revoke(upload: DraftUpload) {
  if (upload.preview?.startsWith("blob:")) URL.revokeObjectURL(upload.preview);
}
