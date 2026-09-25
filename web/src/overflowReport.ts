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
    panel.textContent =
      `vw ${vw} · scrollW ${document.documentElement.scrollWidth} · ${over.length} over\n` +
      roots.slice(0, 12).map(describe).join("\n");
  };
  report();
  setInterval(report, 2000);
}
