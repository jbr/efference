// Transcript density (docs/web-client.md): what renders is a client choice,
// persisted like the theme. Three switches, the design pass's props —
// thinking, clear spot verdicts, ambient entries. Hiding is display-only:
// the entries stay in the store and reappear on toggle.

import { useSyncExternalStore } from "react";

export interface ViewSettings {
  thinking: boolean;
  clearSpots: boolean;
  ambient: boolean;
}

const KEY = "view";

const DEFAULTS: ViewSettings = {
  thinking: true,
  clearSpots: true,
  ambient: true,
};

function load(): ViewSettings {
  try {
    const stored: unknown = JSON.parse(localStorage.getItem(KEY) ?? "{}");
    return { ...DEFAULTS, ...(typeof stored === "object" ? stored : {}) };
  } catch {
    return DEFAULTS;
  }
}

let settings = load();
const listeners = new Set<() => void>();

export function setViewSetting(key: keyof ViewSettings, value: boolean): void {
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

export function useViewSettings(): ViewSettings {
  return useSyncExternalStore(subscribe, () => settings);
}
