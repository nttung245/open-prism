/**
 * Server-sent events over `EventSource`, kept alive. The browser reconnects by itself after a
 * dropped connection, but gives up for good when a reconnection gets an HTTP error (the proxy's
 * 502 while the API restarts, for example). Then this opens a new one, waiting longer each time.
 */

export interface EventStreamHandlers {
  /** The `data` of each event. */
  onMessage(data: string): void;
  /**
   * The stream is (again) open. Events sent while it was closed are lost, so after a reopen
   * the page should fetch what it shows again.
   */
  onOpen(): void;
}

export interface EventStreamOptions {
  /** For tests. */
  EventSource?: new (
    url: string,
  ) => EventSource;
  /** Waits before each new connection after the browser gave up, in milliseconds. */
  delays?: readonly number[];
}

const DELAYS = [1000, 2000, 5000, 10_000, 30_000];

/** Opens `url` and keeps it open until the returned function is called. */
export function openEventStream(
  url: string,
  handlers: EventStreamHandlers,
  options: EventStreamOptions = {},
): () => void {
  const Source = options.EventSource ?? EventSource;
  const delays = options.delays ?? DELAYS;
  let source: EventSource | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let failures = 0;
  let closed = false;

  const connect = () => {
    timer = null;
    if (closed) return;
    const current = new Source(url);
    source = current;
    current.onopen = () => {
      failures = 0;
      handlers.onOpen();
    };
    current.onmessage = (message) => handlers.onMessage(message.data);
    current.onerror = () => {
      // While CONNECTING the browser retries by itself; CLOSED means it gave up.
      if (closed || current.readyState !== current.CLOSED) return;
      current.close();
      const delay = delays[Math.min(failures, delays.length - 1)];
      failures += 1;
      timer = setTimeout(connect, delay);
    };
  };
  connect();

  return () => {
    closed = true;
    if (timer) clearTimeout(timer);
    source?.close();
  };
}
