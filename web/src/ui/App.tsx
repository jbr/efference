import { useState } from "react";
import { spotFor } from "../session/store.ts";
import { useSession } from "../session/useSession.ts";
import { Composer } from "./Composer.tsx";
import { Controls } from "./Controls.tsx";
import { GateSurface } from "./Gate.tsx";
import { Login } from "./Login.tsx";
import { setThemeChoice, themeChoice, type ThemeChoice } from "./theme.ts";
import { Transcript } from "./Transcript.tsx";

function ThemeSelect() {
  const [choice, setChoice] = useState(themeChoice);
  return (
    <select
      aria-label="theme"
      value={choice}
      onChange={(event) => {
        const next = event.target.value as ThemeChoice;
        setThemeChoice(next);
        setChoice(next);
      }}
    >
      <option value="system">system</option>
      <option value="after-dark">after dark</option>
      <option value="daylight">daylight</option>
    </select>
  );
}

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
        <ThemeSelect />
      </header>
      {session.connection !== "live" && (
        <div className="banner">{session.connection}…</div>
      )}
      <Controls
        pace={session.topics.pace ?? null}
        reviewLevel={session.topics.review_level ?? null}
        running={session.topics.running ?? false}
        processes={session.topics.processes ?? []}
        usage={session.topics.usage ?? null}
        notifications={session.topics.notifications ?? []}
      />
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
