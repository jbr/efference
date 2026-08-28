// The one React binding over the session layer: a Store owned for the
// component's lifetime, exposed as a snapshot via useSyncExternalStore.
// The Connector is owned by the effect, not the memo: connecting is a side
// effect, and an effect-owned connector is what survives StrictMode's
// mount → cleanup → remount cycle (close() is permanent on a Connector, so
// the remount must build a fresh one — the store fold tolerates the re-seed).
// The returned connector is a stable facade over whichever connector is
// currently live; actions are the session-layer verbs bound to this store.
// Everything below this hook is framework-free.

import { useEffect, useMemo, useRef, useSyncExternalStore } from "react";
import type { CallId, Decision } from "../api/types.ts";
import { recallTurn, resolveGate, submitTurn } from "./actions.ts";
import { connect, type Connector } from "./connect.ts";
import { ProcessSeeder } from "./processSeed.ts";
import { Store, type Session } from "./store.ts";

export interface SessionActions {
  resolveGate(callId: CallId, decision: Decision): Promise<"applied" | "moved">;
  submitTurn(text: string): Promise<void>;
  /** Withdraw the pending suffix from this submission; resolves with the texts
   * actually popped, for the composer to splice. */
  recallTurn(id: string): Promise<string[]>;
}

export function useSession(): {
  session: Session;
  connector: Connector;
  actions: SessionActions;
} {
  const store = useMemo(() => new Store(), []);
  const live = useRef<Connector | null>(null);
  useEffect(() => {
    // A dev build interposes the scrubber (record.ts) between the stream and
    // the fold, so a momentary pre-commit state can be held and looked at. The
    // connection itself is unchanged — same catch-up read, same stream, same
    // frames. `import.meta.env.DEV` folds to `false` in a production build, so
    // neither the branch nor the module it imports reaches a shipped bundle.
    if (import.meta.env.DEV) {
      let connector: Connector | null = null;
      let recorder: { close(): void } | null = null;
      let cancelled = false;
      void import("./record.ts").then(({ record }) => {
        const installed = record(store);
        connector = connect(store, installed.tap);
        recorder = installed;
        // The import resolves after StrictMode's cleanup has already run, so
        // checking `cancelled` *before* building would leak a connection the
        // cleanup never saw. Build, then tear down if the effect is gone.
        if (cancelled) {
          connector.close();
          installed.close();
        } else {
          live.current = connector;
        }
      });
      return () => {
        cancelled = true;
        live.current = null;
        connector?.close();
        recorder?.close();
      };
    }
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
  const actions = useMemo<SessionActions>(
    () => ({
      resolveGate: (callId, decision) => resolveGate(store, callId, decision),
      submitTurn: (text) => submitTurn(store, text),
      recallTurn: (id) => recallTurn(store, id),
    }),
    [store],
  );
  const session = useSyncExternalStore(store.subscribe, store.snapshot);
  // The tasks pane's tail is built from `output` deltas, which have no replay,
  // so each process needs one read of its retained output to start from. The
  // `processes` topic is the trigger: a row appearing is the first moment there
  // is an id to seed, and it covers a foreground command flipping to background
  // mid-run as well as a client opening onto an already-running one.
  const seeder = useMemo(() => new ProcessSeeder(store), [store]);
  const processes = session.topics.processes;
  useEffect(() => {
    if (processes) seeder.sync(processes);
  }, [seeder, processes]);
  return { session, connector, actions };
}
