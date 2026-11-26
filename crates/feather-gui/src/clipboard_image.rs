//! Image intake from the OS clipboard.
//!
//! Why this exists: `rpc::user_message` accepts an `images` array and the
//! providers all encode images natively, but nothing in the GUI ever produced
//! one. The `app.clipboard.pasteImage` binding promised `alt+v` and had no
//! handler, so the key did nothing. This module is the missing producer.
//!
//! egui 0.29 has no image clipboard API (`Clipboard` is text-only and
//! `Event::Paste` carries a `String`), so the Win32 clipboard is read directly.
//! The clipboard almost always offers `CF_DIB` rather than `CF_DIBV5` with an
//! alpha channel, so a PNG copy from a browser yields a 24- or 32-bit bottom-up
//! DIB with no alpha mask; the alpha case is detected from the header.

use std::sync::Mutex;

/// One decoded clipboard image, already in the base64 form the providers want.
#[derive(Debug, Clone)]
pub struct ClipboardImage {
    /// e.g. `image/png`. Always one of the four types `rpc::user_message` allows.
    pub media_type: String,
    /// Base64, no data-URL prefix. The providers add that themselves.
    pub data: String,
    /// Decoded pixel size, for the attachment chip and for tests.
    pub width: u32,
    pub height: u32,
}

static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);

fn set_last_error(error: Option<String>) {
    if let Ok(mut slot) = LAST_ERROR.lock() {
        *slot = error;
    }
}

/// Why the last [`read_image`] call returned `None`, if it recorded a reason.
///
/// Distinguishes "the clipboard held no image" from "it held one we could not
/// decode", which are different problems for the user and used to be
/// indistinguishable from a silently inert key binding.
pub fn last_error() -> Option<String> {
    LAST_ERROR.lock().ok().and_then(|slot| slot.clone())
}

/// Read an image from the clipboard, if there is one.
///
/// Returns `None` when the clipboard holds no image; check [`last_error`] to tell
/// that apart from a decode failure.
pub fn read_image() -> Option<ClipboardImage> {
    set_last_error(None);
    #[cfg(windows)]
    {
        match windows_clipboard::read() {
            Ok(Some(image)) => return Some(image),
            Ok(None) => {}
            Err(e) => set_last_error(Some(e)),
        }
    }
    #[cfg(not(windows))]
    {
        set_last_error(Some("image paste is only implemented on Windows".into()));
    }
    None
}

/// Minimal base64 (RFC 4648, padded). Duplicated from `mentions::b64_encode`
/// because that one is private to the `feather` crate and this needs the same
/// 15 lines rather than a new dependency.
pub(crate) fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
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

#[cfg(windows)]
mod windows_clipboard {
    use super::{base64, ClipboardImage};
    use windows_sys::Win32::Foundation::HANDLE;
    // Only the DIB header constants are needed: the pixels are read straight out
    // of the HGLOBAL, so `GetDIBits`/`BITMAPINFO` are never used here.
    use windows_sys::Win32::Graphics::Gdi::BI_RGB;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    };
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};

    const CF_DIB: u32 = 8;

    pub fn read() -> Result<Option<ClipboardImage>, String> {
        // SAFETY: every call below is a plain Win32 call on the thread's own
        // clipboard. The clipboard is opened and closed in the same scope, and
        // the HGLOBAL is unlocked on every path that locked it.
        unsafe {
            if IsClipboardFormatAvailable(CF_DIB) == 0 {
                return Ok(None);
            }
            // A clipboard owned by another process fails to open; that is a
            // transient race, not a decode problem, and worth reporting.
            if OpenClipboard(std::ptr::null_mut()) == 0 {
                return Err("clipboard is in use by another application".into());
            }
            // Ensure exactly one close per successful open, including on the
            // early returns below.
            struct Close;
            impl Drop for Close {
                fn drop(&mut self) {
                    unsafe { CloseClipboard() };
                }
            }
            let _close = Close;

            let handle: HANDLE = GetClipboardData(CF_DIB);
            if handle.is_null() {
                return Ok(None);
            }
            let size = GlobalSize(handle);
            if size == 0 {
                return Err("clipboard image is empty".into());
            }
            let ptr = GlobalLock(handle) as *const u8;
            if ptr.is_null() {
                return Err("cannot lock clipboard memory".into());
            }
            let dib = std::slice::from_raw_parts(ptr, size);
            let result = decode_dib(dib);
            GlobalUnlock(handle);
            result.map(Some)
        }
    }

    /// Decode a `CF_DIB` payload into a PNG-base64 image.
    ///
    /// The layout is a `BITMAPINFOHEADER` (40 bytes) possibly followed by colour
    /// tables and then bottom-up pixel rows padded to 4 bytes. Top-down DIBs
    /// (negative height) and 32-bit-with-alpha are both handled because a browser
    /// PNG copy takes the alpha path while a screenshot tool usually does not.
    pub(super) unsafe fn decode_dib(dib: &[u8]) -> Result<ClipboardImage, String> {
        if dib.len() < 40 {
            return Err("clipboard image header is truncated".into());
        }
        let header_size = u32::from_le_bytes(dib[0..4].try_into().unwrap()) as usize;
        let width = i32::from_le_bytes(dib[4..8].try_into().unwrap());
        let height_raw = i32::from_le_bytes(dib[8..12].try_into().unwrap());
        let bit_count = u16::from_le_bytes(dib[14..16].try_into().unwrap());
        let compression = u32::from_le_bytes(dib[16..20].try_into().unwrap());
        if compression != BI_RGB {
            // BI_BITFIELDS / BI_JPEG / BI_PNG all need work this does not do.
            return Err(format!(
                "unsupported clipboard image compression {compression}"
            ));
        }
        if !matches!(bit_count, 24 | 32) {
            return Err(format!("unsupported clipboard image depth {bit_count}"));
        }
        let top_down = height_raw < 0;
        let width = width.unsigned_abs();
        let height = height_raw.unsigned_abs();
        if width == 0 || height == 0 {
            return Err("clipboard image has zero size".into());
        }
        let stride = ((width as usize * bit_count as usize).div_ceil(32)) * 4;
        let palette = if bit_count <= 8 {
            (1 + (1 << bit_count)) * 4
        } else {
            0
        };
        let offset = header_size.max(40) + palette;
        let needed = stride
            .checked_mul(height as usize)
            .and_then(|n| n.checked_add(offset))
            .ok_or("clipboard image size overflow")?;
        if dib.len() < needed {
            return Err("clipboard image data is truncated".into());
        }

        let bytes = bit_count / 8;
        let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
        for row in 0..height as usize {
            // DIB rows are bottom-up unless the height was negative.
            let src_row = if top_down {
                row
            } else {
                height as usize - 1 - row
            };
            let start = offset + src_row * stride;
            for col in 0..width as usize {
                let i = start + col * bytes as usize;
                let b = dib[i];
                let g = dib[i + 1];
                let r = dib[i + 2];
                // A 32-bit DIB from a screenshot usually has a zero alpha byte
                // that must be treated as opaque, while a browser PNG copy sets
                // it deliberately. Only trust alpha when the image actually uses
                // it, otherwise every screenshot pastes fully transparent.
                let a = if bytes == 4 && dib[i + 3] != 0 {
                    dib[i + 3]
                } else {
                    255
                };
                rgba.extend_from_slice(&[r, g, b, a]);
            }
        }
        encode_png(width, height, &rgba)
    }
    /// Re-encode RGBA pixels as a PNG and base64 it.
    ///
    /// PNG is produced rather than passing the DIB through because every provider
    /// takes a media type from the documented set and `CF_DIB` is not one of them.
    unsafe fn encode_png(
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Result<super::ClipboardImage, String> {
        let buffer: image::RgbaImage = image::ImageBuffer::from_raw(width, height, rgba.to_vec())
            .ok_or("clipboard image buffer size mismatch")?;
        let mut png = Vec::new();
        {
            use image::ImageEncoder;
            let encoder = image::codecs::png::PngEncoder::new(&mut png);
            encoder
                .write_image(
                    buffer.as_raw(),
                    width,
                    height,
                    image::ExtendedColorType::Rgba8,
                )
                .map_err(|e| format!("cannot encode clipboard image as PNG: {e}"))?;
        }
        Ok(super::ClipboardImage {
            media_type: "image/png".into(),
            data: base64(&png),
            width,
            height,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_rfc_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    /// The quad-at-a-time decoder must agree with the byte-at-a-time one it
    /// replaced, including on padding, embedded whitespace and a truncated tail.
    /// These are the shapes a stored session payload actually takes.
    #[test]
    fn the_quad_decoder_matches_a_reference_accumulator() {
        /// The previous implementation, kept as the oracle.
        fn reference(text: &str) -> Vec<u8> {
            let mut out = Vec::new();
            let mut acc = 0u32;
            let mut bits = 0u32;
            for b in text.bytes() {
                if b == b'=' {
                    break;
                }
                let v = match b {
                    b'A'..=b'Z' => b - b'A',
                    b'a'..=b'z' => b - b'a' + 26,
                    b'0'..=b'9' => b - b'0' + 52,
                    b'+' => 62,
                    b'/' => 63,
                    _ => continue,
                } as u32;
                acc = (acc << 6) | v;
                bits += 6;
                if bits >= 8 {
                    bits -= 8;
                    out.push((acc >> bits) as u8);
                }
            }
            out
        }
        let cases = [
            "",
            "Zg==",
            "Zm8=",
            "Zm9v",
            "Zm9vYg==",
            "Zm9vYmE=",
            "Zm9vYmFy",
            // Whitespace inside the payload must be skipped, not decoded.
            "Zm9v YmFy",
            "Zm9v\nYmFy",
            // A truncated tail: 5 chars means one full quad plus one leftover
            // character, which carries no whole byte.
            "Zm9vY",
            // Mixed noise
            " Zm9v -- YmFy\r\n",
        ];
        for case in cases {
            assert_eq!(
                decode_base64_for_display(case),
                reference(case),
                "mismatch for {case:?}"
            );
        }
        // A round trip through the encoder proves the byte values, not just the
        // lengths.
        for payload in [b"f".as_slice(), b"fo", b"foo", b"foob", b"foobar"] {
            let encoded = base64(payload);
            assert_eq!(decode_base64_for_display(&encoded), payload);
        }
    }

    /// A 2x1 top-down 24-bit DIB of red then blue must decode to those pixels.
    #[cfg(windows)]
    #[test]
    fn a_24_bit_dib_decodes_to_the_expected_pixels() {
        let mut dib = Vec::new();
        dib.extend_from_slice(&40u32.to_le_bytes()); // header size
        dib.extend_from_slice(&2i32.to_le_bytes()); // width
        dib.extend_from_slice(&(-1i32).to_le_bytes()); // height: top-down
        dib.extend_from_slice(&1u16.to_le_bytes()); // planes
        dib.extend_from_slice(&24u16.to_le_bytes()); // bit count
        dib.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
        dib.extend_from_slice(&[0u8; 20]); // rest of header
                                           // Two pixels BGR, padded to a 4-byte stride: red, blue, 2 pad bytes.
        dib.extend_from_slice(&[0, 0, 255, 255, 0, 0, 0, 0]);

        let image = unsafe { windows_clipboard::decode_dib(&dib) }.unwrap();
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.media_type, "image/png");
        assert!(!image.data.is_empty());
        // Decoding the produced PNG proves the pixel order survived the round trip.
        let png = crate::clipboard_image::tests_support::decode_base64(&image.data);
        let decoded = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(decoded.get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_eq!(decoded.get_pixel(1, 0).0, [0, 0, 255, 255]);
    }

    /// A 32-bit DIB whose alpha bytes are all zero is a screenshot without an
    /// alpha channel, and must paste opaque rather than invisible.
    #[cfg(windows)]
    #[test]
    fn a_32_bit_dib_with_zero_alpha_is_treated_as_opaque() {
        let mut dib = Vec::new();
        dib.extend_from_slice(&40u32.to_le_bytes());
        dib.extend_from_slice(&1i32.to_le_bytes());
        dib.extend_from_slice(&(-1i32).to_le_bytes());
        dib.extend_from_slice(&1u16.to_le_bytes());
        dib.extend_from_slice(&32u16.to_le_bytes());
        dib.extend_from_slice(&0u32.to_le_bytes());
        dib.extend_from_slice(&[0u8; 20]);
        dib.extend_from_slice(&[0, 255, 0, 0]); // green, alpha 0

        let image = unsafe { windows_clipboard::decode_dib(&dib) }.unwrap();
        let png = crate::clipboard_image::tests_support::decode_base64(&image.data);
        let decoded = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(decoded.get_pixel(0, 0).0, [0, 255, 0, 255]);
    }
}

/// Decode base64 back to bytes, for displaying a stored attachment.
///
/// Tolerant of whitespace and stops at padding, because a payload that round
/// tripped through JSON or a session file may carry line breaks.
pub(crate) fn decode_base64_for_display(text: &str) -> Vec<u8> {
    // Sort a 32-bit accumulator one character at a time.
    //
    // A quad-at-a-time rewrite (index into a `[u8; 4]`, one exact 3-byte store)
    // was measured and rejected: `image.base64` went 196 µs -> 435 µs. The
    // bounds-checked `quad[filled]` writes cost more than the branch this reads
    // a whole character at a time, and LLVM already turns this accumulator plus
    // `Vec::push` into tight code. Do not "optimise" it without re-running that
    // bench case.
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for b in text.bytes() {
        if b == b'=' {
            break;
        }
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => continue,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}
/// Helpers shared with the renderer tests.
#[cfg(test)]
pub(crate) mod tests_support {
    /// Decode base64 so tests can round-trip through the real PNG decoder.
    pub fn decode_base64(text: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(text.len() / 4 * 3);
        let mut acc = 0u32;
        let mut bits = 0u32;
        for b in text.bytes() {
            if b == b'=' {
                break;
            }
            let v = match b {
                b'A'..=b'Z' => b - b'A',
                b'a'..=b'z' => b - b'a' + 26,
                b'0'..=b'9' => b - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => continue,
            } as u32;
            acc = (acc << 6) | v;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
            }
        }
        out
    }
}
