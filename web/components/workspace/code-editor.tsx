"use client";

import {
  autocompletion,
  closeBrackets,
  closeBracketsKeymap,
  completionKeymap,
} from "@codemirror/autocomplete";
import {
  defaultKeymap,
  history,
  historyKeymap,
  indentWithTab,
} from "@codemirror/commands";
import {
  bracketMatching,
  HighlightStyle,
  indentOnInput,
  StreamLanguage,
  syntaxHighlighting,
} from "@codemirror/language";
import { stex } from "@codemirror/legacy-modes/mode/stex";
import {
  type Diagnostic as CmDiagnostic,
  lintGutter,
  lintKeymap,
  setDiagnostics,
} from "@codemirror/lint";
import { highlightSelectionMatches, searchKeymap } from "@codemirror/search";
import { EditorSelection, EditorState } from "@codemirror/state";
import {
  drawSelection,
  dropCursor,
  EditorView,
  highlightActiveLine,
  highlightActiveLineGutter,
  keymap,
  lineNumbers,
  rectangularSelection,
} from "@codemirror/view";
import { tags } from "@lezer/highlight";
import { type Ref, useEffect, useImperativeHandle, useRef } from "react";
import type { Diagnostic } from "@/lib/api/types/Diagnostic";

/** Commands the editor triggers in the workspace. */
export type EditorActions = {
  save: () => void;
  compile: () => void;
  syncToPdf: () => void;
};

export type CodeEditorHandle = {
  /** Moves the cursor to a 1-based line (and column) and scrolls it into view. */
  goTo: (line: number, column?: number | null) => void;
  /** 1-based line and column of the main cursor. */
  cursor: () => { line: number; column: number } | null;
  /** The main selection (empty text when nothing is selected). */
  selection: () => { from: number; to: number; text: string } | null;
  /**
   * Replaces `expected` at `from`-`to` with `text`, or the unique other occurrence of
   * `expected` if the document moved. Returns false if it cannot be found.
   */
  replace: (
    from: number,
    to: number,
    expected: string,
    text: string,
  ) => boolean;
  focus: () => void;
};

const highlightStyle = HighlightStyle.define([
  { tag: tags.tagName, color: "var(--cm-command)" },
  { tag: tags.atom, color: "var(--cm-environment)" },
  { tag: tags.keyword, color: "var(--cm-math)" },
  { tag: tags.comment, color: "var(--cm-comment)", fontStyle: "italic" },
  { tag: tags.bracket, color: "var(--cm-bracket)" },
  { tag: [tags.string, tags.number], color: "var(--cm-string)" },
  { tag: tags.invalid, color: "var(--danger)" },
]);

const theme = EditorView.theme({
  "&": {
    height: "100%",
    fontSize: "14px",
    backgroundColor: "var(--background)",
    color: "var(--foreground)",
  },
  ".cm-scroller": {
    fontFamily:
      "ui-monospace, SFMono-Regular, 'JetBrains Mono', Menlo, Consolas, monospace",
    lineHeight: "1.6",
  },
  ".cm-content": { caretColor: "var(--foreground)", paddingBottom: "40vh" },
  ".cm-cursor": { borderLeftColor: "var(--foreground)" },
  "&.cm-focused .cm-selectionBackground, .cm-selectionBackground, ::selection":
    { backgroundColor: "var(--cm-selection) !important" },
  ".cm-activeLine": { backgroundColor: "var(--cm-active-line)" },
  ".cm-gutters": {
    backgroundColor: "var(--background)",
    color: "var(--cm-gutter)",
    border: "none",
  },
  ".cm-activeLineGutter": {
    backgroundColor: "var(--cm-active-line)",
    color: "var(--foreground)",
  },
  ".cm-tooltip": {
    backgroundColor: "var(--panel)",
    border: "1px solid var(--border)",
    color: "var(--foreground)",
  },
  ".cm-panels": {
    backgroundColor: "var(--panel)",
    color: "var(--foreground)",
  },
});

/** Creates the state of one document. `actions` is read when a key binding fires. */
export function createEditorState(
  text: string,
  actions: { current: EditorActions },
): EditorState {
  return EditorState.create({
    doc: text,
    extensions: [
      lineNumbers(),
      highlightActiveLineGutter(),
      history(),
      drawSelection(),
      dropCursor(),
      EditorState.allowMultipleSelections.of(true),
      indentOnInput(),
      bracketMatching(),
      closeBrackets(),
      autocompletion(),
      rectangularSelection(),
      highlightActiveLine(),
      highlightSelectionMatches(),
      lintGutter(),
      StreamLanguage.define(stex),
      syntaxHighlighting(highlightStyle),
      EditorView.lineWrapping,
      theme,
      keymap.of([
        {
          key: "Mod-s",
          preventDefault: true,
          run: () => {
            actions.current.save();
            return true;
          },
        },
        {
          key: "Mod-Enter",
          preventDefault: true,
          run: () => {
            actions.current.compile();
            return true;
          },
        },
        {
          key: "Mod-Alt-ArrowRight",
          preventDefault: true,
          run: () => {
            actions.current.syncToPdf();
            return true;
          },
        },
        ...closeBracketsKeymap,
        ...defaultKeymap,
        ...searchKeymap,
        ...historyKeymap,
        ...completionKeymap,
        ...lintKeymap,
        indentWithTab,
      ]),
    ],
  });
}

function toCmDiagnostics(
  state: EditorState,
  diagnostics: Diagnostic[],
): CmDiagnostic[] {
  return diagnostics.flatMap((diagnostic) => {
    if (diagnostic.line === null) return [];
    const lineNumber = Math.min(Math.max(diagnostic.line, 1), state.doc.lines);
    const line = state.doc.line(lineNumber);
    return [
      {
        from: line.from,
        to: line.to,
        severity:
          diagnostic.severity === "error"
            ? "error"
            : diagnostic.severity === "warning"
              ? "warning"
              : "info",
        source: diagnostic.source,
        message: diagnostic.details
          ? `${diagnostic.message}\n${diagnostic.details}`
          : diagnostic.message,
      },
    ];
  });
}

/**
 * The CodeMirror view. It shows the document state owned by the store and reports every
 * change through `onChange`; switching documents swaps the whole state, so each file keeps
 * its own undo history. The state is read from `getState` (not passed as a prop) so a
 * re-render can never put back a state older than what the user just typed.
 */
export function CodeEditor({
  path,
  revision,
  getState,
  diagnostics,
  onChange,
  onView,
  handle,
}: {
  path: string;
  /** Changes when the store replaced the document's state (load, reload from disk). */
  revision: number;
  getState: (path: string) => EditorState | null;
  diagnostics: Diagnostic[];
  onChange: (path: string, state: EditorState) => void;
  onView: (view: EditorView | null) => void;
  handle?: Ref<CodeEditorHandle>;
}) {
  const parent = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const current = useRef({ path, onChange });
  current.current = { path, onChange };

  // Create the view once.
  // biome-ignore lint/correctness/useExhaustiveDependencies: later states are swapped in below.
  useEffect(() => {
    const initial = getState(path);
    if (!parent.current || !initial) return;
    const editor = new EditorView({
      state: initial,
      parent: parent.current,
      dispatchTransactions(transactions, target) {
        target.update(transactions);
        current.current.onChange(current.current.path, target.state);
      },
    });
    view.current = editor;
    onView(editor);
    return () => {
      onView(null);
      editor.destroy();
      view.current = null;
    };
  }, []);

  // Show another document, or a state the store replaced.
  // biome-ignore lint/correctness/useExhaustiveDependencies: `revision` signals a new state.
  useEffect(() => {
    const editor = view.current;
    const latest = getState(path);
    if (editor && latest && editor.state !== latest) {
      editor.setState(latest);
    }
  }, [path, revision, getState]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: re-apply after a document switch.
  useEffect(() => {
    const editor = view.current;
    if (!editor) return;
    editor.dispatch(
      setDiagnostics(editor.state, toCmDiagnostics(editor.state, diagnostics)),
    );
  }, [diagnostics, path, revision]);

  useImperativeHandle(
    handle,
    () => ({
      goTo(line, column) {
        const editor = view.current;
        if (!editor) return;
        const target = editor.state.doc.line(
          Math.min(Math.max(line, 1), editor.state.doc.lines),
        );
        const pos = Math.min(
          target.from + Math.max((column ?? 1) - 1, 0),
          target.to,
        );
        editor.dispatch({
          selection: EditorSelection.cursor(pos),
          effects: EditorView.scrollIntoView(pos, { y: "center" }),
        });
        editor.focus();
      },
      cursor() {
        const editor = view.current;
        if (!editor) return null;
        const head = editor.state.selection.main.head;
        const line = editor.state.doc.lineAt(head);
        return { line: line.number, column: head - line.from + 1 };
      },
      selection() {
        const editor = view.current;
        if (!editor) return null;
        const { from, to } = editor.state.selection.main;
        return { from, to, text: editor.state.sliceDoc(from, to) };
      },
      replace(from, to, expected, text) {
        const editor = view.current;
        if (!editor || !expected) return false;
        let start = from;
        if (editor.state.sliceDoc(from, to) !== expected) {
          const doc = editor.state.doc.toString();
          start = doc.indexOf(expected);
          if (start === -1 || doc.indexOf(expected, start + 1) !== -1)
            return false;
        }
        editor.dispatch({
          changes: { from: start, to: start + expected.length, insert: text },
          selection: EditorSelection.range(start, start + text.length),
          scrollIntoView: true,
        });
        editor.focus();
        return true;
      },
      focus() {
        view.current?.focus();
      },
    }),
    [],
  );

  return <div ref={parent} className="h-full min-h-0 overflow-hidden" />;
}
