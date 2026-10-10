//! Windows 11 DWM rounds the native borderless window without masking input
//! or introducing transparent regions. Older platforms keep rounded egui cards.
use eframe::egui;
// Stable focus/accessibility ordering follows the visual left-to-right order.
pub fn add_id(ui: &egui::Ui) -> egui::Id {
    ui.id().with("add-api")
}
pub fn control_id(ui: &egui::Ui, control: Control) -> egui::Id {
    ui.id().with(match control {
        Control::Minimize => "Minimize",
        Control::Maximize | Control::Restore => "Maximize",
        Control::Close => "Close",
    })
}
#[derive(Clone, Copy)]
pub enum Control {
    Minimize,
    Maximize,
    Restore,
    Close,
}
/// Identical logical hit targets; vector symbols avoid Unicode fallback metrics.
pub fn control(ui: &mut egui::Ui, rect: egui::Rect, control: Control) -> egui::Response {
    let label = match control {
        Control::Minimize => "Minimize",
        Control::Maximize => "Maximize",
        Control::Restore => "Restore window",
        Control::Close => "Close",
    };
    let response = ui.interact(rect, control_id(ui, control), egui::Sense::click());
    if std::env::var("FEATHER_UI_PREVIEW").ok().as_deref()==Some("1") {
        if let Ok(dir)=std::env::var("FEATHER_UI_PREVIEW_RENDERER_HITS") {
            let _=std::fs::create_dir_all(&dir);
            let _=std::fs::write(std::path::Path::new(&dir).join(format!("{label}.txt")),format!("{} {} ppp={} pointer={:?} hover={} clicked={}",rect.center().x,rect.center().y,ui.ctx().pixels_per_point(),ui.input(|i|i.pointer.latest_pos()),response.hovered(),response.clicked()));
        }
    }
    if response.hovered() && ui.input(|i| i.pointer.time_since_last_movement() > 0.5) {
        // Fixed inward position: native chrome lives at the viewport edge, where
        // pointer-attached tooltips can get cut off during their sizing pass.
        let position = egui::pos2(
            (rect.right() - 100.0).max(ui.ctx().screen_rect().left() + 8.0),
            rect.bottom() + 14.0,
        );
        egui::show_tooltip_at(ui.ctx(), ui.layer_id(), response.id, position, |ui| {
            ui.set_max_width(96.0);
            ui.label(label);
        });
    }
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    let destructive = matches!(control, Control::Close);
    let fill = if response.is_pointer_button_down_on() {
        if destructive {
            egui::Color32::from_rgb(245, 216, 219)
        } else {
            crate::theme::SOFT_ACCENT
        }
    } else if response.hovered() {
        if destructive {
            egui::Color32::from_rgb(250, 233, 235)
        } else {
            crate::theme::SIDEBAR
        }
    } else {
        egui::Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 9.0, fill);
    if response.has_focus() {
        ui.painter()
            .rect_stroke(rect, 9.0, egui::Stroke::new(1.0_f32, crate::theme::TEAL));
    }
    let color = if destructive && response.hovered() {
        crate::theme::RED
    } else {
        crate::theme::INK
    };
    let stroke = egui::Stroke::new(1.8_f32, color);
    let center = rect.center();
    let p = ui.painter();
    match control {
        Control::Minimize => {
            p.line_segment(
                [
                    center + egui::vec2(-5.0, 2.0),
                    center + egui::vec2(5.0, 2.0),
                ],
                stroke,
            );
        }
        Control::Maximize => {
            p.rect_stroke(
                egui::Rect::from_center_size(center, egui::vec2(10.0, 10.0)),
                1.0,
                stroke,
            );
        }
        Control::Restore => {
            let front =
                egui::Rect::from_center_size(center + egui::vec2(-1.5, 1.5), egui::vec2(8.0, 8.0));
            p.line_segment(
                [
                    center + egui::vec2(-3.0, -5.5),
                    center + egui::vec2(5.5, -5.5),
                ],
                stroke,
            );
            p.line_segment(
                [
                    center + egui::vec2(5.5, -5.5),
                    center + egui::vec2(5.5, 3.0),
                ],
                stroke,
            );
            p.rect_stroke(front, 1.0, stroke);
        }
        Control::Close => {
            p.line_segment(
                [
                    center + egui::vec2(-4.0, -4.0),
                    center + egui::vec2(4.0, 4.0),
                ],
                stroke,
            );
            p.line_segment(
                [
                    center + egui::vec2(4.0, -4.0),
                    center + egui::vec2(-4.0, 4.0),
                ],
                stroke,
            );
        }
    }
    response
}
pub fn add_api(ui: &mut egui::Ui, rect: egui::Rect) -> egui::Response {
    let response = ui
        .interact(rect, add_id(ui), egui::Sense::click())
        .on_hover_text("Add an API · manage models and accounts");
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Add API"));
    let fill = if response.is_pointer_button_down_on() {
        egui::Color32::from_rgb(192, 216, 196)
    } else if response.hovered() {
        egui::Color32::from_rgb(205, 225, 209)
    } else {
        crate::theme::PANEL_2
    };
    let p = ui.painter();
    p.rect_filled(rect, 11.0, fill);
    p.rect_stroke(rect,11.0,egui::Stroke::new(1.0_f32,crate::theme::BORDER));
    if response.has_focus() {
        p.rect_stroke(rect, 11.0, egui::Stroke::new(1.0_f32, crate::theme::TEAL));
    }
    let c = egui::pos2(rect.left() + 17.0, rect.center().y);
    let stroke = egui::Stroke::new(1.5_f32, crate::theme::INK);
    p.line_segment([c - egui::vec2(4.0, 0.0), c + egui::vec2(4.0, 0.0)], stroke);
    p.line_segment([c - egui::vec2(0.0, 4.0), c + egui::vec2(0.0, 4.0)], stroke);
    p.text(
        egui::pos2(rect.left() + 29.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        "Add API",
        egui::FontId::proportional(13.0),
        crate::theme::INK,
    );
    response
}
pub fn round_native_window(window: &impl raw_window_handle::HasWindowHandle) {
    #[cfg(windows)]
    {
        use raw_window_handle::{RawWindowHandle};
        if let Ok(handle) = window.window_handle() {
            if let RawWindowHandle::Win32(win) = handle.as_raw() {
                let preference = 2u32; // DWMWCP_ROUND
                                       // SAFETY: HWND is borrowed from the live eframe window; DWM
                                       // copies a correctly sized u32 synchronously, retaining no pointer.
                unsafe {
                    let _ = windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute(
                        win.hwnd.get() as _,
                        33,
                        &preference as *const u32 as _,
                        std::mem::size_of::<u32>() as u32,
                    );
                }
            }
        }
    }
    #[cfg(not(windows))]
    let _ = window;
}
