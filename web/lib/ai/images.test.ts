import { describe, expect, it } from "vitest";
import {
  addImage,
  bytesToBase64,
  type DraftImage,
  imageCaption,
  imageUploads,
  MAX_IMAGES,
} from "./images";

function draft(key: string, bytes = [1, 2, 3]): DraftImage {
  return {
    key,
    blob: new Blob([new Uint8Array(bytes)], { type: "image/png" }),
    url: `blob:${key}`,
  };
}

describe("draft images", () => {
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
