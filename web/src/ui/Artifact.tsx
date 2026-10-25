import { useEffect, useRef, useState } from "react";
import { departFromFrame } from "./followTail.ts";
import type { ThemeName } from "./theme.ts";
import "./Artifact.css";

const THEME_MESSAGE = "efference:artifact:theme";
const colorScheme = (theme: ThemeName): "dark" | "light" =>
  theme === "lilac" ? "light" : "dark";
const RESIZE_MESSAGE = "efference:artifact:resize";
const SCROLLED_UP_MESSAGE = "efference:artifact:scrolled-up";
const INITIAL_HEIGHT = 360;

function pageTheme(): ThemeName {
  return globalThis.document?.documentElement.dataset.theme === "lilac"
    ? "lilac"
    : "after-dark";
}

function isResizeMessage(
  value: unknown,
): value is { type: typeof RESIZE_MESSAGE; height: number } {
  if (value === null || typeof value !== "object") return false;
  const message = value as Record<string, unknown>;
  return (
    message.type === RESIZE_MESSAGE &&
    typeof message.height === "number" &&
    Number.isFinite(message.height) &&
    message.height > 0
  );
}

/** One `html-artifact` fence in committed assistant prose, at the signed URL
 * its entry view carries (docs/artifacts.md). The sandboxed frame loads that
 * page directly; nothing here fetches on its behalf. */
export function Artifact({ source, url }: { source: string; url: string }) {
  const [height, setHeight] = useState(INITIAL_HEIGHT);
  const [theme, setTheme] = useState<ThemeName>(pageTheme);
  const [copied, setCopied] = useState(false);
  const [copyFailed, setCopyFailed] = useState(false);
  const frame = useRef<HTMLIFrameElement>(null);

  // Theme choice is an attribute on the document root. Observe it rather than
  // reading the OS preference: an explicit app choice can differ from it.
  useEffect(() => {
    const root = document.documentElement;
    const observer = new MutationObserver(() => setTheme(pageTheme()));
    observer.observe(root, {
      attributes: true,
      attributeFilter: ["data-theme"],
    });
    return () => observer.disconnect();
  }, []);

  // A sandboxed document has an opaque origin. Validate the sending window,
  // not event.origin. A size is clamped to the viewport; a scrolled-up report
  // is the operator leaving the transcript's tail over the frame.
  useEffect(() => {
    const onMessage = (event: MessageEvent<unknown>) => {
      const target = frame.current?.contentWindow;
      if (target === null || event.source !== target) return;
      if (
        typeof event.data === "object" &&
        event.data !== null &&
        (event.data as { type?: unknown }).type === SCROLLED_UP_MESSAGE
      ) {
        departFromFrame();
        return;
      }
      if (!isResizeMessage(event.data)) return;
      setHeight(
        Math.ceil(Math.min(event.data.height, window.innerHeight * 0.7)),
      );
    };
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, []);

  // Send the current theme on load and immediately on a theme change. The
  // load-time send covers a message sent before the runtime script was ready.
  useEffect(() => {
    const element = frame.current;
    if (element === null) return;
    const sendTheme = () => {
      element.contentWindow?.postMessage(
        { type: THEME_MESSAGE, theme, colorScheme: colorScheme(theme) },
        "*",
      );
    };
    element.addEventListener("load", sendTheme);
    sendTheme();
    return () => element.removeEventListener("load", sendTheme);
  }, [theme, url]);

  const copy = async () => {
    setCopied(false);
    setCopyFailed(false);
    try {
      if (navigator.clipboard === undefined)
        throw new Error("Clipboard unavailable");
      await navigator.clipboard.writeText(source);
      setCopied(true);
    } catch {
      setCopyFailed(true);
    }
  };

  return (
    <div className="artifact">
      <iframe
        ref={frame}
        className="artifact-frame"
        title="HTML artifact preview"
        src={url}
        sandbox="allow-scripts"
        style={{ height: `${height}px` }}
      />
      <details className="artifact-source">
        <summary>Source</summary>
        <div className="artifact-source-tools">
          <button type="button" onClick={() => void copy()}>
            Copy source
          </button>
          {copied && <span role="status">Copied</span>}
          {copyFailed && <span role="status">Copy unavailable</span>}
        </div>
        <pre data-lang="html">
          <code>{source}</code>
        </pre>
      </details>
    </div>
  );
}
