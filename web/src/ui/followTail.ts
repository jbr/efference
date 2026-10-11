// Window-tail intent and input ownership (docs/web-client.md).
const CONTACT = 1;
let following = true;
const listeners = new Set<() => void>();
let active = false;
let navigationPending = false;

export const tailFollowing = () => following;
export function subscribeTail(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}
function setFollowing(next: boolean) {
  if (following === next) return;
  following = next;
  for (const listener of listeners) listener();
}
function atBottom() {
  return (
    window.innerHeight + window.scrollY >=
    document.documentElement.scrollHeight - CONTACT
  );
}
export function pinTail() {
  if (active && following && !navigationPending)
    window.scrollTo({
      top: document.documentElement.scrollHeight,
      behavior: "instant",
    });
}
export function resumeTail() {
  navigationPending = false;
  setFollowing(true);
  // Also usable before the transcript's layout effect has installed its listeners.
  window.scrollTo({
    top: document.documentElement.scrollHeight,
    behavior: "instant",
  });
}

/** Nested scroll containers consume navigation before it reaches the window. */
function nestedScroll(target: EventTarget | null) {
  let element = target instanceof Element ? target : null;
  while (
    element &&
    element !== document.body &&
    element !== document.documentElement
  ) {
    const overflow = getComputedStyle(element).overflowY;
    if (
      (overflow === "auto" || overflow === "scroll") &&
      element.scrollHeight > element.clientHeight &&
      element.scrollTop > 0
    )
      return true;
    element = element.parentElement;
  }
  return false;
}
function editable(target: EventTarget | null) {
  return (
    target instanceof Element &&
    target.closest(
      "input, textarea, select, [contenteditable]:not([contenteditable=false])",
    ) !== null
  );
}

export function installFollowTail(): () => void {
  active = true;
  navigationPending = false;
  setFollowing(true);
  let previousY = window.scrollY;
  let touchY: number | null = null;
  let draggingScrollbar = false;
  let scrollbarUntil = 0;
  let scheduled: number | null = null;
  const schedulePin = () => {
    if (scheduled !== null) return;
    scheduled = requestAnimationFrame(() => {
      scheduled = null;
      if (navigationPending && window.scrollY < previousY) setFollowing(false);
      navigationPending = false;
      pinTail();
      previousY = window.scrollY;
    });
  };
  const depart = (event: Event) => {
    if (
      !event.defaultPrevented &&
      window.scrollY > 0 &&
      !nestedScroll(event.target)
    ) {
      navigationPending = true;
      schedulePin();
    }
  };
  const scroll = () => {
    if (
      (navigationPending || draggingScrollbar || Date.now() < scrollbarUntil) &&
      window.scrollY < previousY
    )
      setFollowing(false);
    // Geometry can engage intent, never disengage it without user navigation.
    if (atBottom()) {
      navigationPending = false;
      setFollowing(true);
    }
    previousY = window.scrollY;
    if (following) schedulePin();
  };
  const wheel = (event: WheelEvent) => {
    if (event.deltaY < 0 && !event.ctrlKey) depart(event);
  };
  const key = (event: KeyboardEvent) => {
    if (editable(event.target)) return;
    if (
      ["ArrowUp", "PageUp", "Home"].includes(event.key) ||
      ((event.key === " " || event.key === "Spacebar") &&
        event.shiftKey &&
        !(event.target instanceof Element && event.target.closest("button")))
    )
      depart(event);
  };
  const touchStart = (event: TouchEvent) => {
    touchY = event.touches[0]?.clientY ?? null;
  };
  const touchMove = (event: TouchEvent) => {
    const next = event.touches[0]?.clientY ?? null;
    if (next !== null && touchY !== null && next > touchY) depart(event);
    touchY = next;
  };
  const touchEnd = () => {
    touchY = null;
  };
  const pointerDown = (event: PointerEvent) => {
    draggingScrollbar =
      event.pointerType === "mouse" &&
      (event.clientX >= document.documentElement.clientWidth - 16 ||
        event.clientX <= 16);
    previousY = window.scrollY;
  };
  const pointerUp = () => {
    if (draggingScrollbar) scrollbarUntil = Date.now() + 200;
    draggingScrollbar = false;
  };
  window.addEventListener("scroll", scroll, { passive: true });
  window.addEventListener("resize", schedulePin);
  window.visualViewport?.addEventListener("resize", schedulePin);
  window.addEventListener("wheel", wheel, { passive: true });
  window.addEventListener("keydown", key);
  window.addEventListener("touchstart", touchStart, { passive: true });
  window.addEventListener("touchmove", touchMove, { passive: true });
  window.addEventListener("touchend", touchEnd, { passive: true });
  window.addEventListener("touchcancel", touchEnd, { passive: true });
  window.addEventListener("pointerdown", pointerDown);
  window.addEventListener("pointerup", pointerUp);
  window.addEventListener("pointercancel", pointerUp);
  const observer = new ResizeObserver(schedulePin);
  observer.observe(document.documentElement);
  observer.observe(document.body);
  pinTail();
  previousY = window.scrollY;
  return () => {
    active = false;
    navigationPending = false;
    observer.disconnect();
    if (scheduled !== null) cancelAnimationFrame(scheduled);
    window.removeEventListener("scroll", scroll);
    window.removeEventListener("resize", schedulePin);
    window.visualViewport?.removeEventListener("resize", schedulePin);
    window.removeEventListener("wheel", wheel);
    window.removeEventListener("keydown", key);
    window.removeEventListener("touchstart", touchStart);
    window.removeEventListener("touchmove", touchMove);
    window.removeEventListener("touchend", touchEnd);
    window.removeEventListener("touchcancel", touchEnd);
    window.removeEventListener("pointerdown", pointerDown);
    window.removeEventListener("pointerup", pointerUp);
    window.removeEventListener("pointercancel", pointerUp);
  };
}
