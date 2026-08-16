import { useSession } from "../session/useSession.ts";
import { Composer } from "./Composer.tsx";
import { Login } from "./Login.tsx";
import { Transcript } from "./Transcript.tsx";

export function App() {
  const { session, connector } = useSession();

  if (session.connection === "unauthenticated") {
    return <Login onAuthenticated={() => connector.retry()} />;
  }

  return (
    <div className="app">
      <header>
        <span>efference</span>
        <span className="dim">
          {session.connection}
          {session.topics.running ? " · running" : ""}
          {session.topics.gate ? " · gate pending" : ""}
        </span>
      </header>
      <Transcript entries={session.entries} streaming={session.streaming} />
      <Composer />
    </div>
  );
}
