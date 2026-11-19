import { EditorState } from "@codemirror/state";
import { describe, expect, it } from "vitest";
import {
  droppedImage,
  MAX_AFTER,
  MAX_BEFORE,
  pastedImage,
  textAroundPos,
} from "./image-input";

function clipboard(files: File[], text = "") {
  return {
    files: files as unknown as FileList,
    getData: (type: string) => (type === "text/plain" ? text : ""),
  };
}

const png = new File([new Uint8Array([0x89, 0x50])], "formula.png", {
  type: "image/png",
});
const tex = new File(["\\section{A}"], "part.tex", { type: "text/x-tex" });

describe("image input", () => {
  it("takes a paste with an image and no text", () => {
    expect(pastedImage(clipboard([png]))).toBe(png);
    // Copied from a page or an office document: the text wins.
    expect(pastedImage(clipboard([png], "E = mc^2"))).toBeNull();
    expect(pastedImage(clipboard([]))).toBeNull();
    expect(pastedImage(clipboard([tex]))).toBeNull();
    expect(pastedImage(null)).toBeNull();
  });

  it("takes the first image of a drop", () => {
    expect(droppedImage(clipboard([tex, png]))).toBe(png);
    expect(droppedImage(clipboard([tex]))).toBeNull();
  });

  it("sends at most 1,500 characters before and 500 after", () => {
    const doc = `${"a".repeat(2_000)}|${"b".repeat(1_000)}`;
    const state = EditorState.create({ doc });
    const { before, after } = textAroundPos(state, 2_000);
    expect(before).toBe("a".repeat(MAX_BEFORE));
    expect(after).toBe(`|${"b".repeat(MAX_AFTER - 1)}`);
    expect(textAroundPos(state, 0)).toEqual({
      before: "",
      after: doc.slice(0, MAX_AFTER),
    });
  });

  it("never cuts a character in half", () => {
    const emoji = "😀".repeat(1_000);
    const ending = EditorState.create({ doc: `${emoji}x` });
    const { before } = textAroundPos(ending, ending.doc.length);
    expect(before.length).toBe(MAX_BEFORE - 1);
    expect(before.startsWith("😀")).toBe(true);
    const starting = EditorState.create({ doc: `x${emoji}` });
    const { after } = textAroundPos(starting, 0);
    expect(after.length).toBe(MAX_AFTER - 1);
    expect(after.endsWith("😀")).toBe(true);
  });
});
