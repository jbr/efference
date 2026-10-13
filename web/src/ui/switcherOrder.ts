// The order of the switcher's tabs (docs/web-client.md): most recently visited
// first, like an application switcher, so going back to the session you just
// left is always the first tab. Visits are remembered per browser, because
// switching is a full page load and nothing else survives it; the listing has
// no recency of its own to sort by.

const KEY = "efference.sessionVisits";
const KEEP = 100;

/** When each session was last opened in this browser, in epoch milliseconds. */
export type Visits = Record<string, number>;

export function readVisits(): Visits {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(KEY) ?? "{}");
    if (typeof parsed !== "object" || parsed === null) return {};
    return Object.fromEntries(
      Object.entries(parsed).filter(
        (entry): entry is [string, number] => typeof entry[1] === "number",
      ),
    );
  } catch {
    return {};
  }
}

/** Note that `id` was opened now, keeping only the newest `KEEP` sessions so
 * ids of long-archived sessions do not accumulate. */
export function recordVisit(id: string, now: number = Date.now()): void {
  try {
    const visits = { ...readVisits(), [id]: now };
    const newest = Object.entries(visits)
      .sort((a, b) => b[1] - a[1])
      .slice(0, KEEP);
    localStorage.setItem(KEY, JSON.stringify(Object.fromEntries(newest)));
  } catch {
    // Storage can be unavailable (private mode, quota); the order then falls
    // back to the listing's.
  }
}

/** Visited sessions newest first, then the never-visited in the order given
 * (the host lists by id, so that order is the same on every page). */
export function orderByRecency<T extends { id: string }>(
  sessions: T[],
  visits: Visits,
): T[] {
  const at = (session: T) => visits[session.id] ?? -Infinity;
  return sessions
    .map((session, index) => ({ session, index }))
    .sort((a, b) => at(b.session) - at(a.session) || a.index - b.index)
    .map(({ session }) => session);
}

/** Keep established tab slots; append newly listed sessions in listing order. */
export function orderByTabs<T extends { id: string }>(
  sessions: T[],
  previous: string[],
): T[] {
  const entries = new Map(sessions.map((session) => [session.id, session]));
  const retained = previous.flatMap((id) => {
    const session = entries.get(id);
    if (!session) return [];
    entries.delete(id);
    return [session];
  });
  return [...retained, ...entries.values()];
}
