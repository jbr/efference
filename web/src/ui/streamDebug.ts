// Temporary stream-recovery diagnostics (docs/web-client.md); installed only in dev.
import type { StreamCallbacks } from "../api/stream.ts";
import type { ScrubHandle } from "../session/record.ts";
import type { Store } from "../session/store.ts";

export const RENDER_EVENT = "efference:stream-render";
let minimized = false;

export function installStreamDebug(
  store: Store,
  scrub: ScrubHandle,
  base: string,
) {
  const badge = document.createElement("pre");
  badge.dataset.streamDebug = "";
  badge.setAttribute("aria-label", "Stream diagnostics");
  Object.assign(badge.style, {
    position: "fixed",
    top: "calc(env(safe-area-inset-top, 0px) + 4px)",
    right: "4px",
    zIndex: "10000",
    margin: "0",
    padding: "5px 7px",
    font: "10px/1.35 monospace",
    color: "#fff",
    background: "rgba(0,0,0,.85)",
    borderRadius: "4px",
    pointerEvents: "none",
    maxWidth: "calc(100vw - 8px)",
    whiteSpace: "pre-wrap",
  });
  const shell = document.createElement("div");
  const toggle = document.createElement("button");
  toggle.type = "button";
  Object.assign(shell.style, {
    position: "fixed",
    zIndex: "10000",
    pointerEvents: "none",
  });
  Object.assign(toggle.style, {
    pointerEvents: "auto",
    font: "11px/1.4 monospace",
    padding: "5px 8px",
    minHeight: "32px",
  });
  Object.assign(badge.style, { position: "static" });
  const arrange = () => {
    badge.hidden = minimized;
    toggle.textContent = minimized ? "Stream debug" : "Minimize stream debug";
    toggle.setAttribute("aria-expanded", String(!minimized));
    Object.assign(shell.style, {
      top: minimized ? "auto" : "calc(env(safe-area-inset-top, 0px) + 4px)",
      right: minimized ? "auto" : "4px",
      bottom: minimized
        ? "calc(env(safe-area-inset-bottom, 0px) + 4px)"
        : "auto",
      left: minimized ? "4px" : "auto",
    });
  };
  toggle.onclick = () => {
    minimized = !minimized;
    arrange();
  };
  shell.append(toggle, badge);
  arrange();
  document.body.append(shell);
  const started = Date.now();
  let attempts = 0,
    connects = 0,
    frames = 0,
    bytes = 0;
  let logs = 0,
    outputs = 0;
  let lastBytes: number | null = null,
    lastFrame: number | null = null;
  let wireSeq: number | null = null,
    renderedSeq: number | null = null;
  let renderedAt: number | null = null;
  let latest = "—",
    error = "—";
  let requested: number | null = null;
  const age = (at: number | null) =>
    at === null ? "—" : `${Math.floor((Date.now() - at) / 1000)}s`;
  const paint = () => {
    const session = store.snapshot();
    const mode = scrub.following ? "live" : "held";
    badge.textContent = [
      `stream debug ${base.replace("/sessions/", "") || "root"}`,
      `${session.connection} · ${document.visibilityState} · ${navigator.onLine ? "online" : "offline"}`,
      `up ${age(started)} · requests ${attempts} / connects ${connects} · since ${requested ?? "—"}`,
      `bytes ${bytes} · last read ${age(lastBytes)}`,
      `frames ${frames} (log ${logs}, out ${outputs}) · last ${age(lastFrame)}`,
      `${latest} · wire seq ${wireSeq ?? "—"}`,
      `store seq ${session.lastSeq ?? "—"} · entries ${session.entries.length} · blocks ${session.streaming.blocks.length}`,
      `render seq ${renderedSeq ?? "—"} · last render ${age(renderedAt)}`,
      `tape ${mode} ${scrub.cursor}/${scrub.length} · rolled ${scrub.evicted}${scrub.expired ? " · EXPIRED" : ""}`,
      `last error ${error}`,
    ].join("\n");
  };
  const observer: Partial<StreamCallbacks> = {
    onAttempt(cursor) {
      attempts++;
      requested = cursor;
      paint();
    },
    onConnect() {
      connects++;
      paint();
    },
    onActivity(count) {
      bytes += count;
      lastBytes = Date.now();
    },
    onFrame(frame) {
      frames++;
      lastFrame = Date.now();
      latest = `${frame.class}/${frame.kind}`;
      if (frame.class === "log") {
        logs++;
        wireSeq = frame.seq;
      }
      if (frame.class === "output") outputs++;
    },
    onDisconnect(cause) {
      error = (cause instanceof Error ? cause.message : String(cause)).slice(
        0,
        120,
      );
      paint();
    },
  };
  const onRender = (event: Event) => {
    const detail = (event as CustomEvent<{ base: string; seq: number | null }>)
      .detail;
    if (detail.base !== base) return;
    renderedSeq = detail.seq;
    renderedAt = Date.now();
  };
  document.addEventListener(RENDER_EVENT, onRender);
  const timer = setInterval(paint, 1000);
  paint();
  return {
    observer,
    close() {
      clearInterval(timer);
      document.removeEventListener(RENDER_EVENT, onRender);
      toggle.onclick = null;
      shell.remove();
    },
  };
}
