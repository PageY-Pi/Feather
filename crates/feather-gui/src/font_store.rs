//! Process-lifetime, shared system-font buffers. egui 0.29 deep-clones owned
//! FontData twice (Context -> FontsImpl -> ab_glyph); borrowed data is shared.
//! OnceLock owns each buffer for the lifetime egui's borrowed FontRef requires.
use std::sync::OnceLock;

pub struct Faces {
    pub ui: Option<&'static [u8]>,
    pub mono: Option<&'static [u8]>,
    pub cjk: Option<(&'static str, &'static [u8])>,
}
static UI: OnceLock<Option<Vec<u8>>> = OnceLock::new();
static MONO: OnceLock<Option<Vec<u8>>> = OnceLock::new();
static CJK: OnceLock<Option<(&'static str, Vec<u8>)>> = OnceLock::new();

fn read_first(paths: &[&str]) -> Option<Vec<u8>> {
    paths
        .iter()
        .find_map(|path| std::fs::read(path).ok().filter(|v| !v.is_empty()))
}
pub fn faces() -> Faces {
    let ui = UI
        .get_or_init(|| {
            read_first(&[
                "C:/Windows/Fonts/segoeui.ttf",
                "C:/Windows/Fonts/SEGOEUI.TTF",
            ])
        })
        .as_deref();
    let mono = MONO
        .get_or_init(|| {
            read_first(&[
                "C:/Windows/Fonts/CascadiaMono.ttf",
                "C:/Windows/Fonts/cascadiamono.ttf",
                "C:/Windows/Fonts/consola.ttf",
                "C:/Windows/Fonts/Consolas.ttf",
                "C:/Windows/Fonts/consolab.ttf",
            ])
        })
        .as_deref();
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
            .find_map(|(name, path)| {
                std::fs::read(path)
                    .ok()
                    .filter(|v| !v.is_empty())
                    .map(|v| (name, v))
            })
        })
        .as_ref()
        .map(|(name, data)| (*name, data.as_slice()));
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
    fn missing_fonts_are_nonfatal() {
        assert!(read_first(&["missing-feather-test-font-file.ttf"]).is_none());
    }
}
