// The auth view, successor to the daemon's inline bootstrap page
// (docs/web-bind.md): trade the boot fragment's token — read and scrubbed at
// bundle load (bootToken.ts) — for the session cookie; render the six-digit
// field when there is no token or the trade fails (with the daemon's error
// message, e.g. "code already used", left visible). A hashchange re-runs the
// trade, so `efference open` landing in an already-open tab works.
// `onAuthenticated` re-runs the session connect.

import { useEffect, useRef, useState } from "react";
import { ApiError, mintSession } from "../api/http.ts";
import { takeBootToken, takeFragmentToken } from "./bootToken.ts";

const PROMPT =
  "enter the current code (efference open, or an enrolled authenticator)";

export function Login({ onAuthenticated }: { onAuthenticated: () => void }) {
  const [status, setStatus] = useState("checking session…");
  const [showForm, setShowForm] = useState(false);
  const [code, setCode] = useState("");
  const started = useRef(false);

  async function attempt(code: string) {
    try {
      await mintSession(code.trim());
      onAuthenticated();
      return true;
    } catch (error) {
      setStatus(
        error instanceof ApiError
          ? error.message
          : `authentication failed: ${error}`,
      );
      return false;
    }
  }

  useEffect(() => {
    const tryFragment = async (initial: boolean) => {
      const token = initial ? takeBootToken() : takeFragmentToken();
      if (token) {
        setStatus("authenticating…");
        await attempt(token);
        setShowForm(true);
      } else if (initial) {
        setStatus(PROMPT);
        setShowForm(true);
      }
    };
    if (!started.current) {
      started.current = true;
      void tryFragment(true);
    }
    const onHashChange = () => void tryFragment(false);
    window.addEventListener("hashchange", onHashChange);
    return () => window.removeEventListener("hashchange", onHashChange);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div className="login col">
      <h1>efference</h1>
      <p>{status}</p>
      {showForm && (
        <form
          onSubmit={(event) => {
            event.preventDefault();
            void attempt(code);
          }}
        >
          <input
            value={code}
            onChange={(event) => setCode(event.target.value)}
            inputMode="numeric"
            pattern="[0-9]{6}"
            maxLength={6}
            autoComplete="one-time-code"
            placeholder="123456"
            autoFocus
          />
          <button>authenticate</button>
        </form>
      )}
    </div>
  );
}
