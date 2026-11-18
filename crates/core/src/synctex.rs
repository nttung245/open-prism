//! SyncTeX: links between the TeX sources and the typeset PDF.
//!
//! TeX engines run with `-synctex=1` write `<job>.synctex.gz`, a record of every box, glue,
//! kern and math node they ship out, each tagged with the input file and line that produced
//! it. [`SyncTex`] reads that file and answers the two questions an editor asks:
//!
//! - forward search ([`SyncTex::forward`]): which rectangles of the PDF show this line?
//! - inverse search ([`SyncTex::inverse`]): which source line produced the text under this
//!   point of the PDF?
//!
//! The search strategies follow the reference implementation (`synctex_parser.c`, behind the
//! `synctex` command-line tool shipped with TeX Live) closely, so the answers match what TeX
//! users get from other editors and viewers. Coordinates are PDF big points (1/72 in) with
//! the origin at the top-left corner of the page, as in the `synctex` CLI.

use std::collections::HashMap;
use std::fmt;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use flate2::read::MultiGzDecoder;
use serde::Serialize;
use ts_rs::TS;

/// A rectangle of a PDF page, in PDF big points.
///
/// The origin is the TOP-LEFT corner of the page and y grows downwards; (`x`, `y`) is the
/// top-left corner of the rectangle.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PdfRect {
    /// 1-based page number.
    pub page: u32,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// A place in a source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePosition {
    /// Path of the input file as recorded by TeX, lexically normalised.
    pub file: PathBuf,
    /// 1-based line number.
    pub line: u32,
    /// Column, when the engine recorded one (TeX engines currently never do).
    pub column: Option<u32>,
}

/// Why a SyncTeX file could not be read.
#[derive(Debug, thiserror::Error)]
pub enum SyncTexError {
    /// The gzip stream is corrupt or truncated.
    #[error("cannot decompress the SyncTeX file: {0}")]
    Decompress(#[from] std::io::Error),
    /// The data does not start with a SyncTeX preamble.
    #[error("not a SyncTeX file: {0}")]
    NotSyncTex(&'static str),
    /// A record of the file cannot be understood.
    #[error("malformed SyncTeX file, line {line}: {reason}")]
    Malformed { line: usize, reason: &'static str },
}

/// A parsed and indexed SyncTeX file.
pub struct SyncTex {
    /// Unique normalised input paths, in order of first appearance.
    paths: Vec<PathBuf>,
    /// Input tag → index into `paths`.
    tags: HashMap<i32, usize>,
    /// Highest line recorded for each input tag.
    max_lines: HashMap<i32, i32>,
    nodes: Vec<Node>,
    /// Visible extent and mean line of each hbox (`Node::aux`).
    hboxes: Vec<HBoxInfo>,
    sheets: Vec<Sheet>,
    /// Nodes that forward search can land on, as `(tag, line, node)`, sorted. The tag and
    /// line are the ones the node had when it was registered (see [`SyncTex::forward`]).
    friends: Vec<(i32, i32, u32)>,
    /// Big points per TeX unit of the file.
    unit: f64,
    x_offset: f64,
    y_offset: f64,
    /// The same values with the reference implementation's `float` precision, to place the
    /// hit point of an inverse search on exactly the same scaled point.
    unit_f32: f32,
    x_offset_f32: f32,
    y_offset_f32: f32,
    /// Boxes wider or taller than this are not climbed past when picking the box of a node.
    big_box: i64,
}

/// How far forward search looks for a neighbouring line with output when the requested line
/// produced none (blank lines, comments, `\begin{...}`): it tries line+1, line-1, line+2, …
/// in the order of the `synctex` CLI, which looks up to 50 lines away.
const NEARBY_LINES: i64 = 10;

/// Deepest box nesting accepted; it bounds the recursion of the searches.
const MAX_DEPTH: usize = 512;

/// Largest decompressed file accepted (a book of thousands of pages is a few hundred MB).
const MAX_DECOMPRESSED: u64 = 1 << 30;

const NONE: u32 = u32::MAX;
/// The reference implementation's "infinite" distance.
const FAR: i64 = i32::MAX as i64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Sheet,
    VBox,
    HBox,
    VoidVBox,
    VoidHBox,
    Kern,
    Glue,
    Rule,
    Math,
    /// An `x` record: the current position at some point of the typesetting.
    Boundary,
    /// Added at both ends of every hbox, like the reference implementation does.
    BoxBoundary,
}

impl Kind {
    fn is_box(self) -> bool {
        matches!(
            self,
            Kind::VBox | Kind::HBox | Kind::VoidVBox | Kind::VoidHBox
        )
    }
}

/// A node of the box tree, in TeX scaled points (or whatever `Unit:` says).
#[derive(Clone, Debug)]
struct Node {
    kind: Kind,
    tag: i32,
    line: i32,
    column: i32,
    h: i32,
    v: i32,
    width: i32,
    height: i32,
    depth: i32,
    parent: u32,
    first_child: u32,
    next_sibling: u32,
    /// Page number of a sheet; index into `SyncTex::hboxes` of an hbox.
    aux: u32,
}

/// TeX boxes may hold material outside their nominal size (zero-width boxes holding text,
/// for instance); hboxes are therefore also given a "visible" extent that contains it.
#[derive(Clone, Copy, Debug)]
struct HBoxInfo {
    h: i32,
    v: i32,
    width: i32,
    height: i32,
    depth: i32,
    /// Weighted mean of the lines of the content.
    mean_line: i32,
    weight: i32,
}

#[derive(Clone, Debug)]
struct Sheet {
    page: u32,
    node: u32,
    /// The hboxes of the page, in the order they were closed.
    hboxes: Vec<u32>,
}

#[derive(Clone, Copy, Debug)]
struct Point {
    h: i64,
    v: i64,
}

#[derive(Clone, Copy, Debug)]
struct Extent {
    min_h: i64,
    max_h: i64,
    min_v: i64,
    max_v: i64,
}

/// A node with its distance to the hit point.
#[derive(Clone, Copy, Debug)]
struct Nd {
    node: Option<u32>,
    distance: i64,
}

impl Nd {
    const NONE: Nd = Nd {
        node: None,
        distance: FAR,
    };

    fn new(node: u32, distance: i64) -> Nd {
        Nd {
            node: Some(node),
            distance,
        }
    }
}

impl fmt::Debug for SyncTex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SyncTex")
            .field("inputs", &self.paths.len())
            .field("pages", &self.sheets.len())
            .field("nodes", &self.nodes.len())
            .field("unit", &self.unit)
            .finish_non_exhaustive()
    }
}

impl SyncTex {
    /// Parses a gzip-compressed `.synctex.gz`.
    pub fn from_gz(bytes: &[u8]) -> Result<Self, SyncTexError> {
        let mut text = Vec::with_capacity(bytes.len().saturating_mul(6));
        MultiGzDecoder::new(bytes)
            .take(MAX_DECOMPRESSED + 1)
            .read_to_end(&mut text)?;
        if text.len() as u64 > MAX_DECOMPRESSED {
            return Err(SyncTexError::Decompress(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "more than 1 GiB of SyncTeX data",
            )));
        }
        Self::parse_bytes(&text)
    }

    /// Parses an uncompressed `.synctex`.
    pub fn parse(text: &str) -> Result<Self, SyncTexError> {
        Self::parse_bytes(text.as_bytes())
    }

    fn parse_bytes(data: &[u8]) -> Result<Self, SyncTexError> {
        let mut parser = Parser::new(data.len());
        for (index, line) in data.split(|&b| b == b'\n').enumerate() {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            parser.line(index + 1, line)?;
        }
        parser.finish()
    }

    /// Input files as recorded (lexically normalised, `./` removed), without duplicates.
    pub fn inputs(&self) -> impl Iterator<Item = &Path> {
        self.paths.iter().map(PathBuf::as_path)
    }

    /// Rectangles that show `line` of `file` (absolute path; compared after lexical
    /// normalisation), best first: by page, then by how much of the line each one holds.
    /// There is one rectangle per output line (hbox), not per glyph.
    ///
    /// If the exact line produced no boxes, the nearest lines that did are used instead
    /// (line+1, line-1, line+2, … up to 10 lines away, in the order of the `synctex` CLI).
    /// `column` is accepted for symmetry with the CLI, which ignores it too: engines do not
    /// record columns. Empty if nothing matches.
    pub fn forward(&self, file: &Path, line: u32, column: Option<u32>) -> Vec<PdfRect> {
        let _ = column;
        let tags = self.tags_of(file);
        let Some(max_line) = tags
            .iter()
            .filter_map(|tag| self.max_lines.get(tag))
            .max()
            .map(|&max| i64::from(max))
        else {
            return Vec::new();
        };
        let start = i64::from(line).min(max_line);
        let nearby = (1..=NEARBY_LINES).flat_map(|distance| [start + distance, start - distance]);
        for candidate in std::iter::once(start).chain(nearby) {
            if candidate < 1 || candidate > max_line {
                continue;
            }
            let Ok(candidate) = i32::try_from(candidate) else {
                continue;
            };
            let rects = self.rects_for_line(&tags, candidate);
            if !rects.is_empty() {
                return rects;
            }
        }
        Vec::new()
    }

    /// Source position under a point of `page` (1-based). `x` and `y` are in PDF big points
    /// from the top-left corner of the page.
    ///
    /// The point is looked up in the smallest hbox (line of text) containing it, then the
    /// closest node of that box by horizontal distance gives the line. When no box contains
    /// the point, the node closest to it on the page is used.
    pub fn inverse(&self, page: u32, x: f64, y: f64) -> Option<SourcePosition> {
        let sheet = self.sheets.iter().find(|sheet| sheet.page == page)?;
        // Same arithmetic as the reference implementation (single precision, truncation).
        let hit = Point {
            h: i64::from(((x as f32 - self.x_offset_f32) / self.unit_f32) as i32),
            v: i64::from(((y as f32 - self.y_offset_f32) / self.unit_f32) as i32),
        };
        let node = self.edit_query(sheet, hit)?;
        let node = &self.nodes[node as usize];
        let file = self.paths.get(*self.tags.get(&node.tag)?)?.clone();
        Some(SourcePosition {
            file,
            line: u32::try_from(node.line).ok()?,
            column: u32::try_from(node.column).ok(),
        })
    }

    // ------------------------------------------------------------------ forward search

    fn tags_of(&self, file: &Path) -> Vec<i32> {
        let file = normalize(file);
        let mut index = self.paths.iter().position(|path| *path == file);
        if index.is_none() && file.is_relative() && !file.as_os_str().is_empty() {
            index = self.paths.iter().position(|path| path.ends_with(&file));
        }
        let Some(index) = index else {
            return Vec::new();
        };
        let mut tags: Vec<i32> = self
            .tags
            .iter()
            .filter(|&(_, &path)| path == index)
            .map(|(&tag, _)| tag)
            .collect();
        tags.sort_unstable();
        tags
    }

    fn friends_of(&self, tag: i32, line: i32) -> &[(i32, i32, u32)] {
        let start = self
            .friends
            .partition_point(|&(t, l, _)| (t, l) < (tag, line));
        let end = self
            .friends
            .partition_point(|&(t, l, _)| (t, l) <= (tag, line));
        &self.friends[start..end]
    }

    fn rects_for_line(&self, tags: &[i32], line: i32) -> Vec<PdfRect> {
        // Nodes registered for this line that still belong to it; boxes only as a last resort.
        let mut matches: Vec<u32> = Vec::new();
        for exclude_boxes in [true, false] {
            for &tag in tags {
                matches.extend(
                    self.friends_of(tag, line)
                        .iter()
                        .map(|&(_, _, node)| node)
                        .filter(|&node| {
                            let n = &self.nodes[node as usize];
                            n.tag == tag && n.line == line && !(exclude_boxes && n.kind.is_box())
                        }),
                );
            }
            if !matches.is_empty() {
                break;
            }
        }
        matches.sort_unstable();

        // One candidate per enclosing box, weighted by how many of its children come from
        // the line, like the CLI orders its results.
        struct Candidate {
            page: u32,
            weight: usize,
            order: usize,
            target: u32,
        }
        let mut seen_parents: Vec<u32> = Vec::new();
        let mut candidates = Vec::new();
        for (order, &node) in matches.iter().enumerate() {
            let n = &self.nodes[node as usize];
            let parent = n.parent;
            if parent == NONE {
                continue;
            }
            let hbox_parent = self.nodes[parent as usize].kind == Kind::HBox;
            if hbox_parent {
                if seen_parents.contains(&parent) {
                    continue;
                }
                seen_parents.push(parent);
            }
            let weight = self
                .children(parent)
                .filter(|&child| {
                    let c = &self.nodes[child as usize];
                    c.tag == n.tag && c.line == n.line
                })
                .count();
            let (Some(page), Some(target)) = (self.page_of(node), self.box_visible(node)) else {
                continue;
            };
            candidates.push(Candidate {
                page,
                weight,
                order,
                target,
            });
        }
        candidates.sort_by(|a, b| {
            a.page
                .cmp(&b.page)
                .then(b.weight.cmp(&a.weight))
                .then(a.order.cmp(&b.order))
        });

        // Several nodes, or nested boxes of the same size, often show the same rectangle.
        let mut rects: Vec<PdfRect> = Vec::new();
        for candidate in candidates {
            let rect = self.rect(candidate.target, candidate.page);
            if !rects.contains(&rect) {
                rects.push(rect);
            }
        }
        rects
    }

    /// The box shown for a node: its enclosing box, widened to the highest enclosing hbox
    /// about the same source line, until a box as wide or tall as a line of text is reached.
    fn box_visible(&self, node: u32) -> Option<u32> {
        let mut node = node;
        if !self.kind(node).is_box() {
            node = self.nodes[node as usize].parent;
            if node == NONE || self.kind(node) == Kind::Sheet {
                return None;
            }
        }
        let mean = self.mean_line(node);
        let mut parent = node;
        loop {
            parent = self.nodes[parent as usize].parent;
            if parent == NONE {
                return Some(node);
            }
            let p = &self.nodes[parent as usize];
            if p.kind == Kind::HBox {
                if (mean - self.mean_line(parent)).abs() > 1 {
                    return Some(node);
                }
                if i64::from(p.width) > self.big_box
                    || i64::from(p.height) + i64::from(p.depth) > self.big_box
                {
                    return Some(parent);
                }
                node = parent;
            }
        }
    }

    fn mean_line(&self, node: u32) -> i64 {
        let n = &self.nodes[node as usize];
        if n.kind == Kind::HBox {
            return i64::from(self.hboxes[n.aux as usize].mean_line);
        }
        if n.parent != NONE {
            let p = &self.nodes[n.parent as usize];
            if p.kind == Kind::HBox {
                return i64::from(self.hboxes[p.aux as usize].mean_line);
            }
        }
        i64::from(n.line)
    }

    fn rect(&self, node: u32, page: u32) -> PdfRect {
        let n = &self.nodes[node as usize];
        let (h, v, width, height, depth) = if n.kind == Kind::HBox {
            let b = &self.hboxes[n.aux as usize];
            (b.h, b.v, b.width, b.height, b.depth)
        } else {
            (n.h, n.v, n.width, n.height, n.depth)
        };
        let mut x = f64::from(h) * self.unit + self.x_offset;
        let mut width = f64::from(width) * self.unit;
        if width < 0.0 {
            x += width;
            width = -width;
        }
        let bottom = f64::from(v) * self.unit + self.y_offset + f64::from(depth) * self.unit;
        // Like the CLI's viewer command: at least one point high, so it can be seen.
        let height = ((f64::from(height) + f64::from(depth)) * self.unit).max(1.0);
        PdfRect {
            page,
            x,
            y: bottom - height,
            width,
            height,
        }
    }

    // ------------------------------------------------------------------ inverse search

    /// The node whose source position answers an inverse search: the first result of the
    /// reference implementation's `synctex_edit_query`.
    fn edit_query(&self, sheet: &Sheet, hit: Point) -> Option<u32> {
        // The hboxes of the page, most recently closed first: the smallest one containing
        // the hit point, then the deepest box inside it.
        let mut hboxes = sheet.hboxes.iter().rev();
        let mut found = None;
        for &hbox in hboxes.by_ref() {
            if self.point_in_box(hit, hbox) {
                found = Some(hbox);
                break;
            }
        }
        let (container, mut left, mut right) = if let Some(mut node) = found {
            for &other in hboxes {
                if self.point_in_box(hit, other) {
                    node = self.smallest_container(other, node);
                }
            }
            let container = self.deepest_container_v2(hit, node)?;
            let (left, right) = self.closest_children_in_box(hit, container);
            (container, left, right)
        } else {
            // Not lucky: the closest node of the whole page.
            let first = self.nodes[sheet.node as usize].first_child;
            if first == NONE {
                return None;
            }
            (first, self.closest_deep_child(hit, first), Nd::NONE)
        };

        match (left.node, right.node) {
            (Some(l), Some(r)) => {
                let (ln, rn) = (&self.nodes[l as usize], &self.nodes[r as usize]);
                if (ln.tag, ln.line, ln.column) != (rn.tag, rn.line, rn.column) {
                    // Two candidates: the smaller line wins, then the closer one.
                    if rn.line < ln.line || (rn.line == ln.line && left.distance > right.distance) {
                        std::mem::swap(&mut left, &mut right);
                    }
                    return left.node;
                }
                if left.distance > right.distance {
                    return right.node;
                }
                left.node
            }
            (None, Some(r)) => Some(r),
            (Some(l), None) => Some(l),
            (None, None) => Some(container),
        }
    }

    fn point_in_box(&self, hit: Point, node: u32) -> bool {
        self.h_ordered_distance(hit, node).distance == 0 && self.v_ordered_distance(hit, node) == 0
    }

    /// Horizontal distance from the hit point to the node: positive when the node is on the
    /// right of the point, negative when it is on the left, 0 when it spans it.
    fn h_ordered_distance(&self, hit: Point, node: u32) -> Nd {
        let n = &self.nodes[node as usize];
        let span = |min: i64, width: i64| {
            let max = min + width.abs();
            if hit.h < min {
                min - hit.h
            } else if hit.h > max {
                max - hit.h
            } else {
                0
            }
        };
        let distance = match n.kind {
            Kind::VBox | Kind::VoidVBox | Kind::VoidHBox => span(n.h.into(), n.width.into()),
            Kind::HBox => {
                let b = &self.hboxes[n.aux as usize];
                span(b.h.into(), b.width.into())
            }
            Kind::Kern => {
                // A kern is recorded after the move; measure to its closest edge, with a
                // penalty so that real material wins over it.
                let width = i64::from(n.width);
                let (min, max) = if width < 0 {
                    (i64::from(n.h), i64::from(n.h) - width)
                } else {
                    (i64::from(n.h) - width, i64::from(n.h))
                };
                let med = (min + max) / 2;
                if hit.h < min {
                    min - hit.h + 1
                } else if hit.h > max {
                    max - hit.h - 1
                } else if hit.h > med {
                    max - hit.h + 1
                } else {
                    min - hit.h - 1
                }
            }
            Kind::Rule | Kind::Glue | Kind::Math | Kind::Boundary | Kind::BoxBoundary => {
                i64::from(n.h) - hit.h
            }
            Kind::Sheet => FAR,
        };
        Nd::new(node, distance)
    }

    /// Vertical counterpart of [`Self::h_ordered_distance`]: positive when the node is above.
    fn v_ordered_distance(&self, hit: Point, node: u32) -> i64 {
        let n = &self.nodes[node as usize];
        let (v, height, depth) = match n.kind {
            Kind::VBox | Kind::VoidVBox | Kind::VoidHBox => (n.v, n.height, n.depth),
            Kind::HBox => {
                let b = &self.hboxes[n.aux as usize];
                (b.v, b.height, b.depth)
            }
            Kind::Rule | Kind::Kern | Kind::Glue | Kind::Math => {
                let (height, depth) = self.parent_height_depth(n);
                (n.v, height, depth)
            }
            Kind::Boundary | Kind::BoxBoundary | Kind::Sheet => return FAR,
        };
        let min = i64::from(v) - i64::from(height).abs();
        let max = i64::from(v) + i64::from(depth).abs();
        if hit.v < min {
            min - hit.v
        } else if hit.v > max {
            max - hit.v
        } else {
            0
        }
    }

    fn parent_height_depth(&self, n: &Node) -> (i32, i32) {
        match self.nodes.get(n.parent as usize) {
            Some(p) if p.kind != Kind::Sheet => (p.height, p.depth),
            _ => (0, 0),
        }
    }

    /// Distance from the hit point to the node, 0 inside it.
    fn node_distance(&self, hit: Point, node: u32) -> i64 {
        let n = &self.nodes[node as usize];
        let (h, v) = (i64::from(n.h), i64::from(n.v));
        let vertical =
            |height: i32, depth: i32| (v - i64::from(height).abs(), v + i64::from(depth).abs());
        match n.kind {
            Kind::VBox => {
                let (min_v, max_v) = vertical(n.height, n.depth);
                let max_h = h + i64::from(n.width).abs();
                distance_to_box(
                    hit,
                    Extent {
                        min_h: h,
                        max_h,
                        min_v,
                        max_v,
                    },
                )
            }
            Kind::HBox => {
                let b = &self.hboxes[n.aux as usize];
                let min_h = i64::from(b.h);
                let max_h = min_h + i64::from(b.width).abs();
                let min_v = i64::from(b.v) - i64::from(b.height).abs();
                let max_v = i64::from(b.v) + i64::from(b.depth).abs();
                distance_to_box(
                    hit,
                    Extent {
                        min_h,
                        max_h,
                        min_v,
                        max_v,
                    },
                )
            }
            Kind::VoidVBox | Kind::VoidHBox => {
                // The closest of the left and right edges.
                let (min_v, max_v) = vertical(n.height, n.depth);
                let right = h + i64::from(n.width).abs();
                let left = distance_to_box(
                    hit,
                    Extent {
                        min_h: h,
                        max_h: h,
                        min_v,
                        max_v,
                    },
                );
                let right = distance_to_box(
                    hit,
                    Extent {
                        min_h: right,
                        max_h: right,
                        min_v,
                        max_v,
                    },
                );
                left.min(right)
            }
            Kind::Kern => {
                let (height, _) = self.parent_height_depth(n);
                let min_v = v - i64::from(height).abs();
                let at = |h: i64| {
                    distance_to_box(
                        hit,
                        Extent {
                            min_h: h,
                            max_h: h,
                            min_v,
                            max_v: v,
                        },
                    )
                };
                at(h).min(at(h - i64::from(n.width)))
            }
            Kind::Glue | Kind::Math | Kind::Boundary | Kind::BoxBoundary => {
                let (height, _) = self.parent_height_depth(n);
                let min_v = v - i64::from(height).abs();
                distance_to_box(
                    hit,
                    Extent {
                        min_h: h,
                        max_h: h,
                        min_v,
                        max_v: v,
                    },
                )
            }
            Kind::Rule | Kind::Sheet => FAR,
        }
    }

    /// Of two hboxes containing the hit point, the one with the smallest area.
    fn smallest_container(&self, node: u32, other: u32) -> u32 {
        let size = |node: u32| {
            let n = &self.nodes[node as usize];
            let b = &self.hboxes[n.aux as usize];
            let width = i128::from(b.width).abs();
            let height = i128::from(b.depth).abs() + i128::from(b.height).abs();
            (width * height, i64::from(n.width).abs(), height)
        };
        let (area, width, height) = size(node);
        let (other_area, other_width, other_height) = size(other);
        if area != other_area {
            return if area < other_area { node } else { other };
        }
        if width != other_width {
            return if width > other_width { node } else { other };
        }
        if height > other_height { other } else { node }
    }

    /// The deepest box containing the hit point, starting with `node` itself.
    fn deepest_container_v2(&self, hit: Point, node: u32) -> Option<u32> {
        if self.nodes[node as usize].first_child == NONE {
            return None;
        }
        for child in self.children(node) {
            if self.point_in_box(hit, child)
                && let Some(deep) = self.deepest_container_v2(hit, child)
            {
                return Some(deep);
            }
        }
        if self.kind(node) == Kind::VBox {
            // In a vbox, the closest child that has content.
            let mut best = Nd::NONE;
            for child in self.children(node) {
                if self.nodes[child as usize].first_child != NONE {
                    let distance = self.node_distance(hit, child);
                    if distance <= best.distance {
                        best = Nd::new(child, distance);
                    }
                }
            }
            if best.node.is_some() {
                return best.node;
            }
        }
        self.point_in_box(hit, node).then_some(node)
    }

    /// Variant of [`Self::deepest_container_v2`] that explores every child.
    fn deepest_container_v3(&self, hit: Point, node: u32) -> Nd {
        if self.nodes[node as usize].first_child == NONE {
            return Nd::NONE;
        }
        for child in self.children(node) {
            let deep = self.deepest_container_v3(hit, child);
            if deep.node.is_some() {
                return deep;
            }
        }
        if self.kind(node) == Kind::VBox {
            let mut best = Nd::NONE;
            for child in self.children(node) {
                if self.nodes[child as usize].first_child != NONE {
                    let distance = self.node_distance(hit, child);
                    if distance < best.distance {
                        best = Nd::new(child, distance);
                    }
                }
            }
            if best.node.is_some() {
                return best;
            }
        }
        if self.point_in_box(hit, node) {
            Nd::new(node, 0)
        } else {
            Nd::NONE
        }
    }

    /// The children of a box closest to the hit point on its left and on its right.
    fn closest_children_in_box(&self, hit: Point, node: u32) -> (Nd, Nd) {
        if self.nodes[node as usize].first_child == NONE || self.kind(node) != Kind::HBox {
            // The reference implementation finds nothing in vboxes; the caller then falls
            // back to the box itself.
            return (Nd::NONE, Nd::NONE);
        }
        let mut left = Nd::NONE;
        let mut right = Nd::NONE;
        let prefer = |current: &Nd, candidate: u32| {
            let Some(current) = current.node else {
                return false;
            };
            let (c, n) = (
                &self.nodes[current as usize],
                &self.nodes[candidate as usize],
            );
            c.tag == n.tag && (c.line > n.line || (c.line == n.line && c.column > n.column))
        };
        for child in self.children(node) {
            let nd = self.h_ordered_distance(hit, child);
            if nd.distance > 0 {
                // On the right of the hit point.
                if right.distance > nd.distance
                    || (right.distance == nd.distance && prefer(&right, child))
                {
                    right = nd;
                }
            } else if nd.distance == 0 {
                if self.nodes[child as usize].first_child != NONE {
                    return self.closest_children_in_box(hit, child);
                }
                left = nd;
            } else {
                let distance = -nd.distance;
                if left.distance > distance || (left.distance == distance && prefer(&left, child)) {
                    left = Nd::new(child, distance);
                }
            }
        }
        for side in [&mut left, &mut right] {
            if let Some(node) = side.node {
                let deep = self.deepest_container_v3(hit, node);
                if deep.node.is_some() {
                    *side = deep;
                }
                if let Some(node) = side.node
                    && let Some(closest) = self.closest_deep_child(hit, node).node
                {
                    side.node = Some(closest);
                }
            }
        }
        (left, right)
    }

    /// The leaf closest to the hit point among the descendants of `node`.
    fn closest_deep_child(&self, hit: Point, node: u32) -> Nd {
        let mut best = Nd::NONE;
        for child in self.children(node) {
            let nd = if self.kind(child).is_box() {
                self.closest_deep_child(hit, child)
            } else {
                Nd::new(child, self.node_distance(hit, child))
            };
            let is_kern = nd.node.is_some_and(|n| self.kind(n) == Kind::Kern);
            if nd.distance < best.distance || (nd.distance == best.distance && !is_kern) {
                best = nd;
            }
        }
        best
    }

    // ------------------------------------------------------------------ tree helpers

    fn kind(&self, node: u32) -> Kind {
        self.nodes[node as usize].kind
    }

    fn children(&self, node: u32) -> Children<'_> {
        Children {
            nodes: &self.nodes,
            next: self.nodes[node as usize].first_child,
        }
    }

    fn page_of(&self, node: u32) -> Option<u32> {
        let mut node = node;
        loop {
            let n = self.nodes.get(node as usize)?;
            if n.kind == Kind::Sheet {
                return Some(n.aux);
            }
            node = n.parent;
        }
    }
}

struct Children<'a> {
    nodes: &'a [Node],
    next: u32,
}

impl Iterator for Children<'_> {
    type Item = u32;

    fn next(&mut self) -> Option<u32> {
        let current = self.next;
        self.next = self.nodes.get(current as usize)?.next_sibling;
        Some(current)
    }
}

/// Distance from a point to a box: 0 inside, the distance to the closest edge beside it,
/// the L1 distance to the closest corner diagonally.
fn distance_to_box(hit: Point, b: Extent) -> i64 {
    let dh = if hit.h < b.min_h {
        b.min_h - hit.h
    } else if hit.h > b.max_h {
        hit.h - b.max_h
    } else {
        0
    };
    let dv = if hit.v < b.min_v {
        b.min_v - hit.v
    } else if hit.v > b.max_v {
        hit.v - b.max_v
    } else {
        0
    };
    dh + dv
}

/// Lexical normalisation: drops `.` components and resolves `..` without touching the file
/// system.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => out.push(".."),
            },
            other => out.push(other.as_os_str()),
        }
    }
    out
}

// ---------------------------------------------------------------------- parser

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Preamble,
    Content,
    Postamble,
    PostScriptum,
}

#[derive(Clone, Copy)]
struct Frame {
    node: u32,
    last_child: u32,
}

/// The fields of a node record, as decoded from `tag,line[,column]:h,v[:width[,height,depth]]`.
#[derive(Clone, Copy, Default)]
struct Fields {
    tag: i32,
    line: i32,
    column: i32,
    h: i32,
    v: i32,
    width: i32,
    height: i32,
    depth: i32,
}

/// Builds the box tree record by record, mirroring the reference parser (including its
/// adjustments of line numbers for boundary, kern and glue nodes).
struct Parser {
    section: Section,
    seen_header: bool,
    pre_unit: i64,
    pre_magnification: i64,
    pre_x_offset: i64,
    pre_y_offset: i64,
    post_magnification: Option<f64>,
    post_x_offset: Option<f64>,
    post_y_offset: Option<f64>,

    inputs: Vec<(i32, PathBuf)>,
    max_lines: HashMap<i32, i32>,
    nodes: Vec<Node>,
    hboxes: Vec<HBoxInfo>,
    sheets: Vec<Sheet>,
    friends: Vec<(i32, i32, u32)>,

    /// Open sheet and boxes.
    stack: Vec<Frame>,
    /// Leading `x` nodes of boxes, waiting for the next node to give them its line
    /// (their own line is often wrong). `pending_levels` holds where each box's start.
    pending: Vec<u32>,
    pending_levels: Vec<usize>,
    last_kern: u32,
    last_glue: u32,
    last_v: i32,
    form_depth: u32,
}

type ParseResult = Result<(), SyncTexError>;

impl Parser {
    fn new(len: usize) -> Self {
        Parser {
            section: Section::Preamble,
            seen_header: false,
            pre_unit: 8192,
            pre_magnification: 1000,
            pre_x_offset: 578,
            pre_y_offset: 578,
            post_magnification: None,
            post_x_offset: None,
            post_y_offset: None,
            inputs: Vec::new(),
            max_lines: HashMap::new(),
            nodes: Vec::with_capacity(len / 24),
            hboxes: Vec::with_capacity(len / 400),
            sheets: Vec::new(),
            friends: Vec::with_capacity(len / 32),
            stack: Vec::new(),
            pending: Vec::new(),
            pending_levels: Vec::new(),
            last_kern: NONE,
            last_glue: NONE,
            last_v: 0,
            form_depth: 0,
        }
    }

    fn line(&mut self, number: usize, line: &[u8]) -> ParseResult {
        let malformed = |reason| SyncTexError::Malformed {
            line: number,
            reason,
        };
        if let Some(rest) = line.strip_prefix(b"Input:") {
            let (tag, name) = parse_input(rest).ok_or(malformed("bad Input record"))?;
            self.inputs.push((tag, name));
            return Ok(());
        }
        match self.section {
            Section::Preamble => self.preamble(line).map_err(malformed),
            Section::Content => self.content(line).map_err(malformed),
            Section::Postamble => {
                if line.starts_with(b"Post scriptum:") {
                    self.section = Section::PostScriptum;
                }
                Ok(())
            }
            Section::PostScriptum => {
                // Like the reference implementation, a bad value here is ignored.
                if let Some(value) = line.strip_prefix(b"Magnification:") {
                    self.post_magnification = parse_float(value).filter(|&m| m > 0.0);
                } else if let Some(value) = line.strip_prefix(b"X Offset:") {
                    self.post_x_offset = parse_dimension(value);
                } else if let Some(value) = line.strip_prefix(b"Y Offset:") {
                    self.post_y_offset = parse_dimension(value);
                }
                Ok(())
            }
        }
    }

    fn preamble(&mut self, line: &[u8]) -> Result<(), &'static str> {
        let int = |value: &[u8]| {
            parse_int(value.trim_ascii())
                .map(i64::from)
                .ok_or("bad number in the preamble")
        };
        if line.starts_with(b"SyncTeX Version:") {
            self.seen_header = true;
        } else if line.starts_with(b"Content:") {
            if !self.seen_header {
                return Err("missing `SyncTeX Version:` header");
            }
            self.section = Section::Content;
        } else if let Some(value) = line.strip_prefix(b"Magnification:") {
            self.pre_magnification = int(value)?;
        } else if let Some(value) = line.strip_prefix(b"Unit:") {
            self.pre_unit = int(value)?;
        } else if let Some(value) = line.strip_prefix(b"X Offset:") {
            self.pre_x_offset = int(value)?;
        } else if let Some(value) = line.strip_prefix(b"Y Offset:") {
            self.pre_y_offset = int(value)?;
        }
        Ok(())
    }

    fn content(&mut self, line: &[u8]) -> Result<(), &'static str> {
        let Some((&first, rest)) = line.split_first() else {
            return Ok(());
        };
        if self.form_depth > 0 {
            // PDF forms (`\pdfxform`) are not placed on pages; their content is skipped.
            match first {
                b'<' => self.form_depth += 1,
                b'>' => self.form_depth -= 1,
                _ => {}
            }
            return Ok(());
        }
        match first {
            b'{' => self.open_sheet(rest),
            b'}' => self.close_sheet(),
            b'[' => self.open_box(Kind::VBox, rest),
            b'(' => self.open_box(Kind::HBox, rest),
            b']' => self.close_vbox(),
            b')' => self.close_hbox(),
            b'v' => self.void_box(Kind::VoidVBox, rest),
            b'h' => self.void_box(Kind::VoidHBox, rest),
            b'k' => self.kern(rest),
            b'g' => self.glue_or_math(Kind::Glue, rest),
            b'$' => self.glue_or_math(Kind::Math, rest),
            b'r' => self.rule(rest),
            b'x' => self.boundary(rest),
            b'<' => {
                self.form_depth = 1;
                Ok(())
            }
            _ if line.starts_with(b"Postamble:") => {
                if !self.stack.is_empty() {
                    return Err("postamble inside a page");
                }
                self.section = Section::Postamble;
                Ok(())
            }
            // `!` byte offsets, `f` form references, `c` characters, comments, and records
            // of later versions of the format.
            _ => Ok(()),
        }
    }

    fn finish(mut self) -> Result<SyncTex, SyncTexError> {
        if !self.seen_header {
            return Err(SyncTexError::NotSyncTex(
                "missing `SyncTeX Version:` header",
            ));
        }
        if self.section == Section::Preamble {
            return Err(SyncTexError::NotSyncTex("missing `Content:` section"));
        }
        if !self.stack.is_empty() {
            return Err(SyncTexError::Malformed {
                line: 0,
                reason: "unexpected end of file inside a page",
            });
        }

        // Units and offsets, as the reference implementation computes them.
        let pre_unit = if self.pre_unit > 0 {
            self.pre_unit
        } else {
            8192
        };
        let pre_magnification = if self.pre_magnification > 0 {
            self.pre_magnification
        } else {
            1000
        };
        let base = pre_unit as f64 / 65781.76;
        let unit_f32 = {
            let unit = match self.post_magnification {
                Some(magnification) => (f64::from(magnification as f32) * base) as f32,
                None => base as f32,
            };
            (f64::from(unit) * (pre_magnification as f64 / 1000.0)) as f32
        };
        // The post scriptum offsets (set by tools that move the output) replace the preamble's.
        let (x_offset_f32, y_offset_f32) = match self.post_x_offset {
            None => (
                (self.pre_x_offset as f64 * base) as f32,
                (self.pre_y_offset as f64 * base) as f32,
            ),
            Some(x) => (
                x as f32 / 65781.76_f32,
                self.post_y_offset.unwrap_or(0.0) as f32 / 65781.76_f32,
            ),
        };
        let big_box = (1_500_000.0 / (pre_magnification as f64 / 1000.0)) as i64;

        self.friends.sort_unstable();

        let mut paths: Vec<PathBuf> = Vec::new();
        let mut tags = HashMap::new();
        for (tag, path) in self.inputs {
            let path = normalize(&path);
            let index = match paths.iter().position(|known| *known == path) {
                Some(index) => index,
                None => {
                    paths.push(path);
                    paths.len() - 1
                }
            };
            tags.insert(tag, index);
        }

        Ok(SyncTex {
            paths,
            tags,
            max_lines: self.max_lines,
            nodes: self.nodes,
            hboxes: self.hboxes,
            sheets: self.sheets,
            friends: self.friends,
            unit: f64::from(unit_f32),
            x_offset: f64::from(x_offset_f32),
            y_offset: f64::from(y_offset_f32),
            unit_f32,
            x_offset_f32,
            y_offset_f32,
            big_box,
        })
    }

    // -------------------------------------------------------------- records

    fn open_sheet(&mut self, rest: &[u8]) -> Result<(), &'static str> {
        if !self.stack.is_empty() {
            return Err("page inside a page");
        }
        let page = parse_int(rest.trim_ascii())
            .and_then(|page| u32::try_from(page).ok())
            .ok_or("bad page number")?;
        let node = self.new_node(Kind::Sheet, Fields::default())?;
        self.nodes[node as usize].aux = page;
        self.sheets.push(Sheet {
            page,
            node,
            hboxes: Vec::new(),
        });
        self.stack.push(Frame {
            node,
            last_child: NONE,
        });
        self.pending_levels.push(self.pending.len());
        self.reset_kern_glue();
        Ok(())
    }

    fn close_sheet(&mut self) -> Result<(), &'static str> {
        match self.stack.as_slice() {
            [_] => {}
            [] => return Err("`}` outside of a page"),
            _ => return Err("unclosed box at the end of a page"),
        }
        self.pop_pending_level();
        self.stack.clear();
        self.reset_kern_glue();
        Ok(())
    }

    fn open_box(&mut self, kind: Kind, rest: &[u8]) -> Result<(), &'static str> {
        if self.stack.len() > MAX_DEPTH {
            return Err("boxes nested too deeply");
        }
        let fields = self.fields(rest, true, true)?;
        let node = self.append(kind, fields)?;
        self.register_line(fields);
        self.stack.push(Frame {
            node,
            last_child: NONE,
        });
        self.pending_levels.push(self.pending.len());
        if kind == Kind::HBox {
            self.nodes[node as usize].aux =
                u32::try_from(self.hboxes.len()).map_err(|_| "too many boxes")?;
            self.hboxes.push(HBoxInfo {
                h: fields.h,
                v: fields.v,
                width: fields.width,
                height: fields.height,
                depth: fields.depth,
                mean_line: fields.line,
                weight: 1,
            });
            // The opening boundary, at the start of the box with the box's own line.
            let boundary = Fields {
                width: 0,
                height: 0,
                depth: 0,
                ..fields
            };
            let start = self.append(Kind::BoxBoundary, boundary)?;
            self.make_friend(start);
        }
        self.reset_kern_glue();
        Ok(())
    }

    fn close_vbox(&mut self) -> Result<(), &'static str> {
        let frame = self.open_box_frame(Kind::VBox, "`]` without a matching `[`")?;
        if self.nodes[frame.node as usize].first_child == NONE {
            // Only empty vboxes can be found by forward search.
            self.make_friend(frame.node);
        }
        self.pop_pending_level();
        self.stack.pop();
        self.resolve_pending(frame.node);
        self.reset_kern_glue();
        Ok(())
    }

    fn close_hbox(&mut self) -> Result<(), &'static str> {
        let frame = self.open_box_frame(Kind::HBox, "`)` without a matching `(`")?;
        let hbox = frame.node;
        if let Some(sheet) = self.sheets.last_mut() {
            sheet.hboxes.push(hbox);
        }
        let info = self.nodes[hbox as usize].aux as usize;

        // Mean line of the content, weighted by the size of nested hboxes. The opening
        // boundary takes the line of the first real node.
        let first = self.nodes[hbox as usize].first_child;
        let second = self.nodes[first as usize].next_sibling;
        if second == NONE {
            self.hboxes[info].mean_line = self.nodes[hbox as usize].line;
            self.hboxes[info].weight = 1;
        } else {
            self.nodes[first as usize].line = self.nodes[second as usize].line;
            let (mut weight, mut lines) = (0_i64, 0_i64);
            let mut node = second;
            while node != NONE {
                let n = &self.nodes[node as usize];
                if n.kind == Kind::HBox {
                    let inner = &self.hboxes[n.aux as usize];
                    let w = i64::from(inner.weight.max(1));
                    weight += w;
                    lines += i64::from(inner.mean_line) * w;
                } else {
                    weight += 1;
                    lines += i64::from(n.line);
                }
                node = n.next_sibling;
            }
            self.hboxes[info].mean_line = saturate((lines + weight / 2) / weight);
            self.hboxes[info].weight = saturate(weight);
        }

        // The closing boundary, at the end of the visible box with the last node's line.
        let visible = self.hboxes[info];
        let last = &self.nodes[frame.last_child as usize];
        let closing = Fields {
            tag: last.tag,
            line: last.line,
            column: last.column,
            h: saturate(i64::from(visible.h) + i64::from(visible.width)),
            v: visible.v,
            ..Fields::default()
        };
        self.append(Kind::BoxBoundary, closing)?;
        let start = &mut self.nodes[first as usize];
        start.h = visible.h;
        start.v = visible.v;

        // A trailing kern + glue (end of paragraph) belongs to the node before the kern.
        if self.last_kern != NONE && self.last_glue != NONE {
            let mut previous = first;
            let mut next = self.nodes[first as usize].next_sibling;
            while next != NONE {
                if next == self.last_kern {
                    let (tag, line, column) = self.tlc(previous);
                    for node in [self.last_kern, self.last_glue] {
                        let n = &mut self.nodes[node as usize];
                        (n.tag, n.line, n.column) = (tag, line, column);
                    }
                    break;
                }
                previous = next;
                next = self.nodes[next as usize].next_sibling;
            }
        }

        self.pop_pending_level();
        self.stack.pop();
        self.resolve_pending(hbox);
        if let Some(parent) = self.stack.last().map(|frame| frame.node) {
            let extent = extent_of(
                visible.h,
                visible.v,
                visible.width,
                visible.height,
                visible.depth,
            );
            self.contain_extent(parent, extent);
        }
        self.reset_kern_glue();
        Ok(())
    }

    fn void_box(&mut self, kind: Kind, rest: &[u8]) -> Result<(), &'static str> {
        let fields = self.fields(rest, true, true)?;
        let node = self.append(kind, fields)?;
        self.resolve_pending(node);
        if kind == Kind::VoidHBox
            && let Some(parent) = self.parent()
        {
            let extent = extent_of(
                fields.h,
                fields.v,
                fields.width,
                fields.height,
                fields.depth,
            );
            self.contain_extent(parent, extent);
        }
        self.register_line(fields);
        self.reset_kern_glue();
        Ok(())
    }

    fn kern(&mut self, rest: &[u8]) -> Result<(), &'static str> {
        let fields = self.fields(rest, true, false)?;
        let node = self.append(Kind::Kern, fields)?;
        self.make_friend(node);
        self.resolve_pending(node);
        if let Some(parent) = self.parent() {
            // The kern is recorded at its end.
            let (h, w) = (i64::from(fields.h), i64::from(fields.width));
            let (min_h, max_h) = if w > 0 { (h - w, h) } else { (h, h - w) };
            let v = i64::from(fields.v);
            self.contain_extent(
                parent,
                Extent {
                    min_h,
                    max_h,
                    min_v: v,
                    max_v: v,
                },
            );
        }
        self.register_line(fields);
        self.last_kern = node;
        self.last_glue = NONE;
        Ok(())
    }

    fn glue_or_math(&mut self, kind: Kind, rest: &[u8]) -> Result<(), &'static str> {
        let fields = self.fields(rest, false, false)?;
        let node = self.append(kind, fields)?;
        self.make_friend(node);
        self.resolve_pending(node);
        self.contain_point(fields);
        self.register_line(fields);
        if kind == Kind::Glue && self.last_kern != NONE {
            self.last_glue = node;
        } else {
            self.reset_kern_glue();
        }
        Ok(())
    }

    fn rule(&mut self, rest: &[u8]) -> Result<(), &'static str> {
        let fields = self.fields(rest, true, true)?;
        let node = self.append(Kind::Rule, fields)?;
        self.make_friend(node);
        self.resolve_pending(node);
        self.register_line(fields);
        self.reset_kern_glue();
        Ok(())
    }

    fn boundary(&mut self, rest: &[u8]) -> Result<(), &'static str> {
        let fields = self.fields(rest, false, false)?;
        let previous = self.stack.last().map_or(NONE, |frame| frame.last_child);
        let node = self.append(Kind::Boundary, fields)?;
        let level = self.pending_levels.last().copied().unwrap_or(0);
        let leading = previous != NONE && self.nodes[previous as usize].kind == Kind::BoxBoundary;
        if leading || self.pending.len() > level {
            // The first `x` of a box often carries a wrong line; it takes the line of the
            // next node instead.
            self.pending.push(node);
        } else {
            self.make_friend(node);
        }
        self.contain_point(fields);
        self.register_line(fields);
        self.reset_kern_glue();
        Ok(())
    }

    // -------------------------------------------------------------- helpers

    /// Decodes `tag,line[,column]:h,v` followed by `:width` and `,height,depth` as asked.
    fn fields(&mut self, rest: &[u8], width: bool, size: bool) -> Result<Fields, &'static str> {
        let mut cursor = Cursor { s: rest, i: 0 };
        let mut fields = Fields {
            tag: cursor.int().ok_or("bad tag")?,
            line: cursor.int().ok_or("bad line number")?,
            column: -1,
            ..Fields::default()
        };
        if cursor.peek() == Some(b',') {
            fields.column = cursor.int().ok_or("bad column")?;
        }
        fields.h = cursor.int().ok_or("bad horizontal position")?;
        fields.v = match cursor.int() {
            Some(v) => v,
            None if cursor.rest().starts_with(b",=") => {
                cursor.i += 2;
                self.last_v
            }
            None => return Err("bad vertical position"),
        };
        self.last_v = fields.v;
        if width {
            fields.width = cursor.int().ok_or("bad width")?;
        }
        if size {
            fields.height = cursor.int().ok_or("bad height")?;
            fields.depth = cursor.int().ok_or("bad depth")?;
        }
        Ok(fields)
    }

    fn new_node(&mut self, kind: Kind, f: Fields) -> Result<u32, &'static str> {
        let index = u32::try_from(self.nodes.len())
            .ok()
            .filter(|&index| index != NONE)
            .ok_or("too many records")?;
        self.nodes.push(Node {
            kind,
            tag: f.tag,
            line: f.line,
            column: f.column,
            h: f.h,
            v: f.v,
            width: f.width,
            height: f.height,
            depth: f.depth,
            parent: NONE,
            first_child: NONE,
            next_sibling: NONE,
            aux: 0,
        });
        Ok(index)
    }

    /// Adds a node as the last child of the innermost open box or sheet.
    fn append(&mut self, kind: Kind, fields: Fields) -> Result<u32, &'static str> {
        let Some(frame) = self.stack.last().copied() else {
            return Err("record outside of a page");
        };
        let node = self.new_node(kind, fields)?;
        self.nodes[node as usize].parent = frame.node;
        if frame.last_child == NONE {
            self.nodes[frame.node as usize].first_child = node;
        } else {
            self.nodes[frame.last_child as usize].next_sibling = node;
        }
        if let Some(frame) = self.stack.last_mut() {
            frame.last_child = node;
        }
        Ok(node)
    }

    fn open_box_frame(&self, kind: Kind, error: &'static str) -> Result<Frame, &'static str> {
        match self.stack.last() {
            Some(&frame) if self.nodes[frame.node as usize].kind == kind => Ok(frame),
            _ => Err(error),
        }
    }

    fn parent(&self) -> Option<u32> {
        self.stack.last().map(|frame| frame.node)
    }

    fn tlc(&self, node: u32) -> (i32, i32, i32) {
        let n = &self.nodes[node as usize];
        (n.tag, n.line, n.column)
    }

    fn make_friend(&mut self, node: u32) {
        let (tag, line, _) = self.tlc(node);
        self.friends.push((tag, line, node));
    }

    fn register_line(&mut self, fields: Fields) {
        let max = self.max_lines.entry(fields.tag).or_insert(fields.line);
        *max = (*max).max(fields.line);
    }

    /// Gives the waiting leading `x` nodes of the current box the line of `model`.
    fn resolve_pending(&mut self, model: u32) {
        let level = self.pending_levels.last().copied().unwrap_or(0);
        if self.pending.len() <= level {
            return;
        }
        let (tag, line, column) = self.tlc(model);
        let pending = std::mem::take(&mut self.pending);
        for &node in &pending[level..] {
            let n = &mut self.nodes[node as usize];
            (n.tag, n.line, n.column) = (tag, line, column);
            self.friends.push((tag, line, node));
        }
        self.pending = pending;
        self.pending.truncate(level);
    }

    /// Closes a box level: its waiting `x` nodes keep their own line.
    fn pop_pending_level(&mut self) {
        let level = self
            .pending_levels
            .pop()
            .unwrap_or(0)
            .min(self.pending.len());
        for index in level..self.pending.len() {
            let node = self.pending[index];
            self.make_friend(node);
        }
        self.pending.truncate(level);
    }

    fn reset_kern_glue(&mut self) {
        self.last_kern = NONE;
        self.last_glue = NONE;
    }

    fn contain_point(&mut self, fields: Fields) {
        if let Some(parent) = self.parent() {
            let (h, v) = (i64::from(fields.h), i64::from(fields.v));
            self.contain_extent(
                parent,
                Extent {
                    min_h: h,
                    max_h: h,
                    min_v: v,
                    max_v: v,
                },
            );
        }
    }

    /// Grows the visible extent of an hbox towards `e`, one side per axis like the
    /// reference implementation.
    fn contain_extent(&mut self, node: u32, e: Extent) {
        let n = &self.nodes[node as usize];
        if n.kind != Kind::HBox {
            return;
        }
        let b = &mut self.hboxes[n.aux as usize];
        let (h, width) = (i64::from(b.h), i64::from(b.width));
        if width < 0 {
            let (max, min) = (h, h + width);
            if e.min_h < min {
                b.width = saturate(e.min_h - max);
            } else if e.max_h > max {
                b.h = saturate(e.max_h);
                b.width = saturate(min - e.max_h);
            }
        } else {
            let (min, max) = (h, h + width);
            if e.min_h < min {
                b.h = saturate(e.min_h);
                b.width = saturate(max - e.min_h);
            } else if e.max_h > max {
                b.width = saturate(e.max_h - min);
            }
        }
        let v = i64::from(b.v);
        let (min, max) = (v - i64::from(b.height), v + i64::from(b.depth));
        if e.min_v < min {
            b.height = saturate(v - e.min_v);
        } else if e.max_v > max {
            b.depth = saturate(e.max_v - v);
        }
    }
}

fn extent_of(h: i32, v: i32, width: i32, height: i32, depth: i32) -> Extent {
    let (h, width) = (i64::from(h), i64::from(width));
    let (min_h, max_h) = if width < 0 {
        (h + width, h)
    } else {
        (h, h + width)
    };
    Extent {
        min_h,
        max_h,
        min_v: i64::from(v) - i64::from(height),
        max_v: i64::from(v) + i64::from(depth),
    }
}

fn saturate(value: i64) -> i32 {
    value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// Reads the integers of a record, each optionally preceded by a `:` or `,` separator.
struct Cursor<'a> {
    s: &'a [u8],
    i: usize,
}

impl Cursor<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn rest(&self) -> &[u8] {
        self.s.get(self.i..).unwrap_or_default()
    }

    fn int(&mut self) -> Option<i32> {
        let mut i = self.i;
        if matches!(self.s.get(i), Some(b':' | b',')) {
            i += 1;
        }
        let (value, used) = parse_int_prefix(self.s.get(i..)?)?;
        self.i = i + used;
        Some(value)
    }
}

/// Parses a decimal integer at the start of `s`; returns it with the number of bytes used.
fn parse_int_prefix(s: &[u8]) -> Option<(i32, usize)> {
    let (negative, start) = match s.first() {
        Some(b'-') => (true, 1),
        Some(b'+') => (false, 1),
        _ => (false, 0),
    };
    let digits = s[start..].iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let mut value: i64 = 0;
    for &digit in &s[start..start + digits] {
        value = value * 10 + i64::from(digit - b'0');
        if value > i64::from(i32::MAX) + 1 {
            return None;
        }
    }
    let value = if negative { -value } else { value };
    Some((i32::try_from(value).ok()?, start + digits))
}

fn parse_int(s: &[u8]) -> Option<i32> {
    match parse_int_prefix(s)? {
        (value, used) if used == s.len() => Some(value),
        _ => None,
    }
}

/// `Input:<tag>:<name>`; the name runs to the end of the line, trailing spaces trimmed.
fn parse_input(rest: &[u8]) -> Option<(i32, PathBuf)> {
    let (tag, used) = parse_int_prefix(rest)?;
    let name = rest.get(used..)?.strip_prefix(b":")?.trim_ascii_end();
    if name.is_empty() {
        return None;
    }
    Some((
        tag,
        PathBuf::from(String::from_utf8_lossy(name).into_owned()),
    ))
}

fn parse_float(s: &[u8]) -> Option<f64> {
    let s = std::str::from_utf8(s).ok()?.trim();
    s.parse().ok().filter(|value: &f64| value.is_finite())
}

/// A post scriptum offset such as `1in` or `-2.5cm`, in scaled points.
fn parse_dimension(s: &[u8]) -> Option<f64> {
    let s = std::str::from_utf8(s).ok()?.trim();
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E')))
        .unwrap_or(s.len());
    let (number, unit) = s.split_at(split);
    let value: f64 = number
        .parse()
        .ok()
        .filter(|value: &f64| value.is_finite())?;
    let factor = match unit.trim() {
        "in" => 72.27 * 65536.0,
        "cm" => 72.27 * 65536.0 / 2.54,
        "mm" => 72.27 * 65536.0 / 25.4,
        "pt" => 65536.0,
        "bp" => 72.27 / 72.0 * 65536.0,
        "pc" => 12.0 * 65536.0,
        "sp" | "" => 1.0,
        "dd" => 1238.0 / 1157.0 * 65536.0,
        "cc" => 14856.0 / 1157.0 * 65536.0,
        "nd" => 685.0 / 642.0 * 65536.0,
        "nc" => 1370.0 / 107.0 * 65536.0,
        _ => return None,
    };
    Some(value * factor)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hand-written file in the format pdfTeX writes: one page, two lines of text from
    /// `main.tex` (lines 3 and 4), the second line ending with material from line 5.
    fn sample(preamble: &str, post_scriptum: &str) -> String {
        format!(
            "SyncTeX Version:1\n\
             Input:1:/project/./main.tex\n\
             Input:2:/project/sub/../chapters/./intro.tex\n\
             Output:pdf\n\
             {preamble}\
             Content:\n\
             !100\n\
             {{1\n\
             [1,3:4736286,4736286:30000000,40000000,0\n\
             (1,3:4736286,10000000:30000000,500000,200000\n\
             x1,3:4736286,10000000\n\
             g1,3:8000000,10000000\n\
             k1,3:12000000,10000000:100000\n\
             g1,3:20000000,10000000\n\
             )\n\
             (1,4:4736286,11000000:30000000,500000,200000\n\
             g1,4:6000000,11000000\n\
             $1,4:9000000,11000000\n\
             $1,4:12000000,11000000\n\
             g1,5:20000000,11000000\n\
             x1,5:25000000,11000000\n\
             )\n\
             (2,7:4736286,12000000:30000000,500000,200000\n\
             g2,7:6000000,12000000\n\
             )\n\
             ]\n\
             !50\n\
             }}1\n\
             Postamble:\n\
             Count:12\n\
             Post scriptum:\n\
             {post_scriptum}"
        )
    }

    fn standard() -> SyncTex {
        SyncTex::parse(&sample(
            "Magnification:1000\nUnit:1\nX Offset:0\nY Offset:0\n",
            "",
        ))
        .unwrap()
    }

    const SP_PER_BP: f64 = 65781.76;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn reads_units_and_offsets_from_the_preamble() {
        let synctex = standard();
        let rects = synctex.forward(Path::new("/project/main.tex"), 3, None);
        assert_eq!(rects.len(), 1);
        let rect = &rects[0];
        assert_eq!(rect.page, 1);
        assert!(close(rect.x, 4736286.0 / SP_PER_BP), "{rect:?}");
        // Top of the box: baseline minus height.
        assert!(
            close(rect.y, (10000000.0 - 500000.0) / SP_PER_BP),
            "{rect:?}"
        );
        assert!(close(rect.width, 30000000.0 / SP_PER_BP), "{rect:?}");
        assert!(close(rect.height, 700000.0 / SP_PER_BP), "{rect:?}");
    }

    #[test]
    fn applies_unit_magnification_and_offsets() {
        // 8192 sp per unit, magnified twice, origin moved by 578 units.
        let synctex = SyncTex::parse(&sample(
            "Magnification:2000\nUnit:8192\nX Offset:578\nY Offset:578\n",
            "",
        ))
        .unwrap();
        let rect = &synctex.forward(Path::new("/project/main.tex"), 3, None)[0];
        let unit = 8192.0 / SP_PER_BP * 2.0;
        let offset = 578.0 * 8192.0 / SP_PER_BP;
        assert!(
            (rect.x - (4736286.0 * unit + offset)).abs() < 1.0,
            "{rect:?}"
        );
        assert!((rect.width - 30000000.0 * unit).abs() < 1.0, "{rect:?}");
    }

    #[test]
    fn post_scriptum_overrides_magnification_and_offsets() {
        let synctex = SyncTex::parse(&sample(
            "Magnification:1000\nUnit:1\nX Offset:0\nY Offset:0\n",
            "Magnification:0.5\nX Offset:1in\nY Offset:-2.54cm\n",
        ))
        .unwrap();
        let rect = &synctex.forward(Path::new("/project/main.tex"), 3, None)[0];
        assert!(
            (rect.x - (4736286.0 * 0.5 / SP_PER_BP + 72.0)).abs() < 0.01,
            "{rect:?}"
        );
        assert!(
            (rect.y - ((10000000.0 - 500000.0) * 0.5 / SP_PER_BP - 72.0)).abs() < 0.01,
            "{rect:?}"
        );
        assert!(
            (rect.width - 30000000.0 * 0.5 / SP_PER_BP).abs() < 0.01,
            "{rect:?}"
        );
    }

    #[test]
    fn normalises_input_paths() {
        let synctex = standard();
        let inputs: Vec<&Path> = synctex.inputs().collect();
        assert_eq!(
            inputs,
            [
                Path::new("/project/main.tex"),
                Path::new("/project/chapters/intro.tex")
            ]
        );
        assert_eq!(
            normalize(Path::new("/a/./b/../../c/./d.tex")),
            Path::new("/c/d.tex")
        );
        assert_eq!(normalize(Path::new("/../a.tex")), Path::new("/a.tex"));
        assert_eq!(normalize(Path::new("./x/../../y")), Path::new("../y"));
        // Queries are normalised too.
        let rects = synctex.forward(Path::new("/project/./x/../chapters/intro.tex"), 7, None);
        assert_eq!(rects.len(), 1);
        assert_eq!(
            synctex.forward(Path::new("chapters/intro.tex"), 7, None),
            rects,
            "relative paths match by suffix"
        );
        assert!(
            synctex
                .forward(Path::new("/elsewhere/main.tex"), 3, None)
                .is_empty()
        );
    }

    #[test]
    fn forward_search_returns_one_rect_per_line_and_falls_back_to_nearby_lines() {
        let synctex = standard();
        // Line 5 ends the second output line.
        let rects = synctex.forward(Path::new("/project/main.tex"), 5, None);
        assert_eq!(rects.len(), 1);
        assert!(close(rects[0].y, (11000000.0 - 500000.0) / SP_PER_BP));
        // Line 4 holds three nodes of the same box: still one rect.
        assert_eq!(
            synctex
                .forward(Path::new("/project/main.tex"), 4, None)
                .len(),
            1
        );
        // Line 1 produced nothing: the next line with output (3, two lines below) is used.
        let fallback = synctex.forward(Path::new("/project/main.tex"), 1, None);
        assert_eq!(
            fallback,
            synctex.forward(Path::new("/project/main.tex"), 3, None)
        );
        // Past the end of the file: the last line with output.
        assert_eq!(
            synctex.forward(Path::new("/project/main.tex"), 400, None),
            synctex.forward(Path::new("/project/main.tex"), 5, None)
        );
    }

    #[test]
    fn inverse_search_finds_the_line_under_the_point() {
        let synctex = standard();
        let bp = |sp: f64| sp / SP_PER_BP;
        let at = |x: f64, y: f64| synctex.inverse(1, bp(x), bp(y)).unwrap();
        let first = at(10000000.0, 9800000.0);
        assert_eq!(first.file, Path::new("/project/main.tex"));
        assert_eq!(first.line, 3);
        assert_eq!(first.column, None);
        assert_eq!(at(7000000.0, 10900000.0).line, 4);
        assert_eq!(at(24000000.0, 10900000.0).line, 5);
        let other = at(7000000.0, 11900000.0);
        assert_eq!(other.file, Path::new("/project/chapters/intro.tex"));
        assert_eq!(other.line, 7);
        // Outside every line: the closest node of the page.
        assert_eq!(at(7000000.0, 30000000.0).line, 7);
        assert!(synctex.inverse(2, 100.0, 100.0).is_none());
        assert!(synctex.inverse(0, 100.0, 100.0).is_none());
        assert!(synctex.inverse(1, f64::NAN, f64::INFINITY).is_some());
    }

    #[test]
    fn skips_unknown_records_and_forms() {
        let text = sample("Magnification:1000\nUnit:1\nX Offset:0\nY Offset:0\n", "")
            .replace(
                "!100\n",
                "!100\n<7\n(1,99:0,0:10,10,10\n)\n>\n%a comment\nWhatever: 12\n",
            )
            .replace(
                "g1,3:8000000,10000000\n",
                "g1,3:8000000,10000000\nf7:8000000,10000000\nc1,3:0,0\n?unknown\n\n",
            );
        let synctex = SyncTex::parse(&text).unwrap();
        assert_eq!(synctex.forward(Path::new("/project/main.tex"), 3, None), {
            standard().forward(Path::new("/project/main.tex"), 3, None)
        });
        // Nothing from the skipped form: line 99 is past the last line with output.
        assert_eq!(
            synctex.forward(Path::new("/project/main.tex"), 99, None),
            synctex.forward(Path::new("/project/main.tex"), 5, None)
        );
    }

    #[test]
    fn accepts_crlf_and_repeated_vertical_positions() {
        let text = sample("Magnification:1000\nUnit:1\nX Offset:0\nY Offset:0\n", "")
            .replace("g1,3:20000000,10000000", "g1,3:20000000,=")
            .replace('\n', "\r\n");
        let synctex = SyncTex::parse(&text).unwrap();
        assert_eq!(
            synctex.forward(Path::new("/project/main.tex"), 3, None),
            standard().forward(Path::new("/project/main.tex"), 3, None)
        );
    }

    #[test]
    fn rejects_malformed_input() {
        let good = sample("Magnification:1000\nUnit:1\nX Offset:0\nY Offset:0\n", "");
        let broken = [
            String::new(),
            "hello world\n".to_owned(),
            "SyncTeX Version:1\nInput:1:/a.tex\n".to_owned(),
            good.replace("SyncTeX Version:1\n", ""),
            good.replace("Unit:1", "Unit:one"),
            good.replace("g1,3:8000000,10000000", "g1,three:8000000,10000000"),
            good.replace("k1,3:12000000,10000000:100000", "k1,3:12000000,10000000"),
            good.replace("g1,3:8000000,10000000", "g1,3:99999999999,10000000"),
            good.replace("x1,5:25000000,11000000\n)\n", "x1,5:25000000,11000000\n"),
            good.replace("]\n", ")\n"),
            good.replace("{1\n", "{1\n{2\n"),
            good.replace("{1\n", ""),
            good.replace("Input:1:/project/./main.tex", "Input:x:/project/./main.tex"),
            good[..good.find("]\n").unwrap()].to_owned(),
            format!(
                "SyncTeX Version:1\nContent:\n{{1\n{}",
                "(1,1:0,0:0,0,0\n".repeat(MAX_DEPTH + 2)
            ),
        ];
        for text in &broken {
            assert!(SyncTex::parse(text).is_err(), "accepted:\n{text}");
        }
        assert!(matches!(
            SyncTex::from_gz(b"definitely not gzip"),
            Err(SyncTexError::Decompress(_))
        ));
        assert!(matches!(
            SyncTex::parse("SyncTeX Version:1\nContent:\n)\n"),
            Err(SyncTexError::Malformed { line: 3, .. })
        ));
    }

    #[test]
    fn can_be_shared_between_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SyncTex>();
    }

    #[test]
    fn reads_gzip() {
        use std::io::Write;

        let text = sample("Magnification:1000\nUnit:1\nX Offset:0\nY Offset:0\n", "");
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(text.as_bytes()).unwrap();
        let gz = encoder.finish().unwrap();
        let synctex = SyncTex::from_gz(&gz).unwrap();
        assert_eq!(
            synctex.forward(Path::new("/project/main.tex"), 4, None),
            standard().forward(Path::new("/project/main.tex"), 4, None)
        );
        // A truncated stream is an error, not a panic.
        assert!(SyncTex::from_gz(&gz[..gz.len() / 2]).is_err());
    }

    #[test]
    fn never_panics_on_truncated_or_mangled_files() {
        let text = sample("Magnification:1000\nUnit:1\nX Offset:0\nY Offset:0\n", "");
        for end in 0..text.len() {
            if let Ok(synctex) = SyncTex::parse(&text[..end]) {
                let _ = synctex.forward(Path::new("/project/main.tex"), 3, None);
                let _ = synctex.inverse(1, 100.0, 140.0);
            }
        }
        let bytes = text.as_bytes();
        for (index, replacement) in b"()[]x-:9\n"
            .iter()
            .cycle()
            .enumerate()
            .take(bytes.len() * 3)
        {
            let mut mangled = bytes.to_vec();
            mangled[index % bytes.len()] = *replacement;
            if let Ok(synctex) = SyncTex::parse_bytes(&mangled) {
                let _ = synctex.forward(Path::new("/project/main.tex"), 4, None);
                let _ = synctex.inverse(1, 150.0, 160.0);
            }
        }
    }
}
