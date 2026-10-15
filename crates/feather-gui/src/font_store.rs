//! Process-lifetime, shared system-font buffers. egui 0.29 deep-clones owned
//! FontData twice (Context -> FontsImpl -> ab_glyph); borrowed data is shared.
//! OnceLock owns each buffer for the lifetime egui's borrowed FontRef requires.
//!
//! The files are mapped read-only rather than copied onto the heap. A heap copy
//! commits and touches every byte up front — the CJK face alone is ~19 MB of
//! private memory held for the whole session — whereas mapped pages are
//! file-backed: only the tables and glyphs actually used get faulted in, the OS
//! is free to evict them under pressure, and they are shared with any other
//! process mapping the same font. That matters most when Feather runs alongside
//! a memory-hungry foreground app.
use memmap2::Mmap;
use std::sync::OnceLock;

pub struct Faces {
    pub ui: Option<&'static [u8]>,
    pub mono: Option<&'static [u8]>,
    pub cjk: Option<(&'static str, &'static [u8])>,
}

/// A read-only mapping plus the family name egui registers it under.
type Mapped = Mmap;

static UI: OnceLock<Option<Mapped>> = OnceLock::new();
static MONO: OnceLock<Option<Mapped>> = OnceLock::new();
static CJK: OnceLock<Option<(&'static str, Mapped)>> = OnceLock::new();

/// Map the first candidate that exists, is non-empty and can be mapped. An
/// empty file cannot be mapped and a locked or malformed one must not abort
/// startup: a missing face only costs that family's glyphs.
fn map_first(paths: &[&str]) -> Option<Mapped> {
    paths.iter().find_map(|path| {
        let file = std::fs::File::open(path).ok()?;
        if file.metadata().ok()?.len() == 0 {
            return None;
        }
        // SAFETY: the mapping is read-only and never exposed mutably, so the
        // file cannot be mutated through it. System font files are not written
        // by this process, and Windows keeps existing mappings valid if they
        // are replaced on disk.
        unsafe { Mmap::map(&file) }.ok()
    })
}

/// Faces in preference order: the first that maps wins.
pub fn faces() -> Faces {
    let ui = UI
        .get_or_init(|| {
            map_first(&[
                "C:/Windows/Fonts/segoeui.ttf",
                "C:/Windows/Fonts/SEGOEUI.TTF",
            ])
        })
        .as_ref()
        .map(|map| &map[..]);
    let mono = MONO
        .get_or_init(|| {
            map_first(&[
                "C:/Windows/Fonts/CascadiaMono.ttf",
                "C:/Windows/Fonts/cascadiamono.ttf",
                "C:/Windows/Fonts/consola.ttf",
                "C:/Windows/Fonts/Consolas.ttf",
                "C:/Windows/Fonts/consolab.ttf",
            ])
        })
        .as_ref()
        .map(|map| &map[..]);
    let cjk = CJK
        .get_or_init(|| {
            [
                ("feather_cjk_yahei", "C:/Windows/Fonts/msyh.ttc"),
                ("feather_cjk_yahei_light", "C:/Windows/Fonts/msyhl.ttc"),
                ("feather_cjk_deng", "C:/Windows/Fonts/Deng.ttf"),
                ("feather_cjk_simhei", "C:/Windows/Fonts/simhei.ttf"),
                ("feather_cjk_simsun", "C:/Windows/Fonts/simsun.ttc"),
            ]
            .into_iter()
            .find_map(|(name, path)| map_first(&[path]).map(|map| (name, map)))
        })
        .as_ref()
        .map(|(name, map)| (*name, &map[..]));
    Faces { ui, mono, cjk }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fonts_are_cached_once_and_clones_share_the_same_bytes() {
        for _ in 0..3 {
            let a = faces();
            let b = faces();
            if let (Some(a), Some(b)) = (a.cjk, b.cjk) {
                assert_eq!(a.0, b.0);
                assert_eq!(a.1.as_ptr(), b.1.as_ptr());
            }
            for face in [a.ui, a.mono, a.cjk.map(|v| v.1)].into_iter().flatten() {
                let data = eframe::egui::FontData::from_static(face);
                assert!(matches!(data.font, std::borrow::Cow::Borrowed(_)));
                assert_eq!(data.font.as_ptr(), data.clone().font.as_ptr());
            }
        }
    }
    #[test]
    fn missing_or_empty_fonts_are_nonfatal() {
        assert!(map_first(&["missing-feather-test-font-file.ttf"]).is_none());
        // An empty file exists but cannot be mapped; it must fall through
        // rather than panic or abort startup.
        let empty = std::env::temp_dir().join(feather_core::provider::gen_id("empty-font"));
        std::fs::write(&empty, b"").unwrap();
        let path = empty.to_string_lossy().into_owned();
        assert!(
            map_first(&[path.as_str(), "missing-feather-test-font-file.ttf"]).is_none(),
            "an empty candidate must fall through to the next path"
        );
        std::fs::remove_file(&empty).ok();
    }
    /// The mapping must expose the real bytes of the file it points at.
    #[test]
    fn mapped_bytes_match_the_file() {
        let path = std::env::temp_dir().join(feather_core::provider::gen_id("font-bytes"));
        let payload: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &payload).unwrap();
        let mapped = map_first(&[path.to_string_lossy().as_ref()]).expect("maps");
        assert_eq!(&mapped[..], &payload[..]);
        std::fs::remove_file(&path).ok();
    }
}
