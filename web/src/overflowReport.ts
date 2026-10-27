// Dev-only: with `?overflow` on the URL, a fixed panel lists the elements that
// extend past the right edge of the viewport, outermost first — for finding
// what makes a phone layout wider than the screen where no inspector is
// attached. Plain DOM, refreshed every two seconds; nothing in ui/ knows.

const describe = (el: Element) => {
  const cls =
    typeof el.className === "string" && el.className !== ""
      ? "." + el.className.trim().split(/\s+/).join(".")
      : "";
  const rect = el.getBoundingClientRect();
  return `${el.tagName.toLowerCase()}${cls} w${Math.round(rect.width)} r${Math.round(rect.right)} minw:${getComputedStyle(el).minWidth}`;
};

export function installOverflowReport(): void {
  const panel = document.createElement("pre");
  panel.style.cssText =
    "position:fixed;left:4px;right:4px;top:120px;z-index:99999;margin:0;" +
    "font:10px/1.3 ui-monospace,monospace;padding:6px;background:#000d;" +
    "color:#0f0;white-space:pre-wrap;pointer-events:none;max-height:50vh;overflow:hidden";
  document.body.append(panel);
  const report = () => {
    const vw = document.documentElement.clientWidth;
    const over = [...document.querySelectorAll("body *")].filter(
      (el) => el !== panel && el.getBoundingClientRect().right > vw + 1,
    );
    // Outermost first: an offender whose parent is not itself an offender.
    const roots = over.filter(
      (el) => !el.parentElement || !over.includes(el.parentElement),
    );
    const spills = [...document.querySelectorAll<HTMLElement>("body *")].filter(
      (el) => {
        if (el === panel || el.scrollWidth <= el.clientWidth + 1) return false;
        for (
          let ancestor: HTMLElement | null = el;
          ancestor && ancestor !== document.body;
          ancestor = ancestor.parentElement
        ) {
          const style = getComputedStyle(ancestor);
          // `clip` is the transcript items' safety net (Transcript.css): it
          // hides the spill rather than containing it, so it still counts.
          if (
            style.visibility === "hidden" ||
            (style.overflowX !== "visible" && style.overflowX !== "clip")
          )
            return false;
        }
        return true;
      },
    );
    const leaves = spills.filter(
      (el) =>
        ![...el.children].some((child) =>
          spills.includes(child as HTMLElement),
        ),
    );
    panel.textContent =
      `vw ${vw} · scrollW ${document.documentElement.scrollWidth} · ${over.length} over · ${leaves.length} text spills\n` +
      [
        ...roots.slice(0, 6).map(describe),
        ...leaves
          .slice(0, 6)
          .map((el) => `${describe(el)} content:${el.scrollWidth}`),
      ].join("\n");
  };
  report();
  setInterval(report, 2000);
}
