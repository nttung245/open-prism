/**
 * Rendering and keys of the ghost text suggestions (see `ghost-text.ts` for the logic).
 */
import { Prec } from "@codemirror/state";
import {
  Decoration,
  EditorView,
  keymap,
  ViewPlugin,
  type ViewUpdate,
  WidgetType,
} from "@codemirror/view";
import {
  acceptGhost,
  acceptGhostWord,
  dismissGhost,
  ghostField,
  type SchedulerOptions,
  SuggestionScheduler,
  setGhost,
} from "./ghost-text";

/** What the workspace provides; read on every use so it always reflects the open file. */
export type GhostHooks = Pick<
  SchedulerOptions,
  "fetch" | "context" | "cache" | "onLoading" | "onError"
>;

class GhostWidget extends WidgetType {
  constructor(readonly text: string) {
    super();
  }

  eq(other: GhostWidget) {
    return other.text === this.text;
  }

  toDOM() {
    const span = document.createElement("span");
    span.className = "cm-ghost-text";
    span.textContent = this.text;
    span.setAttribute("aria-hidden", "true");
    return span;
  }
}

const ghostDecorations = EditorView.decorations.from(ghostField, (ghost) =>
  ghost
    ? Decoration.set([
        Decoration.widget({
          widget: new GhostWidget(ghost.text),
          side: 1,
        }).range(ghost.pos),
      ])
    : Decoration.none,
);

function suggester(hooks: () => GhostHooks) {
  return ViewPlugin.define((view) => {
    const scheduler = new SuggestionScheduler({
      fetch: (request, signal, onText) =>
        hooks().fetch(request, signal, onText),
      context: () => hooks().context(),
      cache: {
        get: (request) => hooks().cache.get(request),
        set: (request, text) => hooks().cache.set(request, text),
      },
      onLoading: (loading) => hooks().onLoading?.(loading),
      onError: (message) => hooks().onError?.(message),
      currentState: () => view.state,
      show: (ghost) => view.dispatch({ effects: setGhost.of(ghost) }),
    });
    return {
      scheduler,
      update(update: ViewUpdate) {
        const changed = update.docChanged || update.selectionSet;
        // Text being composed with an input method is not in the document yet.
        if (changed && update.view.composing) {
          scheduler.cancel();
          return;
        }
        scheduler.update(update.startState, update.state, changed);
      },
      destroy() {
        scheduler.cancel();
      },
    };
  });
}

/** Ghost text suggestions: Tab accepts, Ctrl+→ (Alt+→ on macOS) takes a word, Esc dismisses,
 * Alt+\ asks right away. */
export function ghostText(hooks: () => GhostHooks) {
  const plugin = suggester(hooks);
  const run =
    (command: typeof acceptGhost) =>
    (view: EditorView): boolean => {
      const spec = command(view.state);
      if (!spec) return false;
      view.dispatch(spec);
      return true;
    };
  return [
    ghostField,
    ghostDecorations,
    plugin,
    Prec.highest(
      keymap.of([
        { key: "Tab", run: run(acceptGhost) },
        {
          key: "Ctrl-ArrowRight",
          mac: "Alt-ArrowRight",
          run: run(acceptGhostWord),
        },
        { key: "Escape", run: run(dismissGhost) },
        {
          key: "Alt-\\",
          preventDefault: true,
          run: (view) => {
            view.plugin(plugin)?.scheduler.requestNow(view.state);
            return true;
          },
        },
      ]),
    ),
    EditorView.baseTheme({
      ".cm-ghost-text": {
        opacity: "0.45",
        whiteSpace: "pre-wrap",
        pointerEvents: "none",
      },
    }),
  ];
}
