//! Renderer policy and bounds. Windows CPU never probes a GPU. The accelerated
//! path is native Direct3D 12 (Vulkan only by explicit override), NOT the WGL context-rebind path
//! observed spinning inside NVIDIA on a stalled desktop. See gpu-stall-019.md.
use eframe::{egui, egui_wgpu, wgpu, HardwareAcceleration, NativeOptions, Renderer};

pub const FONT_TEXTURE_SIDE: usize = 4096;
pub const SHAPING_REPAINT_DELAY: std::time::Duration = std::time::Duration::from_millis(33);
fn native_backends(requested: Option<&str>) -> wgpu::Backends {
    match requested {
        Some("dx12") => wgpu::Backends::DX12,
        Some("vulkan") => wgpu::Backends::VULKAN,
        _ if cfg!(windows) => wgpu::Backends::DX12,
        _ => wgpu::Backends::PRIMARY,
    }
}
fn renderer_policy(requested: Option<&str>) -> Renderer {
    #[cfg(windows)]
    {
        let _ = requested;
        Renderer::Wgpu
    }
    #[cfg(not(windows))]
    {
        if matches!(requested, Some("wgpu" | "dx12" | "vulkan")) {
            Renderer::Wgpu
        } else {
            Renderer::Glow
        }
    }
}
#[cfg(test)]
pub fn use_software(requested: Option<&str>) -> bool {
    match requested {
        Some("cpu" | "software") => true,
        Some("glow" | "opengl" | "wgpu" | "dx12" | "vulkan") => false,
        _ => cfg!(windows),
    }
}
pub fn apply(options: &mut NativeOptions) {
    let requested = std::env::var("FEATHER_GUI_RENDERER").ok();
    options.renderer = renderer_policy(requested.as_deref());
    options.vsync = true;
    options.multisampling = 0;
    if options.renderer == Renderer::Wgpu {
        options.hardware_acceleration = HardwareAcceleration::Required;
        options.wgpu_options = egui_wgpu::WgpuConfiguration {
            supported_backends: native_backends(requested.as_deref()),
            // FIFO belongs to the native swap chain. Two frames avoids the
            // Some(1) submit->present serialization without queueing far ahead.
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: Some(2),
            power_preference: wgpu::PowerPreference::LowPower,
            device_descriptor: std::sync::Arc::new(|_| wgpu::DeviceDescriptor {
                label: Some("Feather native UI device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits {
                    max_texture_dimension_2d: FONT_TEXTURE_SIDE as u32,
                    ..Default::default()
                },
                memory_hints: wgpu::MemoryHints::MemoryUsage,
            }),
            ..Default::default()
        };
    } else {
        options.hardware_acceleration = HardwareAcceleration::Preferred;
    }
}
pub fn bound_font_texture(input: &mut egui::RawInput) {
    input.max_texture_side = Some(
        input
            .max_texture_side
            .unwrap_or(FONT_TEXTURE_SIDE)
            .min(FONT_TEXTURE_SIDE),
    );
    // Feather is demand-driven, not a refresh-rate game. egui subtracts
    // predicted_dt from request_repaint_after; the default 16.7ms turned our
    // 33ms animation timer into ~70Hz work on a 165Hz monitor. Keep the stated
    // maintenance delay rather than eagerly advancing every scheduled repaint.
    input.predicted_dt = 0.0;
}
pub fn report_adapter(cc: &eframe::CreationContext<'_>) {
    let Ok(path) = std::env::var("FEATHER_GUI_GRAPHICS_REPORT") else {
        return;
    };
    let report = if let Some(state) = &cc.wgpu_render_state {
        let info = state.adapter.get_info();
        format!("renderer=wgpu\nbackend={:?}\nadapter={}\ndevice_type={:?}\nfont_texture_side={}\nframe_latency=2\npresent_mode=FIFO\n",info.backend,info.name,info.device_type,FONT_TEXTURE_SIDE)
    } else {
        format!("renderer=glow\nfont_texture_side={}\n", FONT_TEXTURE_SIDE)
    };
    if let Some(parent) = std::path::Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, format!("{report}pid={}\n", std::process::id()));
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cpu_default_does_not_probe_gpu_and_windows_acceleration_excludes_gl() {
        assert!(use_software(Some("cpu")));
        assert!(!use_software(Some("wgpu")));
        assert_eq!(use_software(None), cfg!(windows));
        if cfg!(windows) {
            for requested in [None, Some("glow"), Some("opengl"), Some("wgpu")] {
                assert_eq!(renderer_policy(requested), Renderer::Wgpu);
            }
            assert!(!native_backends(None).contains(wgpu::Backends::GL));
            assert!(native_backends(None).contains(wgpu::Backends::DX12));
        }
    }
    #[test]
    fn font_limit_caps_hardware_reported_size() {
        for reported in [None, Some(1024), Some(8192), Some(16384)] {
            let mut input = egui::RawInput {
                max_texture_side: reported,
                ..Default::default()
            };
            bound_font_texture(&mut input);
            assert_eq!(
                input.max_texture_side,
                Some(reported.unwrap_or(FONT_TEXTURE_SIDE).min(FONT_TEXTURE_SIDE))
            );
            assert_eq!(
                input.predicted_dt, 0.0,
                "reactive timers must not be accelerated by the default vsync prediction"
            );
        }
    }
    #[test]
    fn shaping_repaint_is_frame_paced() {
        assert!(SHAPING_REPAINT_DELAY >= std::time::Duration::from_millis(30));
    }
}
