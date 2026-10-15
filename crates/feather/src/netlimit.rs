//! Persistent provider traffic limit.
//!
//! Feather's own network use is dominated by model traffic: a large context
//! uploads on every turn and the response streams back. A user sharing the
//! link (a game in the background, a video call) may want a hard ceiling, so
//! this module persists one number and pushes it into the core transport
//! shaper.
//!
//! Data-only: the value lives in the `netlimit` document of `config.redb`, and
//! parsing is deliberately forgiving because it is typed by hand.

use crate::config_store;
use feather_core::provider::transport::{MAX_RATE_LIMIT, MIN_RATE_LIMIT};
use serde::{Deserialize, Serialize};

const DOC_KEY: &str = "netlimit";

/// Default ceiling offered in the UI; feather rarely needs more while idle.
pub const SUGGESTED: [(&str, &str); 5] = [
    ("Off", "0"),
    ("128 KB/s", "128 KB"),
    ("512 KB/s", "512 KB"),
    ("2 MB/s", "2 MB"),
    ("10 MB/s", "10 MB"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Limit {
    /// Bytes per second; `0` means unlimited.
    pub bytes_per_second: u64,
}

impl Default for Limit {
    fn default() -> Self {
        Self {
            bytes_per_second: 0,
        }
    }
}

impl Limit {
    /// Status-bar text, matching the proxy label's register.
    pub fn label(&self) -> String {
        if self.bytes_per_second == 0 {
            "Limit: Off".into()
        } else {
            format!("Limit: {}/s", human(self.bytes_per_second))
        }
    }

    /// Compact `512 KB` / `2 MB` form for the picker and tooltips.
    pub fn value_text(&self) -> String {
        if self.bytes_per_second == 0 {
            "Off".into()
        } else {
            human(self.bytes_per_second)
        }
    }

    /// Push the value into the process-wide transport shaper.
    pub fn apply(&self) {
        feather_core::provider::transport::set_rate_limit(self.bytes_per_second);
    }
}

/// Bytes-per-second as a short human string (`512 KB`, `1.5 MB`).
pub fn human(bytes_per_second: u64) -> String {
    const KB: f64 = 1024.0;
    let value = bytes_per_second as f64;
    if value < KB {
        format!("{bytes_per_second} B")
    } else if value < KB * KB {
        let kb = value / KB;
        if (kb.fract() * 10.0).round() == 0.0 {
            format!("{} KB", kb.round() as u64)
        } else {
            format!("{kb:.1} KB")
        }
    } else {
        let mb = value / (KB * KB);
        if (mb.fract() * 10.0).round() == 0.0 {
            format!("{} MB", mb.round() as u64)
        } else {
            format!("{mb:.1} MB")
        }
    }
}

/// Read the persisted limit. Missing or malformed data means "unlimited",
/// which is the safe default: never silently throttle a user who did not ask.
pub fn load(home: &std::path::Path) -> Limit {
    match config_store::read(home, DOC_KEY) {
        Ok(Some(value)) => serde_json::from_value(value).unwrap_or_default(),
        _ => Limit::default(),
    }
}

/// Persist and apply a limit. Returns the clamped value so the UI can show what
/// was actually stored when the input was out of range.
pub fn save(home: &std::path::Path, bytes_per_second: u64) -> Result<Limit, String> {
    let limit = Limit {
        bytes_per_second: clamp(bytes_per_second),
    };
    let value = serde_json::to_value(limit).map_err(|e| e.to_string())?;
    config_store::write(home, DOC_KEY, &value).map_err(|e| e.to_string())?;
    limit.apply();
    Ok(limit)
}

/// Clamp into the shaper's accepted range: `0` stays `0` (off).
pub fn clamp(bytes_per_second: u64) -> u64 {
    if bytes_per_second == 0 {
        0
    } else {
        bytes_per_second.clamp(MIN_RATE_LIMIT, MAX_RATE_LIMIT)
    }
}

/// Parse typed input into bytes per second.
///
/// Accepted forms: `off` / `none` / `0`, a bare number (bytes per second),
/// or a number with a unit — `512 KB`, `512kb/s`, `1.5 MB`, `2M`, `1 G`.
/// `b`, `kb`/`kib`, `mb`/`mib`, `gb`/`gib` all work; case and spacing are
/// ignored, and an optional trailing `/s` is stripped. `KB` means KiB (1024),
/// matching how network tooling usually labels it.
pub fn parse(input: &str) -> Result<u64, String> {
    let text = input.trim().to_ascii_lowercase();
    if text.is_empty() {
        return Err("Enter a limit, for example 512 KB/s".into());
    }
    if matches!(text.as_str(), "off" | "none" | "unlimited" | "disable") {
        return Ok(0);
    }
    // Trailing speed suffix: "512 kb/s" and "512 kb per s" both mean the same.
    let text = text
        .strip_suffix("/s")
        .or_else(|| text.strip_suffix("ps"))
        .unwrap_or(&text)
        .trim();
    let text = text.strip_suffix("per s").unwrap_or(text).trim();

    let digits: String = text
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    if digits.is_empty() {
        return Err(format!("'{input}' does not start with a number"));
    }
    let unit = text[digits.len()..].trim();
    let number: f64 = digits
        .parse()
        .map_err(|_| format!("'{digits}' is not a number"))?;
    if !number.is_finite() || number < 0.0 {
        return Err("Enter a positive number".into());
    }
    let scale: f64 = match unit.replace([' ', '_'], "").as_str() {
        "" | "b" | "b/s" | "byte" | "bytes" => 1.0,
        "k" | "kb" | "kib" => 1024.0,
        "m" | "mb" | "mib" => 1024.0 * 1024.0,
        "g" | "gb" | "gib" => 1024.0 * 1024.0 * 1024.0,
        other => {
            return Err(format!(
                "Unknown unit '{other}'. Use B, KB, MB or GB (for example 512 KB/s)"
            ))
        }
    };
    let bytes = number * scale;
    if bytes > MAX_RATE_LIMIT as f64 {
        return Err(format!(
            "{} is above the maximum ({}/s)",
            human(bytes as u64),
            human(MAX_RATE_LIMIT)
        ));
    }
    let bytes = bytes.round() as u64;
    if bytes != 0 && bytes < MIN_RATE_LIMIT {
        return Err(format!(
            "{} is below the minimum ({}/s). Use Off to disable the limit.",
            human(bytes),
            human(MIN_RATE_LIMIT)
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_units_off_and_bare_numbers() {
        assert_eq!(parse("off").unwrap(), 0);
        assert_eq!(parse("OFF").unwrap(), 0);
        assert_eq!(parse("0").unwrap(), 0);
        assert_eq!(parse("512 KB").unwrap(), 512 * 1024);
        assert_eq!(parse("512kb/s").unwrap(), 512 * 1024);
        assert_eq!(parse("1.5 MB").unwrap(), 1024 * 1024 * 3 / 2);
        assert_eq!(parse("2M").unwrap(), 2 * 1024 * 1024);
        assert_eq!(parse("1 gb").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(
            parse(" 65536 ").unwrap(),
            64 * 1024,
            "bare value is bytes/s"
        );
    }

    #[test]
    fn rejects_unusable_input_with_actionable_messages() {
        let too_low = parse("1 kb").unwrap_err();
        assert!(too_low.contains("below the minimum"), "{too_low}");
        assert!(too_low.contains("Off"), "points at the way out: {too_low}");
        let nonsense = parse("banana").unwrap_err();
        assert!(nonsense.contains("number"), "{nonsense}");
        let bad_unit = parse("10 TB").unwrap_err();
        assert!(bad_unit.contains("Unknown unit"), "{bad_unit}");
        assert!(parse("").is_err(), "empty input must not silently disable");
        let huge = parse("4096 GB").unwrap_err();
        assert!(huge.contains("above the maximum"), "{huge}");
    }

    #[test]
    fn labels_read_like_the_proxy_indicator() {
        assert_eq!(Limit::default().label(), "Limit: Off");
        assert_eq!(
            Limit {
                bytes_per_second: 512 * 1024
            }
            .label(),
            "Limit: 512 KB/s"
        );
        assert_eq!(human(1536), "1.5 KB");
        assert_eq!(human(1024), "1 KB");
        assert_eq!(human(1024 * 1024), "1 MB");
        assert_eq!(human(900), "900 B");
    }
}
