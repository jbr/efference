// Whether this page is running the build the host serves (docs/web-client.md,
// "A page outlives a host upgrade"). The host is what serves the shell, so the
// page asks for the shell it would get on a reload and compares the files the
// two name: the same files are the same build.
//
// The comparison is exact because the build hashes its own assets by content
// (Vite's `/assets/<name>-<hash>.js`): a build in which nothing the page runs
// changed names the same URLs and is not an update, whatever else the release
// moved. Nothing is stamped into the build for this, so a bundle built before
// the check existed still compares correctly — which is what lets a tab
// running a previous release notice that it is one.
//
// It states a verdict; it never reloads. A reload costs the page what it holds
// — where a person is reading, what a field had focused — and an installed app
// has no browser chrome to reload with, so the reload is offered as a button
// (`ui/BundleNotice.tsx`) and the person spends it when they are ready.

import { apiBase } from "./base.ts";
import { apiFetch } from "./http.ts";

/** The page's answer about the build: not yet known, the host's, or older. */
export type Bundle = "unknown" | "current" | "stale";

/** The least gap between two checks. A host restart reconnects every stream at
 * once and each connect asks, so one answer stands for the burst. */
export const CHECK_GAP_MS = 5_000;

/** The files a shell names: the scripts it runs and the stylesheets it paints
 * with. Two copies of one build name them identically, and the names carry the
 * build's content hashes.
 *
 * The `modulepreload` links are left out on purpose. Vite's built runtime
 * appends one to the head for every chunk a dynamic import pulls in, so a page
 * that has been used names a growing set the shell document never had — and
 * every page would read as stale the moment it loaded a code-split view. The
 * preloads a shell does declare are covered without them: the entry script
 * carries the hashed names of the chunks it imports, so a changed chunk changes
 * the entry's hash too. */
export function shellAssets(root: ParentNode): string[] {
  const named = (selector: string, attribute: string) =>
    [...root.querySelectorAll(selector)].map(
      (element) => element.getAttribute(attribute) ?? "",
    );
  return [
    ...named("script[src]", "src"),
    ...named('link[rel="stylesheet"][href]', "href"),
  ];
}

/** Whether two shells name the same files, and so are the same build. */
export function sameAssets(
  running: readonly string[],
  served: readonly string[],
): boolean {
  if (running.length !== served.length) return false;
  const a = [...running].sort();
  const b = [...served].sort();
  return a.every((url, i) => url === b[i]);
}

export interface BundleWatchDeps {
  /** The files this page is running. */
  running(): string[];
  /** The files the host serves now, or `null` when it could not be asked. */
  served(): Promise<string[] | null>;
  now(): number;
}

/** Decides, from the page's own shell and the host's, whether they are the same
 * build. `stale` is final for the life of the page: a later check cannot make
 * an old page new, and a reload is what settles it. */
export class BundleWatch {
  private state: Bundle = "unknown";
  private inFlight: Promise<void> | null = null;
  private checkedAt: number | null = null;

  constructor(
    private readonly deps: BundleWatchDeps,
    private readonly onChange: (state: Bundle) => void = () => {},
  ) {}

  /** The verdict so far. */
  get answer(): Bundle {
    return this.state;
  }

  /** Ask the host for its shell, unless the answer is already `stale`, one ask
   * is running, or the last one is younger than `CHECK_GAP_MS`. */
  check(): Promise<void> {
    if (this.state === "stale") return Promise.resolve();
    if (this.inFlight !== null) return this.inFlight;
    const now = this.deps.now();
    if (this.checkedAt !== null && now - this.checkedAt < CHECK_GAP_MS) {
      return Promise.resolve();
    }
    this.checkedAt = now;
    this.inFlight = this.run().finally(() => {
      this.inFlight = null;
    });
    return this.inFlight;
  }

  private async run(): Promise<void> {
    let running: string[];
    let served: string[] | null;
    try {
      running = this.deps.running();
      served = await this.deps.served();
    } catch {
      // The host could not be asked — or there is no page to ask from, as in a
      // test of a stream on its own. The next connect asks again rather than
      // the page deciding on a guess.
      return;
    }
    if (served === null) return;
    this.settle(sameAssets(running, served) ? "current" : "stale");
  }

  private settle(state: Bundle): void {
    if (this.state === state) return;
    this.state = state;
    this.onChange(state);
  }
}

/** The shell the host serves now. A non-page answer — an error, a redirect to
 * sign-in — is `null`: only a shell can be compared. */
async function servedAssets(): Promise<string[] | null> {
  const response = await apiFetch(`${apiBase()}/`, {
    cache: "no-store",
    headers: { accept: "text/html" },
  });
  if (!response.ok) return null;
  const html = await response.text();
  return shellAssets(new DOMParser().parseFromString(html, "text/html"));
}

const listeners = new Set<() => void>();

/** The page's watch. `main.tsx` starts it, and every stream's connect asks it,
 * through `readEventStream` (`api/sse.ts`). */
export const watch = new BundleWatch(
  {
    running: () => shellAssets(document),
    served: servedAssets,
    now: () => performance.now(),
  },
  () => listeners.forEach((listener) => listener()),
);

/** The build verdict for this page, for `useSyncExternalStore`. */
export const bundleState = (): Bundle => watch.answer;

/** Hear the verdict change. */
export function subscribeBundle(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** Start watching: ask once, and again whenever the page returns to view — the
 * two moments the host can have been replaced with no stream connect to say
 * so. Stream connects ask through `checkBundle`. */
export function startBundleWatch(): void {
  void watch.check();
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") void watch.check();
  });
}

/** A stream connected: the host answered, so it may have been replaced. */
export function checkBundle(): void {
  void watch.check();
}
