import { describe, expect, it } from "vitest";
import {
  addImage,
  addImages,
  bytesToBase64,
  type DraftImage,
  fitWithin,
  imageCaption,
  imageUploads,
  MAX_IMAGES,
  sniffImageType,
} from "./images";

function draft(key: string, bytes = [1, 2, 3]): DraftImage {
  return {
    key,
    blob: new Blob([new Uint8Array(bytes)], { type: "image/png" }),
    url: `blob:${key}`,
  };
}

describe("draft images", () => {
  it("recognises supported image formats by their bytes", () => {
    const cases: Array<[number[], string | null]> = [
      [[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a], "image/png"],
      [[0xff, 0xd8, 0xff, 0xe0], "image/jpeg"],
      [
        [0x52, 0x49, 0x46, 0x46, 0, 0, 0, 0, 0x57, 0x45, 0x42, 0x50],
        "image/webp",
      ],
      [[0x25, 0x50, 0x44, 0x46], null],
      [[0x47, 0x49, 0x46, 0x38, 0x39, 0x61], null],
      [[0x52, 0x49, 0x46, 0x46], null],
    ];
    for (const [bytes, expected] of cases) {
      expect(sniffImageType(new Uint8Array(bytes))).toBe(expected);
    }
  });

  it("fits large images within the limit without enlarging small images", () => {
    expect(fitWithin(4000, 3000, 2000)).toEqual({ width: 2000, height: 1500 });
    expect(fitWithin(800, 600, 2000)).toEqual({ width: 800, height: 600 });
    expect(fitWithin(1, 10000, 2000)).toEqual({ width: 1, height: 2000 });
  });

  it("adds the first incoming images up to the shared limit", () => {
    const result = addImages(
      [draft("pdf-1"), draft("pdf-2")],
      [draft("first"), draft("second"), draft("third")],
    );
    expect(result.images.map((image) => image.key)).toEqual([
      "pdf-1",
      "pdf-2",
      "first",
      "second",
    ]);
    expect(result.added).toBe(2);
    expect(result.skipped).toBe(1);
  });

  it("captions external images by name without changing PDF captions", () => {
    expect(imageCaption({ name: "Pasted image" })).toBe("Pasted image");
    expect(imageCaption({ name: "viet-tay.jpg" })).toBe("viet-tay.jpg");
    expect(imageCaption({ name: "capture.png", page: 2 })).toBe("Page 2");
  });

  it("takes at most four images", () => {
    let images: DraftImage[] = [];
    for (let i = 0; i < MAX_IMAGES; i += 1) {
      const result = addImage(images, draft(String(i)));
      expect(result.added).toBe(true);
      images = result.images;
    }
    const fifth = addImage(images, draft("5"));
    expect(fifth.added).toBe(false);
    expect(fifth.images).toHaveLength(4);
    expect(fifth.images).toBe(images);
  });

  it("captions an image with its page and source position", () => {
    expect(
      imageCaption({
        page: 2,
        source: { file: "chapters/intro.tex", line: 12 },
      }),
    ).toBe("Page 2 · chapters/intro.tex:12");
    expect(imageCaption({ page: 3 })).toBe("Page 3");
    expect(imageCaption({})).toBe("");
  });

  it("sends the bytes in base64 with the page and source", async () => {
    const image = {
      ...draft("a", [0x89, 0x50, 0x4e, 0x47]),
      page: 2,
      source: { file: "main.tex", line: 7 },
    };
    expect(await imageUploads([image, draft("b", [255])])).toEqual([
      { data: "iVBORw==", page: 2, source: { file: "main.tex", line: 7 } },
      { data: "/w==" },
    ]);
    // Large images are encoded in chunks.
    const large = new Uint8Array(100_000).fill(65);
    expect(atob(bytesToBase64(large))).toHaveLength(100_000);
  });
});
