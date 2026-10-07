import type { ArchivedSession } from "../api/http.ts";

/**
 * Format relative age from epoch millisecond timestamps, matching TUI age formatting:
 * "just now", "Xm ago", "Xh ago", "Xd ago".
 */
export function formatAge(nowMs: number, thenMs: number): string {
  const diffMs = Math.max(0, nowMs - thenMs);
  const seconds = Math.floor(diffMs / 1000);
  if (seconds < 60) return "just now";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  return `${days}d ago`;
}

export interface PartitionedArchived {
  /** Band 2: Recent archived full cards (within 1h, capped at 5 or 2 on narrow) */
  recentCards: ArchivedSession[];
  /** Band 3: Compact listed rows (up to 1w, capped at 5) */
  listedRows: ArchivedSession[];
  /** Band 4: Number of additional sessions available beyond Band 2 & 3 */
  remainderCount: number;
  /** Total resumable sessions for this workspace (excluding sessions with problems) */
  totalCount: number;
}

const ONE_HOUR_MS = 60 * 60 * 1000;
const ONE_WEEK_MS = 7 * 24 * 60 * 60 * 1000;

/**
 * Partition archived sessions of a workspace into Band 2 (recent cards),
 * Band 3 (compact listed rows), and Band 4 (remainder).
 */
export function partitionArchived(
  sessions: ArchivedSession[],
  nowMs: number = Date.now(),
  narrow: boolean = false,
): PartitionedArchived {
  // Only offer sessions without unrecoverable problems (matching TUI ResumeRow::from_inventory)
  const resumable = sessions.filter((s) => !s.problem);

  const maxCards = narrow ? 2 : 5;
  const maxRows = 5;

  const recentCards: ArchivedSession[] = [];
  const listedRows: ArchivedSession[] = [];

  for (const session of resumable) {
    const ageMs = session.last_active_at
      ? Math.max(0, nowMs - session.last_active_at)
      : Infinity;

    if (ageMs <= ONE_HOUR_MS && recentCards.length < maxCards) {
      recentCards.push(session);
    } else if (ageMs <= ONE_WEEK_MS && listedRows.length < maxRows) {
      listedRows.push(session);
    }
  }

  const remainderCount = Math.max(
    0,
    resumable.length - (recentCards.length + listedRows.length),
  );

  return {
    recentCards,
    listedRows,
    remainderCount,
    totalCount: resumable.length,
  };
}

/**
 * Word-based filtering across title and id, matching TUI matches() logic.
 */
export function filterArchived(
  sessions: ArchivedSession[],
  query: string,
): ArchivedSession[] {
  const trimmed = query.trim().toLowerCase();
  if (!trimmed) return sessions;
  const words = trimmed.split(/\s+/).filter(Boolean);

  return sessions.filter((session) => {
    const text = `${session.id} ${session.title ?? ""}`.toLowerCase();
    return words.every((word) => text.includes(word));
  });
}
