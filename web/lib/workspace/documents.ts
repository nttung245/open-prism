/**
 * Open files of the workspace: their CodeMirror states, autosave and conflict handling.
 *
 * The server holds the reference copy of every file. Each open text file remembers the
 * version (content hash) it was loaded or saved at; saves send it as `If-Match`, so a change
 * made elsewhere in the meantime is reported as a conflict instead of being overwritten.
 */
import type { EditorState, Text } from "@codemirror/state";
import type { EditorView } from "@codemirror/view";
import {
  ApiError,
  apiFetch,
  errorMessage,
  etagVersion,
  fileUrl,
} from "@/lib/api/client";
import { type FileKind, fileKind, isWithin } from "@/lib/files/paths";

export type Conflict =
  /** The file changed on disk while there were unsaved edits. */
  | { kind: "external"; version: string | null }
  /** The server refused a save because the file changed since it was loaded. */
  | { kind: "rejected"; version: string | null }
  /** The file was deleted while there were unsaved edits. */
  | { kind: "deleted"; version: null };

export type DocStatus = {
  path: string;
  kind: FileKind;
  loading: boolean;
  dirty: boolean;
  saving: boolean;
  /** Last load or save error. */
  error: string | null;
  conflict: Conflict | null;
  /** Increases whenever the content is replaced from the server (for image cache busting). */
  revision: number;
};

export type Snapshot = {
  docs: DocStatus[];
  active: string | null;
};

export type WriteResult =
  | { ok: true; version: string }
  | { ok: false; current: string | null };

/** How documents reach the server; replaceable in tests. */
export interface Transport {
  read(path: string): Promise<{ text: string; version: string } | null>;
  write(
    path: string,
    text: string,
    baseVersion: string | null,
  ): Promise<WriteResult>;
}

export function httpTransport(projectId: string): Transport {
  return {
    async read(path) {
      try {
        const response = await apiFetch(fileUrl(projectId, path));
        return {
          text: await response.text(),
          version: etagVersion(response) ?? "",
        };
      } catch (error) {
        if (error instanceof ApiError && error.status === 404) return null;
        throw error;
      }
    },
    async write(path, text, baseVersion) {
      try {
        const response = await apiFetch(fileUrl(projectId, path), {
          method: "PUT",
          headers: baseVersion ? { "if-match": `"${baseVersion}"` } : {},
          body: text,
        });
        return { ok: true, version: etagVersion(response) ?? "" };
      } catch (error) {
        if (error instanceof ApiError && error.status === 412) {
          return {
            ok: false,
            current: error.response ? etagVersion(error.response) : null,
          };
        }
        throw error;
      }
    },
  };
}

type Doc = DocStatus & {
  state: EditorState | null;
  savedDoc: Text | null;
  baseVersion: string | null;
  timer: ReturnType<typeof setTimeout> | null;
  inflight: Promise<void> | null;
  saveAgain: boolean;
};

export type DocumentStoreOptions = {
  transport: Transport;
  createState: (text: string) => EditorState;
  autosaveMs?: number;
  retryMs?: number;
  /** Called after a successful save of `path`. */
  onSaved?: (path: string) => void;
};

export class DocumentStore {
  private docs = new Map<string, Doc>();
  private order: string[] = [];
  private active: string | null = null;
  private view: EditorView | null = null;
  private listeners = new Set<() => void>();
  private snapshot: Snapshot = { docs: [], active: null };
  private readonly autosaveMs: number;
  private readonly retryMs: number;

  constructor(private readonly options: DocumentStoreOptions) {
    this.autosaveMs = options.autosaveMs ?? 800;
    this.retryMs = options.retryMs ?? 3000;
  }

  // --- subscription (useSyncExternalStore) ---------------------------------------------

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  getSnapshot = (): Snapshot => this.snapshot;

  private emit() {
    this.snapshot = {
      docs: this.order.flatMap((path) => {
        const doc = this.docs.get(path);
        if (!doc) return [];
        const { path: p, kind, loading, dirty, saving, error, conflict } = doc;
        return [
          {
            path: p,
            kind,
            loading,
            dirty,
            saving,
            error,
            conflict,
            revision: doc.revision,
          },
        ];
      }),
      active: this.active,
    };
    for (const listener of this.listeners) listener();
  }

  // --- opening and closing -------------------------------------------------------------

  getState(path: string): EditorState | null {
    return this.docs.get(path)?.state ?? null;
  }

  isOpen(path: string): boolean {
    return this.docs.has(path);
  }

  /** Opens `path` (loading it if needed) and makes it the active document. */
  async open(path: string): Promise<void> {
    if (!this.docs.has(path)) {
      const doc: Doc = {
        path,
        kind: fileKind(path),
        loading: true,
        dirty: false,
        saving: false,
        error: null,
        conflict: null,
        revision: 0,
        state: null,
        savedDoc: null,
        baseVersion: null,
        timer: null,
        inflight: null,
        saveAgain: false,
      };
      this.docs.set(path, doc);
      this.order.push(path);
      this.active = path;
      this.emit();
      if (doc.kind === "text") {
        try {
          const file = await this.options.transport.read(path);
          if (!file) throw new Error(`${path} does not exist`);
          doc.state = this.options.createState(file.text);
          doc.savedDoc = doc.state.doc;
          doc.baseVersion = file.version;
        } catch (error) {
          doc.error = errorMessage(error);
        }
      }
      doc.loading = false;
      this.emit();
      return;
    }
    this.activate(path);
  }

  activate(path: string | null) {
    if (path !== null && !this.docs.has(path)) return;
    this.active = path;
    this.emit();
  }

  /** Closes a document, saving it first. */
  async close(path: string): Promise<void> {
    const doc = this.docs.get(path);
    if (!doc) return;
    await this.save(path);
    if (doc.timer) clearTimeout(doc.timer);
    this.docs.delete(path);
    const index = this.order.indexOf(path);
    this.order.splice(index, 1);
    if (this.active === path) {
      this.active = this.order[Math.min(index, this.order.length - 1)] ?? null;
    }
    this.emit();
  }

  // --- editing -------------------------------------------------------------------------

  /** The editor view showing the active document, used to replace its content in place. */
  attachView(view: EditorView | null) {
    this.view = view;
  }

  /** Called by the editor after every transaction on `path`. */
  update(path: string, state: EditorState) {
    const doc = this.docs.get(path);
    if (!doc || !doc.savedDoc) return;
    const changed = doc.state ? !doc.state.doc.eq(state.doc) : true;
    doc.state = state;
    if (!changed) return;
    const dirty = !state.doc.eq(doc.savedDoc);
    if (dirty !== doc.dirty) {
      doc.dirty = dirty;
      this.emit();
    }
    if (dirty) this.schedule(doc, this.autosaveMs);
  }

  private schedule(doc: Doc, delay: number) {
    if (doc.timer) clearTimeout(doc.timer);
    doc.timer = setTimeout(() => {
      doc.timer = null;
      void this.save(doc.path);
    }, delay);
  }

  /** Saves `path` now if it has unsaved changes; resolves when the content is stored. */
  save(path: string): Promise<void> {
    const doc = this.docs.get(path);
    if (!doc || !doc.state || !doc.dirty || doc.conflict) {
      return doc?.inflight ?? Promise.resolve();
    }
    if (doc.timer) {
      clearTimeout(doc.timer);
      doc.timer = null;
    }
    if (doc.inflight) {
      doc.saveAgain = true;
      return doc.inflight;
    }
    doc.inflight = this.write(doc).finally(() => {
      doc.inflight = null;
      if (doc.saveAgain && this.docs.get(doc.path) === doc) {
        doc.saveAgain = false;
        void this.save(doc.path);
      }
    });
    return doc.inflight;
  }

  private async write(doc: Doc, baseVersion = doc.baseVersion): Promise<void> {
    if (!doc.state) return;
    const sent = doc.state.doc;
    doc.saving = true;
    this.emit();
    try {
      const result = await this.options.transport.write(
        doc.path,
        sent.toString(),
        baseVersion,
      );
      if (result.ok) {
        doc.baseVersion = result.version;
        doc.savedDoc = sent;
        doc.dirty = doc.state ? !doc.state.doc.eq(sent) : false;
        doc.error = null;
        doc.conflict = null;
        this.options.onSaved?.(doc.path);
        if (doc.dirty) this.schedule(doc, this.autosaveMs);
      } else {
        doc.conflict = { kind: "rejected", version: result.current };
      }
    } catch (error) {
      doc.error = errorMessage(error);
      this.schedule(doc, this.retryMs);
    } finally {
      doc.saving = false;
      this.emit();
    }
  }

  /** Saves every document with unsaved changes (e.g. before compiling). */
  async flush(): Promise<void> {
    await Promise.all(
      [...this.docs.values()].map((doc) =>
        doc.dirty ? this.save(doc.path) : (doc.inflight ?? Promise.resolve()),
      ),
    );
  }

  hasUnsavedChanges(): boolean {
    return [...this.docs.values()].some((doc) => doc.dirty);
  }

  // --- changes from elsewhere ----------------------------------------------------------

  /** Reacts to files changed on disk. `""` stands for the whole project. */
  async refresh(paths: string[]): Promise<void> {
    const affected = [...this.docs.values()].filter((doc) =>
      paths.some((path) => path === "" || isWithin(doc.path, path)),
    );
    await Promise.all(affected.map((doc) => this.reload(doc)));
  }

  private async reload(doc: Doc) {
    if (doc.kind !== "text") {
      doc.revision += 1;
      this.emit();
      return;
    }
    if (doc.inflight || doc.loading) return; // our own save, or still loading
    let file: { text: string; version: string } | null;
    try {
      file = await this.options.transport.read(doc.path);
    } catch {
      return; // transient; the next change will try again
    }
    if (this.docs.get(doc.path) !== doc) return;
    if (!file) {
      if (doc.dirty) {
        doc.conflict = { kind: "deleted", version: null };
        this.emit();
      } else {
        await this.close(doc.path);
      }
      return;
    }
    if (file.version === doc.baseVersion) return;
    if (doc.dirty || doc.inflight) {
      doc.conflict = { kind: "external", version: file.version };
      this.emit();
      return;
    }
    this.replace(doc, file.text, file.version);
  }

  /** Replaces the content with the server's, keeping undo history and the editor view. */
  private replace(doc: Doc, text: string, version: string) {
    if (!doc.state) {
      doc.state = this.options.createState(text);
      doc.savedDoc = doc.state.doc;
    } else {
      const changes = { from: 0, to: doc.state.doc.length, insert: text };
      // Mark the new text as saved first, so `update` does not schedule a save.
      doc.savedDoc = doc.state.update({ changes }).state.doc;
      if (this.active === doc.path && this.view) {
        this.view.dispatch({ changes });
        doc.state = this.view.state;
      } else {
        doc.state = doc.state.update({ changes }).state;
      }
      doc.savedDoc = doc.state.doc;
    }
    doc.baseVersion = version;
    doc.dirty = false;
    doc.conflict = null;
    doc.error = null;
    doc.revision += 1;
    this.emit();
  }

  /** Settles a conflict: keep the local text (overwrite) or take the server's. */
  async resolve(path: string, keep: "mine" | "theirs"): Promise<void> {
    const doc = this.docs.get(path);
    if (!doc?.conflict) return;
    const conflict = doc.conflict;
    if (keep === "theirs") {
      const file = await this.options.transport.read(path);
      if (!file) {
        doc.conflict = null;
        doc.dirty = false;
        await this.close(path);
        return;
      }
      this.replace(doc, file.text, file.version);
      return;
    }
    doc.conflict = null;
    if (doc.inflight) await doc.inflight;
    await this.write(doc, conflict.version);
  }

  /** Follows a rename made in the file tree. */
  renamed(from: string, to: string) {
    let changed = false;
    for (const path of [...this.order]) {
      if (!isWithin(path, from)) continue;
      const doc = this.docs.get(path);
      if (!doc) continue;
      const next = to + path.slice(from.length);
      this.docs.delete(path);
      doc.path = next;
      this.docs.set(next, doc);
      this.order[this.order.indexOf(path)] = next;
      if (this.active === path) this.active = next;
      changed = true;
    }
    if (changed) this.emit();
  }

  /** Forgets documents removed from the project (without saving them). */
  removed(path: string) {
    for (const open of [...this.order]) {
      if (!isWithin(open, path)) continue;
      const doc = this.docs.get(open);
      if (doc?.timer) clearTimeout(doc.timer);
      this.docs.delete(open);
      const index = this.order.indexOf(open);
      this.order.splice(index, 1);
      if (this.active === open) {
        this.active =
          this.order[Math.min(index, this.order.length - 1)] ?? null;
      }
    }
    this.emit();
  }

  dispose() {
    for (const doc of this.docs.values()) {
      if (doc.timer) clearTimeout(doc.timer);
    }
    this.listeners.clear();
  }
}
