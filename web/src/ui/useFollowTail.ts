// Transcript ownership of window-tail intent (docs/web-client.md).
import { useLayoutEffect } from "react";
import { installFollowTail, pinTail } from "./followTail.ts";

export function useFollowTail() {
  useLayoutEffect(installFollowTail, []);
  useLayoutEffect(pinTail);
}
