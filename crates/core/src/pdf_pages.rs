//! Keeping some pages of a compiled PDF: the page list a user types ("3-7, 10") and a smaller
//! PDF holding just those pages.
//!
//! Removing pages leaves references to them behind: bookmarks, named destinations (`\ref`,
//! `\label` with hyperref), links, page labels. Those are cleaned up *before* the pages are
//! deleted, because deleting a page strips the reference out of a destination array and leaves
//! it malformed.

use std::collections::{BTreeSet, HashSet};

use lopdf::{Dictionary, Document, Object, ObjectId};

/// Larger PDFs are refused: the whole file is held in memory while pages are extracted.
pub const MAX_PDF_BYTES: u64 = 200 * 1024 * 1024;
const MAX_SPEC_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct PageSpecError(String);

/// Pages listed in `spec` ("3-7, 10", "11-"), ascending and without repeats.
pub fn parse_pages(spec: &str, page_count: u32) -> Result<Vec<u32>, PageSpecError> {
    if page_count == 0 {
        return Err(PageSpecError("this PDF has no pages".into()));
    }
    let valid = format!("this PDF has pages 1–{page_count}");
    let spec = spec.trim();
    if spec.is_empty() {
        return Err(PageSpecError(format!(
            "choose the pages, such as 3-7, 10 ({valid})"
        )));
    }
    if spec.chars().count() > MAX_SPEC_CHARS {
        return Err(PageSpecError(format!(
            "the page list is too long ({valid})"
        )));
    }
    let number = |text: &str, item: &str| {
        text.trim().parse::<u32>().map_err(|_| {
            PageSpecError(format!(
                "`{item}` is not a page or a range such as 3-7 ({valid})"
            ))
        })
    };
    let mut pages = BTreeSet::new();
    for item in spec.split(',') {
        let item = item.trim();
        let (first, last) = match item.split_once('-') {
            Some((first, last)) => {
                let first = number(first, item)?;
                let last = if last.trim().is_empty() {
                    page_count
                } else {
                    number(last, item)?
                };
                (first, last)
            }
            None => {
                let page = number(item, item)?;
                (page, page)
            }
        };
        for page in [first, last] {
            if page == 0 || page > page_count {
                return Err(PageSpecError(format!("there is no page {page}: {valid}")));
            }
        }
        if first > last {
            return Err(PageSpecError(format!(
                "`{item}` goes backwards; write {last}-{first} ({valid})"
            )));
        }
        pages.extend(first..=last);
    }
    Ok(pages.into_iter().collect())
}

/// `[3, 4, 5, 6, 7, 10]` → "3-7, 10".
pub fn describe_pages(pages: &[u32]) -> String {
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for &page in pages {
        match runs.last_mut() {
            Some((_, last)) if *last + 1 == page => *last = page,
            _ => runs.push((page, page)),
        }
    }
    runs.iter()
        .map(|&(first, last)| {
            if first == last {
                first.to_string()
            } else {
                format!("{first}-{last}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug, thiserror::Error)]
pub enum PdfPagesError {
    #[error("cannot read this PDF: {0}")]
    Unreadable(String),
    #[error("cannot write the extracted pages: {0}")]
    Write(String),
}

/// A PDF loaded to extract some of its pages.
pub struct PdfPages {
    doc: Document,
}

impl PdfPages {
    pub fn load(pdf: &[u8]) -> Result<Self, PdfPagesError> {
        let doc =
            Document::load_mem(pdf).map_err(|err| PdfPagesError::Unreadable(err.to_string()))?;
        Ok(Self { doc })
    }

    pub fn page_count(&self) -> u32 {
        u32::try_from(self.doc.get_pages().len()).unwrap_or(u32::MAX)
    }

    /// A PDF with only the pages `keep` (1-based, ascending), in that order.
    pub fn extract(mut self, keep: &[u32]) -> Result<Vec<u8>, PdfPagesError> {
        let doc = &mut self.doc;
        let pages = doc.get_pages();
        let keep: BTreeSet<u32> = keep.iter().copied().collect();
        let kept: HashSet<ObjectId> = keep.iter().filter_map(|n| pages.get(n)).copied().collect();
        let removed: Vec<u32> = pages
            .keys()
            .filter(|n| !keep.contains(n))
            .copied()
            .collect();

        let names = keep_named_destinations(doc, &kept);
        let kept_pages: Vec<ObjectId> = keep.iter().filter_map(|n| pages.get(n)).copied().collect();
        for page in kept_pages {
            drop_dead_links(doc, page, &kept, &names);
        }
        clean_catalog(doc, &kept);

        doc.delete_pages(&removed);
        doc.prune_objects();
        let mut out = Vec::new();
        doc.save_to(&mut out)
            .map_err(|err| PdfPagesError::Write(err.to_string()))?;
        Ok(out)
    }
}

/// The page a destination points to: `[page …]`, or a dictionary holding one under `/D`.
fn destination_page(doc: &Document, destination: &Object) -> Option<ObjectId> {
    match doc.dereference(destination).ok()?.1 {
        Object::Array(items) => match items.first()? {
            Object::Reference(id) => Some(*id),
            _ => None,
        },
        Object::Dictionary(dict) => destination_page(doc, dict.get(b"D").ok()?),
        _ => None,
    }
}

fn dictionary<'a>(doc: &'a Document, object: &'a Object) -> Option<&'a Dictionary> {
    match doc.dereference(object).ok()?.1 {
        Object::Dictionary(dict) => Some(dict),
        _ => None,
    }
}

/// Collects the `(name, destination)` pairs of a name tree.
fn name_tree_entries(doc: &Document, node: &Object, depth: u32, out: &mut Vec<(Vec<u8>, Object)>) {
    let Some(node) = dictionary(doc, node) else {
        return;
    };
    if let Ok(Object::Array(items)) = node.get(b"Names").and_then(|n| Ok(doc.dereference(n)?.1)) {
        for [key, destination] in items.as_chunks::<2>().0 {
            if let Object::String(name, _) = key {
                out.push((name.clone(), destination.clone()));
            }
        }
    }
    if depth < 32
        && let Ok(Object::Array(kids)) = node.get(b"Kids").and_then(|k| Ok(doc.dereference(k)?.1))
    {
        for kid in kids {
            name_tree_entries(doc, kid, depth + 1, out);
        }
    }
}

/// Keeps the named destinations that point to a kept page; returns their names. The name tree
/// (`/Names /Dests`) is rebuilt as a single node; the older `/Dests` dictionary is filtered.
fn keep_named_destinations(doc: &mut Document, kept: &HashSet<ObjectId>) -> HashSet<Vec<u8>> {
    let mut valid = HashSet::new();
    let Ok(catalog) = doc.catalog() else {
        return valid;
    };
    let names_dict = catalog.get(b"Names").ok().cloned();
    let old_dests = catalog.get(b"Dests").ok().cloned();

    if let Some(names_object) = names_dict
        && let Some(tree) =
            dictionary(doc, &names_object).and_then(|d| d.get(b"Dests").ok().cloned())
    {
        let mut entries = Vec::new();
        name_tree_entries(doc, &tree, 0, &mut entries);
        let mut entries: Vec<(Vec<u8>, Object)> = entries
            .into_iter()
            .filter(|(_, destination)| {
                destination_page(doc, destination).is_some_and(|page| kept.contains(&page))
            })
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries.dedup_by(|a, b| a.0 == b.0);
        let mut flat = Vec::with_capacity(entries.len() * 2);
        for (name, destination) in entries {
            valid.insert(name.clone());
            flat.push(Object::string_literal(name));
            flat.push(destination);
        }
        let node = doc.add_object(Dictionary::from_iter([("Names", Object::Array(flat))]));
        match names_object {
            Object::Reference(id) => {
                if let Ok(names) = doc.get_dictionary_mut(id) {
                    names.set("Dests", Object::Reference(node));
                }
            }
            Object::Dictionary(_) => {
                if let Ok(catalog) = doc.catalog_mut()
                    && let Ok(Object::Dictionary(names)) = catalog.get_mut(b"Names")
                {
                    names.set("Dests", Object::Reference(node));
                }
            }
            _ => {}
        }
    }

    if let Some(dests_object) = old_dests
        && let Some(dests) = dictionary(doc, &dests_object)
    {
        let filtered: Dictionary = dests
            .iter()
            .filter(|(_, destination)| {
                destination_page(doc, destination).is_some_and(|page| kept.contains(&page))
            })
            .map(|(name, destination)| (name.clone(), destination.clone()))
            .collect();
        for name in filtered.iter().map(|(name, _)| name.clone()) {
            valid.insert(name);
        }
        if let Ok(catalog) = doc.catalog_mut() {
            catalog.set("Dests", Object::Dictionary(filtered));
        }
    }
    valid
}

/// Whether a link annotation still leads somewhere once the other pages are gone. Links to
/// web pages, other files and named actions are kept.
fn link_survives(
    doc: &Document,
    annotation: &Dictionary,
    kept: &HashSet<ObjectId>,
    names: &HashSet<Vec<u8>>,
) -> bool {
    let is_link = matches!(annotation.get(b"Subtype"), Ok(Object::Name(name)) if name == b"Link");
    if !is_link {
        return true;
    }
    let target = if let Ok(destination) = annotation.get(b"Dest") {
        destination.clone()
    } else if let Some(action) = annotation.get(b"A").ok().and_then(|a| dictionary(doc, a)) {
        let is_goto = matches!(action.get(b"S"), Ok(Object::Name(kind)) if kind == b"GoTo");
        match action.get(b"D") {
            Ok(destination) if is_goto => destination.clone(),
            _ => return true,
        }
    } else {
        return true;
    };
    match doc.dereference(&target).map(|(_, object)| object) {
        Ok(Object::String(name, _) | Object::Name(name)) => names.contains(name),
        Ok(_) => destination_page(doc, &target).is_some_and(|page| kept.contains(&page)),
        Err(_) => false,
    }
}

fn drop_dead_links(
    doc: &mut Document,
    page: ObjectId,
    kept: &HashSet<ObjectId>,
    names: &HashSet<Vec<u8>>,
) {
    let Ok(page_dict) = doc.get_dictionary(page) else {
        return;
    };
    let Ok(annots_object) = page_dict.get(b"Annots").cloned() else {
        return;
    };
    let Ok((array_id, Object::Array(annotations))) = doc.dereference(&annots_object) else {
        return;
    };
    let survivors: Vec<Object> = annotations
        .iter()
        .filter(|annotation| {
            dictionary(doc, annotation).is_none_or(|dict| link_survives(doc, dict, kept, names))
        })
        .cloned()
        .collect();
    if survivors.len() == annotations.len() {
        return;
    }
    match array_id {
        Some(id) => {
            if let Ok(object) = doc.get_object_mut(id) {
                *object = Object::Array(survivors);
            }
        }
        None => {
            if let Ok(page_dict) = doc.get_dictionary_mut(page) {
                page_dict.set("Annots", Object::Array(survivors));
            }
        }
    }
}

/// Drops what describes the whole document and would point at missing pages: bookmarks, page
/// labels, the tag tree, forms, article threads, and an opening view on a dropped page.
fn clean_catalog(doc: &mut Document, kept: &HashSet<ObjectId>) {
    let opens_elsewhere = doc
        .catalog()
        .ok()
        .and_then(|catalog| catalog.get(b"OpenAction").ok())
        .and_then(|action| destination_page(doc, action))
        .is_some_and(|page| !kept.contains(&page));
    let Ok(catalog) = doc.catalog_mut() else {
        return;
    };
    for key in [
        b"Outlines".as_slice(),
        b"PageLabels",
        b"StructTreeRoot",
        b"MarkInfo",
        b"AcroForm",
        b"Threads",
    ] {
        catalog.remove(key);
    }
    if matches!(catalog.get(b"PageMode"), Ok(Object::Name(mode)) if mode == b"UseOutlines") {
        catalog.remove(b"PageMode");
    }
    if opens_elsewhere {
        catalog.remove(b"OpenAction");
    }
}

#[cfg(test)]
mod tests {
    use lopdf::{Stream, dictionary};

    use super::*;

    #[test]
    fn page_lists() {
        let ok = |spec: &str| parse_pages(spec, 12).unwrap();
        assert_eq!(ok("4"), [4]);
        assert_eq!(ok("3-7"), [3, 4, 5, 6, 7]);
        assert_eq!(ok("10, 3-7"), [3, 4, 5, 6, 7, 10]);
        assert_eq!(ok("11-"), [11, 12]);
        assert_eq!(ok("  2 ,2, 1-2 "), [1, 2]);
        assert_eq!(ok("5 - 6"), [5, 6]);
    }

    #[test]
    fn bad_page_lists_name_the_valid_range() {
        for spec in ["", "0", "13", "7-3", "3-x", ",", "--", "1,,2", "-3", "5-20"] {
            let err = parse_pages(spec, 12).unwrap_err().to_string();
            assert!(err.contains("1–12"), "{spec:?}: {err}");
        }
        assert!(
            parse_pages("7-3", 12)
                .unwrap_err()
                .to_string()
                .contains("write 3-7")
        );
        assert!(parse_pages(&"1,".repeat(150), 12).is_err());
        assert!(parse_pages("1", 0).is_err());
    }

    #[test]
    fn describes_pages_as_ranges() {
        assert_eq!(describe_pages(&[3, 4, 5, 6, 7, 10]), "3-7, 10");
        assert_eq!(describe_pages(&[4]), "4");
        assert_eq!(describe_pages(&[11, 12]), "11-12");
        assert_eq!(describe_pages(&[1, 3, 5]), "1, 3, 5");
    }

    fn fit(page: ObjectId) -> Object {
        Object::Array(vec![Object::Reference(page), Object::Name(b"Fit".to_vec())])
    }

    fn link(doc: &mut Document, entries: Dictionary) -> Object {
        let mut annotation = dictionary! {
            "Type" => "Annot",
            "Subtype" => "Link",
            "Rect" => vec![0.into(), 0.into(), 10.into(), 10.into()],
        };
        for (key, value) in entries {
            annotation.set(key, value);
        }
        Object::Reference(doc.add_object(annotation))
    }

    /// Five pages with bookmarks, named destinations, page labels and links on page 2: to a
    /// dropped page by name, to page 4 by name, to page 5 directly, and to a web page.
    fn sample() -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let mut page_ids = Vec::new();
        for n in 1..=5 {
            let content = doc.add_object(Stream::new(
                dictionary! {},
                format!("BT 72 720 Td (page {n}) Tj ET").into_bytes(),
            ));
            page_ids.push(doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
                "Contents" => content,
            }));
        }
        let page = |n: usize| page_ids[n - 1];
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => page_ids.iter().map(|id| Object::Reference(*id)).collect::<Vec<_>>(),
                "Count" => 5,
            }),
        );
        let leaf = doc.add_object(dictionary! {
            "Names" => vec![
                Object::string_literal("sec.2"), fit(page(2)),
                Object::string_literal("sec.3"), fit(page(3)),
            ],
        });
        let other = doc.add_object(dictionary! {
            "Names" => vec![Object::string_literal("sec.4"), fit(page(4))],
        });
        let tree = doc.add_object(dictionary! {
            "Kids" => vec![Object::Reference(leaf), Object::Reference(other)],
        });
        let names = doc.add_object(dictionary! { "Dests" => tree });
        let outlines = doc.new_object_id();
        let item = doc.add_object(dictionary! {
            "Title" => Object::string_literal("Two"),
            "Parent" => outlines,
            "Dest" => fit(page(2)),
        });
        doc.objects.insert(
            outlines,
            Object::Dictionary(dictionary! {
                "Type" => "Outlines", "First" => item, "Last" => item, "Count" => 1,
            }),
        );
        let to_dropped = link(
            &mut doc,
            dictionary! { "Dest" => Object::string_literal("sec.3") },
        );
        let to_kept = link(
            &mut doc,
            dictionary! { "A" => dictionary! { "S" => "GoTo", "D" => Object::string_literal("sec.4") } },
        );
        let to_page_5 = link(&mut doc, dictionary! { "Dest" => fit(page(5)) });
        let to_web = link(
            &mut doc,
            dictionary! { "A" => dictionary! { "S" => "URI", "URI" => Object::string_literal("https://example.org") } },
        );
        doc.get_dictionary_mut(page(2))
            .unwrap()
            .set("Annots", vec![to_dropped, to_kept, to_page_5, to_web]);
        let catalog = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
            "Names" => names,
            "Outlines" => outlines,
            "PageMode" => "UseOutlines",
            "PageLabels" => dictionary! { "Nums" => vec![0.into(), dictionary! { "S" => "r" }.into()] },
            "OpenAction" => fit(page(5)),
        });
        doc.trailer.set("Root", catalog);
        let mut out = Vec::new();
        doc.save_to(&mut out).unwrap();
        out
    }

    /// Every reference in the document points to an object that exists.
    fn assert_no_dangling_references(doc: &Document) {
        fn walk(object: &Object, doc: &Document) {
            match object {
                Object::Reference(id) => {
                    assert!(doc.objects.contains_key(id), "dangling reference {id:?}")
                }
                Object::Array(items) => items.iter().for_each(|item| walk(item, doc)),
                Object::Dictionary(dict) => dict.iter().for_each(|(_, value)| walk(value, doc)),
                Object::Stream(stream) => {
                    stream.dict.iter().for_each(|(_, value)| walk(value, doc))
                }
                _ => {}
            }
        }
        for object in doc.objects.values() {
            walk(object, doc);
        }
        doc.trailer.iter().for_each(|(_, value)| walk(value, doc));
    }

    #[test]
    fn keeps_the_chosen_pages_and_drops_what_points_elsewhere() {
        let source = sample();
        let pages = PdfPages::load(&source).unwrap();
        assert_eq!(pages.page_count(), 5);
        let out = pages.extract(&[2, 4]).unwrap();

        let doc = Document::load_mem(&out).unwrap();
        let page_ids: Vec<ObjectId> = doc.get_pages().into_values().collect();
        assert_eq!(page_ids.len(), 2);
        let text = |id| String::from_utf8_lossy(&doc.get_page_content(id)).into_owned();
        assert!(text(page_ids[0]).contains("page 2"));
        assert!(text(page_ids[1]).contains("page 4"));

        let catalog = doc.catalog().unwrap();
        for key in [
            b"Outlines".as_slice(),
            b"PageLabels",
            b"OpenAction",
            b"PageMode",
        ] {
            assert!(!catalog.has(key), "{}", String::from_utf8_lossy(key));
        }

        let mut entries = Vec::new();
        let tree = dictionary(&doc, catalog.get(b"Names").unwrap())
            .unwrap()
            .get(b"Dests")
            .unwrap();
        name_tree_entries(&doc, tree, 0, &mut entries);
        let names: Vec<_> = entries
            .iter()
            .map(|(name, _)| String::from_utf8_lossy(name).into_owned())
            .collect();
        assert_eq!(names, ["sec.2", "sec.4"]);

        let annots = doc
            .get_dictionary(page_ids[0])
            .unwrap()
            .get(b"Annots")
            .unwrap();
        let Object::Array(annots) = doc.dereference(annots).unwrap().1 else {
            panic!("no annotations");
        };
        let kinds: Vec<String> = annots
            .iter()
            .map(|annotation| {
                let action = dictionary(&doc, annotation).unwrap().get(b"A").unwrap();
                let action = dictionary(&doc, action).unwrap();
                String::from_utf8_lossy(action.get(b"S").unwrap().as_name().unwrap()).into_owned()
            })
            .collect();
        assert_eq!(kinds, ["GoTo", "URI"]);

        assert_no_dangling_references(&doc);
    }

    #[test]
    fn a_broken_file_is_an_error() {
        assert!(matches!(
            PdfPages::load(b"%PDF-1.5 not really"),
            Err(PdfPagesError::Unreadable(_))
        ));
    }

    /// A real hyperref document from the TeX Live container: `cargo test -- --ignored`.
    #[test]
    #[ignore = "needs Docker and the texlive/texlive image"]
    fn extracts_a_page_of_a_hyperref_document() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("main.tex"),
            "\\documentclass{article}\n\\usepackage{hyperref}\n\\begin{document}\n\
             \\tableofcontents\\newpage\n\
             \\section{One}\\label{one} See \\ref{two} and \\url{https://example.org}.\\newpage\n\
             \\section{Two}\\label{two} Back to \\ref{one}.\\newpage\n\
             \\section{Three} End.\n\\end{document}\n",
        )
        .unwrap();
        let status = std::process::Command::new("docker")
            .args(["run", "--rm", "--network", "none", "-e", "HOME=/tmp", "-u"])
            .arg(format!("{}:{}", nix_like_id("-u"), nix_like_id("-g")))
            .arg("-v")
            .arg(format!("{}:/work", dir.path().display()))
            .args([
                "-w",
                "/work",
                "texlive/texlive:latest",
                "latexmk",
                "-pdf",
                "-interaction=nonstopmode",
                "main.tex",
            ])
            .status()
            .unwrap();
        assert!(status.success());
        let pdf = std::fs::read(dir.path().join("main.pdf")).unwrap();
        let pages = PdfPages::load(&pdf).unwrap();
        assert_eq!(pages.page_count(), 4);
        let out = pages.extract(&[2]).unwrap();
        let doc = Document::load_mem(&out).unwrap();
        assert_eq!(doc.get_pages().len(), 1);
        assert!(!doc.catalog().unwrap().has(b"Outlines"));
        assert_no_dangling_references(&doc);
    }

    /// The current user's or group's id, for running the container as them.
    fn nix_like_id(flag: &str) -> String {
        let output = std::process::Command::new("id").arg(flag).output().unwrap();
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }
}
