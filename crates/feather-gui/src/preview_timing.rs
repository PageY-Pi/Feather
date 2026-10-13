//! Opt-in native GPU diagnostics; synthetic previews only. App interval includes
//! the preceding native paint/present wait unlike eframe's UI-only CPU timing.
use eframe::egui;
use std::time::{Duration, Instant};
pub struct PreviewTiming {
    active: bool,
    start: Instant,
    last: Option<Instant>,
    frame: Option<u64>,
    frames: u64,
    intervals: u64,
    sum: Duration,
    max: Duration,
}
impl Default for PreviewTiming {
    fn default() -> Self {
        Self {
            active: std::env::var("FEATHER_UI_PREVIEW").ok().as_deref() == Some("1")
                && std::env::var_os("FEATHER_GUI_FRAME_REPORT").is_some(),
            start: Instant::now(),
            last: None,
            frame: None,
            frames: 0,
            intervals: 0,
            sum: Duration::ZERO,
            max: Duration::ZERO,
        }
    }
}
impl PreviewTiming {
    pub fn tick(&mut self, ctx: &egui::Context) {
        if !self.active {
            return;
        }
        if std::env::var("FEATHER_UI_PREVIEW_PACED").ok().as_deref() == Some("1") {
            ctx.request_repaint_after(Duration::from_millis(33));
        }
        let frame = ctx.input(|i| i.time.to_bits());
        if self.frame == Some(frame) {
            return;
        }
        self.frame = Some(frame);
        let now = Instant::now();
        self.frames += 1;
        if let Some(previous) = self.last.replace(now) {
            if self.frames > 12 {
                let gap = now - previous;
                self.intervals += 1;
                self.sum += gap;
                self.max = self.max.max(gap);
            }
        }
        if self.frames == 120 {
            if let Ok(path) = std::env::var("FEATHER_GUI_FRAME_REPORT") {
                let _ = std::fs::write(
                    format!("{path}.causes"),
                    format!("{:?}", ctx.repaint_causes()),
                );
            }
        }
        if self.frames % 30 == 0 {
            self.report();
        }

        if let Some(seconds) = std::env::var("FEATHER_UI_PREVIEW_DURATION")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
        {
            if now.duration_since(self.start) >= Duration::from_secs(seconds) {
                self.report();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }
    pub fn report(&self) {
        if !self.active {
            return;
        }
        if let Ok(path) = std::env::var("FEATHER_GUI_FRAME_REPORT") {
            if let Some(parent) = std::path::Path::new(&path).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _=std::fs::write(path,format!("frames={}\nintervals={}\nsteady_mean_interval_ms={:.3}\nsteady_max_interval_ms={:.3}\nelapsed_seconds={:.3}\n",self.frames,self.intervals,self.sum.as_secs_f64()*1000.0/self.intervals.max(1) as f64,self.max.as_secs_f64()*1000.0,self.start.elapsed().as_secs_f64()));
        }
    }
}
