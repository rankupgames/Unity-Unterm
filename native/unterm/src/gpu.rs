//! Process-global GPU + font resources shared across all terminal windows.
//!
//! A terminal editor can hold many windows at once; each is its own native
//! terminal in the registry but they all render through one wgpu `Device`/
//! `Queue` and one `FontSystem`. Sharing keeps opening the Nth window cheap
//! (only a fresh IOSurface target is allocated) and lets the glyph atlas be
//! warmed once. All of this lives in `OnceLock`s so it survives — like the
//! terminal registry — across Unity C# domain reloads.

use glyphon::{Cache, FontSystem};
use std::sync::{Mutex, OnceLock};

/// sRGB target so Unity's external texture (created with `linear=false`)
/// hardware-decodes on sample. The Metal IOSurface uses `RGBA8Unorm_sRGB`.
pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    /// Shared glyphon cache (pipelines/bind layouts) for this device.
    pub cache: Cache,
}

fn init_gpu() -> Gpu {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        ..Default::default()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .expect("unterm: no suitable GPU adapter");

    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("unterm-device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::default(),
        },
        None,
    ))
    .expect("unterm: failed to create device");

    let cache = Cache::new(&device);
    Gpu { device, queue, cache }
}

/// The shared GPU context, created on first use.
pub fn gpu() -> &'static Gpu {
    static GPU: OnceLock<Gpu> = OnceLock::new();
    GPU.get_or_init(init_gpu)
}

/// The shared font database. Locked briefly during layout/render.
pub fn font_system() -> &'static Mutex<FontSystem> {
    static FS: OnceLock<Mutex<FontSystem>> = OnceLock::new();
    FS.get_or_init(|| Mutex::new(FontSystem::new()))
}
