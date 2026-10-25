/* global window, document, matchMedia, requestAnimationFrame, ResizeObserver, setTimeout, Element */

(() => {
  const themeMessage = "efference:artifact:theme";
  const resizeMessage = "efference:artifact:resize";
  const themes = new Set(["after-dark", "lilac"]);
  const root = document.documentElement;

  // The stylesheet's prose rules apply under `.prose`, as they do in the
  // transcript; here the whole document is the prose (docs/artifacts.md).
  root.classList.add("prose");

  // Opened in a tab of its own there is no parent to name a theme, so follow
  // the system's, as the app does before it knows the operator's choice.
  if (window.parent === window) {
    const light = matchMedia("(prefers-color-scheme: light)");
    const follow = () => {
      root.dataset.theme = light.matches ? "lilac" : "after-dark";
      root.style.colorScheme = light.matches ? "light" : "dark";
    };
    follow();
    light.addEventListener("change", follow);
  }

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
    root.dataset.theme = message.theme;
    root.style.colorScheme = message.colorScheme;
  });

  // The parent's window never sees a wheel or touch over this frame — only the
  // scroll the browser chains to it once this page can scroll no further, which
  // alone reads there as layout, and the transcript would pin itself back to
  // its tail (docs/artifacts.md). So report that moment: upward input that
  // nothing here handled or scrolled. Checked after dispatch, so an artifact's
  // own handlers have had their turn.
  const scrolledUpMessage = "efference:artifact:scrolled-up";
  const scrolledHere = (target) => {
    for (
      let element = target instanceof Element ? target : null;
      element !== null && element !== root;
      element = element.parentElement
    ) {
      if (element.scrollTop > 0) return true;
    }
    return (document.scrollingElement?.scrollTop ?? 0) > 0;
  };
  const reportUpward = (event) =>
    setTimeout(() => {
      if (!event.defaultPrevented && !scrolledHere(event.target)) {
        window.parent.postMessage({ type: scrolledUpMessage }, "*");
      }
    });
  window.addEventListener(
    "wheel",
    (event) => {
      if (event.deltaY < 0 && !event.ctrlKey) reportUpward(event);
    },
    { passive: true },
  );
  let touchY = null;
  window.addEventListener(
    "touchstart",
    (event) => {
      touchY = event.touches[0]?.clientY ?? null;
    },
    { passive: true },
  );
  window.addEventListener(
    "touchmove",
    (event) => {
      const next = event.touches[0]?.clientY ?? null;
      if (next !== null && touchY !== null && next > touchY) {
        reportUpward(event);
      }
      touchY = next;
    },
    { passive: true },
  );

  let queued = false;
  const sendSize = () => {
    queued = false;
    // The root element's box: in standards mode it is as tall as the content,
    // collapsed child margins included, and is not stretched to the frame, so a
    // page that gets shorter shrinks its frame. The body's box would miss those
    // margins, and quirks mode stretches both to the viewport — the server adds a
    // doctype to every artifact so a fragment never renders in quirks mode.
    const height = Math.ceil(
      document.documentElement.getBoundingClientRect().height,
    );
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
