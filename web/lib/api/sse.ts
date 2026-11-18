/**
 * Server-sent events over `fetch`, for POST requests (`EventSource` only does GET).
 */
import { apiFetch } from "./client";

/** Splits a byte stream into the `data` payloads of SSE events. */
export class SseParser {
  private buffer = "";

  /** Feeds text; returns the payloads of the events completed by it. */
  push(text: string): string[] {
    this.buffer += text.replace(/\r\n/g, "\n");
    const payloads: string[] = [];
    let end = this.buffer.indexOf("\n\n");
    while (end !== -1) {
      const block = this.buffer.slice(0, end);
      this.buffer = this.buffer.slice(end + 2);
      const data = block
        .split("\n")
        .filter((line) => line.startsWith("data:"))
        .map((line) => line.slice(5).replace(/^ /, ""));
      if (data.length > 0) payloads.push(data.join("\n"));
      end = this.buffer.indexOf("\n\n");
    }
    return payloads;
  }
}

/** POSTs `body` as JSON and calls `onEvent` for every JSON event until the stream ends. */
export async function postEventStream<T>(
  path: `/api/${string}`,
  body: unknown,
  onEvent: (event: T) => void,
  signal?: AbortSignal,
): Promise<void> {
  const response = await apiFetch(path, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      accept: "text/event-stream",
    },
    body: JSON.stringify(body),
    signal,
  });
  if (!response.body) return;
  const reader = response.body.pipeThrough(new TextDecoderStream()).getReader();
  const parser = new SseParser();
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    for (const payload of parser.push(value)) {
      try {
        onEvent(JSON.parse(payload) as T);
      } catch {
        // Ignore malformed events.
      }
    }
  }
}
