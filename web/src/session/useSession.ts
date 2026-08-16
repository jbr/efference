// The one React binding over the session layer: a Store + Connector pair
// owned for the component's lifetime, exposed as a snapshot via
// useSyncExternalStore. Everything below this hook is framework-free.

import { useMemo, useEffect, useSyncExternalStore } from "react";
import { connect, type Connector } from "./connect.ts";
import { Store, type Session } from "./store.ts";

export function useSession(): { session: Session; connector: Connector } {
  const { store, connector } = useMemo(() => {
    const store = new Store();
    return { store, connector: connect(store) };
  }, []);
  useEffect(() => () => connector.close(), [connector]);
  const session = useSyncExternalStore(store.subscribe, store.snapshot);
  return { session, connector };
}
