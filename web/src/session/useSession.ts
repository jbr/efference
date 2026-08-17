// The one React binding over the session layer: a Store owned for the
// component's lifetime, exposed as a snapshot via useSyncExternalStore.
// The Connector is owned by the effect, not the memo: connecting is a side
// effect, and an effect-owned connector is what survives StrictMode's
// mount → cleanup → remount cycle (close() is permanent on a Connector, so
// the remount must build a fresh one — the store fold tolerates the re-seed).
// The returned connector is a stable facade over whichever connector is
// currently live. Everything below this hook is framework-free.

import { useEffect, useMemo, useRef, useSyncExternalStore } from "react";
import { connect, type Connector } from "./connect.ts";
import { Store, type Session } from "./store.ts";

export function useSession(): { session: Session; connector: Connector } {
  const store = useMemo(() => new Store(), []);
  const live = useRef<Connector | null>(null);
  useEffect(() => {
    const connector = connect(store);
    live.current = connector;
    return () => {
      live.current = null;
      connector.close();
    };
  }, [store]);
  const connector = useMemo<Connector>(
    () => ({
      retry: () => live.current?.retry(),
      close: () => live.current?.close(),
    }),
    [],
  );
  const session = useSyncExternalStore(store.subscribe, store.snapshot);
  return { session, connector };
}
