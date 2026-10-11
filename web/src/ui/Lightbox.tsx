// The lightbox (docs/web-client.md): one image, full screen, zoomable by
// pinch, wheel, or double-tap, dismissed without leaving the page. The
// transform is zoom.ts's; the gestures here are the stage's. It renders over
// everything else (the inspector's sheet and the session drawer included), so
// its z-index is above both. `LightboxHost` is the single instance the app and
// the gallery each mount once.

import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type PointerEvent,
  type MouseEvent,
} from "react";
import {
  closeLightbox,
  useLightboxImage,
  type LightboxImage,
} from "./lightbox.ts";
import { dismisses } from "./swipe.ts";
import {
  IDENTITY,
  MIN_SCALE,
  fingerSpan,
  panBy,
  toggleZoom,
  zoomAbout,
  type Point,
  type Transform,
} from "./zoom.ts";
import "./Lightbox.css";

/** How long the fade-out runs; the host unmounts on its end. */
const EXIT_MS = 200;

/** A double-tap in time: pointerups closer than this at (almost) the same spot
 * toggle the zoom. The click the browser synthesizes after it is swallowed. */
const DOUBLE_TAP_MS = 300;

/** Movement past this stops counting as a tap, so a drag never reads as one. */
const TAP_SLOP_PX = 6;

/** A one-finger drag is one of two things, decided by the scale when the finger
 * lands: a pan while the image is zoomed, and the dismissal swipe at fit, where
 * there is nothing to pan (swipe.ts). */
type Gesture =
  | { kind: "pan"; start: Transform; from: Point }
  | { kind: "dismiss"; from: Point; t0: number };

export function Lightbox({
  image,
  closing,
  onClose,
}: {
  image: LightboxImage;
  /** The exit fade is running; the host unmounts when it lands. */
  closing: boolean;
  onClose: () => void;
}) {
  const stage = useRef<HTMLDivElement>(null);
  const picture = useRef<HTMLImageElement>(null);
  const [transform, setTransform] = useState<Transform>(IDENTITY);
  // The transform the handlers read: state alone would be a render behind a
  // fast gesture. `apply` writes both, so a pinch in progress never reads a
  // stale scale.
  const current = useRef(transform);

  // The dismissal swipe's live offset, applied to the whole viewer as the
  // finger moves; null at rest. `swiping` drops the transition so the surface
  // tracks the finger rather than easing behind it.
  const [offset, setOffset] = useState<Point | null>(null);
  const [swiping, setSwiping] = useState(false);
  // Where the pointer landed, recorded at pointerdown. The stage's pointer
  // capture retargets the following pointerup and click to the stage itself, so
  // neither event's own target can say whether the tap was on the picture or on
  // the backdrop — the recorded one can.
  const downTarget = useRef<Element | null>(null);
  const pointers = useRef(new Map<number, Point>());
  const gesture = useRef<Gesture | null>(null);
  const pinch = useRef<{
    start: Transform;
    span: ReturnType<typeof fingerSpan>;
  } | null>(null);
  const moved = useRef(false);
  const lastTap = useRef<number | null>(null);

  const size = useCallback(() => {
    const element = stage.current;
    if (element === null) return { width: 1, height: 1 };
    return { width: element.clientWidth, height: element.clientHeight };
  }, []);
  const base = useCallback(() => {
    const image = picture.current;
    if (image === null) return { width: 1, height: 1 };
    return { width: image.offsetWidth, height: image.offsetHeight };
  }, []);
  const center = useCallback((): Point => {
    const element = stage.current;
    if (element === null) return { x: 0, y: 0 };
    const rect = element.getBoundingClientRect();
    return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
  }, []);
  const apply = useCallback((next: Transform) => {
    current.current = next;
    setTransform(next);
  }, []);
  const at = (event: PointerEvent): Point => ({
    x: event.clientX,
    y: event.clientY,
  });

  // The wheel is a native listener: React registers `onWheel` passively, so a
  // handler there could not `preventDefault` the page zoom the gesture would
  // otherwise trigger.
  useEffect(() => {
    const element = stage.current;
    if (element === null) return;
    const onWheel = (event: WheelEvent) => {
      event.preventDefault();
      const point = { x: event.clientX, y: event.clientY };
      const next = current.current.scale * Math.exp(-event.deltaY * 0.0015);
      apply(zoomAbout(current.current, next, point, point, center()));
    };
    element.addEventListener("wheel", onWheel, { passive: false });
    return () => element.removeEventListener("wheel", onWheel);
  }, [apply, center]);

  // Escape closes, and the page behind does not scroll while the image is up.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    const previous = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => {
      window.removeEventListener("keydown", onKey);
      document.body.style.overflow = previous;
    };
  }, [onClose]);

  // Which single-finger drag this is, from the scale when the finger lands:
  // zoomed, it pans; at fit, it is the dismissal swipe.
  const singleGesture = (from: Point, t0: number): Gesture =>
    current.current.scale > MIN_SCALE
      ? { kind: "pan", start: current.current, from }
      : { kind: "dismiss", from, t0 };

  const onPointerDown = (event: PointerEvent) => {
    downTarget.current = event.target instanceof Element ? event.target : null;
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
    pointers.current.set(event.pointerId, at(event));
    moved.current = false;
    if (pointers.current.size === 1) {
      gesture.current = singleGesture(at(event), event.timeStamp);
      pinch.current = null;
      if (gesture.current.kind === "dismiss") setSwiping(true);
    } else if (pointers.current.size === 2) {
      // A second finger cancels a swipe in progress: the gesture is a pinch.
      setSwiping(false);
      setOffset(null);
      const [a, b] = [...pointers.current.values()];
      pinch.current = { start: current.current, span: fingerSpan(a, b) };
    }
  };

  const onPointerMove = (event: PointerEvent) => {
    if (!pointers.current.has(event.pointerId)) return;
    pointers.current.set(event.pointerId, at(event));
    if (pinch.current !== null && pointers.current.size === 2) {
      moved.current = true;
      const [a, b] = [...pointers.current.values()];
      const span = fingerSpan(a, b);
      const scale =
        pinch.current.start.scale *
        (span.dist / Math.max(1, pinch.current.span.dist));
      apply(
        zoomAbout(
          pinch.current.start,
          scale,
          pinch.current.span.mid,
          span.mid,
          center(),
        ),
      );
      return;
    }
    const dragging = gesture.current;
    if (dragging === null || pointers.current.size !== 1) return;
    const dx = event.clientX - dragging.from.x;
    const dy = event.clientY - dragging.from.y;
    if (Math.abs(dx) + Math.abs(dy) > TAP_SLOP_PX) moved.current = true;
    if (dragging.kind === "dismiss") {
      // The whole viewer follows the finger, in whichever direction it went.
      setOffset({ x: dx, y: dy });
      return;
    }
    apply(panBy(dragging.start, dx, dy, size(), base()));
  };

  const onPointerUp = (event: PointerEvent) => {
    const onImage = downTarget.current?.closest(".lightbox-image") != null;
    const ended = gesture.current;
    pointers.current.delete(event.pointerId);
    if (pointers.current.size === 1) {
      // A finger lifted out of a pinch: the remaining one keeps dragging.
      pinch.current = null;
      const [only] = [...pointers.current.values()];
      gesture.current = singleGesture(only, event.timeStamp);
    } else if (pointers.current.size === 0) {
      gesture.current = null;
      pinch.current = null;
    }
    if (ended?.kind === "dismiss" && pointers.current.size === 0) {
      setSwiping(false);
      const dx = event.clientX - ended.from.x;
      const dy = event.clientY - ended.from.y;
      if (dismisses({ dx, dy, elapsed: event.timeStamp - ended.t0 })) {
        // Leave the offset in place: the exit fades from where the finger let
        // go, rather than snapping back first.
        onClose();
        return;
      }
      setOffset(null);
    }
    if (onImage && !moved.current) {
      const now = event.timeStamp;
      if (lastTap.current !== null && now - lastTap.current < DOUBLE_TAP_MS) {
        apply(toggleZoom(current.current, at(event), center()));
        lastTap.current = null;
        // The follow-up click must not read as a backdrop tap.
        moved.current = true;
      } else {
        lastTap.current = now;
      }
    }
  };

  const onClick = (event: MouseEvent) => {
    if (moved.current) {
      moved.current = false;
      return;
    }
    // Only the backdrop (the stage itself) closes on a tap; a tap on the
    // picture is for zooming. The recorded pointerdown target is preferred
    // because capture retargets the click; a programmatic click, with no
    // pointerdown before it, falls back to the click's own target.
    const target = downTarget.current ?? (event.target as Element);
    if (target === stage.current) onClose();
  };

  // The swipe moves the stage, and the picture with it; the dim layer behind
  // stays put and only fades, proportionally to how far the finger has dragged,
  // so the page shows through as the image leaves. No transition while the
  // finger is down, so neither eases behind it; released short of the threshold
  // the offset clears and the CSS transitions settle them back.
  const scrimStyle =
    offset === null
      ? undefined
      : {
          opacity: Math.max(0, 1 - Math.hypot(offset.x, offset.y) / 300),
          transition: swiping ? "none" : undefined,
        };
  const stageStyle =
    offset === null
      ? undefined
      : {
          transform: `translate(${offset.x}px, ${offset.y}px)`,
          transition: swiping ? "none" : undefined,
        };

  return (
    <div
      className={closing ? "lightbox closing" : "lightbox"}
      role="dialog"
      aria-modal="true"
      aria-label={image.alt}
    >
      <div className="lightbox-scrim" style={scrimStyle} />
      <div className="lightbox-head">
        <span className="lightbox-caption">{image.caption ?? image.alt}</span>
        <button
          type="button"
          className="lightbox-close"
          aria-label="close image"
          onClick={onClose}
          autoFocus
        >
          ✕
        </button>
      </div>
      <div
        ref={stage}
        className="lightbox-stage"
        style={stageStyle}
        onClick={onClick}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerUp}
      >
        <img
          ref={picture}
          className="lightbox-image"
          src={image.src}
          alt={image.alt}
          draggable={false}
          style={{
            transform: `translate(${transform.x}px, ${transform.y}px) scale(${transform.scale})`,
          }}
        />
      </div>
    </div>
  );
}

/** The app's one lightbox, over the module store. Mounted by the session view
 * (which also counts it for back-navigation, overlayHistory.ts) and by the
 * gallery, which has no history to integrate with. */
export function LightboxHost() {
  const image = useLightboxImage();
  // The exit fade needs the element mounted after the store clears (React
  // unmounts before CSS can animate), so `shown` holds the last image through
  // it — the inspector's `shown`/`closing` pattern.
  const [shown, setShown] = useState<LightboxImage | null>(null);
  if (image !== null && image !== shown) setShown(image);
  const closing = image === null && shown !== null;
  const done = useCallback(() => setShown(null), []);
  useEffect(() => {
    if (!closing) return;
    const id = setTimeout(done, EXIT_MS);
    return () => clearTimeout(id);
  }, [closing, done]);
  if (shown === null) return null;
  // Keyed by src so a second image remounts at fit rather than inheriting the
  // first one's transform.
  return (
    <Lightbox
      key={shown.src}
      image={shown}
      closing={closing}
      onClose={closeLightbox}
    />
  );
}
