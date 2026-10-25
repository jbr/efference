import { useEffect, useRef, useState } from "react";
import type { ArtifactDescriptor, Seq } from "../api/types.ts";
import { resolveArtifact } from "../api/http.ts";
import type { ThemeName } from "./theme.ts";
import "./Artifact.css";

const THEME_MESSAGE = "efference:artifact:theme";
const colorScheme = (theme: ThemeName): "dark" | "light" =>
  theme === "lilac" ? "light" : "dark";
const RESIZE_MESSAGE = "efference:artifact:resize";
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

function artifactPath(value: unknown): string | null {
  if (
    typeof value !== "string" ||
    !value.startsWith("/artifacts/") ||
    value.startsWith("//")
  ) {
    return null;
  }
  return value;
}

/** A single opt-in fence in committed assistant prose. Resolution carries only
 * log coordinates; the returned same-origin page is loaded directly by the
 * sandboxed frame and never through the session's authenticated fetch path. */
export function Artifact({
  source,
  seq,
  textBlock,
  fence,
}: {
  source: string;
  seq: Seq;
  textBlock: number;
  fence: number;
}) {
  const requestKey = `${seq}/${textBlock}/${fence}`;
  const [resolution, setResolution] = useState<{
    key: string;
    status: "loading" | "ready" | "failed";
    url: string | null;
  }>({ key: requestKey, status: "loading", url: null });
  const currentResolution =
    resolution.key === requestKey
      ? resolution
      : { key: requestKey, status: "loading" as const, url: null };
  const { status, url } = currentResolution;
  const [height, setHeight] = useState(INITIAL_HEIGHT);
  const [theme, setTheme] = useState<ThemeName>(pageTheme);
  const [copied, setCopied] = useState(false);
  const [copyFailed, setCopyFailed] = useState(false);
  const frame = useRef<HTMLIFrameElement>(null);

  useEffect(() => {
    let current = true;
    void resolveArtifact(seq, textBlock, fence)
      .then((descriptor: ArtifactDescriptor) => {
        const path = artifactPath(descriptor.url);
        if (path === null) throw new Error("Invalid artifact URL");
        if (current) {
          setResolution({ key: requestKey, url: path, status: "ready" });
        }
      })
      .catch(() => {
        if (current) {
          setResolution({ key: requestKey, url: null, status: "failed" });
        }
      });
    return () => {
      current = false;
    };
  }, [fence, requestKey, seq, textBlock]);

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
  // not event.origin, then clamp the content measurement to the viewport.
  useEffect(() => {
    const onMessage = (event: MessageEvent<unknown>) => {
      const target = frame.current?.contentWindow;
      if (
        target === null ||
        event.source !== target ||
        !isResizeMessage(event.data)
      )
        return;
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
    if (url === null || element === null) return;
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
      {status === "loading" && (
        <p className="artifact-state" role="status">
          Loading preview…
        </p>
      )}
      {status === "failed" && (
        <p className="artifact-state" role="alert">
          Preview unavailable; source is still available below.
        </p>
      )}
      {url !== null && (
        <iframe
          ref={frame}
          className="artifact-frame"
          title="HTML artifact preview"
          src={url}
          sandbox="allow-scripts"
          style={{ height: `${height}px` }}
        />
      )}
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
