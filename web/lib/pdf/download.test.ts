import { describe, expect, it } from "vitest";
import { fileNameFromDisposition, pagesUrl } from "./download";

describe("pagesUrl", () => {
  it("swaps output.pdf for the pages endpoint of the same run", () => {
    expect(pagesUrl("/api/projects/p1/runs/r1/output.pdf")).toBe(
      "/api/projects/p1/runs/r1/pages",
    );
  });

  it("refuses other URLs", () => {
    expect(pagesUrl("/api/projects/p1/runs/r1/output.log")).toBeNull();
    expect(pagesUrl("https://example.org/output.pdf")).toBeNull();
    expect(pagesUrl("/api/projects/p1/runs/r1/output.pdf?x=1")).toBeNull();
  });
});

describe("fileNameFromDisposition", () => {
  it("prefers the UTF-8 name", () => {
    expect(
      fileNameFromDisposition(
        "attachment; filename=\"Lu_n v_n (pages 3-7_ 10).pdf\"; filename*=UTF-8''Lu%E1%BA%ADn%20v%C4%83n%20%28pages%203-7%2C%2010%29.pdf",
      ),
    ).toBe("Luận văn (pages 3-7, 10).pdf");
  });

  it("falls back to the plain name", () => {
    expect(fileNameFromDisposition('attachment; filename="Paper.pdf"')).toBe(
      "Paper.pdf",
    );
    expect(
      fileNameFromDisposition(
        "attachment; filename=\"a.pdf\"; filename*=UTF-8''%E0%A4%A",
      ),
    ).toBe("a.pdf");
  });

  it("has nothing to offer without a header", () => {
    expect(fileNameFromDisposition(null)).toBeNull();
    expect(fileNameFromDisposition("inline")).toBeNull();
  });
});
