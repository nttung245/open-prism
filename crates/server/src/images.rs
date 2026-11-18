//! Images sent with AI chat messages: checked, stored as files next to their conversation
//! (outside the project, so never exported or snapshotted), served back to the browser and
//! loaded again to send to the model.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use openprism_ai::llm::{ImageRef, Message, SourceRef};
use openprism_core::paths::RelPath;
use serde::Deserialize;
use ts_rs::TS;
use uuid::Uuid;

use crate::error::ApiError;

/// Most images in one message.
pub const MAX_IMAGES: usize = 4;
/// Largest image, decoded.
pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
/// Most images sent to the model in one turn: the most recent ones.
pub const MAX_IMAGES_PER_TURN: usize = 8;

/// An image as the browser sends it.
#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ImageUpload {
    /// The image file, base64-encoded (a `data:` URL prefix is ignored). PNG, JPEG or WebP, at
    /// most 5 MB.
    pub data: String,
    /// The PDF page it was captured from (1-based).
    #[serde(default)]
    #[ts(optional)]
    pub page: Option<u32>,
    /// Where SyncTeX places the captured region.
    #[serde(default)]
    #[ts(optional)]
    pub source: Option<SourceRef>,
}

/// The image formats we accept, recognised from the file's first bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Png,
    Jpeg,
    Webp,
}

impl ImageKind {
    const ALL: [Self; 3] = [Self::Png, Self::Jpeg, Self::Webp];

    pub fn of(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Self::Png)
        } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            Some(Self::Jpeg)
        } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            Some(Self::Webp)
        } else {
            None
        }
    }

    pub fn media_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Webp => "webp",
        }
    }
}

/// An uploaded image that passed [`check`].
#[derive(Debug)]
pub struct CheckedImage {
    bytes: Vec<u8>,
    kind: ImageKind,
    page: Option<u32>,
    source: Option<SourceRef>,
}

/// Checks every image of a message before anything is stored: at most [`MAX_IMAGES`], each
/// valid base64 of a PNG, JPEG or WebP file of at most [`MAX_IMAGE_BYTES`].
pub fn check(uploads: Vec<ImageUpload>) -> Result<Vec<CheckedImage>, ApiError> {
    if uploads.len() > MAX_IMAGES {
        return Err(ApiError::bad_request(format!(
            "a message can have at most {MAX_IMAGES} images"
        )));
    }
    uploads
        .into_iter()
        .enumerate()
        .map(|(index, upload)| {
            let number = index + 1;
            let data = upload
                .data
                .split_once(";base64,")
                .map_or(upload.data.as_str(), |(_, data)| data);
            // Refuse before decoding what would be too large anyway.
            if data.len() > (MAX_IMAGE_BYTES / 3 + 1) * 4 {
                return Err(too_large(number));
            }
            let bytes = STANDARD.decode(data.trim()).map_err(|_| {
                ApiError::bad_request(format!("image {number} is not valid base64"))
            })?;
            if bytes.len() > MAX_IMAGE_BYTES {
                return Err(too_large(number));
            }
            let kind = ImageKind::of(&bytes).ok_or_else(|| {
                ApiError::bad_request(format!("image {number} is not a PNG, JPEG or WebP image"))
            })?;
            if let Some(source) = &upload.source {
                RelPath::parse(&source.file)?;
            }
            Ok(CheckedImage {
                bytes,
                kind,
                page: upload.page,
                source: upload.source,
            })
        })
        .collect()
}

fn too_large(number: usize) -> ApiError {
    ApiError::bad_request(format!(
        "image {number} is larger than {} MB",
        MAX_IMAGE_BYTES / (1024 * 1024)
    ))
}

/// Writes checked images into a conversation's directory; returns the references to keep in
/// the message.
pub async fn store(dir: &Path, images: Vec<CheckedImage>) -> Result<Vec<ImageRef>, ApiError> {
    if images.is_empty() {
        return Ok(Vec::new());
    }
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(ApiError::internal)?;
    let mut refs = Vec::with_capacity(images.len());
    for image in images {
        let id = Uuid::now_v7().to_string();
        let path = dir.join(format!("{id}.{}", image.kind.extension()));
        tokio::fs::write(&path, &image.bytes)
            .await
            .map_err(ApiError::internal)?;
        refs.push(ImageRef {
            id,
            media_type: image.kind.media_type().to_owned(),
            page: image.page,
            source: image.source,
            data: None,
        });
    }
    Ok(refs)
}

/// The file of an image in a conversation's directory, if there is one. `id` must be a UUID,
/// so it can never point outside the directory.
pub fn find(dir: &Path, id: &str) -> Option<(PathBuf, ImageKind)> {
    let id = Uuid::parse_str(id).ok()?.hyphenated().to_string();
    ImageKind::ALL.into_iter().find_map(|kind| {
        let path = dir.join(format!("{id}.{}", kind.extension()));
        path.is_file().then_some((path, kind))
    })
}

/// Loads the bytes of the most recent images of a conversation (at most `max`) so they are
/// sent to the model; older images are left without bytes and are only mentioned.
pub async fn load_recent(messages: &mut [Message], dir: &Path, max: usize) {
    let mut loaded = 0;
    for message in messages.iter_mut().rev() {
        let Message::User { images, .. } = message else {
            continue;
        };
        for image in images.iter_mut().rev() {
            if loaded == max {
                return;
            }
            let Some((path, _)) = find(dir, &image.id) else {
                tracing::warn!(id = %image.id, "an attached image is missing");
                continue;
            };
            match tokio::fs::read(&path).await {
                Ok(bytes) => {
                    image.data = Some(Arc::from(STANDARD.encode(bytes)));
                    loaded += 1;
                }
                Err(err) => tracing::warn!(?err, id = %image.id, "cannot read an attached image"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    fn upload(bytes: &[u8]) -> ImageUpload {
        ImageUpload {
            data: STANDARD.encode(bytes),
            page: Some(2),
            source: None,
        }
    }

    fn message_of(result: Result<Vec<CheckedImage>, ApiError>) -> String {
        format!("{:?}", result.unwrap_err())
    }

    #[test]
    fn png_jpeg_and_webp_are_recognised_by_their_content() {
        let webp = b"RIFF\x24\0\0\0WEBPVP8 ";
        for (bytes, kind) in [
            (PNG, ImageKind::Png),
            (&[0xff, 0xd8, 0xff, 0xe0, 0, 0x10][..], ImageKind::Jpeg),
            (&webp[..], ImageKind::Webp),
        ] {
            let checked = check(vec![upload(bytes)]).unwrap();
            assert_eq!(checked[0].kind, kind);
            assert_eq!(checked[0].page, Some(2));
        }
        // A data URL prefix is ignored.
        let data = format!("data:image/png;base64,{}", STANDARD.encode(PNG));
        let checked = check(vec![ImageUpload {
            data,
            page: None,
            source: None,
        }])
        .unwrap();
        assert_eq!(checked[0].kind, ImageKind::Png);
    }

    #[test]
    fn other_files_bad_base64_large_images_and_too_many_are_refused() {
        // A PDF, whatever the browser says it is.
        assert!(message_of(check(vec![upload(b"%PDF-1.7\n")])).contains("not a PNG"));
        let bad = ImageUpload {
            data: "not base64!".into(),
            page: None,
            source: None,
        };
        assert!(message_of(check(vec![bad])).contains("not valid base64"));
        let mut large = PNG.to_vec();
        large.resize(6 * 1024 * 1024, 0);
        assert!(message_of(check(vec![upload(&large)])).contains("larger than 5 MB"));
        let mut just_under = PNG.to_vec();
        just_under.resize(MAX_IMAGE_BYTES, 0);
        assert!(check(vec![upload(&just_under)]).is_ok());
        let five = (0..5).map(|_| upload(PNG)).collect();
        assert!(message_of(check(five)).contains("at most 4 images"));
        // A source outside the project.
        let outside = ImageUpload {
            source: Some(SourceRef {
                file: "../secret.tex".into(),
                line: 1,
            }),
            ..upload(PNG)
        };
        assert!(check(vec![outside]).is_err());
    }

    #[tokio::test]
    async fn stored_images_are_found_by_id_only() {
        let dir = tempfile::tempdir().unwrap();
        let refs = store(dir.path(), check(vec![upload(PNG)]).unwrap())
            .await
            .unwrap();
        assert_eq!(refs[0].media_type, "image/png");
        let (path, kind) = find(dir.path(), &refs[0].id).unwrap();
        assert_eq!(kind, ImageKind::Png);
        assert_eq!(std::fs::read(path).unwrap(), PNG);
        assert!(find(dir.path(), "../../etc/passwd").is_none());
        assert!(find(dir.path(), &Uuid::now_v7().to_string()).is_none());
    }
}
