import { afterEach, describe, expect, it, vi } from "vitest";
import { streamImageLatex } from "./image-latex";

function respond(...chunks: string[]) {
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      const encoder = new TextEncoder();
      for (const chunk of chunks) controller.enqueue(encoder.encode(chunk));
      controller.close();
    },
  });
  const fetch = vi.fn(
    async (_url: string, _init?: RequestInit) =>
      new Response(body, {
        headers: { "content-type": "text/event-stream" },
      }),
  );
  vi.stubGlobal("fetch", fetch);
  return fetch;
}

const request = {
  projectId: "p1",
  path: "main.tex",
  image: { data: "iVBORw0KGgo=" },
  before: "Results:\n",
  after: "",
};

describe("streamImageLatex", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("passes on the answer so far and resolves with the cleaned result", async () => {
    const fetch = respond(
      'data: {"type":"text","text":"\\\\begin{tabular}"}\n\n',
      'data: {"type":"text","text":"\\\\begin{tabular}{ll}',
      '"}\n\ndata: {"type":"done","latex":"\\\\begin{tabular}{ll}\\n\\\\end{tabular}","packages":["booktabs"],"citationsRemoved":true}\n\n',
    );
    const shown: string[] = [];
    const result = await streamImageLatex(
      request,
      new AbortController().signal,
      (t) => shown.push(t),
    );
    expect(shown).toEqual(["\\begin{tabular}", "\\begin{tabular}{ll}"]);
    expect(result).toEqual({
      latex: "\\begin{tabular}{ll}\n\\end{tabular}",
      packages: ["booktabs"],
      citationsRemoved: true,
    });
    const [url, init] = fetch.mock.calls[0];
    expect(url).toBe("/api/ai/image-to-latex");
    expect(JSON.parse(String(init?.body))).toEqual(request);
  });

  it("fails on an error event or a stream that stops early", async () => {
    respond(
      'data: {"type":"text","text":"x"}\n\n',
      'data: {"type":"error","message":"rate limited"}\n\n',
    );
    await expect(
      streamImageLatex(request, new AbortController().signal, () => {}),
    ).rejects.toThrow("rate limited");

    respond('data: {"type":"text","text":"x"}\n\n');
    await expect(
      streamImageLatex(request, new AbortController().signal, () => {}),
    ).rejects.toThrow("stopped before it was complete");
  });
});
