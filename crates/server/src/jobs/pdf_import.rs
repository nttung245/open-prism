//! Importing a PDF as a LaTeX project (`pdf-import` spec): the Agent reads the image of every
//! page, first to write a preamble that looks like the document, then to copy each page; the
//! pictures are cut out of the page renders; the pages are joined, compiled and, where they do
//! not compile, fixed by the Agent; the result becomes a new project.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use openprism_ai::llm::{ImageRef, LlmRequest, Message, ReasoningLevel};
use openprism_ai::patch::{self, Hunk};
use openprism_ai::search_replace;
use openprism_core::latexlog::{Diagnostic, Severity};
use openprism_core::pdf_render::{PdfDocument, Region, TextLayer};
use serde::{Deserialize, Serialize};

use super::{AiAnswer, JobContext, JobError, JobKind};
use crate::settings::ModelRef;

pub const KIND: &str = "pdfImport";
/// Resolution of the page images the model reads.
const PAGE_DPI: f32 = 150.0;
/// Resolution of the pictures cut out of the pages.
const FIGURE_DPI: f32 = 250.0;
/// Added on every side of a picture's box, as a fraction of the page.
const FIGURE_MARGIN: f64 = 0.03;
/// Rounds of fixing compile errors.
const FIX_ROUNDS: usize = 2;
/// Compile errors shown to the model in one round.
const ERRORS_PER_ROUND: usize = 10;
/// Pages copied at once (as many as the AI gate lets through per provider).
const PAGES_AT_ONCE: usize = 3;
/// Failed attempts of the preamble request that are retried (the scheduler retries steps only).
const PLAN_RETRIES: u32 = 2;

/// The packages the preamble may load; others are dropped.
pub const ALLOWED_PACKAGES: &[&str] = &[
    "amsmath",
    "amssymb",
    "amsthm",
    "booktabs",
    "tabularx",
    "array",
    "multirow",
    "multicol",
    "graphicx",
    "xcolor",
    "hyperref",
    "geometry",
    "fontspec",
    "newtxtext",
    "newtxmath",
    "lmodern",
    "titlesec",
    "enumitem",
    "fancyhdr",
    "parskip",
    "setspace",
    "ragged2e",
    "microtype",
    "caption",
    "float",
    "wrapfig",
    "fontawesome5",
    "inputenc",
    "fontenc",
    "babel",
];

/// What a PDF import job needs, saved with the job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfImportParams {
    /// The new project's name.
    pub name: String,
    /// The uploaded file's name.
    pub file_name: String,
    /// Pages to import, 1-based and ascending.
    pub pages: Vec<u32>,
    /// The Agent's model when the import started.
    pub model: ModelRef,
}

pub struct PdfImport;

impl JobKind for PdfImport {
    fn name(&self) -> &'static str {
        KIND
    }

    fn step_concurrency(&self) -> usize {
        PAGES_AT_ONCE
    }

    fn plan<'a>(&'a self, job: &'a JobContext) -> BoxFuture<'a, Result<u32, JobError>> {
        async move {
            let params = params(job)?;
            let source = source(job).await?;
            let first = *params
                .pages
                .first()
                .ok_or_else(|| JobError::Fatal("no page to import".into()))?;
            let count = {
                let source = source.clone();
                job.cpu(move || PdfDocument::load(source.to_vec()).map(|doc| doc.page_count()))
                    .await?
                    .map_err(|err| JobError::Fatal(err.to_string()))?
            };
            if let Some(missing) = params.pages.iter().find(|page| **page > count) {
                return Err(JobError::Fatal(format!(
                    "this PDF has no page {missing} (it has {count})"
                )));
            }
            job.set_message(&format!("Reading the layout of page {first}"))
                .await;
            let page = read_page(job, &source, first).await?;
            let mut attempt = 0;
            let answer = loop {
                let answer = job
                    .ai_request(&params.model, preamble_request(job, &page))
                    .await
                    .and_then(|answer| non_empty(answer, "the preamble"));
                match answer {
                    Err(JobError::Retryable(_)) if attempt < PLAN_RETRIES => attempt += 1,
                    other => break other?,
                }
            };
            tracing::info!(
                job = %job.id,
                page = first,
                input_tokens = answer.usage.input_tokens,
                output_tokens = answer.usage.output_tokens,
                "imported preamble written"
            );
            let (preamble, dropped) = clean_preamble(&answer.text);
            if !dropped.is_empty() {
                tracing::info!(job = %job.id, ?dropped, "packages left out of the imported preamble");
            }
            write_atomic(&job.dir.join("preamble.tex"), preamble.as_bytes()).await?;
            u32::try_from(params.pages.len()).map_err(|_| JobError::Fatal("too many pages".into()))
        }
        .boxed()
    }

    fn run_step<'a>(
        &'a self,
        job: &'a JobContext,
        step: u32,
    ) -> BoxFuture<'a, Result<(), JobError>> {
        async move {
            let params = params(job)?;
            let index = usize::try_from(step).unwrap_or(usize::MAX);
            let page_number = *params
                .pages
                .get(index)
                .ok_or_else(|| JobError::Fatal(format!("there is no step {}", step + 1)))?;
            let source = source(job).await?;
            let preamble = tokio::fs::read_to_string(job.dir.join("preamble.tex"))
                .await
                .map_err(|err| JobError::Fatal(format!("the preamble is missing: {err}")))?;
            job.set_message(&format!("Reading page {page_number}"))
                .await;
            let page = read_page(job, &source, page_number).await?;
            let previous = match index.checked_sub(1).and_then(|i| params.pages.get(i)) {
                Some(&before) => text_of(job, &source, before).await?,
                None => None,
            };
            let answer = job
                .ai_request(
                    &params.model,
                    page_request(job, &preamble, &page, previous.as_deref()),
                )
                .await;
            let answer = non_empty(answer?, &format!("page {page_number}"))?;
            tracing::info!(
                job = %job.id,
                page = page_number,
                input_tokens = answer.usage.input_tokens,
                output_tokens = answer.usage.output_tokens,
                "imported page copied"
            );
            let body = clean_body(&answer.text);
            let body = cut_out_figures(job, &source, page_number, &body).await?;
            write_atomic(&page_file(job, step), body.as_bytes()).await
        }
        .boxed()
    }

    fn finish<'a>(
        &'a self,
        job: &'a JobContext,
    ) -> BoxFuture<'a, Result<Option<String>, JobError>> {
        async move {
            let params = params(job)?;
            job.set_message("Putting the pages together").await;
            let preamble = tokio::fs::read_to_string(job.dir.join("preamble.tex"))
                .await
                .map_err(|err| JobError::Fatal(format!("the preamble is missing: {err}")))?;
            let mut pages = Vec::with_capacity(params.pages.len());
            for (step, number) in params.pages.iter().enumerate() {
                let step = u32::try_from(step).unwrap_or(u32::MAX);
                let body = tokio::fs::read_to_string(page_file(job, step))
                    .await
                    .map_err(|err| JobError::Fatal(format!("page {number} is missing: {err}")))?;
                pages.push((*number, body));
            }
            let out = job.dir.join("out");
            tokio::fs::create_dir_all(&out).await.map_err(fatal_io)?;
            let main = out.join("main.tex");
            write_atomic(&main, assemble(&preamble, &pages).as_bytes()).await?;
            copy_figures(&job.dir.join("figures"), &out.join("figures")).await?;
            fix_compile_errors(job, &params.model, &out).await?;

            job.set_message("Creating the project").await;
            let from = out.clone();
            let project = crate::projects::create_from_dir(
                &job.state,
                &params.name,
                "Imported from PDF",
                move |dest| {
                    std::fs::copy(from.join("main.tex"), dest.join("main.tex"))
                        .map_err(crate::error::ApiError::internal)?;
                    copy_dir_files(&from.join("figures"), &dest.join("figures"))
                        .map_err(crate::error::ApiError::internal)?;
                    Ok(Some("main.tex".to_owned()))
                },
            )
            .await
            .map_err(|err| JobError::Fatal(format!("cannot create the project: {err}")))?;
            if let Err(err) = job.state.compiler.start(&job.state, &project) {
                tracing::warn!(project = %project.id, %err, "cannot compile the imported project");
            }
            Ok(Some(project.id))
        }
        .boxed()
    }
}

/// An empty answer (often a reasoning model that spent its budget thinking) is tried again.
fn non_empty(answer: AiAnswer, what: &str) -> Result<AiAnswer, JobError> {
    if strip_fences(&answer.text).is_empty() {
        return Err(JobError::Retryable(format!(
            "the model gave an empty answer for {what} (it may have spent its tokens thinking)"
        )));
    }
    Ok(answer)
}

fn params(job: &JobContext) -> Result<PdfImportParams, JobError> {
    serde_json::from_value(job.params.clone())
        .map_err(|err| JobError::Fatal(format!("this import's settings are broken: {err}")))
}

async fn source(job: &JobContext) -> Result<Arc<Vec<u8>>, JobError> {
    tokio::fs::read(job.dir.join("source.pdf"))
        .await
        .map(Arc::new)
        .map_err(|err| JobError::Fatal(format!("the PDF is missing: {err}")))
}

fn page_file(job: &JobContext, step: u32) -> PathBuf {
    job.dir.join("pages").join(format!("{:04}.tex", step + 1))
}

/// A page's image and text layer.
struct Page {
    number: u32,
    png: Vec<u8>,
    text: Option<String>,
}

async fn read_page(job: &JobContext, source: &Arc<Vec<u8>>, number: u32) -> Result<Page, JobError> {
    let source = source.clone();
    job.cpu(move || {
        let png = PdfDocument::load(source.to_vec())?
            .render_page(number, PAGE_DPI)?
            .png;
        let text = TextLayer::load(&source)
            .ok()
            .and_then(|layer| layer.page_text(number));
        Ok::<_, openprism_core::pdf_render::RenderError>(Page { number, png, text })
    })
    .await?
    .map_err(|err| JobError::Fatal(err.to_string()))
}

async fn text_of(
    job: &JobContext,
    source: &Arc<Vec<u8>>,
    number: u32,
) -> Result<Option<String>, JobError> {
    let source = source.clone();
    job.cpu(move || {
        TextLayer::load(&source)
            .ok()
            .and_then(|layer| layer.page_text(number))
    })
    .await
}

fn image(page: &Page) -> ImageRef {
    ImageRef {
        id: format!("page-{}", page.number),
        media_type: "image/png".into(),
        page: Some(page.number),
        source: None,
        data: Some(
            base64::engine::general_purpose::STANDARD
                .encode(&page.png)
                .into(),
        ),
    }
}

/// Copying a page needs no thinking, and a reasoning model can spend its whole budget thinking and
/// answer nothing; so the preamble and the pages are asked with reasoning off (a model that only
/// works with it gets the nearest level it takes). Fixing compile errors keeps the Agent's level.
const COPY_REASONING: Option<ReasoningLevel> = Some(ReasoningLevel::Off);

fn request(
    job: &JobContext,
    system: String,
    text: String,
    images: Vec<ImageRef>,
    reasoning: Option<ReasoningLevel>,
    max_tokens: u32,
) -> LlmRequest {
    LlmRequest {
        system,
        messages: vec![Message::User { text, images }],
        tools: Vec::new(),
        max_tokens: Some(max_tokens),
        reasoning,
        // One conversation per import, for gateways that route by session (OpenCode Go).
        session_id: Some(format!("open-prism-import-{}", job.id)),
    }
}

fn text_layer_note(text: &Option<String>) -> String {
    match text {
        Some(text) => format!(
            "\n\nThe PDF's text layer for this page follows. It may be incomplete, out of order \
             and has no math: use it to check words and spelling, and trust the image for \
             everything else.\n<text-layer>\n{text}\n</text-layer>"
        ),
        None => String::new(),
    }
}

pub(crate) const PREAMBLE_PROMPT: &str = r"You look at the first page of a PDF and write the LaTeX preamble that typesets a document looking like it.

Write only the preamble: from \documentclass up to, but not including, \begin{document}.
- \documentclass with the paper size (letterpaper, a4paper, ...) and base font size you see; article, or report when the page shows chapters.
- geometry with the margins you estimate.
- A font close to the printed one, from TeX Live: newtxtext and newtxmath for Times-like text, lmodern for Computer Modern; when the text has Vietnamese or other letters beyond Latin-1, fontspec with \setmainfont{TeX Gyre Termes} (Times-like) or \setmainfont{TeX Gyre Heros} (Helvetica-like) instead.
- titlesec so headings look like the page: size, weight, capitals, rules under them, spacing. Headings without numbers on the page get none (\setcounter{secnumdepth}{0}).
- enumitem for the list style, xcolor for colours, hyperref with hidelinks, \pagestyle{empty} when the page shows no page number.
- Spacing as tight as the page: no parskip package unless the page leaves blank space between paragraphs; a rule under a heading is [\titlerule] or [\titlerule[0.6pt]] in \titleformat, never a \rule there, which starts a new line with extra space.
- It must compile. Known traps: with newtxmath do not load amssymb or amsfonts (newtxmath has their symbols); with fontspec do not load fontenc, inputenc or newtxtext; in \titleformat put \MakeUppercase in the last argument, as in \titleformat{\section}{\large\bfseries}{}{0em}{\MakeUppercase}[\titlerule], never in the format argument.
- You may define short macros the pages can reuse (for example an entry with a title on the left and a date on the right); put a % comment above each saying how to use it.
Use only these packages: ";

pub(crate) const PAGE_PROMPT: &str = r"You turn one page of a PDF into LaTeX. You see the page as an image. The document's preamble is given: it was written from the first page so the document looks like the PDF.

Write only the LaTeX body of this page: no preamble, no \usepackage, no \begin{document} or \end{document}.
- Keep the text exactly as printed, in its language, with every diacritic. Do not translate, summarise, fix or complete it. Never leave out text you see in the image.
- Keep the layout of the page: centred lines in a center environment, text pushed to the right with \hfill, tables with the rules they have (booktabs or \hline; tabularx for full-width tables), columns with multicols or minipage, the same emphasis (bold, italic, capitals) and list style. Use the commands and macros the preamble defines. Headings without a number on the page get none.
- Math: inline $...$, displayed equation or align. Keep equation numbers with \tag{n}.
- It must compile. Known traps: in a p{...} column, centre with >{\centering\arraybackslash}p{...} in the column spec, never a bare \centering in a cell; escape & % $ # _ { } in text; subscripts and superscripts only in math.
- Pictures (diagrams, plots, photos; not tables, not equations): write
  \begin{figure}[h]\centering\includegraphics[width=<w>\linewidth]{box:<left>,<top>,<right>,<bottom>}\caption{<caption as printed>}\end{figure}
  where the four box numbers are fractions (0 to 1) of the page width and height measured from the top left corner, covering the picture only (not its caption), and <w> is the fraction of the text width it takes. A picture without a caption gets the \includegraphics alone.
- Citations stay as printed text, for example [12] or (Devlin et al., 2019). Never write \cite, \ref, \label or \bibliography. Reference lists are plain paragraphs or an itemize.
- Footnotes: \footnote{...} at the place of their mark.
- Leave out running headers, footers and page numbers.
- If the page starts in the middle of a sentence or paragraph, start with that text as is; if it ends in the middle of one, stop there.
Answer with the LaTeX only, no explanation and no code fences.";

fn preamble_request(job: &JobContext, page: &Page) -> LlmRequest {
    let system = format!(
        "{PREAMBLE_PROMPT}{}.\nAnswer with the LaTeX only, no explanation and no code fences.",
        ALLOWED_PACKAGES.join(", ")
    );
    let text = format!(
        "This is page {} of the PDF.{}",
        page.number,
        text_layer_note(&page.text)
    );
    request(job, system, text, vec![image(page)], COPY_REASONING, 4000)
}

fn page_request(
    job: &JobContext,
    preamble: &str,
    page: &Page,
    previous: Option<&str>,
) -> LlmRequest {
    let mut text = format!(
        "The preamble:\n<preamble>\n{preamble}\n</preamble>\n\nPage {} of the PDF.",
        page.number
    );
    if let Some(previous) = previous {
        let tail: Vec<&str> = previous.lines().filter(|l| !l.trim().is_empty()).collect();
        let tail = tail[tail.len().saturating_sub(3)..].join("\n");
        text.push_str(&format!("\n\nThe previous page ended with:\n{tail}"));
    }
    text.push_str(&text_layer_note(&page.text));
    request(
        job,
        PAGE_PROMPT.to_owned(),
        text,
        vec![image(page)],
        COPY_REASONING,
        8000,
    )
}

/// Removes a code fence around the whole answer.
pub(crate) fn strip_fences(text: &str) -> &str {
    let text = text.trim();
    let Some(rest) = text.strip_prefix("```") else {
        return text;
    };
    let rest = rest.split_once('\n').map_or("", |(_, body)| body);
    rest.strip_suffix("```").unwrap_or(rest).trim()
}

const FALLBACK_PREAMBLE: &str = "\\documentclass[11pt]{article}\n\\usepackage{amsmath,amssymb}\n";

/// The preamble without anything after `\begin{document}` and without packages outside the
/// allowed list (returned); `graphicx` is always loaded, for the pictures.
pub(crate) fn clean_preamble(text: &str) -> (String, Vec<String>) {
    let text = strip_fences(text);
    let text = text.split("\\begin{document}").next().unwrap_or_default();
    let text = if text.contains("\\documentclass") {
        text
    } else {
        FALLBACK_PREAMBLE
    };
    let mut dropped = Vec::new();
    let mut lines = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        let command = ["\\usepackage", "\\RequirePackage"]
            .into_iter()
            .find(|command| trimmed.starts_with(command));
        let Some(command) = command else {
            lines.push(line.to_owned());
            continue;
        };
        let Some((options, names, rest)) = package_line(&trimmed[command.len()..]) else {
            dropped.push(trimmed.to_owned());
            continue;
        };
        let (kept, refused): (Vec<&str>, Vec<&str>) = names
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .partition(|name| ALLOWED_PACKAGES.contains(name));
        dropped.extend(refused.iter().map(|name| (*name).to_owned()));
        if !kept.is_empty() {
            lines.push(format!("{command}{options}{{{}}}{rest}", kept.join(",")));
        }
    }
    let mut preamble = lines.join("\n").trim_end().to_owned();
    // newtxmath defines amssymb's symbols: loading both stops with "\\Bbbk already defined".
    if uses_package(&preamble, "newtxmath") {
        let (kept, removed) = without_packages(&preamble, &["amssymb", "amsfonts"]);
        preamble = kept;
        dropped.extend(removed);
    }
    // pdfLaTeX's default OT1 encoding prints | < > as other glyphs (| becomes an em dash), and
    // copied text often has them: T1 unless the fonts come from fontspec.
    if !uses_package(&preamble, "fontspec") && !uses_package(&preamble, "fontenc") {
        preamble.push_str("\n\\usepackage[T1]{fontenc}");
    }
    if !uses_package(&preamble, "graphicx") {
        preamble.push_str("\n\\usepackage{graphicx}");
    }
    preamble.push('\n');
    (preamble, dropped)
}

/// `[options]{names}rest` after `\usepackage`.
fn package_line(text: &str) -> Option<(&str, &str, &str)> {
    let text = text.trim_start();
    let (options, text) = match text.strip_prefix('[') {
        Some(after) => {
            let end = after.find(']')? + 2;
            (&text[..end], text[end..].trim_start())
        }
        None => ("", text),
    };
    let names = text.strip_prefix('{')?;
    let end = names.find('}')?;
    Some((options, &names[..end], &names[end + 1..]))
}

/// The preamble without the packages `names`, and the ones it removed.
fn without_packages(preamble: &str, names: &[&str]) -> (String, Vec<String>) {
    let mut removed = Vec::new();
    let lines: Vec<String> = preamble
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim_start();
            let Some(rest) = trimmed.strip_prefix("\\usepackage") else {
                return Some(line.to_owned());
            };
            let Some((options, packages, tail)) = package_line(rest) else {
                return Some(line.to_owned());
            };
            let (out, kept): (Vec<&str>, Vec<&str>) = packages
                .split(',')
                .map(str::trim)
                .partition(|name| names.contains(name));
            removed.extend(out.iter().map(|name| (*name).to_owned()));
            (!kept.is_empty()).then(|| format!("\\usepackage{options}{{{}}}{tail}", kept.join(",")))
        })
        .collect();
    (lines.join("\n"), removed)
}

fn uses_package(preamble: &str, name: &str) -> bool {
    preamble.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("\\usepackage")
            && package_line(&line["\\usepackage".len()..])
                .is_some_and(|(_, names, _)| names.split(',').any(|n| n.trim() == name))
    })
}

/// A page's body: no fence, no preamble or document lines, and citations, references and labels
/// turned into plain text (the prompt asks for that already; this is the safety net).
pub(crate) fn clean_body(text: &str) -> String {
    let text = strip_fences(text);
    let kept: Vec<&str> = text
        .lines()
        .filter(|line| {
            let line = line.trim_start();
            ![
                "\\documentclass",
                "\\usepackage",
                "\\begin{document}",
                "\\end{document}",
                "\\bibliographystyle",
                "\\bibliography{",
            ]
            .iter()
            .any(|prefix| line.starts_with(prefix))
        })
        .collect();
    let mut body = kept.join("\n");
    body = replace_commands(&body, &["label"], |_| String::new());
    body = replace_commands(
        &body,
        &["ref", "eqref", "autoref", "cref", "Cref", "pageref"],
        |_| "??".to_owned(),
    );
    body = replace_commands(
        &body,
        &[
            "cite",
            "citep",
            "citet",
            "citeauthor",
            "citeyear",
            "parencite",
            "textcite",
        ],
        |keys| {
            format!(
                "[{}]",
                keys.split(',')
                    .map(str::trim)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        },
    );
    body.trim_end().to_owned() + "\n"
}

/// Replaces `\name*[...]{arg}` for each name with `replace(arg)`.
fn replace_commands(text: &str, names: &[&str], replace: impl Fn(&str) -> String) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('\\') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let name_len = after
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(after.len());
        let name = &after[..name_len];
        if names.contains(&name)
            && let Some((arg, used)) = command_argument(&after[name_len..])
        {
            out.push_str(&replace(arg));
            rest = &after[name_len + used..];
            continue;
        }
        out.push('\\');
        rest = after;
    }
    out.push_str(rest);
    out
}

/// After a command's name: an optional `*`, optional `[...]` groups, then `{arg}`; returns the
/// argument and how many bytes all that took.
fn command_argument(text: &str) -> Option<(&str, usize)> {
    let mut used = usize::from(text.starts_with('*'));
    while text[used..].starts_with('[') {
        used += text[used..].find(']')? + 1;
    }
    let inner = text[used..].strip_prefix('{')?;
    let mut depth = 1;
    for (i, c) in inner.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((&inner[..i], used + 1 + i + 1));
                }
            }
            _ => {}
        }
    }
    None
}

/// A picture the model placed: `\includegraphics[...]{box:l,t,r,b}`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BoxedPicture {
    /// Byte range of the `box:...` argument in the body.
    pub span: std::ops::Range<usize>,
    pub region: Option<Region>,
}

pub(crate) fn boxed_pictures(body: &str) -> Vec<BoxedPicture> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(start) = body[from..].find("\\includegraphics") {
        let at = from + start + "\\includegraphics".len();
        from = at;
        let Some((arg, used)) = command_argument(&body[at..]) else {
            continue;
        };
        let Some(spec) = arg.trim().strip_prefix("box:") else {
            continue;
        };
        let numbers: Vec<f64> = spec
            .split(',')
            .filter_map(|n| n.trim().parse().ok())
            .collect();
        let region = (numbers.len() == 4)
            .then(|| Region {
                left: numbers[0],
                top: numbers[1],
                right: numbers[2],
                bottom: numbers[3],
            })
            .and_then(|region| region.checked_with_margin(FIGURE_MARGIN));
        let arg_start = at + used - 1 - arg.len();
        found.push(BoxedPicture {
            span: arg_start..arg_start + arg.len(),
            region,
        });
        from = at + used;
    }
    found
}

const PLACEHOLDER: &str =
    "\\fbox{\\parbox{0.8\\linewidth}{\\centering Figure (could not be cut out)}}";

/// Cuts every boxed picture of page `number` out into `figures/`, and points its
/// `\includegraphics` at the file; a box that is not a real area becomes a placeholder.
async fn cut_out_figures(
    job: &JobContext,
    source: &Arc<Vec<u8>>,
    number: u32,
    body: &str,
) -> Result<String, JobError> {
    let pictures = boxed_pictures(body);
    if pictures.is_empty() {
        return Ok(body.to_owned());
    }
    let figures = job.dir.join("figures");
    tokio::fs::create_dir_all(&figures)
        .await
        .map_err(fatal_io)?;
    let mut out = String::with_capacity(body.len());
    let mut cursor = 0;
    for (k, picture) in pictures.iter().enumerate() {
        let name = format!("figures/p{number}-{}.png", k + 1);
        let replacement = match picture.region {
            Some(region) => {
                let source = source.clone();
                let rendered = job
                    .cpu(move || {
                        PdfDocument::load(source.to_vec())?
                            .render_region(number, region, FIGURE_DPI)
                    })
                    .await?;
                match rendered {
                    Ok(image) => {
                        write_atomic(&job.dir.join(&name), &image.png).await?;
                        Some(name)
                    }
                    Err(err) => {
                        tracing::warn!(job = %job.id, %err, "cannot cut out a picture");
                        None
                    }
                }
            }
            None => None,
        };
        match replacement {
            Some(name) => {
                out.push_str(&body[cursor..picture.span.start]);
                out.push_str(&name);
                cursor = picture.span.end;
            }
            None => {
                // Replace the whole \includegraphics command with a placeholder.
                let command = body[..picture.span.start]
                    .rfind("\\includegraphics")
                    .unwrap_or(picture.span.start);
                out.push_str(&body[cursor..command.max(cursor)]);
                out.push_str(PLACEHOLDER);
                cursor = picture.span.end + 1; // the closing brace
            }
        }
    }
    out.push_str(&body[cursor.min(body.len())..]);
    Ok(out)
}

/// The preamble and the pages as one document. A page that ends in the middle of a sentence and
/// a next page that starts in lower case are one paragraph; otherwise a blank line separates
/// them.
pub(crate) fn assemble(preamble: &str, pages: &[(u32, String)]) -> String {
    let mut doc = format!("{}\n\\begin{{document}}\n", preamble.trim_end());
    let mut previous: Option<&str> = None;
    for (number, body) in pages {
        let body = body.trim_end();
        if let Some(before) = previous {
            let continues = before
                .chars()
                .last()
                .is_some_and(|c| c.is_alphanumeric() || matches!(c, ',' | '-'))
                && body
                    .trim_start()
                    .chars()
                    .next()
                    .is_some_and(char::is_lowercase);
            doc.push_str(if continues { "%\n" } else { "\n\n" });
        }
        doc.push_str(&format!("% --- page {number} ---\n"));
        doc.push_str(body);
        previous = Some(body);
    }
    doc.push_str("\n\\end{document}\n");
    doc
}

/// Compiles `out/main.tex`; while it has errors, asks the Agent for SEARCH/REPLACE fixes, at
/// most `FIX_ROUNDS` times. Errors left are not fatal: the project shows them.
async fn fix_compile_errors(
    job: &JobContext,
    model: &ModelRef,
    out: &Path,
) -> Result<(), JobError> {
    let main = out.join("main.tex");
    let work = job.dir.join("work");
    // The previous round's blocks matched nothing: say so, so the model copies lines exactly.
    let mut missed = false;
    for round in 0..=FIX_ROUNDS {
        job.set_message(if round == 0 {
            "Compiling"
        } else {
            "Compiling again"
        })
        .await;
        let result = crate::compiler::compile_dir(&job.state, out, "main.tex", &work, &job.cancel)
            .await
            .map_err(|err| JobError::Fatal(err.to_string()))?;
        if job.cancel.is_cancelled() {
            return Err(JobError::Cancelled);
        }
        let errors: Vec<_> = result
            .diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .take(ERRORS_PER_ROUND)
            .collect();
        if errors.is_empty() || round == FIX_ROUNDS {
            if !errors.is_empty() {
                tracing::info!(job = %job.id, errors = errors.len(), "imported with compile errors left");
            }
            return Ok(());
        }
        tracing::info!(job = %job.id, round = round + 1, errors = errors.len(), "asking the Agent to fix compile errors");
        job.set_message(&format!("Fixing compile errors (round {})", round + 1))
            .await;
        let source = tokio::fs::read_to_string(&main).await.map_err(fatal_io)?;
        let report = fix_report(&source, &errors);
        let answer = job
            .ai_request(
                model,
                request(
                    job,
                    format!(
                        "You fix LaTeX compile errors in main.tex, a document rebuilt from a PDF. Change as \
                         little as you can and keep the text and the layout.\n\n{}",
                        search_replace::INSTRUCTIONS
                    ),
                    format!(
                        "main.tex does not compile.{}\n\n{report}",
                        if missed {
                            " Your previous SEARCH blocks did not match: copy the lines exactly as \
                             numbered below, without the line numbers."
                        } else {
                            ""
                        }
                    ),
                    Vec::new(),
                    model.reasoning,
                    16000,
                ),
            )
            .await?;
        let (fixed, applied) = apply_fixes(&source, &answer.text);
        tracing::info!(
            job = %job.id,
            round = round + 1,
            blocks = search_replace::parse(&answer.text).len(),
            applied,
            "fixes for compile errors"
        );
        missed = applied == 0;
        if !missed {
            write_atomic(&main, fixed.as_bytes()).await?;
        }
    }
    Ok(())
}

/// What the Agent sees to fix the errors: the numbered preamble (where most causes are, even of
/// errors reported later), then each error with the lines around it when it is in main.tex, or
/// the file it was reported in (a package) otherwise.
pub(crate) fn fix_report(source: &str, errors: &[&Diagnostic]) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let preamble_end = lines
        .iter()
        .position(|line| line.contains("\\begin{document}"))
        .unwrap_or(0);
    let numbered = |first: usize, last: usize, mark: usize| -> String {
        (first..=last.min(lines.len()))
            .map(|n| {
                format!(
                    "{n:>5}{} {}\n",
                    if n == mark { ">" } else { ":" },
                    lines[n - 1]
                )
            })
            .collect()
    };
    let mut report = format!(
        "The preamble (lines 1-{preamble_end}):\n{}\nThe errors (> marks the line):\n\n",
        numbered(1, preamble_end, 0)
    );
    for error in errors {
        report.push_str(&format!("Error: {}\n", error.message));
        let in_main = error
            .file
            .as_deref()
            .is_none_or(|file| file == "main.tex" || file.ends_with("/main.tex"));
        match (in_main, error.line) {
            (true, Some(line)) => {
                let line = usize::try_from(line).unwrap_or(0);
                report.push_str(&numbered(line.saturating_sub(3).max(1), line + 3, line));
            }
            (false, line) => report.push_str(&format!(
                "(reported in {}{}: the cause is usually in the preamble, such as two packages \
                 that clash)\n",
                error.file.as_deref().unwrap_or("another file"),
                line.map(|l| format!(", line {l}")).unwrap_or_default()
            )),
            (true, None) => {}
        }
        if !error.details.is_empty() {
            report.push_str(&format!(
                "{}\n",
                error.details.lines().take(3).collect::<Vec<_>>().join("\n")
            ));
        }
        report.push('\n');
    }
    report
}

/// Applies each SEARCH/REPLACE block that matches exactly one place; returns the text and how
/// many blocks were applied.
pub(crate) fn apply_fixes(source: &str, answer: &str) -> (String, usize) {
    let mut text = source.to_owned();
    let mut applied = 0;
    for block in search_replace::parse(answer) {
        let hunk = Hunk {
            old: block.hunk.old,
            new: block.hunk.new,
        };
        if hunk.old.is_empty() {
            continue;
        }
        if let Ok(changed) = patch::apply(&text, std::slice::from_ref(&hunk), &[true], "main.tex") {
            text = changed;
            applied += 1;
        }
    }
    (text, applied)
}

async fn copy_figures(from: &Path, to: &Path) -> Result<(), JobError> {
    let (from, to) = (from.to_path_buf(), to.to_path_buf());
    tokio::task::spawn_blocking(move || copy_dir_files(&from, &to))
        .await
        .map_err(|err| JobError::Fatal(err.to_string()))?
        .map_err(fatal_io)
}

/// Copies the files of `from` (not recursive) into `to`; a missing `from` copies nothing.
fn copy_dir_files(from: &Path, to: &Path) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(from) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    std::fs::create_dir_all(to)?;
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            std::fs::copy(entry.path(), to.join(entry.file_name()))?;
        }
    }
    Ok(())
}

/// Writes a file through a temporary file and a rename, so a step that stops half way leaves no
/// half-written result.
async fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), JobError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(fatal_io)?;
    }
    let temporary = path.with_extension("part");
    tokio::fs::write(&temporary, bytes)
        .await
        .map_err(fatal_io)?;
    tokio::fs::rename(&temporary, path).await.map_err(fatal_io)
}

fn fatal_io(err: std::io::Error) -> JobError {
    JobError::Fatal(format!("cannot write the import's files: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fences_around_the_answer_go() {
        assert_eq!(strip_fences("```latex\n\\section{A}\n```"), "\\section{A}");
        assert_eq!(strip_fences("```\nx\n```\n"), "x");
        assert_eq!(strip_fences("  plain  "), "plain");
    }

    #[test]
    fn the_preamble_keeps_allowed_packages_and_nothing_after_the_document_starts() {
        let (preamble, dropped) = clean_preamble(
            "```latex\n\\documentclass[11pt,letterpaper]{article}\n\\usepackage[margin=0.6in]{geometry}\n\
             \\usepackage{minted}\n\\usepackage{amsmath,tikz,booktabs}\n\\newcommand{\\entry}[2]{#1\\hfill #2}\n\
             \\begin{document}\nHello\n\\end{document}\n```",
        );
        assert!(
            preamble.starts_with("\\documentclass[11pt,letterpaper]{article}\n"),
            "{preamble}"
        );
        assert!(preamble.contains("\\usepackage[margin=0.6in]{geometry}"));
        assert!(
            preamble.contains("\\usepackage{amsmath,booktabs}"),
            "{preamble}"
        );
        assert!(preamble.contains("\\newcommand{\\entry}[2]{#1\\hfill #2}"));
        assert!(
            preamble.contains("\\usepackage{graphicx}"),
            "added for the pictures"
        );
        assert!(!preamble.contains("minted") && !preamble.contains("tikz"));
        assert!(!preamble.contains("begin{document}") && !preamble.contains("Hello"));
        assert_eq!(dropped, ["minted", "tikz"]);

        let (fallback, _) = clean_preamble("I cannot see the page.");
        assert!(fallback.starts_with("\\documentclass[11pt]{article}"));
        assert_eq!(fallback.matches("graphicx").count(), 1);
    }

    #[test]
    fn citations_references_and_labels_become_plain_text() {
        let body = clean_body(
            "```\n\\usepackage{x}\n\\begin{document}\n\\section{Intro}\\label{sec:intro}\n\
             As shown~\\cite{vaswani2017, devlin2019} and in Section~\\ref{sec:intro}, see \\citep[p.~3]{x}.\n\
             \\bibliographystyle{plain}\n\\end{document}\n```",
        );
        assert_eq!(
            body,
            "\\section{Intro}\nAs shown~[vaswani2017, devlin2019] and in Section~??, see [x].\n"
        );
        // Other commands, and \citation-like words, are untouched.
        assert_eq!(
            clean_body("\\textbf{bold} \\citeword"),
            "\\textbf{bold} \\citeword\n"
        );
    }

    #[test]
    fn finds_the_boxes_of_pictures() {
        let body = "a \\includegraphics[width=0.5\\linewidth]{box:0.1,0.2,0.5,0.6} b \
                    \\includegraphics{figure.png} c \\includegraphics{box:0.9,0.1,0.2,0.5}";
        let pictures = boxed_pictures(body);
        assert_eq!(pictures.len(), 2);
        assert_eq!(&body[pictures[0].span.clone()], "box:0.1,0.2,0.5,0.6");
        let region = pictures[0].region.unwrap();
        assert!((region.left - 0.07).abs() < 1e-9 && (region.bottom - 0.63).abs() < 1e-9);
        assert_eq!(&body[pictures[1].span.clone()], "box:0.9,0.1,0.2,0.5");
        assert!(pictures[1].region.is_none(), "left is right of right");
    }

    #[test]
    fn pages_join_into_one_paragraph_only_mid_sentence() {
        let doc = assemble(
            "\\documentclass{article}\n",
            &[
                (3, "Text that goes on\n".to_owned()),
                (4, "on the next page. End.\n".to_owned()),
                (5, "New paragraph.\n".to_owned()),
            ],
        );
        assert_eq!(
            doc,
            "\\documentclass{article}\n\\begin{document}\n% --- page 3 ---\nText that goes on%\n\
             % --- page 4 ---\non the next page. End.\n\n% --- page 5 ---\nNew paragraph.\n\\end{document}\n"
        );
    }

    #[test]
    fn only_fixes_that_match_once_are_applied() {
        let source = "a\n$x_1$ \\textbf{B}_{C}\nb\nb\n";
        let answer = "Fixed.\nmain.tex\n<<<<<<< SEARCH\n\\textbf{B}_{C}\n=======\n$\\textbf{B}_{C}$\n>>>>>>> REPLACE\n\
                      main.tex\n<<<<<<< SEARCH\nb\n=======\nB\n>>>>>>> REPLACE\n\
                      main.tex\n<<<<<<< SEARCH\nnot there\n=======\nx\n>>>>>>> REPLACE\n";
        let (fixed, applied) = apply_fixes(source, answer);
        assert_eq!(
            applied, 1,
            "\"b\" is twice in the file, \"not there\" nowhere"
        );
        assert_eq!(fixed, "a\n$x_1$ $\\textbf{B}_{C}$\nb\nb\n");
    }

    #[test]
    fn newtxmath_and_amssymb_never_load_together() {
        let (preamble, dropped) = clean_preamble(
            "\\documentclass{article}\n\\usepackage{newtxtext}\n\\usepackage{newtxmath}\n\\usepackage{amsmath,amssymb}\n\\usepackage{amsfonts}\n",
        );
        assert!(preamble.contains("\\usepackage{amsmath}\n"), "{preamble}");
        assert!(
            !preamble.contains("amssymb") && !preamble.contains("amsfonts"),
            "{preamble}"
        );
        // amsfonts is not on the list at all; amssymb goes because of newtxmath.
        let mut dropped = dropped;
        dropped.sort();
        assert_eq!(dropped, ["amsfonts", "amssymb"]);
        // Without newtxmath, amssymb stays.
        let (preamble, _) = clean_preamble("\\documentclass{article}\n\\usepackage{amssymb}\n");
        assert!(preamble.contains("\\usepackage{amssymb}"));
    }

    fn diagnostic(file: Option<&str>, line: Option<u32>, message: &str) -> Diagnostic {
        Diagnostic {
            severity: Severity::Error,
            source: openprism_core::latexlog::DiagnosticSource::Latex,
            file: file.map(str::to_owned),
            line,
            message: message.to_owned(),
            details: String::new(),
        }
    }

    #[test]
    fn the_fix_report_shows_the_preamble_and_the_right_lines() {
        let source = "\\documentclass{article}\n\\usepackage{titlesec}\n\\titleformat{\\section}{\\MakeUppercase}{}{0em}{}\n\
                      \\begin{document}\na\nb\n\\section{Skills}\nc\n\\end{document}\n";
        let in_body = diagnostic(Some("main.tex"), Some(7), "Undefined control sequence.");
        let in_package = diagnostic(
            Some("/usr/local/texlive/2026/texmf-dist/tex/latex/amsfonts/amssymb.sty"),
            Some(261),
            "LaTeX Error: Command `\\Bbbk' already defined.",
        );
        let report = fix_report(source, &[&in_body, &in_package]);
        assert!(
            report.starts_with("The preamble (lines 1-3):\n    1: \\documentclass{article}\n"),
            "{report}"
        );
        assert!(report.contains("    3: \\titleformat{\\section}{\\MakeUppercase}{}{0em}{}\n"));
        assert!(report.contains("    7> \\section{Skills}\n"), "{report}");
        assert!(report.contains("reported in /usr/local/texlive/2026/texmf-dist/tex/latex/amsfonts/amssymb.sty, line 261"));
        // Line 261 of main.tex does not exist and is not quoted.
        assert!(!report.contains("  261"), "{report}");
    }

    #[test]
    fn pdflatex_preambles_get_t1_so_bars_print_as_bars() {
        let (preamble, _) = clean_preamble("\\documentclass{article}\n\\usepackage{newtxtext}\n");
        assert!(preamble.contains("\\usepackage[T1]{fontenc}"), "{preamble}");
        let (preamble, _) = clean_preamble("\\documentclass{article}\n\\usepackage[T1]{fontenc}\n");
        assert_eq!(preamble.matches("fontenc").count(), 1);
        let (preamble, _) = clean_preamble(
            "\\documentclass{article}\n\\usepackage{fontspec}\n\\setmainfont{TeX Gyre Termes}\n",
        );
        assert!(
            !preamble.contains("fontenc"),
            "fontspec has its own encoding"
        );
    }
}
