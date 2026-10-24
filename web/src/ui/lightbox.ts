// The lightbox (docs/web-client.md): any image in the client opened full
// screen. A module store rather than threaded props because an image opens
// from three unrelated places — a transcript result, the inspector, a sent
// attachment — and the overlay itself is a single instance mounted once
// (LightboxHost). `useSyncExternalStore` mirrors the pattern of the other
// module stores (route.ts, theme.ts).

import { useSyncExternalStore } from "react";

export interface LightboxImage {
  /** The image's address: an API URL, which the viewer resolves through
   * `useApiImage`, or a `blob:` URL, which it shows as given. */
  src: string;
  /** The accessible name, and the fallback caption. */
  alt: string;
  /** What the head bar states about the image; `alt` when absent. */
  caption?: string;
}

let current: LightboxImage | null = null;
const listeners = new Set<() => void>();
const notify = () => listeners.forEach((listener) => listener());

export function openLightbox(image: LightboxImage): void {
  current = image;
  notify();
}

export function closeLightbox(): void {
  if (current === null) return;
  current = null;
  notify();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** The open image, or null. The snapshot is the same object until a call
 * changes it, which is what `useSyncExternalStore` requires. */
export function useLightboxImage(): LightboxImage | null {
  return useSyncExternalStore(subscribe, () => current);
}
