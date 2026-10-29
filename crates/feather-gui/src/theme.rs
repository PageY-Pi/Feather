// Feather GUI theme.
//
// A light, warm, friendly egui skin: pearl and sage canvas, white cards,
// lavender and soft-sage accents, dark readable type, and a clean feather
// mark shared with the native Windows icon. No web views or runtime scripts.

use eframe::egui;
use egui::{
    Color32, Context, FontData, FontDefinitions, FontFamily, FontId, Margin, Painter, Rect,
    Response, Rounding, Sense, Shadow, Stroke, TextStyle, Ui,
};

// ---------------------------------------------------------------------------
// palette
// ---------------------------------------------------------------------------

/// Dark sage. Primary ink for headings, icons and strong marks.
pub const INK: Color32 = Color32::from_rgb(0x2F, 0x4A, 0x3F);
/// Dark sage accent. Links, rails and secondary emphasis.
pub const TEAL: Color32 = Color32::from_rgb(0x3E, 0x6B, 0x57);
/// Deeper sage accent. A darker sibling of TEAL for readable small marks.
pub const TEAL_BRIGHT: Color32 = Color32::from_rgb(0x33, 0x58, 0x45);
/// Warm off-white. Main application canvas.
pub const PANEL: Color32 = Color32::from_rgb(0xFA, 0xF6, 0xF0);
/// Clean white. Cards and raised surfaces.
pub const PANEL_2: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
/// Warm pearl. Sidebar and recessed rails.
pub const SIDEBAR: Color32 = Color32::from_rgb(0xF2, 0xED, 0xE6);
/// Soft sage. User chat bubbles.
pub const USER_BUBBLE: Color32 = Color32::from_rgb(0xE6, 0xEF, 0xE5);
/// Soft lavender-gray. Tool output blocks.
pub const TOOL_BG: Color32 = Color32::from_rgb(0xF0, 0xED, 0xF6);
/// Gentle lavender. Chips, empty states and mascot highlights.
pub const LAVENDER: Color32 = Color32::from_rgb(0xE6, 0xE0, 0xF3);
/// Subtle sage. Selection and hover washes.
pub const SOFT_ACCENT: Color32 = Color32::from_rgb(0xDC, 0xE9, 0xDE);
/// Charcoal. Primary text on every light surface.
pub const TEXT: Color32 = Color32::from_rgb(0x33, 0x30, 0x2E);
/// Readable warm gray. Secondary text, at least 4.5:1 on every surface.
pub const MUTED: Color32 = Color32::from_rgb(0x67, 0x61, 0x5A);
/// Dark rose. Errors and destructive marks.
pub const RED: Color32 = Color32::from_rgb(0xA8, 0x40, 0x55);
/// Deep amber. Warnings, readable on light surfaces.
pub const WARN: Color32 = Color32::from_rgb(0x7C, 0x56, 0x0F);
/// Whisper-soft edge tint. Used instead of harsh outlines.
pub const BORDER: Color32 = Color32::from_rgb(0xE8, 0xE2, 0xD9);
pub const ERROR_BG: Color32 = Color32::from_rgb(0xFC, 0xED, 0xEE);
pub const TOOL_ERROR_BG: Color32 = Color32::from_rgb(0xFA, 0xEE, 0xEF);

// ---------------------------------------------------------------------------
// typography
// ---------------------------------------------------------------------------

/// Install Segoe UI as the primary proportional face, Microsoft YaHei and
/// other Windows CJK faces as fallback, and Cascadia Mono or Consolas for code.
/// Missing files are skipped.
pub fn install_fonts(ctx: &Context) {
    ctx.set_fonts(font_definitions());
}

fn font_definitions() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let faces = crate::font_store::faces();
    if let Some(data) = faces.ui {
        fonts
            .font_data
            .insert("feather_ui".into(), FontData::from_static(data));
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, "feather_ui".into());
    }
    if let Some(data) = faces.mono {
        fonts
            .font_data
            .insert("feather_mono".into(), FontData::from_static(data));
        fonts
            .families
            .entry(FontFamily::Monospace)
            .or_default()
            .insert(0, "feather_mono".into());
    }
    if let Some((name, data)) = faces.cjk {
        fonts
            .font_data
            .insert(name.into(), FontData::from_static(data));
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push(name.into());
        }
    }
    fonts
}

#[cfg(test)]
fn first_readable(paths: &[&str]) -> Option<Vec<u8>> {
    paths.iter().find_map(|path| std::fs::read(path).ok())
}

// ---------------------------------------------------------------------------
// elevation
// ---------------------------------------------------------------------------

/// Keep normal windows readable while giving large desktops a balanced column.
pub fn content_width(available: f32) -> f32 {
    available.min((available * 0.62).clamp(790.0, 1080.0))
}
/// A very soft drop shadow: low opacity, no darkness. Never a hard black slab.
pub fn soft_shadow() -> Shadow {
    Shadow {
        offset: egui::vec2(0.0, 3.0),
        blur: 14.0,
        spread: 0.0,
        color: Color32::from_rgba_unmultiplied(0x5A, 0x51, 0x46, 15),
    }
}

// ---------------------------------------------------------------------------
// global theme
// ---------------------------------------------------------------------------

/// Apply the light feather skin, typography and comfy spacing to a context.
pub fn apply_theme(ctx: &Context) {
    ctx.set_theme(egui::Theme::Light);
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Body, FontId::proportional(15.5)),
            (TextStyle::Small, FontId::proportional(12.5)),
            (TextStyle::Button, FontId::proportional(15.0)),
            (TextStyle::Heading, FontId::proportional(24.0)),
            (TextStyle::Monospace, FontId::monospace(13.5)),
        ]
        .into_iter()
        .collect();

        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.window_margin = Margin::same(12.0);
        style.spacing.menu_margin = Margin::same(8.0);
        style.spacing.indent = 18.0;
        style.spacing.interact_size.y = 26.0;
        // A reserved, visible rail makes overflow discoverable and avoids text
        // sliding beneath a floating thumb in dense forms or narrow workspaces.
        style.spacing.scroll = egui::style::ScrollStyle::thin();
        style.spacing.scroll.floating_width = 5.0;
        style.spacing.scroll.bar_width = 8.0;
        style.spacing.scroll.floating_allocated_width = 14.0;
        style.spacing.scroll.bar_inner_margin = 6.0;
        style.spacing.scroll.foreground_color = true;
        style.spacing.scroll.dormant_handle_opacity = 0.52;
        style.spacing.scroll.active_handle_opacity = 0.55;
        style.spacing.scroll.interact_handle_opacity = 0.8;
        style.spacing.scroll.dormant_background_opacity = 0.15;

        let v = &mut style.visuals;
        v.dark_mode = false;
        v.override_text_color = Some(TEXT);

        v.panel_fill = PANEL;
        v.window_fill = PANEL_2;
        v.extreme_bg_color = PANEL_2;
        v.faint_bg_color = SIDEBAR;
        v.code_bg_color = TOOL_BG;
        v.hyperlink_color = TEAL;
        v.warn_fg_color = WARN;
        v.error_fg_color = RED;

        v.window_stroke = Stroke::NONE;
        v.window_rounding = Rounding::same(14.0);
        v.menu_rounding = Rounding::same(12.0);
        v.window_shadow = soft_shadow();
        v.popup_shadow = soft_shadow();
        v.striped = true;
        v.indent_has_left_vline = false;

        v.selection.bg_fill = SOFT_ACCENT;
        v.selection.stroke = Stroke::new(1.0_f32, TEAL);

        let round = Rounding::same(10.0);
        v.widgets.noninteractive.rounding = round;
        v.widgets.noninteractive.bg_fill = PANEL_2;
        v.widgets.noninteractive.weak_bg_fill = PANEL_2;
        v.widgets.noninteractive.bg_stroke = Stroke::NONE;
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.2_f32, TEXT);

        v.widgets.inactive.rounding = round;
        v.widgets.inactive.bg_fill = PANEL_2;
        v.widgets.inactive.weak_bg_fill = SIDEBAR;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, BORDER);
        v.widgets.inactive.fg_stroke = Stroke::new(1.2_f32, TEXT);

        v.widgets.hovered.rounding = round;
        v.widgets.hovered.bg_fill = SIDEBAR;
        v.widgets.hovered.weak_bg_fill = SOFT_ACCENT;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, TEAL);
        v.widgets.hovered.fg_stroke = Stroke::new(1.2_f32, INK);

        v.widgets.active.rounding = round;
        v.widgets.active.bg_fill = SOFT_ACCENT;
        v.widgets.active.weak_bg_fill = SOFT_ACCENT;
        v.widgets.active.bg_stroke = Stroke::new(1.0_f32, TEAL);
        v.widgets.active.fg_stroke = Stroke::new(1.2_f32, INK);

        v.widgets.open.rounding = round;
        v.widgets.open.bg_fill = LAVENDER;
        v.widgets.open.weak_bg_fill = LAVENDER;
        v.widgets.open.bg_stroke = Stroke::new(1.0_f32, TEAL);
        v.widgets.open.fg_stroke = Stroke::new(1.2_f32, INK);
    });
}

// ---------------------------------------------------------------------------
// mascot
// ---------------------------------------------------------------------------

/// Allocate a square and paint the minimal feather mark inside it.
pub fn mascot(ui: &mut Ui, size: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), Sense::hover());
    paint_mascot(ui.painter(), rect);
    response
}

/// Paint the same minimal feather used by the Windows application icon.
/// One shared texture replaces procedural cutouts that could escape the vane.
pub fn paint_mascot(painter: &Painter, rect: Rect) {
    let id = egui::Id::new("feather-mark-texture-v2");
    let texture = painter
        .ctx()
        .data_mut(|d| d.get_temp::<egui::TextureHandle>(id))
        .unwrap_or_else(|| {
            let image = image::load_from_memory(include_bytes!("../assets/feather-logo.png"))
                .expect("embedded feather mark")
                .to_rgba8();
            let size = [image.width() as usize, image.height() as usize];
            let texture = painter.ctx().load_texture(
                "feather-mark",
                egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw()),
                egui::TextureOptions::LINEAR,
            );
            painter
                .ctx()
                .data_mut(|d| d.insert_temp(id, texture.clone()));
            texture
        });
    let side = rect.width().min(rect.height());
    painter.image(
        texture.id(),
        Rect::from_center_size(rect.center(), egui::vec2(side, side)),
        Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
        Color32::WHITE,
    );
}

// ---------------------------------------------------------------------------
// contrast
// ---------------------------------------------------------------------------

/// WCAG relative luminance of a color.
#[cfg(test)]
pub fn relative_luminance(color: Color32) -> f32 {
    let lin = |value: u8| {
        let c = value as f32 / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(color.r()) + 0.7152 * lin(color.g()) + 0.0722 * lin(color.b())
}

/// WCAG contrast ratio between two colors, from 1.0 to 21.0.
#[cfg(test)]
pub fn contrast_ratio(a: Color32, b: Color32) -> f32 {
    let la = relative_luminance(a);
    let lb = relative_luminance(b);
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Every light surface a text color may land on.
#[cfg(test)]
pub fn surfaces() -> [Color32; 9] {
    [
        PANEL,
        PANEL_2,
        SIDEBAR,
        USER_BUBBLE,
        TOOL_BG,
        LAVENDER,
        SOFT_ACCENT,
        ERROR_BG,
        TOOL_ERROR_BG,
    ]
}

#[cfg(test)]
mod tests {
    #[test]
    fn feather_mark_uses_shared_texture_without_background_cutout_shapes() {
        let ctx = egui::Context::default();
        let mut textures = Vec::new();
        for _ in 0..2 {
            let output = ctx.run(Default::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    super::mascot(ui, 100.0);
                });
            });
            let meshes: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|s| {
                    if let egui::Shape::Mesh(m) = &s.shape {
                        (m.texture_id != egui::TextureId::default()).then_some(m)
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(meshes.len(), 1);
            assert_eq!(meshes[0].vertices.len(), 4);
            textures.push(meshes[0].texture_id);
        }
        assert_eq!(
            textures[0], textures[1],
            "logo texture must be reused, not uploaded per repaint"
        );
    }
    use super::*;

    const MIN: f32 = 4.5;

    fn assert_readable(name: &str, color: Color32) {
        for surface in surfaces() {
            let ratio = contrast_ratio(color, surface);
            assert!(
                ratio >= MIN,
                "{name} on {surface:?} is {ratio:.2}, below {MIN}"
            );
        }
    }

    fn old_font_definitions_for_regression() -> FontDefinitions {
        let mut fonts = FontDefinitions::default();

        if let Some(data) = first_readable(&[
            "C:/Windows/Fonts/segoeui.ttf",
            "C:/Windows/Fonts/SEGOEUI.TTF",
        ]) {
            fonts
                .font_data
                .insert("old_ui".to_owned(), FontData::from_owned(data));
            fonts
                .families
                .entry(FontFamily::Proportional)
                .or_default()
                .insert(0, "old_ui".to_owned());
        }

        if let Some(data) = first_readable(&[
            "C:/Windows/Fonts/msyh.ttc",
            "C:/Windows/Fonts/msyhl.ttc",
            "C:/Windows/Fonts/simhei.ttf",
            "C:/Windows/Fonts/simsun.ttc",
        ]) {
            fonts
                .font_data
                .insert("old_cjk".to_owned(), FontData::from_owned(data));
            for family in [FontFamily::Proportional, FontFamily::Monospace] {
                fonts
                    .families
                    .entry(family)
                    .or_default()
                    .push("old_cjk".to_owned());
            }
        }

        if let Some(data) = first_readable(&[
            "C:/Windows/Fonts/CascadiaMono.ttf",
            "C:/Windows/Fonts/cascadiamono.ttf",
            "C:/Windows/Fonts/consola.ttf",
            "C:/Windows/Fonts/Consolas.ttf",
            "C:/Windows/Fonts/consolab.ttf",
        ]) {
            fonts
                .font_data
                .insert("old_mono".to_owned(), FontData::from_owned(data));
            fonts
                .families
                .entry(FontFamily::Monospace)
                .or_default()
                .insert(0, "old_mono".to_owned());
        }

        fonts
    }

    fn assert_chinese_lays_out_to_mesh(definitions: FontDefinitions, family: FontFamily) {
        let ctx = Context::default();
        ctx.set_fonts(definitions);
        let _ = ctx.run(Default::default(), |ctx| {
            ctx.fonts(|fonts| {
                assert!(
                    fonts.has_glyphs(&FontId::new(13.0, family.clone()), "中文工具输出参数路径"),
                    "Chinese must resolve to actual glyphs, not replacement boxes"
                );
                let galley = fonts.layout_no_wrap(
                    "中文工具输出参数路径".to_owned(),
                    FontId::new(13.0, family.clone()),
                    TEXT,
                );
                assert!(
                    galley.num_vertices > 0,
                    "Chinese text must produce a non-empty glyph mesh"
                );
                assert!(
                    galley
                        .rows
                        .iter()
                        .any(|row| !row.visuals.mesh.vertices.is_empty()),
                    "Chinese text rows must contain mesh vertices"
                );
            });
        });
    }

    #[test]
    fn cjk_fallbacks_preserve_latin_monospace_first() {
        let fonts = font_definitions();
        let monospace = fonts.families.get(&FontFamily::Monospace).unwrap();
        let first_cjk = monospace
            .iter()
            .position(|name| name.starts_with("feather_cjk_"));
        let latin_mono = monospace.iter().position(|name| name == "feather_mono");

        if let (Some(first_cjk), Some(latin_mono)) = (first_cjk, latin_mono) {
            assert!(
                latin_mono < first_cjk,
                "Latin mono must stay before CJK fallback to preserve code alignment"
            );
        }
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn old_and_new_tool_card_fonts_render_chinese_meshes() {
        assert_chinese_lays_out_to_mesh(
            old_font_definitions_for_regression(),
            FontFamily::Monospace,
        );
        assert_chinese_lays_out_to_mesh(font_definitions(), FontFamily::Monospace);
        assert_chinese_lays_out_to_mesh(font_definitions(), FontFamily::Proportional);
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn cascadia_does_not_claim_tested_chinese_glyphs() {
        let Some(data) = first_readable(&[
            "C:/Windows/Fonts/CascadiaMono.ttf",
            "C:/Windows/Fonts/cascadiamono.ttf",
        ]) else {
            return;
        };
        let mut fonts = FontDefinitions::default();
        fonts
            .font_data
            .insert("only_cascadia".to_owned(), FontData::from_owned(data));
        fonts
            .families
            .entry(FontFamily::Monospace)
            .or_default()
            .insert(0, "only_cascadia".to_owned());

        let ctx = Context::default();
        ctx.set_fonts(fonts);
        let _ = ctx.run(Default::default(), |ctx| {
            ctx.fonts(|fonts| {
                assert!(
                    !fonts.has_glyphs(&FontId::monospace(13.0), "中文"),
                    "Cascadia should not be recorded as claiming the tested Chinese glyphs"
                );
            });
        });
    }
    #[test]
    fn body_text_is_readable() {
        assert_readable("TEXT", TEXT);
    }

    #[test]
    fn secondary_text_is_readable() {
        assert_readable("MUTED", MUTED);
    }

    #[test]
    fn accents_are_readable() {
        assert_readable("INK", INK);
        assert_readable("TEAL", TEAL);
        assert_readable("TEAL_BRIGHT", TEAL_BRIGHT);
    }

    #[test]
    fn status_colors_are_readable() {
        assert_readable("RED", RED);
        assert_readable("WARN", WARN);
    }

    #[test]
    fn order_is_stable() {
        assert!(contrast_ratio(TEXT, PANEL_2) > contrast_ratio(MUTED, PANEL_2));
        assert!(relative_luminance(PANEL_2) > relative_luminance(SOFT_ACCENT));
    }
}
