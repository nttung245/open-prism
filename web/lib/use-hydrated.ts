import { useSyncExternalStore } from "react";

const subscribe = () => () => {};

/**
 * False while the server renders and while React hydrates, true afterwards. Parts that depend
 * on what only the browser has cached (who is signed in) render after hydration, so the HTML
 * the server sent still matches.
 */
export function useHydrated(): boolean {
  return useSyncExternalStore(
    subscribe,
    () => true,
    () => false,
  );
}
