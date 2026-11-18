/** Pure helpers for project-relative paths (`/`-separated, no leading slash). */
import type { FileEntry } from "@/lib/api/types/FileEntry";

export function basename(path: string): string {
  const slash = path.lastIndexOf("/");
  return slash === -1 ? path : path.slice(slash + 1);
}

export function dirname(path: string): string {
  const slash = path.lastIndexOf("/");
  return slash === -1 ? "" : path.slice(0, slash);
}

export function joinPath(dir: string, name: string): string {
  return dir ? `${dir}/${name}` : name;
}

export function extension(path: string): string {
  const name = basename(path);
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(dot + 1).toLowerCase() : "";
}

const TEXT_EXTENSIONS = new Set([
  "",
  "tex",
  "ltx",
  "latex",
  "sty",
  "cls",
  "bib",
  "bst",
  "bbx",
  "cbx",
  "dtx",
  "ins",
  "txt",
  "md",
  "tikz",
  "pgf",
  "def",
  "cfg",
  "clo",
  "fd",
  "lco",
  "csv",
  "dat",
  "latexmkrc",
]);
const IMAGE_EXTENSIONS = new Set(["png", "jpg", "jpeg", "gif", "svg", "webp"]);

export type FileKind = "text" | "image" | "pdf" | "binary";

export function fileKind(path: string): FileKind {
  const ext = extension(path);
  if (TEXT_EXTENSIONS.has(ext)) return "text";
  if (IMAGE_EXTENSIONS.has(ext)) return "image";
  if (ext === "pdf") return "pdf";
  return "binary";
}

/** `path` itself or anything inside it, when `path` is a directory. */
export function isWithin(candidate: string, path: string): boolean {
  return candidate === path || candidate.startsWith(`${path}/`);
}

export type TreeNode = {
  name: string;
  path: string;
  kind: "file" | "dir";
  size: number;
  children: TreeNode[];
};

/** Nests a flat, path-sorted list into a tree: directories first, then files, by name. */
export function buildTree(entries: FileEntry[]): TreeNode[] {
  const root: TreeNode[] = [];
  const dirs = new Map<string, TreeNode>();
  const sorted = [...entries].sort((a, b) => a.path.localeCompare(b.path));
  for (const entry of sorted) {
    const node: TreeNode = {
      name: basename(entry.path),
      path: entry.path,
      kind: entry.kind,
      size: entry.size,
      children: [],
    };
    if (entry.kind === "dir") dirs.set(entry.path, node);
    const parent = dirname(entry.path);
    const siblings = parent ? (dirs.get(parent)?.children ?? root) : root;
    siblings.push(node);
  }
  const order = (nodes: TreeNode[]) => {
    nodes.sort((a, b) =>
      a.kind === b.kind
        ? a.name.localeCompare(b.name, undefined, { numeric: true })
        : a.kind === "dir"
          ? -1
          : 1,
    );
    for (const node of nodes) order(node.children);
  };
  order(root);
  return root;
}

/** Checks a file or folder name typed by the user; returns an error message or null. */
export function validateName(name: string): string | null {
  if (!name.trim()) return "The name is empty.";
  if (name.includes("/") || name.includes("\\"))
    return "Use a name without slashes.";
  if (name === "." || name === "..") return "This name is not allowed.";
  return null;
}
