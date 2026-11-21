import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { openEventStream } from "./event-stream";

/** Stands in for the browser's `EventSource`; the test drives its events. */
class FakeSource {
  static made: FakeSource[] = [];
  readonly CONNECTING = 0;
  readonly OPEN = 1;
  readonly CLOSED = 2;
  readyState = 0;
  onopen: (() => void) | null = null;
  onmessage: ((message: { data: string }) => void) | null = null;
  onerror: (() => void) | null = null;
  constructor(readonly url: string) {
    FakeSource.made.push(this);
  }
  open() {
    this.readyState = this.OPEN;
    this.onopen?.();
  }
  send(data: string) {
    this.onmessage?.({ data });
  }
  /** A dropped connection the browser retries by itself. */
  drop() {
    this.readyState = this.CONNECTING;
    this.onerror?.();
  }
  /** An HTTP error on reconnecting: the browser gives up. */
  fail() {
    this.readyState = this.CLOSED;
    this.onerror?.();
  }
  close() {
    this.readyState = this.CLOSED;
  }
}

function start() {
  const messages: string[] = [];
  let opens = 0;
  const close = openEventStream(
    "/api/events",
    {
      onMessage: (data) => messages.push(data),
      onOpen: () => {
        opens += 1;
      },
    },
    {
      EventSource: FakeSource as unknown as new (url: string) => EventSource,
      delays: [100, 500],
    },
  );
  return { messages, opens: () => opens, close };
}

describe("openEventStream", () => {
  beforeEach(() => {
    FakeSource.made = [];
    vi.useFakeTimers();
  });
  afterEach(() => vi.useRealTimers());

  it("passes events on and reports each opening", () => {
    const stream = start();
    const [source] = FakeSource.made;
    source.open();
    source.send("a");
    source.drop();
    source.open();
    source.send("b");
    expect(stream.messages).toEqual(["a", "b"]);
    expect(stream.opens()).toBe(2);
    // The browser retried by itself: no second EventSource.
    expect(FakeSource.made).toHaveLength(1);
  });

  it("opens a new stream after the browser gives up, waiting longer each time", () => {
    const stream = start();
    FakeSource.made[0].fail();
    vi.advanceTimersByTime(99);
    expect(FakeSource.made).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(FakeSource.made).toHaveLength(2);

    FakeSource.made[1].fail();
    vi.advanceTimersByTime(499);
    expect(FakeSource.made).toHaveLength(2);
    vi.advanceTimersByTime(1);
    expect(FakeSource.made).toHaveLength(3);

    // Opening resets the wait.
    FakeSource.made[2].open();
    FakeSource.made[2].fail();
    vi.advanceTimersByTime(100);
    expect(FakeSource.made).toHaveLength(4);
    FakeSource.made[3].open();
    FakeSource.made[3].send("back");
    expect(stream.messages).toEqual(["back"]);
    expect(stream.opens()).toBe(2);
  });

  it("stops for good when closed", () => {
    const stream = start();
    const [source] = FakeSource.made;
    source.fail();
    stream.close();
    vi.advanceTimersByTime(10_000);
    expect(FakeSource.made).toHaveLength(1);
    expect(source.readyState).toBe(source.CLOSED);
  });
});
