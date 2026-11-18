//! Compares forward and inverse search with the answers of the official `synctex` CLI,
//! recorded in `fixtures/synctex/<case>/expected.json` by `fixtures/synctex/regen.sh`.

// Test helpers outside `#[test]` functions are not covered by `allow-unwrap-in-tests`.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::time::Instant;

use openprism_core::synctex::{PdfRect, SyncTex};
use serde::Deserialize;

const CASES: [&str; 5] = [
    "article-multifile",
    "twocolumn",
    "xelatex",
    "lualatex",
    "footnotes-lists",
];

/// Required share of queries answered like the CLI.
const MIN_ACCURACY: f64 = 0.95;
/// Slack, in big points, when comparing the vertical spans of rectangles.
const SPAN_TOLERANCE: f64 = 3.0;

#[derive(Deserialize)]
struct Expected {
    forward: Vec<ForwardQuery>,
    inverse: Vec<InverseQuery>,
}

#[derive(Deserialize)]
struct ForwardQuery {
    file: PathBuf,
    line: u32,
    results: Vec<ViewRecord>,
}

/// A `synctex view` record: `v` is the bottom of the box, `height` includes the depth.
#[derive(Deserialize)]
struct ViewRecord {
    page: u32,
    v: f64,
    height: f64,
}

#[derive(Deserialize)]
struct InverseQuery {
    page: u32,
    x: f64,
    y: f64,
    results: Vec<EditRecord>,
}

#[derive(Deserialize)]
struct EditRecord {
    file: PathBuf,
    line: u32,
}

/// `SYNCTEX_FIXTURES` points the comparisons at other recordings, such as the larger sets
/// `regen.sh` writes with `ORACLE_COUNTS` and `SYNCTEX_FIXTURE_OUT`.
fn fixture(case: &str) -> PathBuf {
    std::env::var_os("SYNCTEX_FIXTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/synctex"))
        .join(case)
}

fn load(case: &str) -> (SyncTex, Expected) {
    let dir = fixture(case);
    let gz = std::fs::read(dir.join("main.synctex.gz")).unwrap();
    let start = Instant::now();
    let synctex = SyncTex::from_gz(&gz).unwrap();
    println!("{case}: parsed in {:?}", start.elapsed());
    let expected = std::fs::read_to_string(dir.join("expected.json")).unwrap();
    (synctex, serde_json::from_str(&expected).unwrap())
}

fn overlaps(rect: &PdfRect, record: &ViewRecord) -> bool {
    let (top, bottom) = (record.v - record.height, record.v);
    rect.page == record.page
        && rect.y <= bottom + SPAN_TOLERANCE
        && rect.y + rect.height >= top - SPAN_TOLERANCE
}

fn report(what: &str, case: &str, hits: usize, total: usize, misses: &[String]) -> f64 {
    let accuracy = hits as f64 / total as f64;
    println!(
        "{case}: {what} {hits}/{total} ({:.1}%) like the CLI",
        accuracy * 100.0
    );
    for miss in misses {
        println!("  miss: {miss}");
    }
    accuracy
}

#[test]
fn inverse_search_matches_the_cli() {
    for case in CASES {
        let (synctex, expected) = load(case);
        let (mut hits, mut exact, mut misses) = (0, 0, Vec::new());
        for query in &expected.inverse {
            let ours = synctex.inverse(query.page, query.x, query.y);
            let cli = query.results.first();
            let ok = match (&ours, cli) {
                (Some(ours), Some(cli)) => {
                    if ours.file == cli.file && ours.line == cli.line {
                        exact += 1;
                    }
                    ours.file == cli.file && ours.line.abs_diff(cli.line) <= 1
                }
                (None, None) => {
                    exact += 1;
                    true
                }
                _ => false,
            };
            if ok {
                hits += 1;
            } else {
                misses.push(format!(
                    "page {} ({}, {}): ours {:?}, CLI {:?}",
                    query.page,
                    query.x,
                    query.y,
                    ours.map(|p| (p.file, p.line)),
                    cli.map(|c| (&c.file, c.line)),
                ));
            }
        }
        let total = expected.inverse.len();
        println!("{case}: inverse exact line {exact}/{total}");
        let accuracy = report("inverse ±1 line", case, hits, total, &misses);
        assert!(
            accuracy >= MIN_ACCURACY,
            "{case}: inverse accuracy {accuracy}"
        );
    }
}

#[test]
fn forward_search_matches_the_cli() {
    for case in CASES {
        let (synctex, expected) = load(case);
        let (mut hits, mut all_records, mut misses) = (0, 0, Vec::new());
        for query in &expected.forward {
            let ours = synctex.forward(&query.file, query.line, None);
            // The CLI's best record is shown by some rectangle of ours.
            let ok = match query.results.first() {
                Some(best) => ours.iter().any(|rect| overlaps(rect, best)),
                None => ours.is_empty(),
            };
            if query
                .results
                .iter()
                .all(|record| ours.iter().any(|rect| overlaps(rect, record)))
            {
                all_records += 1;
            }
            if ok {
                hits += 1;
            } else {
                misses.push(format!(
                    "{}:{}: ours {:?}, CLI {:?}",
                    query.file.display(),
                    query.line,
                    ours.iter()
                        .map(|r| (r.page, r.y, r.y + r.height))
                        .collect::<Vec<_>>(),
                    query
                        .results
                        .iter()
                        .map(|r| (r.page, r.v - r.height, r.v))
                        .collect::<Vec<_>>(),
                ));
            }
        }
        let total = expected.forward.len();
        println!("{case}: forward covers every CLI record {all_records}/{total}");
        let accuracy = report("forward", case, hits, total, &misses);
        assert!(
            accuracy >= MIN_ACCURACY,
            "{case}: forward accuracy {accuracy}"
        );
    }
}

#[test]
fn lists_the_project_inputs() {
    let (synctex, _) = load("article-multifile");
    let project: Vec<&Path> = synctex
        .inputs()
        .filter(|path| path.starts_with("/data/projects/fixture"))
        .collect();
    assert_eq!(
        project,
        [
            Path::new("/data/projects/fixture/main.tex"),
            Path::new("/data/projects/fixture/chapters/intro.tex"),
            Path::new("/data/projects/fixture/chapters/methods.tex"),
            Path::new("/data/projects/fixture/chapters/results.tex"),
        ]
    );
    assert!(
        synctex
            .inputs()
            .any(|path| path.ends_with("tex/latex/base/article.cls"))
    );
}

/// A synthetic book of more than 100 pages, made of the pages of a fixture, parses quickly.
#[test]
fn parses_a_hundred_pages_quickly() {
    let gz = std::fs::read(fixture("footnotes-lists").join("main.synctex.gz")).unwrap();
    let text = gunzip(&gz);
    let content = text.find("Content:\n").unwrap() + "Content:\n".len();
    let postamble = text.find("Postamble:").unwrap();
    let (preamble, pages) = (&text[..content], &text[content..postamble]);

    let mut book = String::from(preamble);
    let copies = 10;
    let mut page = 0;
    for _ in 0..copies {
        for line in pages.lines() {
            if line.starts_with('{') {
                page += 1;
                book.push_str(&format!("{{{page}\n"));
            } else if line.starts_with('}') {
                book.push_str(&format!("}}{page}\n"));
            } else {
                book.push_str(line);
                book.push('\n');
            }
        }
    }
    book.push_str("Postamble:\nCount:0\nPost scriptum:\n");
    assert!(page > 100, "{page} pages");

    let start = Instant::now();
    let synctex = SyncTex::parse(&book).unwrap();
    let elapsed = start.elapsed();
    println!(
        "{page} pages ({:.1} MB uncompressed) parsed in {elapsed:?}",
        book.len() as f64 / 1e6
    );
    let main = Path::new("/data/projects/fixture/main.tex");
    let mut pages: Vec<u32> = synctex
        .forward(main, 10, None)
        .iter()
        .map(|rect| rect.page)
        .collect();
    pages.dedup();
    assert_eq!(
        pages.len(),
        copies,
        "line 10 shows once per copy: {pages:?}"
    );
    assert!(synctex.inverse(page, 200.0, 400.0).is_some());

    // Debug builds (CI) are an order of magnitude slower; only time release builds.
    if !cfg!(debug_assertions) {
        let gz = gzip(&book);
        let start = Instant::now();
        SyncTex::from_gz(&gz).unwrap();
        let elapsed_gz = start.elapsed();
        println!(
            "the same from {} KB of gzip in {elapsed_gz:?}",
            gz.len() / 1000
        );
        assert!(elapsed.as_millis() < 100, "parsed in {elapsed:?}");
        assert!(
            elapsed_gz.as_millis() < 200,
            "decompressed and parsed in {elapsed_gz:?}"
        );
    }
}

fn gzip(text: &str) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(text.as_bytes()).unwrap();
    encoder.finish().unwrap()
}

fn gunzip(gz: &[u8]) -> String {
    use std::io::Read;
    let mut text = String::new();
    flate2::read::MultiGzDecoder::new(gz)
        .read_to_string(&mut text)
        .unwrap();
    text
}
