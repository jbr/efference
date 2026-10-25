// What a sign-in calls this browser — "Safari on macOS", "Chrome on Android" —
// so `efference devices` can say which row is which (docs/web-bind.md). Display
// text only: nothing decides anything by it, and the host cleans it on the way
// in. An iPad that asks for desktop sites reports itself as a Mac.

// Order matters: Edge and Opera carry "Chrome", and Chrome carries "Safari".
const BROWSERS: [RegExp, string][] = [
  [/Edg(e|A|iOS)?\//, "Edge"],
  [/OPR\/|Opera/, "Opera"],
  [/Firefox\/|FxiOS\//, "Firefox"],
  [/Chrome\/|CriOS\//, "Chrome"],
  [/Safari\//, "Safari"],
];

// iOS reports "like Mac OS X", and Android reports Linux.
const SYSTEMS: [RegExp, string][] = [
  [/iPhone|iPad|iPod/, "iOS"],
  [/Android/, "Android"],
  [/Mac OS X|Macintosh/, "macOS"],
  [/Windows/, "Windows"],
  [/CrOS/, "ChromeOS"],
  [/Linux/, "Linux"],
];

const first = (table: [RegExp, string][], userAgent: string) =>
  table.find(([pattern]) => pattern.test(userAgent))?.[1];

export function deviceLabel(userAgent: string): string {
  const browser = first(BROWSERS, userAgent);
  const system = first(SYSTEMS, userAgent);
  if (browser && system) return `${browser} on ${system}`;
  return browser ?? system ?? "browser";
}
