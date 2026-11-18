/**
 * Logic of the grey "ghost text" suggestions shown at the editor cursor: the suggestion state,
 * the commands that accept or dismiss it, when to ask for one, and the scheduler that asks.
 * The rendering lives in `ghost-text-view.ts`; this module needs no DOM so it can be tested.
 *
 * A suggestion is never part of the document: autosave, history snapshots and undo do not
 * see it until the writer accepts it.
 */
import { completionStatus } from "@codemirror/autocomplete";
import { isolateHistory } from "@codemirror/commands";
import {
  EditorSelection,
  type EditorState,
  StateEffect,
  StateField,
  type Text,
  type Transaction,
  type TransactionSpec,
} from "@codemirror/state";

/** Characters of text before / after the cursor sent with a request. */
export const MAX_PREFIX = 3_000;
export const MAX_SUFFIX = 1_000;
/** Pause in typing before a suggestion is requested. */
export const DELAY_MS = 600;
const CACHE_SIZE = 50;
const LATEX_EXTENSIONS = new Set(["tex", "ltx", "sty", "cls"]);

export type Ghost = { pos: number; text: string };

/** Shows (`Ghost`) or removes (`null`) the suggestion. */
export const setGhost = StateEffect.define<Ghost | null>();

/**
 * The suggestion after a transaction. Typing exactly its next characters keeps the rest;
 * any other edit, or moving the cursor away, removes it.
 */
export function mapGhost(ghost: Ghost | null, tr: Transaction): Ghost | null {
  for (const effect of tr.effects) {
    if (effect.is(setGhost)) ghost = effect.value;
  }
  if (!ghost) return null;
  const current = ghost;
  const selection = tr.newSelection.main;
  if (tr.docChanged) {
    const typed: string[] = [];
    let changes = 0;
    tr.changes.iterChanges((fromA, toA, _fromB, _toB, inserted) => {
      changes += 1;
      if (fromA === current.pos && toA === current.pos) {
        typed.push(inserted.toString());
      }
    });
    const text = typed[0] ?? "";
    if (
      changes !== 1 ||
      !text ||
      text.length >= current.text.length ||
      !current.text.startsWith(text)
    ) {
      return null;
    }
    const pos = current.pos + text.length;
    if (!selection.empty || selection.head !== pos) return null;
    return { pos, text: current.text.slice(text.length) };
  }
  if (!selection.empty || selection.head !== current.pos) return null;
  return current;
}

export const ghostField = StateField.define<Ghost | null>({
  create: () => null,
  update: (ghost, tr) => mapGhost(ghost, tr),
});

function currentGhost(state: EditorState): Ghost | null {
  const ghost = state.field(ghostField, false) ?? null;
  if (!ghost || state.selection.main.head !== ghost.pos) return null;
  return ghost;
}

function insert(ghost: Ghost, text: string): TransactionSpec {
  return {
    changes: { from: ghost.pos, insert: text },
    selection: EditorSelection.cursor(ghost.pos + text.length),
    userEvent: "input.complete",
    // Its own undo step, even right after typing.
    annotations: isolateHistory.of("full"),
    scrollIntoView: true,
  };
}

/** Tab: inserts the whole suggestion. `null` when there is none (Tab then indents). */
export function acceptGhost(state: EditorState): TransactionSpec | null {
  const ghost = currentGhost(state);
  return ghost ? insert(ghost, ghost.text) : null;
}

/** The next word of a suggestion, with the spaces before it. */
export function nextWord(text: string): string {
  return /^\s*\S+/.exec(text)?.[0] ?? text;
}

/** Ctrl+→: inserts the next word; the rest stays. */
export function acceptGhostWord(state: EditorState): TransactionSpec | null {
  const ghost = currentGhost(state);
  return ghost ? insert(ghost, nextWord(ghost.text)) : null;
}

/** Esc: removes the suggestion. `null` when there is none. */
export function dismissGhost(state: EditorState): TransactionSpec | null {
  return state.field(ghostField, false) ? { effects: setGhost.of(null) } : null;
}

/** What the workspace knows about the open file. */
export type SuggestContext = {
  enabled: boolean;
  /** Requests failed: no more until suggestions are switched on again. */
  paused: boolean;
  path: string | null;
  conflict: boolean;
};

export function isLatexFile(path: string): boolean {
  const name = path.slice(path.lastIndexOf("/") + 1);
  const dot = name.lastIndexOf(".");
  return dot > 0 && LATEX_EXTENSIONS.has(name.slice(dot + 1).toLowerCase());
}

/** Whether a suggestion may be requested at the cursor. */
export function shouldSuggest(
  state: EditorState,
  context: SuggestContext,
): boolean {
  if (!context.enabled || context.paused || context.conflict) return false;
  if (!context.path || !isLatexFile(context.path)) return false;
  if (state.selection.ranges.length !== 1 || !state.selection.main.empty) {
    return false;
  }
  if (completionStatus(state) !== null) return false;
  const pos = state.selection.main.head;
  // In the middle of a word.
  if (/[\p{L}\p{N}]/u.test(state.doc.sliceString(pos, pos + 1))) return false;
  const { prefix, suffix } = textAround(state);
  return (prefix + suffix).trim() !== "";
}

/** The text sent with a request: up to 3,000 characters before and 1,000 after the cursor. */
export function textAround(state: EditorState): {
  prefix: string;
  suffix: string;
} {
  const pos = state.selection.main.head;
  return {
    prefix: state.doc.sliceString(Math.max(0, pos - MAX_PREFIX), pos),
    suffix: state.doc.sliceString(
      pos,
      Math.min(state.doc.length, pos + MAX_SUFFIX),
    ),
  };
}

export type SuggestionRequest = {
  path: string;
  prefix: string;
  suffix: string;
};

/** Recent suggestions by the text around the cursor, shared by all files. */
export class SuggestionCache {
  private entries = new Map<string, string>();

  static key(request: SuggestionRequest): string {
    return [
      request.path,
      request.prefix.slice(-400),
      request.suffix.slice(0, 100),
    ].join("\u0000");
  }

  get(request: SuggestionRequest): string | undefined {
    const key = SuggestionCache.key(request);
    const text = this.entries.get(key);
    if (text !== undefined) {
      // Most recently used last.
      this.entries.delete(key);
      this.entries.set(key, text);
    }
    return text;
  }

  set(request: SuggestionRequest, text: string) {
    const key = SuggestionCache.key(request);
    this.entries.delete(key);
    this.entries.set(key, text);
    while (this.entries.size > CACHE_SIZE) {
      const oldest = this.entries.keys().next().value;
      if (oldest === undefined) break;
      this.entries.delete(oldest);
    }
  }
}

export type SchedulerOptions = {
  /** Asks for a suggestion: calls `onText` with the suggestion so far while it is written,
   * and resolves with the whole suggestion. */
  fetch: (
    request: SuggestionRequest,
    signal: AbortSignal,
    onText: (text: string) => void,
  ) => Promise<string>;
  context: () => SuggestContext;
  /** The editor state now, to drop results for a position that is gone. */
  currentState: () => EditorState;
  /** Shows a suggestion, or removes it (`null`). */
  show: (ghost: Ghost | null) => void;
  cache: Pick<SuggestionCache, "get" | "set">;
  onLoading?: (loading: boolean) => void;
  onError?: (message: string) => void;
  delayMs?: number;
};

/** Where a request was made. */
type Origin = { doc: Text; pos: number };

/**
 * Asks for suggestions: after a pause in typing, one request at a time, shown while they are
 * written, cancelled as soon as the writer does anything but type the suggestion, with recent
 * answers cached.
 */
export class SuggestionScheduler {
  private timer: ReturnType<typeof setTimeout> | null = null;
  private controller: AbortController | null = null;

  constructor(private readonly options: SchedulerOptions) {}

  /**
   * After an editor update (`changed` when the document or the cursor changed). A suggestion
   * that went away (accepted, dismissed, typed over) stops its request.
   */
  update(startState: EditorState, state: EditorState, changed: boolean) {
    if (
      startState.field(ghostField, false) &&
      !state.field(ghostField, false)
    ) {
      this.cancel();
    }
    if (changed) this.onChange(state);
  }

  /** The document or the cursor changed. */
  onChange(state: EditorState) {
    // A suggestion is still shown (the writer is typing it): nothing to ask, and one still
    // being written keeps coming.
    if (state.field(ghostField, false)) return;
    this.cancel();
    if (!shouldSuggest(state, this.options.context())) return;
    this.timer = setTimeout(
      () => void this.run(state),
      this.options.delayMs ?? DELAY_MS,
    );
  }

  /** Alt+\: asks right away. */
  requestNow(state: EditorState) {
    this.cancel();
    if (!shouldSuggest(state, this.options.context())) return;
    void this.run(state);
  }

  cancel() {
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
    if (this.controller) {
      this.controller.abort();
      this.finish(this.controller);
    }
  }

  private finish(controller: AbortController) {
    if (this.controller !== controller) return;
    this.controller = null;
    this.options.onLoading?.(false);
  }

  private async run(state: EditorState) {
    this.timer = null;
    const path = this.options.context().path;
    if (!path) return;
    const request = { path, ...textAround(state) };
    const origin = { doc: state.doc, pos: state.selection.main.head };
    const cached = this.options.cache.get(request);
    if (cached !== undefined) {
      this.present(origin, cached, true);
      return;
    }
    const controller = new AbortController();
    this.controller = controller;
    this.options.onLoading?.(true);
    try {
      const text = await this.options.fetch(
        request,
        controller.signal,
        (partial) => {
          if (!controller.signal.aborted) this.present(origin, partial, false);
        },
      );
      if (controller.signal.aborted) return;
      this.finish(controller);
      this.options.cache.set(request, text);
      this.present(origin, text, true);
    } catch (error) {
      if (controller.signal.aborted) return;
      this.options.onError?.(
        error instanceof Error ? error.message : String(error),
      );
    } finally {
      this.finish(controller);
    }
  }

  /**
   * Shows `text`, the suggestion for `origin` so far (`final` once complete), after what the
   * writer has typed of it since.
   */
  private present(origin: Origin, text: string, final: boolean) {
    const now = this.options.currentState();
    const { head, empty } = now.selection.main;
    const ghost = now.field(ghostField, false) ?? null;
    let pos = origin.pos;
    let rest = text;
    if (now.doc !== origin.doc) {
      // Only typing the suggestion shown keeps it (any other change removes it).
      if (!ghost || ghost.pos <= origin.pos) return;
      const typed = now.doc.sliceString(origin.pos, ghost.pos);
      if (!text.startsWith(typed)) return;
      pos = ghost.pos;
      rest = text.slice(typed.length);
    }
    if (!empty || head !== pos) return;
    if (rest) {
      this.options.show({ pos, text: rest });
    } else if (final && ghost) {
      // Nothing left to suggest.
      this.options.show(null);
    }
  }
}
