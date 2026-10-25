// The sign-in view (docs/web-bind.md). With a sign-in link's claim — read and
// scrubbed at bundle load (bootClaim.ts), or arriving later on a hashchange, so
// `efference dev open` landing in an already-open tab works — it claims at once.
// Otherwise it asks the host to sign this browser in under a label naming it
// (deviceLabel.ts), shows the short code for the operator to type into
// `efference devices approve` at the host, and asks every second whether they
// have. The token that comes back is kept (api/bearer.ts), and the app takes it
// from there: it shows the page's view in this one's place (ui/App.tsx). A code
// that expires is said so, with a new one a click away; a link that names
// nothing falls back to a code.

import { useEffect, useRef, useState } from "react";
import { bearer } from "../api/bearer.ts";
import { ApiError, claimSignIn, requestSignIn } from "../api/http.ts";
import { takeBootClaim, takeFragmentClaim } from "./bootClaim.ts";
import { deviceLabel } from "./deviceLabel.ts";

/** How often a waiting sign-in asks whether the host approved it. */
export const POLL_MS = 1000;

/** How long past its stated life a code is still asked about, so a slow clock
 * or a slow answer does not end it early; the host's `404` is what ends it. */
const GRACE_MS = 5000;

/** How long a link's claim is asked about. A link is approved before it is
 * opened, so its first claim answers; this bounds the case where it does not. */
const LINK_WAIT_MS = 90_000;

type View =
  | { step: "asking"; note?: string }
  | { step: "claiming" }
  | {
      step: "showing";
      code: string;
      deadline: number;
      note?: string;
      problem?: string;
    }
  | { step: "expired" }
  | { step: "failed"; message: string };

/** What polling one claim came to: nothing more to do here, or a claim the host
 * no longer knows. */
type Polled = "done" | "unknown";

const sleep = (ms: number) =>
  new Promise<void>((resolve) => setTimeout(resolve, ms));

const reason = (error: unknown) =>
  error instanceof ApiError
    ? error.message
    : `could not reach the host: ${error}`;

/** `expires in 1:05`, counting down to the code's end. */
function remaining(ms: number): string {
  const secs = Math.max(0, Math.ceil(ms / 1000));
  return `expires in ${Math.floor(secs / 60)}:${String(secs % 60).padStart(2, "0")}`;
}

export function Login() {
  const [view, setView] = useState<View>({ step: "asking" });
  const [now, setNow] = useState(() => Date.now());
  // The running flow's number: asking for a new code or claiming a link starts a
  // flow, and a newer one stops the older's polling.
  const flow = useRef(0);
  // Read by the polling loop at every step rather than captured, so StrictMode's
  // rehearsal unmount does not end a flow the remount keeps.
  const mounted = useRef(true);
  const started = useRef(false);
  const label = deviceLabel(navigator.userAgent);

  const current = (mine: number) => mounted.current && flow.current === mine;

  /** Ask about one claim every second until the token arrives, the host says
   * the claim names nothing, `deadline` passes, or a newer flow takes over. */
  async function poll(
    mine: number,
    claim: string,
    deadline: number,
  ): Promise<Polled> {
    for (;;) {
      try {
        const answer = await claimSignIn(claim, label);
        if ("token" in answer) {
          await bearer.store(answer.token);
          return "done";
        }
        // Still waiting, so whatever went wrong on an earlier ask has passed.
        if (current(mine)) {
          setView((shown) =>
            shown.step === "showing" && shown.problem
              ? { ...shown, problem: undefined }
              : shown,
          );
        }
      } catch (error) {
        if (!current(mine)) return "done";
        if (error instanceof ApiError && error.status === 404) return "unknown";
        // Anything else may pass — a restart, a dropped connection — so it is
        // shown and the asking goes on until the code's life is over.
        setView((shown) =>
          shown.step === "showing"
            ? { ...shown, problem: reason(error) }
            : shown,
        );
      }
      if (Date.now() > deadline + GRACE_MS) return "unknown";
      await sleep(POLL_MS);
      if (!current(mine)) return "done";
    }
  }

  /** Ask the host for a code, show it, and wait on its approval. */
  async function ask(note?: string) {
    const mine = ++flow.current;
    setView({ step: "asking", note });
    let request;
    try {
      request = await requestSignIn(label);
    } catch (error) {
      if (current(mine)) setView({ step: "failed", message: reason(error) });
      return;
    }
    if (!current(mine)) return;
    const deadline = Date.now() + request.expires_in * 1000;
    setNow(Date.now());
    setView({ step: "showing", code: request.code, deadline, note });
    if (
      (await poll(mine, request.claim, deadline)) === "unknown" &&
      current(mine)
    ) {
      setView({ step: "expired" });
    }
  }

  /** Claim a link's sign-in; one that names nothing falls back to a code. */
  async function claimLink(claim: string) {
    const mine = ++flow.current;
    setView({ step: "claiming" });
    if (
      (await poll(mine, claim, Date.now() + LINK_WAIT_MS)) === "unknown" &&
      current(mine)
    ) {
      void ask("That sign-in link was used already or has expired.");
    }
  }

  useEffect(() => {
    mounted.current = true;
    if (!started.current) {
      started.current = true;
      const claim = takeBootClaim();
      if (claim) void claimLink(claim);
      else void ask();
    }
    const onHashChange = () => {
      const claim = takeFragmentClaim();
      if (claim) void claimLink(claim);
    };
    window.addEventListener("hashchange", onHashChange);
    return () => {
      mounted.current = false;
      window.removeEventListener("hashchange", onHashChange);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // The countdown's clock, running only while a code is up.
  const showing = view.step === "showing";
  useEffect(() => {
    if (!showing) return;
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [showing]);

  return (
    <div className="login col">
      <h1>efference</h1>
      {(view.step === "asking" || view.step === "showing") && view.note && (
        <p>{view.note}</p>
      )}
      {view.step === "asking" && <p>asking the host for a sign-in code…</p>}
      {view.step === "claiming" && <p>signing in…</p>}
      {view.step === "showing" && (
        <>
          <p>
            To sign this browser in, run <code>efference devices approve</code>{" "}
            at the host and type this code:
          </p>
          <p className="login-code">{view.code}</p>
          <p>
            {remaining(view.deadline - now)} · the host lists this browser as{" "}
            {label}
          </p>
          {view.problem && <p>{view.problem}</p>}
        </>
      )}
      {view.step === "expired" && (
        <>
          <p>That code expired before the host approved it.</p>
          <button onClick={() => void ask()}>new code</button>
        </>
      )}
      {view.step === "failed" && (
        <>
          <p>{view.message}</p>
          <button onClick={() => void ask()}>try again</button>
        </>
      )}
    </div>
  );
}
