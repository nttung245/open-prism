/**
 * Geometry of clicks and region captures on PDF pages. Pure functions so they can be tested
 * without a browser; the viewer turns pointer events into these inputs.
 */

/** Movement (CSS pixels) after pressing the button that makes it a drag instead of a click. */
export const DRAG_THRESHOLD = 8;
/** Least resolution of a captured image. */
export const MIN_CAPTURE_DPI = 150;
/** Most pixels on the longest side of a captured image. */
export const MAX_CAPTURE_SIDE = 2_000;

export type Point = { x: number; y: number };
export type Size = { width: number; height: number };
/** A rectangle in PDF points from the top-left corner of the page. */
export type Rect = { x: number; y: number; width: number; height: number };

/** A press and release `start` → `end` (CSS pixels) is a click or a drag. */
export function classifyGesture(
  start: Point,
  end: Point,
  threshold = DRAG_THRESHOLD,
): "click" | "drag" {
  return Math.hypot(end.x - start.x, end.y - start.y) >= threshold
    ? "drag"
    : "click";
}

/**
 * The region dragged from `start` to `end` (CSS pixels from the page's top-left corner, at
 * `scale` CSS pixels per point), in PDF points and clamped to the page.
 */
export function clampRect(
  start: Point,
  end: Point,
  page: Size,
  scale: number,
): Rect {
  const clamp = (value: number, max: number) =>
    Math.min(Math.max(value / scale, 0), max);
  const x1 = clamp(start.x, page.width);
  const x2 = clamp(end.x, page.width);
  const y1 = clamp(start.y, page.height);
  const y2 = clamp(end.y, page.height);
  return {
    x: Math.min(x1, x2),
    y: Math.min(y1, y2),
    width: Math.abs(x2 - x1),
    height: Math.abs(y2 - y1),
  };
}

/**
 * Pixels per PDF point for the image of `rect`: at least 150 DPI and twice what the screen
 * shows (`zoom` CSS pixels per point, times the device pixel ratio), but never more than
 * 2,000 pixels on the longest side.
 */
export function captureScale(
  rect: Rect,
  zoom: number,
  pixelRatio: number,
): number {
  const scale = Math.max(MIN_CAPTURE_DPI / 72, 2 * zoom * pixelRatio);
  const longest = Math.max(rect.width, rect.height);
  if (longest * scale > MAX_CAPTURE_SIDE) return MAX_CAPTURE_SIDE / longest;
  return scale;
}

/** The centre of a rectangle, for looking up its source position. */
export function centre(rect: Rect): Point {
  return { x: rect.x + rect.width / 2, y: rect.y + rect.height / 2 };
}
