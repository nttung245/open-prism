import { describe, expect, it } from "vitest";
import {
  basename,
  buildTree,
  dirname,
  extension,
  fileKind,
  isWithin,
  joinPath,
  validateName,
} from "./paths";

describe("path helpers", () => {
  it("splits paths", () => {
    expect(basename("a/b/c.tex")).toBe("c.tex");
    expect(basename("main.tex")).toBe("main.tex");
    expect(dirname("a/b/c.tex")).toBe("a/b");
    expect(dirname("main.tex")).toBe("");
    expect(joinPath("", "x.tex")).toBe("x.tex");
    expect(joinPath("a", "x.tex")).toBe("a/x.tex");
    expect(extension("Fig.PNG")).toBe("png");
    expect(extension(".latexmkrc")).toBe("");
  });

  it("classifies files", () => {
    expect(fileKind("main.tex")).toBe("text");
    expect(fileKind("refs.bib")).toBe("text");
    expect(fileKind("latexmkrc")).toBe("text");
    expect(fileKind("fig.svg")).toBe("image");
    expect(fileKind("paper.pdf")).toBe("pdf");
    expect(fileKind("data.xlsx")).toBe("binary");
  });

  it("matches paths inside directories only on component boundaries", () => {
    expect(isWithin("figs/a.png", "figs")).toBe(true);
    expect(isWithin("figs", "figs")).toBe(true);
    expect(isWithin("figures/a.png", "figs")).toBe(false);
  });

  it("validates names", () => {
    expect(validateName("intro.tex")).toBeNull();
    expect(validateName("  ")).not.toBeNull();
    expect(validateName("a/b")).not.toBeNull();
    expect(validateName("..")).not.toBeNull();
  });
});

describe("buildTree", () => {
  it("nests entries with directories first and natural ordering", () => {
    const tree = buildTree([
      { path: "main.tex", kind: "file", size: 10 },
      { path: "chapters", kind: "dir", size: 0 },
      { path: "chapters/ch10.tex", kind: "file", size: 1 },
      { path: "chapters/ch2.tex", kind: "file", size: 1 },
      { path: "appendix.tex", kind: "file", size: 1 },
    ]);
    expect(tree.map((n) => n.name)).toEqual([
      "chapters",
      "appendix.tex",
      "main.tex",
    ]);
    expect(tree[0].children.map((n) => n.name)).toEqual([
      "ch2.tex",
      "ch10.tex",
    ]);
  });
});
