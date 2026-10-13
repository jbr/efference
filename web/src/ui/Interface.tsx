import { useEffect, useState } from "react";
import { fetchPushKey, subscribePush, unsubscribePush } from "../api/http.ts";
import { readPushState, type PushState } from "../push.ts";
import { setThemeChoice, themeChoice, type ThemeChoice } from "./theme.ts";
import {
  setViewSetting,
  useViewSettings,
  type ViewSettings,
} from "./viewSettings.ts";

/** base64url → the raw bytes `applicationServerKey` takes. The daemon answers
 * the VAPID key in the encoding the RFC spells it in; `subscribe` wants the
 * point itself. */
function decodeKey(base64url: string): Uint8Array<ArrayBuffer> {
  const padded = base64url
    .replace(/-/g, "+")
    .replace(/_/g, "/")
    .padEnd(Math.ceil(base64url.length / 4) * 4, "=");
  const raw = atob(padded);
  const bytes = new Uint8Array(new ArrayBuffer(raw.length));
  for (let i = 0; i < raw.length; i += 1) bytes[i] = raw.charCodeAt(i);
  return bytes;
}

/** Why the notifications checkbox cannot be used, if it cannot. `null` means it
 * can. Rendered rather than hidden: a checkbox that silently does nothing is
 * worse than one that says why (docs/web-push.md). */
function pushObstacle(): string | null {
  if (Notification.permission === "denied") {
    // JS cannot re-prompt once denied; only the browser's own settings can
    // undo it, so the checkbox says where to go rather than offering a click
    // that would do nothing.
    return "blocked in browser settings";
  }
  // iOS delivers push only to the home-screen-installed app, so a subscribe
  // from a Safari tab fails at `requestPermission` with nothing to show for it.
  const ios = /iPad|iPhone|iPod/.test(navigator.userAgent);
  const standalone = window.matchMedia("(display-mode: standalone)").matches;
  if (ios && !standalone) {
    return "add to home screen first — iOS notifies only the installed app";
  }
  return null;
}

/** The "receive notifications" opt-in (docs/web-push.md).
 *
 * The checkbox reflects `pushManager.getSubscription()` rather than a stored
 * preference: the browser is the source of truth about whether this device is
 * subscribed, and a preference could disagree with it after a permission reset
 * or a cleared site. The prompt and the subscribe run only from the click,
 * which is what Safari requires and what Chrome quiet-prompts without. */
function Notifications() {
  const [state, setState] = useState<PushState>("pending");
  const subscribed = state === "subscribed";
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    void readPushState(
      "serviceWorker" in navigator ? navigator.serviceWorker : undefined,
    ).then((next) => {
      if (live) setState(next);
    });
    return () => {
      live = false;
    };
  }, []);

  const obstacle = pushObstacle();

  const toggle = async (wanted: boolean) => {
    setBusy(true);
    setError(null);
    try {
      const registration = await navigator.serviceWorker.ready;
      if (wanted) {
        const permission = await Notification.requestPermission();
        if (permission !== "granted") {
          setError("permission was not granted");
          return;
        }
        const { key } = await fetchPushKey();
        const subscription = await registration.pushManager.subscribe({
          // Required by every platform: a push must produce a visible
          // notification, which is also all this harness sends push for.
          userVisibleOnly: true,
          applicationServerKey: decodeKey(key),
        });
        await subscribePush(subscription.toJSON());
        setState("subscribed");
      } else {
        // Both sides, in the order that leaves nothing sending to a dead
        // endpoint: the daemon forgets it, then the browser drops it.
        const subscription = await registration.pushManager.getSubscription();
        if (subscription) {
          await unsubscribePush(subscription.endpoint);
          await subscription.unsubscribe();
        }
        setState("unsubscribed");
      }
    } catch (e) {
      setError(`${e}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="push-toggle">
      <label>
        <input
          type="checkbox"
          checked={subscribed}
          disabled={
            busy ||
            obstacle !== null ||
            state === "pending" ||
            state === "unavailable"
          }
          onChange={(event) => void toggle(event.target.checked)}
        />
        receive notifications
      </label>
      {obstacle !== null && <span className="dim"> — {obstacle}</span>}
      {obstacle === null && state === "unavailable" && (
        <span className="dim"> — no service worker on this page</span>
      )}
      {error !== null && <span className="error"> — {error}</span>}
    </div>
  );
}

const VIEW_LABELS: Record<keyof ViewSettings, string> = {
  thinking: "thinking",
  ambient: "ambient",
};

/** Interface preferences: the theme, and the transcript density switches.
 * Shared across sessions in the home-accessible app settings view. Hiding
 * via the switches is display-only; the entries stay in the store. */
export function Interface() {
  const settings = useViewSettings();
  const [choice, setChoice] = useState(themeChoice);
  return (
    <div className="panel-body interface">
      <label className="dim">
        theme{" "}
        <select
          value={choice}
          onChange={(event) => {
            const next = event.target.value as ThemeChoice;
            setThemeChoice(next);
            setChoice(next);
          }}
        >
          <option value="system">system</option>
          <option value="after-dark">after dark</option>
          <option value="lilac">lilac</option>
        </select>
      </label>
      <div className="view-toggles">
        show:
        {(Object.keys(VIEW_LABELS) as (keyof ViewSettings)[]).map((key) => (
          <label key={key}>
            <input
              type="checkbox"
              checked={settings[key]}
              onChange={(event) => setViewSetting(key, event.target.checked)}
            />
            {VIEW_LABELS[key]}
          </label>
        ))}
      </div>
      {/* Hidden entirely where the browser has no push at all — there is
          nothing to explain, and an obstacle notice implies a path forward. */}
      {"PushManager" in window && <Notifications />}
    </div>
  );
}
