import { afterEach, describe, expect, it, vi } from "vitest";
import { prepareImage } from "./prepare-image";

const png = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
const jpeg = new Uint8Array([0xff, 0xd8, 0xff, 0xe0]);

afterEach(() => vi.unstubAllGlobals());

describe("prepareImage", () => {
  it("rejects unsupported content even when the file claims to be an image", async () => {
    const file = new File(["%PDF-1.7"], "fake.png", { type: "image/png" });
    await expect(prepareImage(file, file.name)).rejects.toThrow(
      /PNG, JPEG or WebP/,
    );
  });

  it("keeps the bytes of a small PNG unchanged", async () => {
    const file = new File([png], "small.png", { type: "image/png" });
    vi.stubGlobal("createImageBitmap", async () => ({
      width: 800,
      height: 600,
      close() {},
    }));
    const prepared = await prepareImage(file, file.name);
    expect(prepared.name).toBe("small.png");
    expect(prepared.blob).toBe(file);
    expect(new Uint8Array(await prepared.blob.arrayBuffer())).toEqual(png);
  });

  it("scales a large JPEG to 2000 by 1500 and keeps JPEG output", async () => {
    const file = new File([jpeg], "photo.jpg", { type: "image/jpeg" });
    vi.stubGlobal("createImageBitmap", async () => ({
      width: 4000,
      height: 3000,
      close() {},
    }));
    const canvas = {
      width: 0,
      height: 0,
      getContext: () => ({ drawImage() {} }),
      toBlob(callback: (blob: Blob) => void, type: string, quality: number) {
        expect(type).toBe("image/jpeg");
        expect(quality).toBe(0.9);
        callback(new Blob([jpeg], { type }));
      },
    };
    vi.stubGlobal("document", { createElement: () => canvas });
    const prepared = await prepareImage(file, file.name);
    expect({ width: canvas.width, height: canvas.height }).toEqual({
      width: 2000,
      height: 1500,
    });
    expect(prepared.blob.type).toBe("image/jpeg");
  });

  it("rejects an image that remains over 5 MiB", async () => {
    const file = new File([png, new Uint8Array(5 * 1024 * 1024)], "large.png", {
      type: "image/png",
    });
    vi.stubGlobal("createImageBitmap", async () => ({
      width: 800,
      height: 600,
      close() {},
    }));
    await expect(prepareImage(file, file.name)).rejects.toThrow(/5 MB/);
  });
});
