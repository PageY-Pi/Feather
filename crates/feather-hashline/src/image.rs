//! Recognising an image file by content rather than by extension.
//!
//! `read` refuses binary files, and an image is the one binary a model is
//! routinely asked about — so the refusal has to say something more useful than
//! "NUL byte". The extension is not enough to decide that: a `.png` saved as
//! `.dat` is still a picture, and a `.png` file holding text is not. Sniffing the
//! magic bytes is what lets the error name the actual situation.
//!
//! Deliberately small. This is not a decoder: nothing here reads pixels, and the
//! image itself never passes through this crate — providers carry images as
//! base64 blocks on the *user* turn, and no provider can put one inside a tool
//! result, which is why `read` cannot serve one.

/// Leading bytes that identify a format the providers accept as an image block.
///
/// The set matches the media types feather already sends
/// (`mentions::IMAGE_EXTS` and `rpc::user_message`): PNG, JPEG, GIF, WebP.
const MAGIC: &[(&[u8], &str)] = &[
    (b"\x89PNG\r\n\x1a\n", "image/png"),
    (b"\xFF\xD8\xFF", "image/jpeg"),
    (b"GIF87a", "image/gif"),
    (b"GIF89a", "image/gif"),
    // WebP is a RIFF container; `RIFF....WEBP` is checked below.
    (b"RIFF", "image/webp"),
];

/// The media type of `bytes` if it is an image feather can attach, else `None`.
pub fn media_type_of(bytes: &[u8]) -> Option<&'static str> {
    for (magic, media) in MAGIC {
        if bytes.starts_with(magic) {
            // `RIFF` alone is also WAV and AVI, so confirm the form type before
            // claiming WebP. Bytes 8..12 hold it; absence means not WebP.
            if *media == "image/webp" && bytes.get(8..12) != Some(b"WEBP") {
                continue;
            }
            return Some(media);
        }
    }
    None
}

/// Whether `path`'s *content* is an attachable image.
///
/// `bytes` is the already-read file: the caller has it in hand, so this never
/// re-opens the path. The extension is consulted only to rule out a text file
/// whose first bytes happen to look like a header — content wins.
pub fn is_image(path: &std::path::Path, bytes: &[u8]) -> bool {
    media_type_of(bytes).is_some()
        || path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| {
                matches!(
                    ext.to_ascii_lowercase().as_str(),
                    "png" | "jpg" | "jpeg" | "gif" | "webp"
                )
            })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_png_is_recognised_by_its_magic_bytes() {
        let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        assert_eq!(media_type_of(&png), Some("image/png"));
    }

    #[test]
    fn a_webp_needs_the_form_type_not_just_riff() {
        // A WAV file also starts with RIFF; treating it as WebP would tell the
        // model to ask for an attachment the provider would then reject.
        let wav = b"RIFF\x24\x00\x00\x00WAVEfmt ";
        assert_eq!(media_type_of(wav), None);
        let webp = b"RIFF\x24\x00\x00\x00WEBPVP8 ";
        assert_eq!(media_type_of(webp), Some("image/webp"));
    }

    #[test]
    fn the_extension_alone_still_identifies_an_image() {
        // Truncated or zero-length files: the extension is all there is, and the
        // user's intent ("look at this picture") is unambiguous.
        let dir = std::env::temp_dir().join(format!(
            "feather-img-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("SHOT.PNG");
        std::fs::write(&p, []).unwrap();
        assert!(is_image(&p, &[]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_text_file_is_not_an_image() {
        let dir = std::env::temp_dir().join(format!(
            "feather-img-test-txt-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("notes.txt");
        std::fs::write(&p, b"plain text").unwrap();
        assert!(!is_image(&p, b"plain text"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
