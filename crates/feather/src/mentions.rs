//! @file mentions — pi-style prompt attachments.
//!
//! `@path` tokens in a prompt pull the file's content into the user message
//! as an `<attachment>` block (images become base64 image blocks once the
//! message model carries them). Missing files become inline error markers
//! rather than hard failures, matching pi's forgiving attachment flow.

const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];
const MAX_TEXT_ATTACHMENT_CHARS: usize = 8_000;
const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;

/// One attached image (base64, provider-native image block).
pub type ImageAttachment = (String, String); // (media_type, base64 data)

/// Expand every `@path` token in `line`: text files become `<attachment>`
/// blocks appended to the prompt text; image files become native image
/// blocks returned alongside (pi's attachment pipeline). Non-mention text
/// is preserved verbatim.
pub fn expand_mentions(line: &str, cwd: &std::path::Path) -> (String, Vec<ImageAttachment>) {
    let mut out = String::from(line);
    let mut images: Vec<ImageAttachment> = Vec::new();
    for tok in line.split_whitespace() {
        let Some(p) = tok.strip_prefix('@') else {
            continue;
        };
        if p.is_empty() || p.len() > 512 {
            continue;
        }
        let path = cwd.join(p);
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        // Bound the read before loading: a 1 GB @attachment used to be read
        // fully and only truncated afterwards. 256 KiB is far above the worst
        // case for MAX_TEXT_ATTACHMENT_CHARS (8 000 four-byte chars), so the
        // "(truncated)" decision is unchanged; images size-check via metadata.
        let is_image = IMAGE_EXTS.contains(&ext.as_str());
        let read = || -> std::io::Result<Vec<u8>> {
            use std::io::Read;
            if is_image {
                let len = std::fs::metadata(&path)?.len();
                if len > MAX_IMAGE_BYTES as u64 {
                    return Ok(Vec::new());
                }
                std::fs::read(&path)
            } else {
                let f = std::fs::File::open(&path)?;
                let mut buf = Vec::new();
                f.take(256 * 1024).read_to_end(&mut buf)?;
                Ok(buf)
            }
        };
        match read() {
            Ok(bytes) if is_image => {
                if bytes.is_empty() {
                    out.push_str(&format!(
                        "\n<attachment path=\"{p}\" error=\"image too large ({} bytes)\"/>",
                        std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
                    ));
                    continue;
                }
                let media = match ext.as_str() {
                    "jpg" | "jpeg" => "image/jpeg",
                    "gif" => "image/gif",
                    "webp" => "image/webp",
                    _ => "image/png",
                };
                images.push((media.to_string(), b64_encode(&bytes)));
            }
            Ok(bytes) => {
                let mut text = String::from_utf8_lossy(&bytes).to_string();
                if text.chars().count() > MAX_TEXT_ATTACHMENT_CHARS {
                    text = text.chars().take(MAX_TEXT_ATTACHMENT_CHARS).collect();
                    text.push_str("\n… (truncated)");
                }
                out.push_str(&format!(
                    "\n<attachment path=\"{p}\">\n{text}\n</attachment>"
                ));
            }
            Err(e) => {
                out.push_str(&format!("\n<attachment path=\"{p}\" error=\"{e}\"/>"));
            }
        }
    }
    (out, images)
}

/// Shared attachment pipeline for interactive and one-shot prompts.
pub fn message_from_mentions(line: &str, cwd: &std::path::Path) -> feather_core::message::Message {
    use feather_core::message::{ContentBlock, Message};
    let (text, images) = expand_mentions(line, cwd);
    let mut message = Message::user_text(text);
    message.content.extend(
        images
            .into_iter()
            .map(|(media_type, data)| ContentBlock::Image { media_type, data }),
    );
    message
}

/// Whether the line carries any @mention (the TUI can hint about it).
pub fn has_mentions(line: &str) -> bool {
    line.split_whitespace()
        .any(|t| t.len() > 1 && t.starts_with('@'))
}

/// Minimal base64 (RFC 4648, padding included) — avoids a dependency for
/// the one place feather needs binary-to-text.
fn b64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "feather-mentions-{tag}-{}-{}",
            std::process::id(),
            feather_core::message::now_millis()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn text_mentions_become_attachment_blocks() {
        let dir = temp("t");
        std::fs::write(dir.join("notes.md"), "hello attachment").unwrap();
        let (out, images) = expand_mentions("summarize @notes.md please", &dir);
        assert!(images.is_empty());
        assert!(out.starts_with("summarize @notes.md please"), "{out}");
        assert!(out.contains("<attachment path=\"notes.md\">"));
        assert!(out.contains("hello attachment"));
        assert!(out.contains("</attachment>"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_files_become_inline_errors_not_failures() {
        let dir = temp("m");
        let (out, _imgs) = expand_mentions("look at @nope.txt", &dir);
        assert!(out.contains("error="), "{out}");
        assert!(has_mentions("look at @nope.txt"));
        assert!(!has_mentions("no mentions here"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn image_mentions_become_base64_blocks() {
        let dir = temp("i");
        // minimal valid-ish PNG header bytes (content irrelevant to the test)
        let bytes: Vec<u8> = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        std::fs::write(dir.join("shot.png"), &bytes).unwrap();
        let (out, images) = expand_mentions("what is @shot.png", &dir);
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].0, "image/png");
        assert!(
            !out.contains("media_type="),
            "image no longer inlined into text"
        );
        assert_eq!(b64_decode_roundtrip(&images[0].1), bytes);
        // (text keeps only the plain prompt — images ride as message blocks)
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn b64_decode_roundtrip(s: &str) -> Vec<u8> {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let val = |c: u8| T.iter().position(|&t| t == c).unwrap() as u32;
        let bytes = s.as_bytes();
        let mut out = Vec::new();
        for chunk in bytes.chunks(4) {
            let n = (val(chunk[0]) << 18)
                | (val(chunk[1]) << 12)
                | (if chunk[2] == b'=' { 0 } else { val(chunk[2]) } << 6)
                | (if chunk[3] == b'=' { 0 } else { val(chunk[3]) });
            out.push((n >> 16) as u8);
            if chunk[2] != b'=' {
                out.push((n >> 8) as u8);
            }
            if chunk[3] != b'=' {
                out.push(n as u8);
            }
        }
        out
    }

    #[test]
    fn large_text_is_truncated_with_marker() {
        let dir = temp("big");
        let big = "x".repeat(20_000);
        std::fs::write(dir.join("big.txt"), &big).unwrap();
        let (out, _imgs) = expand_mentions("read @big.txt", &dir);
        assert!(out.contains("… (truncated)"));
        assert!(out.chars().count() < big.len());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
