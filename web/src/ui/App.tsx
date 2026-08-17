import { spotFor } from "../session/store.ts";
import { useSession } from "../session/useSession.ts";
import { Composer } from "./Composer.tsx";
import { GateSurface } from "./Gate.tsx";
import { Login } from "./Login.tsx";
import { Transcript } from "./Transcript.tsx";

export function App() {
  const { session, connector, actions } = useSession();

  if (session.connection === "unauthenticated") {
    return <Login onAuthenticated={() => connector.retry()} />;
  }

  const gate = session.topics.gate;
  return (
    <div className="app">
      <header>
        <span>efference</span>
        <span className="dim">
          {session.connection}
          {session.topics.running ? " · running" : ""}
          {gate ? " · gate pending" : ""}
        </span>
      </header>
      <Transcript entries={session.entries} streaming={session.streaming} />
      {gate && (
        <GateSurface
          gate={gate}
          spot={spotFor(session.entries, gate.call_id)}
          resolve={(decision) => actions.resolveGate(gate.call_id, decision)}
        />
      )}
      <Composer />
    </div>
  );
}
