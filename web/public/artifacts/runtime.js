/* global window, document, requestAnimationFrame, ResizeObserver */

(() => {
  const themeMessage = "efference:artifact:theme";
  const resizeMessage = "efference:artifact:resize";
  const themes = new Set(["after-dark", "lilac"]);

  window.addEventListener("message", (event) => {
    if (event.source !== window.parent) return;
    const message = event.data;
    if (
      message === null ||
      typeof message !== "object" ||
      message.type !== themeMessage ||
      !themes.has(message.theme) ||
      message.colorScheme !== (message.theme === "lilac" ? "light" : "dark")
    ) {
      return;
    }
    const root = document.documentElement;
    root.dataset.theme = message.theme;
    root.style.colorScheme = message.colorScheme;
  });

  let queued = false;
  const sendSize = () => {
    queued = false;
    const body = document.body;
    if (body === null) return;
    // Measure the content box rather than the document root: root height often
    // includes the iframe viewport and would prevent a short page shrinking.
    const height = Math.ceil(Math.max(body.scrollHeight, body.offsetHeight));
    if (Number.isFinite(height) && height > 0) {
      window.parent.postMessage({ type: resizeMessage, height }, "*");
    }
  };
  const scheduleSize = () => {
    if (queued) return;
    queued = true;
    requestAnimationFrame(sendSize);
  };

  const start = () => {
    const observer = new ResizeObserver(scheduleSize);
    observer.observe(document.documentElement);
    if (document.body !== null) observer.observe(document.body);
    window.addEventListener("load", scheduleSize);
    window.addEventListener("resize", scheduleSize);
    scheduleSize();
    if (document.fonts?.ready !== undefined) {
      void document.fonts.ready.then(scheduleSize);
    }
  };

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", start, { once: true });
  } else {
    start();
  }
})();
