// API images as object URLs (docs/web-client.md). An `<img src>` makes its own
// request, which no client code touches; an image that addresses the API is
// fetched through `apiFetch` instead and shown from an object URL over the
// bytes.
//
// The cache is module-level and keyed by URL. The first holder of a URL starts
// the request, holders that arrive while it is in flight share it, and the
// last holder to leave aborts what is still in flight, revokes the object URL,
// and deletes the entry. A URL addresses bytes that do not change — a log
// entry's image, an uploaded attachment — so an entry is never refreshed, and a
// failed request leaves the cache at once so the next holder asks again.
//
// The last holder's departure takes effect once the effects of the commit it
// happened in have all run. A holder that leaves and one that arrives in the
// same commit — a pending turn's echo replaced by the committed turn, a
// remount — therefore share one entry rather than revoking and fetching again,
// and a holder that arrives after the bytes have landed draws them on its first
// render rather than after its effect.

import { useEffect, useState } from "react";
import { apiFetch } from "../api/http.ts";

/** The cache: one entry per URL that has a holder, or that lost its last one
 * in the commit still finishing. */
const entries = new Map<string, Entry>();

/** One URL's bytes, shared by every holder of it. */
class Entry {
  holders = 0;
  /** The object URL once the bytes have landed, kept so the last release can
   * revoke it. */
  objectUrl: string | null = null;
  private readonly abort = new AbortController();
  private disposed = false;
  /** The object URL over the bytes, or `null` if the request failed. */
  readonly src: Promise<string | null>;

  constructor(private readonly url: string) {
    entries.set(url, this);
    this.src = this.load();
  }

  private async load(): Promise<string | null> {
    try {
      const response = await apiFetch(this.url, { signal: this.abort.signal });
      if (!response.ok) return this.fail();
      const bytes = await response.blob();
      // Released while the body was arriving: nobody is left to revoke an
      // object URL made now.
      if (this.abort.signal.aborted) return null;
      this.objectUrl = URL.createObjectURL(bytes);
      return this.objectUrl;
    } catch {
      return this.fail();
    }
  }

  private fail(): null {
    this.leaveCache();
    return null;
  }

  /** Delete this entry unless a newer one has taken its URL. */
  private leaveCache(): void {
    if (entries.get(this.url) === this) entries.delete(this.url);
  }

  release(): void {
    this.holders -= 1;
    if (this.holders === 0) queueMicrotask(() => this.dispose());
  }

  /** Let go of the request and the object URL, unless a holder has arrived
   * since the last one left. */
  private dispose(): void {
    if (this.holders > 0 || this.disposed) return;
    this.disposed = true;
    this.leaveCache();
    this.abort.abort();
    if (this.objectUrl !== null) URL.revokeObjectURL(this.objectUrl);
  }
}

/** The object URL of an entry whose bytes have landed, if the cache holds one. */
const landed = (url: string): string | null =>
  entries.get(url)?.objectUrl ?? null;

/** Hold the URL's entry, creating it (and starting its request) for the first
 * holder. */
function acquire(url: string): Entry {
  const entry = entries.get(url) ?? new Entry(url);
  entry.holders += 1;
  return entry;
}

/**
 * The `src` and `alt` an `<img>` takes for what `useApiImage` returned. With no
 * `src` the element draws as an empty box that keeps the aspect of its
 * `width`/`height` attributes; a non-empty `alt` there would draw the text under
 * a broken-image icon, and lay the element out at its `width` attribute, past
 * the column, instead of within it.
 */
export const imageAttrs = (src: string | null, alt: string) => ({
  src: src ?? undefined,
  alt: src === null ? "" : alt,
});

/**
 * The object URL for an API image, or `null` while the request is in flight
 * and if it failed — the caller renders its `<img>` without a `src` then, which
 * keeps the element and its box in place. The hook holds its URL for as long as
 * the component is mounted, so the component that renders the `<img>` is the
 * one that resolves the URL; a component that only passes the URL on (to the
 * lightbox, into draft state) holds nothing. A `blob:` URL is already local
 * bytes — a staged file's own preview (ui/drafts.ts) — and comes back unchanged
 * without a request. When the URL's bytes have already landed for another
 * holder, the first render returns them.
 */
export function useApiImage(url: string | null): string | null {
  const remote = url === null || url.startsWith("blob:") ? null : url;
  const [loaded, setLoaded] = useState<{
    url: string;
    src: string | null;
  } | null>(null);
  useEffect(() => {
    if (remote === null) return;
    let held = true;
    const entry = acquire(remote);
    void entry.src.then((src) => {
      if (held) setLoaded({ url: remote, src });
    });
    return () => {
      held = false;
      entry.release();
      // The object URL may have just been revoked; a later hold on the same
      // URL starts from nothing rather than from it.
      setLoaded(null);
    };
  }, [remote]);
  if (remote === null) return url;
  return loaded?.url === remote ? loaded.src : landed(remote);
}
