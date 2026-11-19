// Dev instrumentation opt-in (docs/web-client.md): the stream scrubber
// (session/record.ts) and the stream debug overlay (ui/streamDebug.ts) run
// only while switched on here — while installed, both cost frame time on
// every stream frame. Persisted like the theme and shared across sessions; a
// switch takes effect on the open session's connection immediately. A
// production build folds the instrumentation away regardless of what is
// stored (session/useSession.ts).

import { useSyncExternalStore } from "react";

export interface DevSettings {
  /** Interpose the recording scrubber (session/record.ts). */
  scrub: boolean;
  /** Install the stream debug overlay (ui/streamDebug.ts). */
  streamDebug: boolean;
}

const KEY = "dev";

const DEFAULTS: DevSettings = {
  scrub: false,
  streamDebug: false,
};

function load(): DevSettings {
  try {
    const stored: unknown = JSON.parse(localStorage.getItem(KEY) ?? "{}");
    return { ...DEFAULTS, ...(typeof stored === "object" ? stored : {}) };
  } catch {
    return DEFAULTS;
  }
}

let settings = load();
const listeners = new Set<() => void>();

export function setDevSetting(key: keyof DevSettings, value: boolean): void {
  settings = { ...settings, [key]: value };
  localStorage.setItem(KEY, JSON.stringify(settings));
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function useDevSettings(): DevSettings {
  return useSyncExternalStore(subscribe, () => settings);
}
