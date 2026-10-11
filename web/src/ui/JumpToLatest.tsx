// The way back to following the window tail (docs/web-client.md).
import { useSyncExternalStore } from "react";
import { subscribeTail, tailFollowing } from "./followTail.ts";
import { jumpToTail, useDockOffset } from "./float.ts";

export function JumpToLatest({ hidden }: { hidden: boolean }) {
  const following = useSyncExternalStore(subscribeTail, tailFollowing);
  const bottom = useDockOffset();
  if (hidden || following) return null;
  return (
    <button className="jump-latest" style={{ bottom }} onClick={jumpToTail}>
      ↓ latest
    </button>
  );
}
