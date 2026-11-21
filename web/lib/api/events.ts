/** The event stream every open page listens to (`/api/events`): jobs and their progress. */
import { useEffect, useRef } from "react";
import { openEventStream } from "./event-stream";
import type { AppEvent } from "./types/AppEvent";

/**
 * Calls `onEvent` for every `AppEvent` while the component is shown. Each time the stream
 * opens, including the first, it also gets a `resync`: events sent while it was closed are
 * lost, and Next.js closes it while the page is hidden (another page is open).
 */
export function useAppEvents(onEvent: (event: AppEvent) => void): void {
  const handler = useRef(onEvent);
  useEffect(() => {
    handler.current = onEvent;
  });
  useEffect(
    () =>
      openEventStream("/api/events", {
        onMessage: (data) => {
          try {
            handler.current(JSON.parse(data) as AppEvent);
          } catch {
            // Ignore malformed events.
          }
        },
        onOpen: () => handler.current({ type: "resync" }),
      }),
    [],
  );
}
