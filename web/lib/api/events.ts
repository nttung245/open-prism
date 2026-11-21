/** The event stream every open page listens to (`/api/events`): jobs and their progress. */
import { useEffect, useRef } from "react";
import type { AppEvent } from "./types/AppEvent";

/**
 * Calls `onEvent` for every `AppEvent` while the component is mounted. `EventSource` reconnects
 * by itself after a drop; events missed meanwhile are covered by a `resync` once it is back.
 */
export function useAppEvents(onEvent: (event: AppEvent) => void): void {
  const handler = useRef(onEvent);
  useEffect(() => {
    handler.current = onEvent;
  });
  useEffect(() => {
    const source = new EventSource("/api/events");
    let reconnecting = false;
    source.onmessage = (message) => {
      try {
        handler.current(JSON.parse(message.data) as AppEvent);
      } catch {
        // Ignore malformed events.
      }
    };
    source.onerror = () => {
      reconnecting = true;
    };
    source.onopen = () => {
      if (reconnecting) handler.current({ type: "resync" });
      reconnecting = false;
    };
    return () => source.close();
  }, []);
}
