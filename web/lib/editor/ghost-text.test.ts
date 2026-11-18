import { autocompletion, startCompletion } from "@codemirror/autocomplete";
import { history, undo } from "@codemirror/commands";
import {
  EditorSelection,
  EditorState,
  type TransactionSpec,
} from "@codemirror/state";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  acceptGhost,
  acceptGhostWord,
  dismissGhost,
  type Ghost,
  ghostField,
  isLatexFile,
  MAX_PREFIX,
  MAX_SUFFIX,
  nextWord,
  type SuggestContext,
  SuggestionCache,
  type SuggestionRequest,
  SuggestionScheduler,
  setGhost,
  shouldSuggest,
} from "./ghost-text";

function stateWith(doc: string, cursor = doc.length) {
  return EditorState.create({
    doc,
    selection: EditorSelection.cursor(cursor),
    extensions: [ghostField, history(), autocompletion()],
  });
}

function withGhost(doc: string, text: string) {
  const state = stateWith(doc);
  return state.update({ effects: setGhost.of({ pos: doc.length, text }) })
    .state;
}

function apply(state: EditorState, spec: TransactionSpec | null) {
  if (!spec) throw new Error("no transaction");
  return state.update(spec).state;
}

function type(state: EditorState, text: string) {
  const pos = state.selection.main.head;
  return state.update({
    changes: { from: pos, insert: text },
    selection: EditorSelection.cursor(pos + text.length),
    userEvent: "input.type",
  }).state;
}

const context: SuggestContext = {
  enabled: true,
  paused: false,
  path: "main.tex",
  conflict: false,
};

describe("ghost text state", () => {
  it("shrinks while the writer types it and disappears otherwise", () => {
    let state = withGhost("Our model", " has three parts");
    state = type(state, " has");
    expect(state.field(ghostField)).toEqual({
      pos: "Our model has".length,
      text: " three parts",
    });
    // Typing something else.
    expect(type(state, "X").field(ghostField)).toBeNull();
    // Moving the cursor.
    const moved = state.update({ selection: EditorSelection.cursor(0) }).state;
    expect(moved.field(ghostField)).toBeNull();
    // Typing all of it leaves nothing to show.
    expect(type(state, " three parts").field(ghostField)).toBeNull();
  });

  it("accepts the whole suggestion as one undo step, even right after typing", () => {
    let state = stateWith("We propose");
    state = type(state, " a");
    state = state.update({
      effects: setGhost.of({ pos: state.doc.length, text: " new method." }),
    }).state;
    state = apply(state, acceptGhost(state));
    expect(state.doc.toString()).toBe("We propose a new method.");
    expect(state.selection.main.head).toBe(state.doc.length);
    expect(state.field(ghostField)).toBeNull();

    let undone = state;
    undo({
      state: undone,
      dispatch: (tr) => {
        undone = tr.state;
      },
    });
    expect(undone.doc.toString()).toBe("We propose a");
  });

  it("accepts one word at a time", () => {
    let state = withGhost("Results", " are shown below.");
    state = apply(state, acceptGhostWord(state));
    expect(state.doc.toString()).toBe("Results are");
    expect(state.field(ghostField)?.text).toBe(" shown below.");
    expect(nextWord("\n\\item Next")).toBe("\n\\item");
    expect(nextWord("   ")).toBe("   ");
  });

  it("does nothing without a suggestion, so Tab and Esc keep their usual meaning", () => {
    const state = stateWith("Text");
    expect(acceptGhost(state)).toBeNull();
    expect(acceptGhostWord(state)).toBeNull();
    expect(dismissGhost(state)).toBeNull();
    const shown = withGhost("Text", " more");
    expect(apply(shown, dismissGhost(shown)).field(ghostField)).toBeNull();
    expect(apply(shown, dismissGhost(shown)).doc.toString()).toBe("Text");
  });
});

describe("shouldSuggest", () => {
  it("asks only in LaTeX files, at a word end, without selection or conflict", () => {
    expect(shouldSuggest(stateWith("Our model "), context)).toBe(true);
    expect(isLatexFile("chapters/intro.tex")).toBe(true);
    expect(isLatexFile("style.STY")).toBe(true);
    for (const path of ["refs.bib", "notes.md", "Makefile", ".tex"]) {
      expect(isLatexFile(path), path).toBe(false);
      expect(shouldSuggest(stateWith("Text "), { ...context, path })).toBe(
        false,
      );
    }
    // In the middle of a word.
    expect(shouldSuggest(stateWith("thànhphần", 5), context)).toBe(false);
    // Before punctuation or a line break is fine.
    expect(shouldSuggest(stateWith("word.", 4), context)).toBe(true);
    // A selection.
    const selected = EditorState.create({
      doc: "some text",
      selection: EditorSelection.range(0, 4),
      extensions: [ghostField],
    });
    expect(shouldSuggest(selected, context)).toBe(false);
    expect(
      shouldSuggest(stateWith("Text "), { ...context, enabled: false }),
    ).toBe(false);
    expect(
      shouldSuggest(stateWith("Text "), { ...context, paused: true }),
    ).toBe(false);
    expect(
      shouldSuggest(stateWith("Text "), { ...context, conflict: true }),
    ).toBe(false);
    // Nothing around the cursor.
    expect(shouldSuggest(stateWith("  \n"), context)).toBe(false);
  });

  it("does not ask while the LaTeX command popup is open", () => {
    let popup = EditorState.create({
      doc: "\\sec",
      selection: EditorSelection.cursor(4),
      extensions: [
        ghostField,
        autocompletion({
          override: [() => ({ from: 0, options: [{ label: "\\section" }] })],
        }),
      ],
    });
    expect(shouldSuggest(popup, context)).toBe(true);
    startCompletion({
      get state() {
        return popup;
      },
      dispatch: (spec: TransactionSpec) => {
        popup = popup.update(spec).state;
      },
    } as never);
    // `startCompletion` marks the completion as pending.
    expect(shouldSuggest(popup, context)).toBe(false);
  });
});

describe("SuggestionScheduler", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  function setup(
    answer: (
      request: SuggestionRequest,
      onText: (text: string) => void,
    ) => Promise<string>,
  ) {
    let state = stateWith("Our model ");
    const shown: (Ghost | null)[] = [];
    const errors: string[] = [];
    const signals: AbortSignal[] = [];
    const fetch = vi.fn(
      (
        request: SuggestionRequest,
        signal: AbortSignal,
        onText: (text: string) => void,
      ) => {
        signals.push(signal);
        return answer(request, onText);
      },
    );
    // Like the view plugin: every transaction goes through `update`.
    const dispatch = (spec: TransactionSpec) => {
      const tr = state.update(spec);
      const before = state;
      state = tr.state;
      scheduler.update(
        before,
        state,
        tr.docChanged || tr.selection !== undefined,
      );
    };
    const scheduler: SuggestionScheduler = new SuggestionScheduler({
      fetch,
      context: () => context,
      currentState: () => state,
      show: (ghost) => {
        shown.push(ghost);
        dispatch({ effects: setGhost.of(ghost) });
      },
      cache: new SuggestionCache(),
      onError: (message) => errors.push(message),
    });
    const edit = (text: string) => {
      const pos = state.selection.main.head;
      dispatch({
        changes: { from: pos, insert: text },
        selection: EditorSelection.cursor(pos + text.length),
        userEvent: "input.type",
      });
    };
    return {
      scheduler,
      fetch,
      shown,
      errors,
      signals,
      edit,
      dispatch,
      get state() {
        return state;
      },
      set state(next: EditorState) {
        state = next;
      },
    };
  }

  /** An answer written piece by piece: `write` sends the suggestion so far, `end` the whole. */
  function streamed() {
    let onText: (text: string) => void = () => {};
    let resolve: (text: string) => void = () => {};
    return {
      answer: (_: SuggestionRequest, send: (text: string) => void) =>
        new Promise<string>((done) => {
          onText = send;
          resolve = done;
        }),
      write: (text: string) => onText(text),
      end: async (text: string) => {
        resolve(text);
        await vi.advanceTimersByTimeAsync(0);
      },
    };
  }

  it("waits for a pause in typing and sends one request", async () => {
    const t = setup(async () => " has three parts.");
    t.edit("h");
    await vi.advanceTimersByTimeAsync(300);
    t.edit("a");
    await vi.advanceTimersByTimeAsync(300);
    t.edit("s");
    expect(t.fetch).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(600);
    expect(t.fetch).toHaveBeenCalledTimes(1);
    expect(t.fetch.mock.calls[0][0]).toEqual({
      path: "main.tex",
      prefix: "Our model has",
      suffix: "",
    });
    expect(t.shown).toEqual([
      { pos: "Our model has".length, text: " has three parts." },
    ]);
  });

  it("cancels a pending request when the writer keeps typing and drops its answer", async () => {
    let resolve: (text: string) => void = () => {};
    const t = setup(
      () =>
        new Promise<string>((done) => {
          resolve = done;
        }),
    );
    t.edit(" ");
    await vi.advanceTimersByTimeAsync(600);
    expect(t.fetch).toHaveBeenCalledTimes(1);
    t.edit("x");
    expect(t.signals[0].aborted).toBe(true);
    resolve("late answer");
    await vi.advanceTimersByTimeAsync(0);
    expect(t.shown).toEqual([]);
  });

  it("reuses a cached answer for the same text around the cursor", async () => {
    const t = setup(async () => " cached.");
    t.edit(" ");
    await vi.advanceTimersByTimeAsync(600);
    expect(t.fetch).toHaveBeenCalledTimes(1);
    // Type a character, then delete it: same text as before.
    t.edit("x");
    const pos = t.state.selection.main.head;
    t.state = t.state.update({
      changes: { from: pos - 1, to: pos },
      selection: EditorSelection.cursor(pos - 1),
    }).state;
    t.scheduler.onChange(t.state);
    await vi.advanceTimersByTimeAsync(600);
    expect(t.fetch).toHaveBeenCalledTimes(1);
    expect(t.shown).toHaveLength(2);
  });

  it("reports errors once and does not ask while a suggestion is shown", async () => {
    const t = setup(async () => {
      throw new Error("no model is set up");
    });
    t.edit(" ");
    await vi.advanceTimersByTimeAsync(600);
    expect(t.errors).toEqual(["no model is set up"]);

    const shown = setup(async () => "x");
    shown.state = shown.state.update({
      effects: setGhost.of({ pos: shown.state.doc.length, text: " rest" }),
    }).state;
    shown.scheduler.onChange(shown.state);
    await vi.advanceTimersByTimeAsync(600);
    expect(shown.fetch).not.toHaveBeenCalled();
  });

  it("asks right away on request and sends at most 3,000 + 1,000 characters", async () => {
    const t = setup(async () => "");
    const before = "a ".repeat(2_000);
    const after = " b".repeat(1_000);
    t.state = EditorState.create({
      doc: before + after,
      selection: EditorSelection.cursor(before.length),
      extensions: [ghostField],
    });
    t.scheduler.requestNow(t.state);
    await vi.advanceTimersByTimeAsync(0);
    expect(t.fetch).toHaveBeenCalledTimes(1);
    const request = t.fetch.mock.calls[0][0];
    expect(request.prefix).toBe(before.slice(-MAX_PREFIX));
    expect(request.suffix).toBe(after.slice(0, MAX_SUFFIX));
    // An empty answer shows nothing.
    expect(t.shown).toEqual([]);
  });

  it("shows a suggestion while it is written", async () => {
    const model = streamed();
    const t = setup(model.answer);
    t.edit("gồm");
    await vi.advanceTimersByTimeAsync(600);
    const pos = "Our model gồm".length;
    model.write(" encoder");
    expect(t.state.field(ghostField)).toEqual({ pos, text: " encoder" });
    model.write(" encoder, decoder");
    expect(t.state.field(ghostField)).toEqual({
      pos,
      text: " encoder, decoder",
    });
    await model.end(" encoder, decoder và bộ nhớ.");
    expect(t.state.field(ghostField)).toEqual({
      pos,
      text: " encoder, decoder và bộ nhớ.",
    });
    expect(t.signals[0].aborted).toBe(false);
  });

  it("keeps a suggestion coming while the writer types it", async () => {
    const model = streamed();
    const t = setup(model.answer);
    t.edit("gồm");
    await vi.advanceTimersByTimeAsync(600);
    model.write(" encoder");
    t.edit(" enc");
    expect(t.signals[0].aborted).toBe(false);
    const pos = "Our model gồm enc".length;
    expect(t.state.field(ghostField)).toEqual({ pos, text: "oder" });
    model.write(" encoder, decoder");
    expect(t.state.field(ghostField)).toEqual({ pos, text: "oder, decoder" });
    await model.end(" encoder, decoder và bộ nhớ.");
    expect(t.state.field(ghostField)).toEqual({
      pos,
      text: "oder, decoder và bộ nhớ.",
    });
    expect(t.fetch).toHaveBeenCalledTimes(1);
  });

  it("stops a suggestion being written when it is accepted, dismissed or typed over", async () => {
    const actions: Record<string, (t: ReturnType<typeof setup>) => void> = {
      Tab: (t) => {
        const spec = acceptGhost(t.state);
        if (spec) t.dispatch(spec);
      },
      Esc: (t) => {
        const spec = dismissGhost(t.state);
        if (spec) t.dispatch(spec);
      },
      "other text": (t) => t.edit("X"),
      "cursor moved": (t) =>
        t.dispatch({ selection: EditorSelection.cursor(0) }),
    };
    for (const [name, act] of Object.entries(actions)) {
      const model = streamed();
      const t = setup(model.answer);
      t.edit("gồm");
      await vi.advanceTimersByTimeAsync(600);
      model.write(" encoder");
      act(t);
      expect(t.signals[0].aborted, name).toBe(true);
      const count = t.shown.length;
      model.write(" encoder, decoder");
      await model.end(" encoder, decoder và bộ nhớ.");
      expect(t.shown, name).toHaveLength(count);
      if (name === "Tab") {
        // Tab takes what was shown.
        expect(t.state.doc.toString()).toBe("Our model gồm encoder");
      }
      // Nothing half written was cached: the same place asks again.
      t.state = stateWith("Our model gồm");
      t.scheduler.requestNow(t.state);
      expect(t.fetch, name).toHaveBeenCalledTimes(2);
    }
  });

  it("removes what was shown when the whole suggestion is empty", async () => {
    const model = streamed();
    const t = setup(model.answer);
    t.edit("gồm");
    await vi.advanceTimersByTimeAsync(600);
    model.write(" encoder");
    expect(t.state.field(ghostField)).not.toBeNull();
    await model.end("");
    expect(t.state.field(ghostField)).toBeNull();
    expect(t.shown.at(-1)).toBeNull();
  });
});
