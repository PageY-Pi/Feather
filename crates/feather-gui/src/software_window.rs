//! Window/event-loop integration for the CPU backend. egui-winit handles the
//! same keyboard, pointer, wheel, clipboard, IME and DPI plumbing as eframe;
//! softbuffer presents CPU pixels through native window buffers, not WGL/wgpu.
use super::{egui, FeatherApp};
use crate::software_painter::Painter;
use std::{
    num::NonZeroU32,
    sync::Arc,
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowId},
};

#[derive(Default)]
struct Timing {
    frames: u64,
    total: Duration,
    max: Duration,
    raster: Duration,
    peak_vertices: usize,
    peak_atlas_texels: usize,
}
impl Timing {
    fn record(
        &mut self,
        total: Duration,
        raster: Duration,
        vertices: usize,
        delta: &egui::TexturesDelta,
    ) {
        self.frames += 1;
        self.total += total;
        self.raster += raster;
        self.max = self.max.max(total);
        self.peak_vertices = self.peak_vertices.max(vertices);
        for (_, u) in &delta.set {
            if u.pos.is_none() {
                let s = u.image.size();
                self.peak_atlas_texels = self.peak_atlas_texels.max(s[0] * s[1]);
            }
        }
    }
    fn report(&self) {
        if let Ok(path) = std::env::var("FEATHER_GUI_FRAME_REPORT") {
            if let Some(parent) = std::path::Path::new(&path).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let n = self.frames.max(1) as f64;
            let _=std::fs::write(path,format!("frames={}\nmean_frame_ms={:.3}\nmean_raster_ms={:.3}\nmax_frame_ms={:.3}\npeak_vertices={}\npeak_texture_texels={}\n",self.frames,self.total.as_secs_f64()*1000.0/n,self.raster.as_secs_f64()*1000.0/n,self.max.as_secs_f64()*1000.0,self.peak_vertices,self.peak_atlas_texels));
        }
    }
}

#[derive(Debug)]
enum Wake {
    Repaint(Instant),
    Accessibility(accesskit_winit::Event),
}
impl From<accesskit_winit::Event> for Wake {
    fn from(event: accesskit_winit::Event) -> Self {
        Self::Accessibility(event)
    }
}
struct Native {
    window: Arc<Window>,
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
    input: egui_winit::State,
    app: FeatherApp,
    painter: Painter,
    screenshot: Option<Arc<egui::ColorImage>>,
    viewport: egui::ViewportInfo,
    next_frame: Option<Instant>,
    last_frame: Instant,
    timing: Timing,
}
struct Host {
    ctx: egui::Context,
    proxy: winit::event_loop::EventLoopProxy<Wake>,
    builder: egui::ViewportBuilder,
    root: std::path::PathBuf,
    native: Option<Native>,
    failure: Option<anyhow::Error>,
}
impl Host {
    fn fail(&mut self, event_loop: &ActiveEventLoop, error: anyhow::Error) {
        self.failure = Some(error);
        event_loop.exit();
    }
    fn redraw(&mut self, event_loop: &ActiveEventLoop) -> anyhow::Result<()> {
        let Some(n) = &mut self.native else {
            return Ok(());
        };
        let size = n.window.inner_size();
        if size.width == 0 || size.height == 0 || n.window.is_minimized() == Some(true) {
            n.next_frame = None;
            return Ok(());
        }
        n.last_frame = Instant::now();
        n.next_frame = None;
        egui_winit::update_viewport_info(&mut n.viewport, &self.ctx, &n.window, false);
        let mut input = n.input.take_egui_input(&n.window);
        input
            .viewports
            .insert(egui::ViewportId::ROOT, n.viewport.clone());
        if let Some(image) = n.screenshot.take() {
            input.events.push(egui::Event::Screenshot {
                viewport_id: egui::ViewportId::ROOT,
                image,
            });
        }
        <FeatherApp as eframe::App>::raw_input_hook(&mut n.app, &self.ctx, &mut input);
        let mut out = self.ctx.run(input, |ctx| n.app.update_ui(ctx));
        if n.app.exit_ready {
            n.window.set_visible(false);
            event_loop.exit();
            return Ok(());
        }
        n.input
            .handle_platform_output(&n.window, out.platform_output);
        let jobs = self.ctx.tessellate(out.shapes, out.pixels_per_point);
        let raster_started = Instant::now();
        n.painter.paint(
            [size.width as usize, size.height as usize],
            out.pixels_per_point,
            &jobs,
            &out.textures_delta,
            crate::theme::PANEL,
        )?;
        let raster_time = raster_started.elapsed();
        n.surface
            .resize(
                NonZeroU32::new(size.width).unwrap(),
                NonZeroU32::new(size.height).unwrap(),
            )
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let mut buffer = n
            .surface
            .buffer_mut()
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        buffer.copy_from_slice(n.painter.pixels());
        buffer
            .present()
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let vertices = jobs
            .iter()
            .map(|j| match &j.primitive {
                egui::epaint::Primitive::Mesh(m) => m.vertices.len(),
                _ => 0,
            })
            .sum();
        n.timing.record(
            n.last_frame.elapsed(),
            raster_time,
            vertices,
            &out.textures_delta,
        );
        if let Some(viewport) = out.viewport_output.get_mut(&egui::ViewportId::ROOT) {
            let mut actions: egui::ahash::HashSet<egui_winit::ActionRequested> = Default::default();
            let mut close = false;
            let mut commands = Vec::new();
            for command in viewport.commands.drain(..) {
                match command {
                    egui::ViewportCommand::Close => close = true,
                    egui::ViewportCommand::CancelClose => close = false,
                    command => commands.push(command),
                }
            }
            egui_winit::process_viewport_commands(
                &self.ctx,
                &mut n.viewport,
                commands,
                &n.window,
                &mut actions,
            );
            for action in actions {
                match action {
                    egui_winit::ActionRequested::Screenshot => {
                        n.screenshot = Some(Arc::new(n.painter.screenshot()));
                        n.next_frame = Some(Instant::now());
                    }
                    egui_winit::ActionRequested::Copy => {
                        n.input.egui_input_mut().events.push(egui::Event::Copy)
                    }
                    egui_winit::ActionRequested::Cut => {
                        n.input.egui_input_mut().events.push(egui::Event::Cut)
                    }
                    egui_winit::ActionRequested::Paste => {
                        if let Some(text) = n.input.clipboard_text() {
                            n.input
                                .egui_input_mut()
                                .events
                                .push(egui::Event::Paste(text));
                        }
                    }
                }
            }
            if close {
                event_loop.exit();
            }
            let next = Instant::now().checked_add(viewport.repaint_delay);
            if let Some(next) = next {
                if n.next_frame.map_or(true, |old| next < old) {
                    n.next_frame = Some(next);
                }
            }
        }
        Ok(())
    }
}
impl ApplicationHandler<Wake> for Host {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.native.is_some() {
            return;
        }
        let result = (|| -> anyhow::Result<Native> {
            let window = Arc::new(egui_winit::create_window(
                &self.ctx,
                event_loop,
                &self.builder,
            )?);
            crate::window_chrome::round_native_window(&*window);
            let context = softbuffer::Context::new(window.clone())
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            let surface = softbuffer::Surface::new(&context, window.clone())
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            let mut input = egui_winit::State::new(
                self.ctx.clone(),
                egui::ViewportId::ROOT,
                &*window,
                Some(window.scale_factor() as f32),
                window.theme(),
                Some(crate::graphics::FONT_TEXTURE_SIDE),
            );
            input.init_accesskit(&window, self.proxy.clone());
            let mut app = FeatherApp::new_with_context(self.ctx.clone(), self.root.clone())?;
            app.frame_pacer.set_window(&*window);
            let mut viewport = egui::ViewportInfo::default();
            egui_winit::update_viewport_info(&mut viewport, &self.ctx, &window, true);
            if let Ok(path) = std::env::var("FEATHER_GUI_GRAPHICS_REPORT") {
                if let Some(parent) = std::path::Path::new(&path).parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(
                    path,
                    format!(
                        "renderer=cpu\npresentation=softbuffer\nfont_texture_side={}\npid={}\n",
                        crate::graphics::FONT_TEXTURE_SIDE,
                        std::process::id()
                    ),
                );
            }
            Ok(Native {
                window,
                surface,
                input,
                app,
                painter: Painter::default(),
                screenshot: None,
                viewport,
                next_frame: Some(Instant::now()),
                last_frame: Instant::now() - Duration::from_secs(1),
                timing: Timing::default(),
            })
        })();
        match result {
            Ok(n) => self.native = Some(n),
            Err(error) => self.fail(event_loop, error),
        }
    }
    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(n) = &mut self.native else {
            return;
        };
        if window_id != n.window.id() {
            return;
        }
        if matches!(event, WindowEvent::CloseRequested) {
            n.app.request_close();
            n.next_frame = Some(Instant::now());
            return;
        }
        let response = n.input.on_window_event(&n.window, &event);
        if matches!(event, WindowEvent::RedrawRequested) {
            if let Err(error) = self.redraw(event_loop) {
                self.fail(event_loop, error);
            }
        } else if response.repaint {
            n.next_frame = Some(Instant::now());
        }
    }
    fn user_event(&mut self, _: &ActiveEventLoop, event: Wake) {
        if let Some(n) = &mut self.native {
            let when = match event {
                Wake::Repaint(when) => when,
                Wake::Accessibility(event) => {
                    if event.window_id != n.window.id() {
                        return;
                    }
                    match event.window_event {
                        accesskit_winit::WindowEvent::InitialTreeRequested => {
                            self.ctx.enable_accesskit()
                        }
                        accesskit_winit::WindowEvent::ActionRequested(request) => {
                            n.input.on_accesskit_action_request(request)
                        }
                        accesskit_winit::WindowEvent::AccessibilityDeactivated => {
                            self.ctx.disable_accesskit()
                        }
                    }
                    Instant::now()
                }
            };
            if n.next_frame.map_or(true, |old| when < old) {
                n.next_frame = Some(when);
            }
        }
    }
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(n) = &mut self.native else {
            return;
        };
        if n.app.exit.is_some() {
            n.app.drain_events();
            n.app.poll_exit(&self.ctx);
            if n.app.exit_ready {
                n.window.set_visible(false);
                event_loop.exit();
                return;
            }
            // Minimized windows do not reliably deliver redraw requests. Close
            // cancellation must still advance without depending on paint.
            if n.window.is_minimized() != Some(true) {
                n.window.request_redraw();
            }
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                Instant::now() + Duration::from_millis(25),
            ));
            return;
        }
        if let Some(next) = n.next_frame {
            // A busy stream/hover must not saturate a CPU core or DWM. Idle
            // windows sleep indefinitely until egui/input requests a repaint.
            let next = next.max(n.last_frame + n.app.frame_pacer.interval);
            if next <= Instant::now() {
                n.window.request_redraw();
                event_loop.set_control_flow(ControlFlow::Wait);
            } else {
                event_loop.set_control_flow(ControlFlow::WaitUntil(next));
            }
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }
}
pub fn run(root: std::path::PathBuf, builder: egui::ViewportBuilder) -> anyhow::Result<()> {
    let event_loop = EventLoop::<Wake>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let ctx = egui::Context::default();
    let repaint_proxy = proxy.clone();
    ctx.set_request_repaint_callback(move |info| {
        if let Some(when) = Instant::now().checked_add(info.delay) {
            let _ = repaint_proxy.send_event(Wake::Repaint(when));
        }
    });
    let mut host = Host {
        ctx,
        proxy,
        builder,
        root,
        native: None,
        failure: None,
    };
    event_loop.run_app(&mut host)?;
    if let Some(native) = host.native.take() {
        native.window.set_visible(false);
        native.timing.report();
        drop(native); // CPU surfaces/window tear down before any restart is launched.
    }
    if let Some(error) = host.failure {
        Err(error)
    } else {
        Ok(())
    }
}
