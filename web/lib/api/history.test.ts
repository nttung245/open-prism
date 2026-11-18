import { describe, expect, it } from "vitest";
import { diffQuery, pageQuery } from "./history";

describe("history queries", () => {
  it("builds timeline pages", () => {
    expect(pageQuery(50)).toBe("?limit=50");
    expect(pageQuery(50, null)).toBe("?limit=50");
    expect(pageQuery(20, "abc123")).toBe("?limit=20&before=abc123");
  });

  it("leaves out the default comparison and encodes paths", () => {
    expect(diffQuery("parent")).toBe("");
    expect(diffQuery("current")).toBe("?against=current");
    expect(diffQuery("parent", "chapters/intro & more.tex")).toBe(
      "?path=chapters%2Fintro+%26+more.tex",
    );
    expect(diffQuery("0123abcd", "main.tex")).toBe(
      "?against=0123abcd&path=main.tex",
    );
  });
});
