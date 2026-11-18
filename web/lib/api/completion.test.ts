import { afterEach, describe, expect, it, vi } from "vitest";
import { streamSuggestion } from "./completion";

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

const request = { path: "main.tex", prefix: "Our model", suffix: "" };

describe("streamSuggestion", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("passes on the suggestion so far and resolves with the whole one", async () => {
    const fetch = respond(
      'data: {"type":"text","text":" has"}\n\n',
      'data: {"type":"text","text":" has three',
      ' parts"}\n\ndata: {"type":"done","text":" has three parts."}\n\n',
    );
    const shown: string[] = [];
    const text = await streamSuggestion(
      "p1",
      request,
      new AbortController().signal,
      (t) => shown.push(t),
    );
    expect(shown).toEqual([" has", " has three parts"]);
    expect(text).toBe(" has three parts.");
    const [url, init] = fetch.mock.calls[0];
    expect(url).toBe("/api/ai/complete");
    expect(JSON.parse(String(init?.body))).toEqual({
      projectId: "p1",
      ...request,
    });
  });

  it("fails on an error event or a stream that stops early", async () => {
    respond(
      'data: {"type":"text","text":" has"}\n\n',
      'data: {"type":"error","message":"rate limited"}\n\n',
    );
    await expect(
      streamSuggestion("p1", request, new AbortController().signal, () => {}),
    ).rejects.toThrow("rate limited");

    respond('data: {"type":"text","text":" has"}\n\n');
    await expect(
      streamSuggestion("p1", request, new AbortController().signal, () => {}),
    ).rejects.toThrow("stopped before it was complete");
  });
});
