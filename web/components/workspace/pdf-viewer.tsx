"use client";

import {
  Download,
  LoaderCircle,
  Maximize2,
  ZoomIn,
  ZoomOut,
} from "lucide-react";
import * as pdfjs from "pdfjs-dist";
import {
  type MouseEvent,
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { IconButton } from "@/components/ui/button";
import type { PdfRect } from "@/lib/api/types/PdfRect";

pdfjs.GlobalWorkerOptions.workerSrc = new URL(
  "pdfjs-dist/build/pdf.worker.min.mjs",
  import.meta.url,
).toString();

export type Highlight = { rects: PdfRect[]; nonce: number };

type Size = { width: number; height: number };

const PADDING = 16;
const GAP = 12;
const MIN_SCALE = 0.25;
const MAX_SCALE = 5;

/** Where the reader is: a page and how far down it, so a reload or zoom can come back. */
type Anchor = { page: number; ratio: number };

function pageTops(sizes: Size[], scale: number): number[] {
  const tops: number[] = [];
  let y = PADDING;
  for (const size of sizes) {
    tops.push(y);
    y += size.height * scale + GAP;
  }
  return tops;
}

export function PdfViewer({
  url,
  downloadName,
  highlight,
  onInverseSearch,
}: {
  /** PDF to show; changing it keeps the old pages on screen until the new ones are ready. */
  url: string | null;
  downloadName: string;
  highlight: Highlight | null;
  /** Double-click or Ctrl/⌘-click on a page: 1-based page and point in PDF points. */
  onInverseSearch?: (page: number, x: number, y: number) => void;
}) {
  const container = useRef<HTMLDivElement>(null);
  const [doc, setDoc] = useState<pdfjs.PDFDocumentProxy | null>(null);
  const [sizes, setSizes] = useState<Size[]>([]);
  const [scale, setScale] = useState(1);
  const [fitWidth, setFitWidth] = useState(true);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [currentPage, setCurrentPage] = useState(1);
  const anchor = useRef<Anchor | null>(null);
  const layout = useRef({ sizes, scale });
  layout.current = { sizes, scale };

  const captureAnchor = useCallback(() => {
    const element = container.current;
    const { sizes: current, scale: currentScale } = layout.current;
    if (!element || current.length === 0) return;
    const tops = pageTops(current, currentScale);
    const y = element.scrollTop;
    let page = 0;
    while (page + 1 < tops.length && tops[page + 1] <= y) page += 1;
    const height = current[page].height * currentScale;
    anchor.current = { page, ratio: (y - tops[page]) / Math.max(height, 1) };
  }, []);

  // Load the document; keep the previous one visible meanwhile.
  useEffect(() => {
    if (!url) {
      setDoc(null);
      setSizes([]);
      return;
    }
    let cancelled = false;
    let adopted = false;
    const task = pdfjs.getDocument({ url });
    setLoading(true);
    task.promise
      .then(async (loaded) => {
        const pages = await Promise.all(
          Array.from({ length: loaded.numPages }, (_, i) =>
            loaded.getPage(i + 1),
          ),
        );
        if (cancelled) return;
        captureAnchor();
        setSizes(
          pages.map((page) => {
            const viewport = page.getViewport({ scale: 1 });
            return { width: viewport.width, height: viewport.height };
          }),
        );
        adopted = true;
        setDoc(loaded);
        setError(null);
      })
      .catch((reason: unknown) => {
        if (!cancelled) setError(String(reason));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
      // A document on screen is released by the effect below once it is replaced.
      if (!adopted) void task.destroy();
    };
  }, [url, captureAnchor]);

  useEffect(() => () => void doc?.loadingTask.destroy(), [doc]);

  // Fit to width when asked, and whenever the panel is resized.
  useEffect(() => {
    const element = container.current;
    if (!element || !fitWidth || sizes.length === 0) return;
    const widest = Math.max(...sizes.map((size) => size.width));
    const fit = () => {
      const next = (element.clientWidth - 2 * PADDING) / widest;
      if (next > 0) {
        captureAnchor();
        setScale(Math.min(Math.max(next, MIN_SCALE), MAX_SCALE));
      }
    };
    fit();
    const observer = new ResizeObserver(fit);
    observer.observe(element);
    return () => observer.disconnect();
  }, [fitWidth, sizes, captureAnchor]);

  // Restore the reading position after a reload or zoom.
  useLayoutEffect(() => {
    const element = container.current;
    const saved = anchor.current;
    if (!element || !saved || sizes.length === 0) return;
    const page = Math.min(saved.page, sizes.length - 1);
    const tops = pageTops(sizes, scale);
    element.scrollTop = tops[page] + saved.ratio * sizes[page].height * scale;
    anchor.current = null;
  }, [sizes, scale]);

  // Scroll to and flash SyncTeX rectangles.
  useEffect(() => {
    const element = container.current;
    const first = highlight?.rects[0];
    if (!element || !first || first.page > sizes.length) return;
    const tops = pageTops(sizes, scale);
    element.scrollTo({
      top: Math.max(
        tops[first.page - 1] + first.y * scale - element.clientHeight / 3,
        0,
      ),
      behavior: "smooth",
    });
  }, [highlight, sizes, scale]);

  const zoom = (factor: number) => {
    captureAnchor();
    setFitWidth(false);
    setScale((current) =>
      Math.min(Math.max(current * factor, MIN_SCALE), MAX_SCALE),
    );
  };

  const onScroll = () => {
    const element = container.current;
    if (!element || sizes.length === 0) return;
    const tops = pageTops(sizes, scale);
    const middle = element.scrollTop + element.clientHeight / 2;
    let page = 0;
    while (page + 1 < tops.length && tops[page + 1] <= middle) page += 1;
    setCurrentPage(page + 1);
  };

  const onPageClick = (
    pageIndex: number,
    event: MouseEvent<HTMLDivElement>,
  ) => {
    const isInverse = event.detail === 2 || event.ctrlKey || event.metaKey;
    if (!isInverse || !onInverseSearch) return;
    const box = event.currentTarget.getBoundingClientRect();
    onInverseSearch(
      pageIndex + 1,
      (event.clientX - box.left) / scale,
      (event.clientY - box.top) / scale,
    );
  };

  return (
    <div className="flex h-full min-h-0 flex-col bg-zinc-100 dark:bg-zinc-900">
      <div className="flex h-9 shrink-0 items-center gap-1 border-b border-border bg-background px-2 text-xs text-muted">
        <span className="tabular-nums">
          {sizes.length > 0 ? `${currentPage} / ${sizes.length}` : "—"}
        </span>
        <span className="flex-1" />
        {loading && <LoaderCircle className="size-3.5 animate-spin" />}
        <IconButton
          label="Zoom out"
          onClick={() => zoom(1 / 1.2)}
          disabled={!doc}
        >
          <ZoomOut className="size-4" />
        </IconButton>
        <span className="w-10 text-center tabular-nums">
          {Math.round(scale * 100)}%
        </span>
        <IconButton label="Zoom in" onClick={() => zoom(1.2)} disabled={!doc}>
          <ZoomIn className="size-4" />
        </IconButton>
        <IconButton
          label="Fit width"
          onClick={() => setFitWidth(true)}
          disabled={!doc}
          className={fitWidth ? "text-accent" : ""}
        >
          <Maximize2 className="size-4" />
        </IconButton>
        {url && (
          <a
            href={url}
            download={downloadName}
            aria-label="Download PDF"
            title="Download PDF"
            className="inline-flex size-7 items-center justify-center rounded-md hover:bg-zinc-200/60 hover:text-foreground dark:hover:bg-zinc-800/60"
          >
            <Download className="size-4" />
          </a>
        )}
      </div>
      <div
        ref={container}
        onScroll={onScroll}
        className="relative min-h-0 flex-1 overflow-auto"
        style={{ padding: PADDING }}
      >
        {error && !doc && (
          <p className="text-sm text-danger">Cannot open the PDF: {error}</p>
        )}
        {!url && !error && (
          <p className="mt-16 text-center text-sm text-muted">
            Compile the project to see the PDF.
          </p>
        )}
        {doc && (
          <div className="mx-auto flex w-max flex-col" style={{ gap: GAP }}>
            {sizes.map((size, index) => (
              <PdfPage
                // Keyed by position so canvases survive a reload without flashing.
                // biome-ignore lint/suspicious/noArrayIndexKey: pages are positional.
                key={index}
                doc={doc}
                index={index}
                size={size}
                scale={scale}
                root={container}
                rects={
                  highlight?.rects.filter((r) => r.page === index + 1) ?? []
                }
                highlightNonce={highlight?.nonce ?? 0}
                onClick={(event) => onPageClick(index, event)}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function PdfPage({
  doc,
  index,
  size,
  scale,
  root,
  rects,
  highlightNonce,
  onClick,
}: {
  doc: pdfjs.PDFDocumentProxy;
  index: number;
  size: Size;
  scale: number;
  root: React.RefObject<HTMLDivElement | null>;
  rects: PdfRect[];
  highlightNonce: number;
  onClick: (event: MouseEvent<HTMLDivElement>) => void;
}) {
  const box = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const [visible, setVisible] = useState(false);

  useEffect(() => {
    const element = box.current;
    if (!element) return;
    const observer = new IntersectionObserver(
      ([entry]) => setVisible(entry.isIntersecting),
      { root: root.current, rootMargin: "800px 0px" },
    );
    observer.observe(element);
    return () => observer.disconnect();
  }, [root]);

  useEffect(() => {
    const target = canvas.current;
    if (!visible || !target) return;
    let cancelled = false;
    let task: pdfjs.RenderTask | null = null;
    const pixelRatio = Math.min(window.devicePixelRatio || 1, 2);
    (async () => {
      const page = await doc.getPage(index + 1);
      if (cancelled) return;
      const viewport = page.getViewport({ scale: scale * pixelRatio });
      // Draw off-screen, then swap: the old image stays until the new one is ready.
      const offscreen = document.createElement("canvas");
      offscreen.width = Math.floor(viewport.width);
      offscreen.height = Math.floor(viewport.height);
      task = page.render({ canvas: offscreen, viewport });
      await task.promise;
      if (cancelled) return;
      target.width = offscreen.width;
      target.height = offscreen.height;
      target.getContext("2d")?.drawImage(offscreen, 0, 0);
    })().catch(() => {
      // Cancelled renders reject; nothing to do.
    });
    return () => {
      cancelled = true;
      task?.cancel();
    };
  }, [doc, index, scale, visible]);

  return (
    // biome-ignore lint/a11y/noStaticElementInteractions: mouse-only shortcut; the editor offers the keyboard path.
    // biome-ignore lint/a11y/useKeyWithClickEvents: see above.
    <div
      ref={box}
      onClick={onClick}
      className="relative bg-white shadow-sm ring-1 ring-black/5"
      style={{ width: size.width * scale, height: size.height * scale }}
    >
      <canvas
        ref={canvas}
        className="absolute inset-0 size-full"
        aria-label={`Page ${index + 1}`}
      />
      {rects.map((rect, i) => (
        <div
          // Restart the animation for every new search.
          // biome-ignore lint/suspicious/noArrayIndexKey: rects have no identity.
          key={`${highlightNonce}-${i}`}
          className="synctex-highlight pointer-events-none absolute rounded-sm bg-yellow-300/50 ring-1 ring-yellow-500/60"
          style={{
            left: rect.x * scale - 2,
            top: rect.y * scale - 2,
            width: rect.width * scale + 4,
            height: rect.height * scale + 4,
          }}
        />
      ))}
    </div>
  );
}
