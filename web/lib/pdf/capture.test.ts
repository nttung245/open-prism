import { describe, expect, it } from "vitest";
import {
  captureScale,
  centre,
  clampRect,
  classifyGesture,
  MAX_CAPTURE_SIDE,
} from "./capture";

const A4 = { width: 595, height: 842 };

describe("classifyGesture", () => {
  it("is a click below 8 pixels of movement and a drag from 8 on", () => {
    expect(classifyGesture({ x: 10, y: 10 }, { x: 10, y: 10 })).toBe("click");
    expect(classifyGesture({ x: 10, y: 10 }, { x: 17, y: 10 })).toBe("click");
    expect(classifyGesture({ x: 10, y: 10 }, { x: 18, y: 10 })).toBe("drag");
    expect(classifyGesture({ x: 10, y: 10 }, { x: 60, y: 10 })).toBe("drag");
  });
});

describe("clampRect", () => {
  it("turns CSS pixels into PDF points, whichever way the drag went", () => {
    // 2 CSS pixels per point.
    expect(clampRect({ x: 300, y: 200 }, { x: 100, y: 100 }, A4, 2)).toEqual({
      x: 50,
      y: 50,
      width: 100,
      height: 50,
    });
  });

  it("stops at the edge of the page where the drag started", () => {
    // Dragged far below the page, onto the next one.
    const rect = clampRect({ x: 100, y: 1600 }, { x: 400, y: 2500 }, A4, 2);
    expect(rect.y + rect.height).toBe(A4.height);
    const left = clampRect({ x: 100, y: 100 }, { x: -50, y: 300 }, A4, 1);
    expect(left.x).toBe(0);
  });
});

describe("captureScale", () => {
  it("captures at least 150 DPI however far the PDF is zoomed out", () => {
    // A 2-inch wide region (144 points) at 50 %.
    const rect = { x: 0, y: 0, width: 144, height: 30 };
    const scale = captureScale(rect, 0.5, 1);
    expect(rect.width * scale).toBeGreaterThanOrEqual(300);
  });

  it("captures twice the screen resolution when that is more", () => {
    const rect = { x: 0, y: 0, width: 100, height: 20 };
    expect(captureScale(rect, 1.5, 2)).toBe(6);
  });

  it("keeps the longest side within 2,000 pixels", () => {
    const page = { x: 5, y: 5, width: 585, height: 832 };
    const scale = captureScale(page, 1.5, 2);
    expect(Math.round(page.height * scale)).toBe(MAX_CAPTURE_SIDE);
    expect(page.width * scale).toBeLessThan(MAX_CAPTURE_SIDE);
  });
});

describe("centre", () => {
  it("is the middle of the region", () => {
    expect(centre({ x: 10, y: 20, width: 100, height: 40 })).toEqual({
      x: 60,
      y: 40,
    });
  });
});
