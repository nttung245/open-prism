//! Renders pages of a PDF to PNG images with hayro (pure Rust), and reads their text layer with
//! lopdf, for importing a PDF as a LaTeX project.

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_interpret::util::TransformExt;
use hayro::hayro_syntax::Pdf;
use hayro::kurbo::Affine;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::vello_cpu::{Pixmap, RasterizerSettings, RenderContext, Resources, TargetInit};
use hayro::{RenderCache, RenderSettings, render_into};

/// Largest side of a rendered image: a pixmap is limited to 16 bits per side.
const MAX_SIDE: f64 = 16_000.0;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RenderError {
    #[error("cannot read this PDF: {0}")]
    Unreadable(String),
    #[error("this PDF has no page {page} (it has {count})")]
    NoSuchPage { page: u32, count: u32 },
    #[error("cannot render page {0}: the area to render is empty")]
    EmptyArea(u32),
    #[error("cannot write the image of page {page}: {reason}")]
    Encode { page: u32, reason: String },
}

/// Part of a page, each side a fraction (0–1) of the page width or height from its top left
/// corner.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Region {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl Region {
    pub const WHOLE: Self = Self {
        left: 0.0,
        top: 0.0,
        right: 1.0,
        bottom: 1.0,
    };

    /// The region grown by `margin` (a fraction of the page) on every side, kept on the page;
    /// `None` when the region is not a real area of the page.
    pub fn checked_with_margin(self, margin: f64) -> Option<Self> {
        let valid = [self.left, self.top, self.right, self.bottom]
            .iter()
            .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
            && self.left < self.right
            && self.top < self.bottom;
        valid.then(|| Self {
            left: (self.left - margin).max(0.0),
            top: (self.top - margin).max(0.0),
            right: (self.right + margin).min(1.0),
            bottom: (self.bottom + margin).min(1.0),
        })
    }
}

/// A rendered image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// A PDF loaded for rendering.
pub struct PdfDocument {
    pdf: Pdf,
}

impl PdfDocument {
    pub fn load(bytes: Vec<u8>) -> Result<Self, RenderError> {
        let pdf = Pdf::new(bytes).map_err(|err| RenderError::Unreadable(format!("{err:?}")))?;
        Ok(Self { pdf })
    }

    pub fn page_count(&self) -> u32 {
        u32::try_from(self.pdf.pages().len()).unwrap_or(u32::MAX)
    }

    /// Renders page `page` (1-based) at `dpi`.
    pub fn render_page(&self, page: u32, dpi: f32) -> Result<Rendered, RenderError> {
        self.render_region(page, Region::WHOLE, dpi)
    }

    /// Renders the part `region` of page `page` (1-based) at `dpi`, on white.
    pub fn render_region(
        &self,
        page: u32,
        region: Region,
        dpi: f32,
    ) -> Result<Rendered, RenderError> {
        let pages = self.pdf.pages();
        let count = self.page_count();
        let index = usize::try_from(page.wrapping_sub(1)).unwrap_or(usize::MAX);
        let Some(page_ref) = (page >= 1).then(|| pages.get(index)).flatten() else {
            return Err(RenderError::NoSuchPage { page, count });
        };
        let scale = f64::from(dpi) / 72.0;
        let (width, height) = page_ref.render_dimensions();
        let (width, height) = (f64::from(width), f64::from(height));
        let out_width = ((region.right - region.left) * width * scale).round();
        let out_height = ((region.bottom - region.top) * height * scale).round();
        if !(out_width >= 1.0 && out_height >= 1.0) {
            return Err(RenderError::EmptyArea(page));
        }
        // Keep within a pixmap's limits by lowering the resolution of huge requests.
        let shrink = (MAX_SIDE / out_width.max(out_height)).min(1.0);
        let (scale, out_width, out_height) = (
            scale * shrink,
            (out_width * shrink).max(1.0),
            (out_height * shrink).max(1.0),
        );

        // Page space (top left origin) → the region at the origin → pixels.
        let transform = Affine::scale(scale)
            * Affine::translate((-region.left * width, -region.top * height))
            * page_ref.initial_transform(true).to_kurbo();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (w, h) = (out_width as u16, out_height as u16);
        let mut ctx = RenderContext::new(w, h);
        let cache = RenderCache::new();
        render_into(
            page_ref,
            &cache,
            &InterpreterSettings::default(),
            &RenderSettings::default(),
            &mut ctx,
            transform,
        );
        ctx.flush();
        let mut pixmap = Pixmap::new(w, h);
        ctx.render_with(
            &mut pixmap,
            &mut Resources::default(),
            RasterizerSettings {
                target_init: TargetInit::Clear(WHITE),
                ..Default::default()
            },
        );
        let png = pixmap.into_png().map_err(|err| RenderError::Encode {
            page,
            reason: err.to_string(),
        })?;
        Ok(Rendered {
            png,
            width: u32::from(w),
            height: u32::from(h),
        })
    }
}

/// The text layer of a PDF, read with lopdf.
pub struct TextLayer {
    doc: lopdf::Document,
}

impl TextLayer {
    pub fn load(bytes: &[u8]) -> Result<Self, RenderError> {
        lopdf::Document::load_mem(bytes)
            .map(|doc| Self { doc })
            .map_err(|err| RenderError::Unreadable(err.to_string()))
    }

    /// The text of page `page` (1-based), or `None` when it has none or it does not look like
    /// text (a scan, or a font without a usable character map gives empty or garbled text).
    pub fn page_text(&self, page: u32) -> Option<String> {
        let text = self.doc.extract_text(&[page]).ok()?;
        looks_like_text(&text).then_some(text)
    }
}

/// Whether extracted text is worth showing a model: enough of it, and mostly letters and digits.
pub fn looks_like_text(text: &str) -> bool {
    let visible: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).collect();
    if visible.len() < 20 {
        return false;
    }
    let readable = visible.iter().filter(|c| c.is_alphanumeric()).count();
    readable * 2 >= visible.len()
}

#[cfg(test)]
mod tests {
    use hayro::vello_cpu::Pixmap;
    use lopdf::{Document, Object, Stream, dictionary};

    use super::*;

    /// Letter pages, each with a black rectangle covering the middle of its top half.
    fn sample(pages: u32) -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let mut kids = Vec::new();
        for _ in 0..pages {
            // PDF space has its origin at the bottom left: y from 446 to 746 is the top half.
            let content = doc.add_object(Stream::new(
                dictionary! {},
                b"0 0 0 rg 153 446 306 300 re f".to_vec(),
            ));
            kids.push(Object::Reference(doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
                "Contents" => content,
            })));
        }
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Count" => i64::from(pages), "Kids" => kids,
            }),
        );
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog);
        let mut out = Vec::new();
        doc.save_to(&mut out).unwrap();
        out
    }

    /// The share of dark pixels in a rendered image.
    fn ink(rendered: &Rendered) -> f64 {
        let pixmap = Pixmap::from_png(std::io::Cursor::new(&rendered.png)).unwrap();
        assert_eq!(
            (u32::from(pixmap.width()), u32::from(pixmap.height())),
            (rendered.width, rendered.height)
        );
        let pixels = pixmap.data();
        let dark = pixels.iter().filter(|p| p.r < 64 && p.a > 200).count();
        dark as f64 / pixels.len() as f64
    }

    #[test]
    fn counts_and_renders_pages_at_the_asked_resolution() {
        let doc = PdfDocument::load(sample(3)).unwrap();
        assert_eq!(doc.page_count(), 3);
        let page = doc.render_page(2, 150.0).unwrap();
        assert_eq!((page.width, page.height), (1275, 1650));
        // The rectangle is half the page wide (306/612) and 300/792 tall: about 0.189.
        let share = ink(&page);
        assert!((0.17..0.21).contains(&share), "{share}");
    }

    #[test]
    fn renders_only_the_asked_region() {
        let doc = PdfDocument::load(sample(1)).unwrap();
        let top = Region {
            left: 0.0,
            top: 0.0,
            right: 1.0,
            bottom: 0.5,
        };
        let bottom = Region {
            left: 0.0,
            top: 0.5,
            right: 1.0,
            bottom: 1.0,
        };
        let top = doc.render_region(1, top, 250.0).unwrap();
        assert_eq!((top.width, top.height), (2125, 1375));
        assert!(ink(&top) > 0.15, "the rectangle is in the top half");
        let bottom = doc.render_region(1, bottom, 250.0).unwrap();
        assert!(ink(&bottom) < 0.001, "nothing is drawn in the bottom half");
        // The rectangle alone: 153..459 × 46..346 points from the top left.
        let rectangle = Region {
            left: 153.0 / 612.0,
            top: 46.0 / 792.0,
            right: 459.0 / 612.0,
            bottom: 346.0 / 792.0,
        };
        assert!(ink(&doc.render_region(1, rectangle, 200.0).unwrap()) > 0.98);
    }

    #[test]
    fn refuses_missing_pages_and_empty_regions_and_bad_files() {
        let doc = PdfDocument::load(sample(2)).unwrap();
        assert_eq!(
            doc.render_page(3, 72.0).unwrap_err(),
            RenderError::NoSuchPage { page: 3, count: 2 }
        );
        assert!(matches!(
            doc.render_page(0, 72.0),
            Err(RenderError::NoSuchPage { .. })
        ));
        let flat = Region {
            left: 0.2,
            top: 0.5,
            right: 0.8,
            bottom: 0.5,
        };
        assert_eq!(
            doc.render_region(1, flat, 72.0).unwrap_err(),
            RenderError::EmptyArea(1)
        );
        assert!(matches!(
            PdfDocument::load(b"not a pdf".to_vec()),
            Err(RenderError::Unreadable(_))
        ));
    }

    #[test]
    fn a_region_grows_by_its_margin_and_stays_on_the_page() {
        let region = Region {
            left: 0.1,
            top: 0.02,
            right: 0.5,
            bottom: 0.99,
        };
        let grown = region.checked_with_margin(0.03).unwrap();
        assert!((grown.left - 0.07).abs() < 1e-9 && grown.top == 0.0);
        assert!((grown.right - 0.53).abs() < 1e-9 && grown.bottom == 1.0);
        for bad in [
            Region {
                left: 0.9,
                top: 0.1,
                right: 0.2,
                bottom: 0.5,
            },
            Region {
                left: 0.1,
                top: 0.1,
                right: 1.2,
                bottom: 0.5,
            },
            Region {
                left: f64::NAN,
                top: 0.1,
                right: 0.2,
                bottom: 0.5,
            },
        ] {
            assert!(bad.checked_with_margin(0.03).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn text_that_is_mostly_symbols_or_too_short_is_not_text() {
        assert!(looks_like_text(
            "Trong những năm gần đây, học sâu đã đạt được nhiều thành tựu"
        ));
        assert!(!looks_like_text("   \n\x0c  "));
        assert!(!looks_like_text("short"));
        assert!(!looks_like_text(
            "�� ?? ### $$ %% ^^ && ** (( )) �� ?? ### $$ %% a b"
        ));
    }
}
