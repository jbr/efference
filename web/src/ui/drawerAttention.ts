// The drawer control's count (docs/web-client.md): what inside the drawer is
// asking for attention, so the top bar can say so without opening it.

import type { PendingNotification, Process, Proposal } from "../api/types.ts";

/** How many things in the drawer are asking for attention, for the top-bar
 * control's count. */
export function drawerAttention(
  notifications: PendingNotification[],
  proposals: Proposal[],
  processes: Process[],
): number {
  return notifications.length + proposals.length + processes.length;
}
