// Visible-node anchoring for backward pages (docs/web-client.md).
export function captureHistoryAnchor(): (() => void) | null {
  const node = [
    ...document.querySelectorAll<HTMLElement>("[data-transcript-key]"),
  ].find((element) => element.getBoundingClientRect().bottom > 0);
  if (!node) return null;
  const top = node.getBoundingClientRect().top;
  return () => {
    if (!node.isConnected) return;
    const delta = node.getBoundingClientRect().top - top;
    if (Math.abs(delta) > 0.5)
      window.scrollBy({ top: delta, behavior: "instant" });
  };
}
