//! Fixed-size, smoothly moving activity cues; no blinking text or layout jitter.
use crate::theme::*;
use eframe::egui;
use std::f32::consts::TAU;

fn orbit_points(center: egui::Pos2, radius: f32, now: f64) -> [egui::Pos2; 8] {
    let phase = (now.rem_euclid(1.4) / 1.4) as f32 * TAU;
    std::array::from_fn(|i| {
        center
            + egui::vec2(
                (phase - i as f32 * 0.32).cos(),
                (phase - i as f32 * 0.32).sin(),
            ) * radius
    })
}

pub fn orbit(ui: &mut egui::Ui, size: f32, now: f64) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let radius = size * 0.34;
    ui.painter().circle_stroke(
        rect.center(),
        radius,
        egui::Stroke::new(2.0_f32, SOFT_ACCENT),
    );
    for (i, pos) in orbit_points(rect.center(), radius, now)
        .into_iter()
        .enumerate()
        .rev()
    {
        ui.painter().circle_filled(
            pos,
            1.5 + (7 - i) as f32 * 0.18,
            TEAL.gamma_multiply(1.0 - i as f32 * 0.085),
        );
    }
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(33));
}
/// A quiet breathing marker for a live reasoning section. The pulse stays in the
/// header so expanding/collapsing never causes layout jitter.
pub fn pulse_in(ui: &mut egui::Ui, rect: egui::Rect, now: f64, color: egui::Color32) {
    let phase = (now.rem_euclid(1.25) / 1.25 * TAU as f64) as f32;
    let breath = (phase.sin() + 1.0) * 0.5;
    let center = egui::pos2(rect.right() - 12.0, rect.center().y);
    let radius = 2.2 + breath * 1.6;
    ui.painter()
        .circle_filled(center, radius, color.gamma_multiply(0.72 + breath * 0.28));
    ui.painter().circle_stroke(
        center,
        radius + 2.0 + breath * 2.0,
        egui::Stroke::new(1.0_f32, color.gamma_multiply(0.18 + breath * 0.22)),
    );
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(33));
}
/// A restrained moving highlight along a running tool card. This is more
/// informative than blinking the whole card and does not change its height.
pub fn sweep(ui: &mut egui::Ui, rect: egui::Rect, now: f64, color: egui::Color32) {
    let left = rect.left() + 12.0;
    let right = rect.right() - 12.0;
    if right <= left {
        return;
    }
    let phase = (now.rem_euclid(1.8) / 1.8) as f32;
    let x = egui::lerp(left..=right, phase);
    let y = rect.top() + 1.5;
    ui.painter().line_segment(
        [egui::pos2(left, y), egui::pos2(right, y)],
        egui::Stroke::new(1.0_f32, color.gamma_multiply(0.18)),
    );
    ui.painter().circle_filled(egui::pos2(x, y), 2.0, color);
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(33));
}

pub fn waiting_card(ui: &mut egui::Ui, now: f64) {
    egui::Frame::none()
        .fill(PANEL_2)
        .rounding(20.0)
        .inner_margin(egui::Margin::symmetric(20.0, 14.0))
        .show(ui, |ui| {
            ui.set_width((ui.available_width() - 1.0).max(120.0));
            ui.horizontal(|ui| {
                orbit(ui, 36.0, now);
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new("Feather is working")
                            .size(15.0)
                            .strong()
                            .color(INK),
                    );
                    ui.label(
                        egui::RichText::new("Preparing the next response · Esc to stop")
                            .size(12.0)
                            .color(MUTED),
                    );
                });
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn orbit_moves_continuously_with_constant_bounds() {
        let center = egui::pos2(40.0, 40.0);
        let a = orbit_points(center, 12.0, 0.0);
        let b = orbit_points(center, 12.0, 0.3);
        assert_ne!(a, b);
        for point in a.into_iter().chain(b) {
            assert!(((point - center).length() - 12.0).abs() < 0.001);
        }
    }
    #[test]
    fn working_badge_keeps_layout_fixed_across_animation_frames() {
        let ctx = egui::Context::default();
        let mut sizes = Vec::new();
        for now in [0.0, 0.2, 0.5, 1.0] {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let response = ui.scope(|ui| waiting_card(ui, now));
                    sizes.push(response.response.rect.size());
                });
            });
        }
        assert!(sizes.windows(2).all(|pair| pair[0] == pair[1]));
    }
}
