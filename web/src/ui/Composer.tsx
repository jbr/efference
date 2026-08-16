// The composer: POST /v1/turns. Submission is decoupled from inference
// (docs/control-api.md), so a 202 is success — the turn's effects arrive on
// the stream like everything else.

import { useState } from "react";
import { submitTurn } from "../api/http.ts";

export function Composer() {
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string | null>(null);

  async function submit() {
    const text = draft.trim();
    if (!text) return;
    try {
      await submitTurn(text);
      setDraft("");
      setError(null);
    } catch (e) {
      setError(`${e}`);
    }
  }

  return (
    <form
      className="composer"
      onSubmit={(event) => {
        event.preventDefault();
        void submit();
      }}
    >
      {error && <div className="error">{error}</div>}
      <textarea
        value={draft}
        onChange={(event) => setDraft(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter" && !event.shiftKey) {
            event.preventDefault();
            void submit();
          }
        }}
        placeholder="send a turn (enter to send, shift-enter for newline)"
        rows={3}
      />
      <button>send</button>
    </form>
  );
}
