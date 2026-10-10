//! One-shot GPU backend probe (offline diagnostic; no config/secret/session I/O).
fn main() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        for (name, backend) in [
            ("PRIMARY", eframe::wgpu::Backends::PRIMARY),
            ("DX12", eframe::wgpu::Backends::DX12),
            ("VULKAN", eframe::wgpu::Backends::VULKAN),
            ("GL", eframe::wgpu::Backends::GL),
        ] {
            let inst = eframe::wgpu::Instance::new(eframe::wgpu::InstanceDescriptor {
                backends: backend,
                ..Default::default()
            });
            let adapters = inst.enumerate_adapters(backend);
            println!("{name}: {} adapter(s)", adapters.len());
            for a in adapters {
                let info = a.get_info();
                println!("   backend={:?} name={} device={:?}", info.backend, info.name, info.device_type);
            }
        }
    });
}
