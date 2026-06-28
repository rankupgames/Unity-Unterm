//! Native completion popup rendered by wgpu: a borderless, non-activating,
//! click-through OS window — an `NSPanel` on macOS, a layered `WS_POPUP` HWND on
//! Windows — that wgpu renders into. Because it's a real OS window it can overflow
//! the editor's bounds, and because it never activates it doesn't steal key focus
//! from the editor (the host keeps driving selection over the FFI; this is
//! display-only). The wgpu/glyphon rendering is shared; only the window creation,
//! placement, and reveal/hide are platform-specific.
#![cfg(any(target_os = "macos", windows))]

use std::cell::RefCell;

use glyphon::{
    Attrs, Buffer, Color, Family, Metrics, Resolution, Shaping, TextArea, TextAtlas, TextBounds,
    TextRenderer, Viewport,
};

use crate::gpu::{self};
use crate::quads::{Quad, QuadRenderer};

#[cfg(target_os = "macos")]
use std::ffi::c_void;
#[cfg(target_os = "macos")]
use objc2::rc::Retained;
#[cfg(target_os = "macos")]
use objc2::runtime::AnyObject;
#[cfg(target_os = "macos")]
use objc2::{class, msg_send, MainThreadMarker, MainThreadOnly};
#[cfg(target_os = "macos")]
use objc2_app_kit::{NSBackingStoreType, NSPanel, NSScreen, NSWindowStyleMask};
#[cfg(target_os = "macos")]
use objc2_foundation::{NSPoint, NSRect, NSSize};
#[cfg(target_os = "macos")]
use objc2_quartz_core::CAMetalLayer;

#[cfg(windows)]
use std::num::NonZeroIsize;
#[cfg(windows)]
use windows::core::w;
#[cfg(windows)]
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
#[cfg(windows)]
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, SetLayeredWindowAttributes,
    SetWindowPos, ShowWindow, HWND_TOPMOST, LWA_ALPHA, SWP_NOACTIVATE, SW_HIDE, SW_SHOWNOACTIVATE,
    WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
    WS_POPUP,
};

const ROW: f32 = 18.0; // logical row height (scaled)
const PAD: f32 = 6.0;
const MAX_ROWS: usize = 10; // visible rows; the list scrolls past this

struct Popup {
    #[cfg(target_os = "macos")]
    panel: Retained<NSPanel>,
    #[cfg(target_os = "macos")]
    layer: Retained<CAMetalLayer>,
    #[cfg(windows)]
    hwnd: HWND,
    #[cfg(windows)]
    shown: bool,
    surface: wgpu::Surface<'static>,
    atlas: TextAtlas,
    viewport: Viewport,
    text: TextRenderer,
    quads: QuadRenderer,
    swash: glyphon::SwashCache,
    format: wgpu::TextureFormat,
    alpha: wgpu::CompositeAlphaMode,
    w: u32,
    h: u32,
}

// The window handle is an OS object (!Send/!Sync) and the popup is only ever touched
// on the main (UI) thread, so it lives in thread-local storage.
thread_local! {
    static POPUP: RefCell<Option<Popup>> = const { RefCell::new(None) };
}

// ------------------------------------------------------------------ macOS backend

#[cfg(target_os = "macos")]
fn create() -> Option<Popup> {
    let mtm = MainThreadMarker::new()?; // must be the main (AppKit) thread
    let g = gpu::gpu();

    // A borderless, non-activating panel that floats above other windows.
    let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(200.0, 100.0));
    let alloc = NSPanel::alloc(mtm);
    let panel: Retained<NSPanel> = NSPanel::initWithContentRect_styleMask_backing_defer(
        alloc,
        rect,
        style,
        NSBackingStoreType::Buffered,
        false,
    );
    panel.setOpaque(false);
    panel.setHasShadow(true);
    panel.setLevel(objc2_app_kit::NSPopUpMenuWindowLevel);
    panel.setHidesOnDeactivate(false);
    // Let mouse/scroll events pass through to the editor window beneath.
    panel.setIgnoresMouseEvents(true);
    unsafe {
        let _: () = msg_send![&*panel, setReleasedWhenClosed: false];
    }

    // A CAMetalLayer added as a SUBLAYER (not the view's hosting layer): a hosting
    // layer is auto-resized by AppKit on the next layout pass, which races our
    // explicit drawableSize and intermittently shows a scaled frame. As a sublayer
    // we own its geometry and set its frame on every show.
    let layer: Retained<CAMetalLayer> = CAMetalLayer::new();
    unsafe {
        let content: *mut AnyObject = msg_send![&*panel, contentView];
        let _: () = msg_send![content, setWantsLayer: true];
        let backing: *mut AnyObject = msg_send![content, layer];
        let _: () = msg_send![backing, addSublayer: &*layer];
        // Present drawables in lockstep with CoreAnimation transactions so a resize
        // (window/layer geometry) and the newly-rendered frame commit atomically.
        let _: () = msg_send![&*layer, setPresentsWithTransaction: true];
    }

    let layer_ptr: *mut c_void = Retained::as_ptr(&layer) as *mut c_void;
    let surface = unsafe {
        g.instance
            .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::CoreAnimationLayer(layer_ptr))
            .ok()?
    };

    let (format, alpha) = pick_format_alpha(&surface);
    let (atlas, viewport, text, quads, swash) = make_renderers(format);

    // Order the panel in once and keep it there — hidden via alpha, never orderOut —
    // so its occlusionState stays "visible" and wgpu's acquire isn't skipped.
    panel.setAlphaValue(0.0);
    panel.orderFrontRegardless();

    Some(Popup { panel, layer, surface, atlas, viewport, text, quads, swash, format, alpha, w: 0, h: 0 })
}

#[cfg(target_os = "macos")]
#[allow(clippy::too_many_arguments)]
fn show_inner(p: &mut Popup, lines: &[&str], selected: usize, scroll: usize, x: f32, y: f32, scale: f32, clear: wgpu::Color, text_color: Color, dark: bool) {
    let s = scale.max(0.5);
    let font_size = 14.0 * s;
    let row_h = ROW * s;
    let pad = PAD * s;
    let (wpx, hpx) = list_size(lines, font_size, row_h, pad);

    // Disable implicit CALayer animations so the panel doesn't animate its size
    // when the completion list changes between keystrokes.
    let txn = class!(CATransaction);
    let _: () = unsafe { msg_send![txn, begin] };
    let _: () = unsafe { msg_send![txn, setDisableActions: true] };

    // Position/size in POINTS FIRST (top-left origin caret → bottom-left AppKit), so
    // the panel hangs from the caret bottom. configure() (exact drawableSize +
    // matching surface size) runs AFTERWARDS so the two never disagree.
    if let Some(mtm) = MainThreadMarker::new() {
        let screen_h = NSScreen::mainScreen(mtm)
            .map(|sc| sc.frame().size.height)
            .unwrap_or(0.0);
        let w_pts = wpx as f64 / s as f64;
        let h_pts = hpx as f64 / s as f64;
        p.panel.setContentSize(NSSize::new(w_pts, h_pts));
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w_pts, h_pts));
        let _: () = unsafe { msg_send![&*p.layer, setFrame: frame] };
        p.panel.setFrameOrigin(NSPoint::new(x as f64, screen_h - y as f64 - h_pts));
    }

    p.configure(wpx, hpx, s);

    // Render INSIDE the transaction so the geometry and the presented frame commit
    // together; reveal (alpha 1) only if the frame was actually presented.
    let presented = render(p, lines, selected, scroll, font_size, row_h, pad, clear, text_color, dark);
    if presented {
        p.panel.setAlphaValue(1.0);
    }

    let _: () = unsafe { msg_send![txn, commit] };
}

#[cfg(target_os = "macos")]
fn hide_impl() {
    POPUP.with(|cell| {
        if let Some(p) = cell.borrow().as_ref() {
            p.panel.setAlphaValue(0.0);
        }
    });
}

#[cfg(target_os = "macos")]
fn disable_on_error(p: &Popup) {
    p.panel.orderOut(None);
}

// ---------------------------------------------------------------- Windows backend

#[cfg(windows)]
unsafe extern "system" fn wndproc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    DefWindowProcW(h, msg, wp, lp)
}

#[cfg(windows)]
fn register_class(hinstance: HINSTANCE) {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: w!("UntermPopupClass"),
            ..Default::default()
        };
        unsafe {
            RegisterClassW(&wc);
        }
    });
}

#[cfg(windows)]
fn create() -> Option<Popup> {
    let g = gpu::gpu();
    unsafe {
        let hmod = GetModuleHandleW(None).ok()?;
        let hinstance = HINSTANCE(hmod.0);
        register_class(hinstance);

        // A layered, no-activate, click-through, always-on-top popup with no frame.
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_TRANSPARENT,
            w!("UntermPopupClass"),
            w!("UntermPopup"),
            WS_POPUP,
            0,
            0,
            200,
            100,
            None,
            None,
            Some(hinstance),
            None,
        )
        .ok()?;
        // Whole-window opacity (per-pixel alpha needs UpdateLayeredWindow, which a
        // swapchain can't feed; the popup is an opaque rectangle instead).
        SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA).ok()?;

        let mut wh = raw_window_handle::Win32WindowHandle::new(NonZeroIsize::new(hwnd.0 as isize)?);
        wh.hinstance = NonZeroIsize::new(hinstance.0 as isize);
        let surface = g
            .instance
            .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: Some(raw_window_handle::RawDisplayHandle::Windows(
                    raw_window_handle::WindowsDisplayHandle::new(),
                )),
                raw_window_handle: raw_window_handle::RawWindowHandle::Win32(wh),
            })
            .ok()?;

        let (format, alpha) = pick_format_alpha(&surface);
        let (atlas, viewport, text, quads, swash) = make_renderers(format);
        Some(Popup { hwnd, shown: false, surface, atlas, viewport, text, quads, swash, format, alpha, w: 0, h: 0 })
    }
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
fn show_inner(p: &mut Popup, lines: &[&str], selected: usize, scroll: usize, x: f32, y: f32, scale: f32, clear: wgpu::Color, text_color: Color, dark: bool) {
    let s = scale.max(0.5);
    let font_size = 14.0 * s;
    let row_h = ROW * s;
    let pad = PAD * s;
    let (wpx, hpx) = list_size(lines, font_size, row_h, pad);

    // `x`/`y` are the caret's screen position in points (top-left origin); Win32
    // screen coordinates are physical pixels, so scale by pixels-per-point. The
    // window is sized to the physical drawable and hangs from the caret bottom.
    let px = (x * s) as i32;
    let py = (y * s) as i32;
    unsafe {
        let _ = SetWindowPos(p.hwnd, Some(HWND_TOPMOST), px, py, wpx as i32, hpx as i32, SWP_NOACTIVATE);
    }

    p.configure(wpx, hpx, s);

    let presented = render(p, lines, selected, scroll, font_size, row_h, pad, clear, text_color, dark);
    if presented && !p.shown {
        unsafe {
            let _ = ShowWindow(p.hwnd, SW_SHOWNOACTIVATE);
        }
        p.shown = true;
    }
}

#[cfg(windows)]
fn hide_impl() {
    POPUP.with(|cell| {
        if let Some(p) = cell.borrow_mut().as_mut() {
            unsafe {
                let _ = ShowWindow(p.hwnd, SW_HIDE);
            }
            p.shown = false;
        }
    });
}

#[cfg(windows)]
fn disable_on_error(p: &Popup) {
    unsafe {
        let _ = ShowWindow(p.hwnd, SW_HIDE);
    }
}

#[cfg(windows)]
impl Drop for Popup {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

// ------------------------------------------------------------------------- shared

/// Pick a surface format (prefer sRGB) and alpha mode (prefer transparency-capable).
fn pick_format_alpha(surface: &wgpu::Surface<'static>) -> (wgpu::TextureFormat, wgpu::CompositeAlphaMode) {
    let g = gpu::gpu();
    let caps = surface.get_capabilities(&g.adapter);
    let format = caps
        .formats
        .iter()
        .copied()
        .find(|f| f.is_srgb())
        .unwrap_or_else(|| caps.formats.first().copied().unwrap_or(wgpu::TextureFormat::Bgra8UnormSrgb));
    let alpha = caps
        .alpha_modes
        .iter()
        .copied()
        .find(|&a| a == wgpu::CompositeAlphaMode::PostMultiplied)
        .unwrap_or_else(|| caps.alpha_modes.first().copied().unwrap_or(wgpu::CompositeAlphaMode::Auto));
    (format, alpha)
}

fn make_renderers(format: wgpu::TextureFormat) -> (TextAtlas, Viewport, TextRenderer, QuadRenderer, glyphon::SwashCache) {
    let g = gpu::gpu();
    let swash = glyphon::SwashCache::new();
    let viewport = Viewport::new(&g.device, &g.cache);
    let mut atlas = TextAtlas::new(&g.device, &g.queue, &g.cache, format);
    let text = TextRenderer::new(&mut atlas, &g.device, wgpu::MultisampleState::default(), None);
    let quads = QuadRenderer::new(&g.device, format);
    (atlas, viewport, text, quads, swash)
}

/// Physical-pixel (width, height) for the popup given its lines.
fn list_size(lines: &[&str], font_size: f32, row_h: f32, pad: f32) -> (u32, u32) {
    let visible = lines.len().min(MAX_ROWS);
    let max_chars = lines.iter().map(|l| l.chars().count()).max().unwrap_or(8);
    let pw = ((max_chars as f32) * font_size * 0.6 + pad * 2.0).clamp(80.0, 900.0);
    let ph = visible as f32 * row_h + pad;
    (pw.ceil() as u32, ph.ceil() as u32)
}

impl Popup {
    fn configure(&mut self, w: u32, h: u32, scale: f32) {
        #[cfg(target_os = "macos")]
        {
            self.layer.setContentsScale(scale as f64);
            self.layer.setDrawableSize(NSSize::new(w as f64, h as f64));
        }
        #[cfg(windows)]
        {
            let _ = scale;
        }
        if self.w == w && self.h == h {
            return;
        }
        self.w = w;
        self.h = h;
        self.reconfigure();
    }

    fn reconfigure(&self) {
        let g = gpu::gpu();
        self.surface.configure(
            &g.device,
            &wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: self.format,
                width: self.w.max(1),
                height: self.h.max(1),
                present_mode: wgpu::PresentMode::Fifo,
                alpha_mode: self.alpha,
                view_formats: vec![],
                desired_maximum_frame_latency: 2,
            },
        );
    }
}

/// Show/refresh the completion popup (anchored below the caret). `items` are
/// '\n'-joined `kind+label` lines; `x`/`y` are the caret's screen position in POINTS
/// (top-left origin, from Unity's GUIToScreenPoint); `scale` is pixels-per-point.
#[allow(clippy::too_many_arguments)]
pub fn show(items: &str, selected: usize, scroll: usize, x: f32, y: f32, scale: f32, clear: wgpu::Color, text_color: Color, dark: bool) {
    if items.is_empty() {
        hide();
        return;
    }
    let lines: Vec<&str> = items.split('\n').collect();
    POPUP.with(|cell| {
        let mut guard = cell.borrow_mut();
        if guard.is_none() {
            *guard = create();
        }
        let Some(p) = guard.as_mut() else { return };
        // Catch wgpu validation errors instead of letting the default handler
        // abort() the whole Unity process; on error, disable the popup.
        let g = gpu::gpu();
        let scope = g.device.push_error_scope(wgpu::ErrorFilter::Validation);
        show_inner(p, &lines, selected, scroll, x, y, scale, clear, text_color, dark);
        if let Some(err) = pollster::block_on(scope.pop()) {
            log::error!("unterm: native popup disabled after GPU error: {err}");
            disable_on_error(p);
            *guard = None;
        }
    });
}

/// Hide the completion popup. It stays alive (transparent / hidden) so the next show
/// is cheap and, on macOS, the window's occlusionState stays "visible".
pub fn hide() {
    hide_impl();
}

fn kind_badge(kind: char) -> char {
    match kind {
        'X' => 'M',
        'A' | 'L' => 'v',
        ' ' => '·',
        k => k,
    }
}

#[allow(clippy::too_many_arguments)]
fn render(
    p: &mut Popup,
    lines: &[&str],
    selected: usize,
    scroll: usize,
    font_size: f32,
    row_h: f32,
    pad: f32,
    clear: wgpu::Color,
    text_color: Color,
    dark: bool,
) -> bool {
    let g = gpu::gpu();
    let (w, h) = (p.w as f32, p.h as f32);
    let total = lines.len();
    let visible = total.min(MAX_ROWS);
    let top = scroll.min(total.saturating_sub(visible));
    let win = &lines[top.min(total)..(top + visible).min(total)];

    // Quads: a background fill + the selected row highlight. On Windows the layered
    // HWND can't show per-pixel alpha, so keep the background a full opaque rectangle
    // (square corners) instead of the rounded, see-through one used on macOS.
    let mut quads: Vec<Quad> = Vec::with_capacity(2);
    let shade = if dark { 0.10 } else { -0.06 };
    quads.push(Quad {
        x: 0.0,
        y: 0.0,
        w,
        h,
        color: [
            (clear.r as f32 + shade).clamp(0.0, 1.0),
            (clear.g as f32 + shade).clamp(0.0, 1.0),
            (clear.b as f32 + shade).clamp(0.0, 1.0),
            1.0,
        ],
        radius: if cfg!(windows) { 0.0 } else { 4.0 * (font_size / 14.0) },
    });
    if selected >= top && selected < top + visible {
        let sel_row = selected - top;
        quads.push(Quad {
            x: 0.0,
            y: pad * 0.5 + sel_row as f32 * row_h,
            w,
            h: row_h,
            color: [0.30, 0.50, 0.90, 0.55],
            radius: 0.0,
        });
    }

    // Text: strip the 1-char kind tag and show it as a colored letter badge before
    // the label, so e.g. a namespace (N) reads differently from a class (T) at a
    // glance — colour alone is too subtle to tell them apart.
    let mut fs = gpu::lock_font_system();
    let mut joined = String::new();
    let mut kinds: Vec<char> = Vec::with_capacity(win.len());
    for (i, line) in win.iter().enumerate() {
        let mut chars = line.chars();
        let kind = chars.next().unwrap_or(' ');
        kinds.push(kind);
        if i > 0 {
            joined.push('\n');
        }
        joined.push(kind_badge(kind));
        joined.push(' ');
        joined.push_str(chars.as_str());
    }
    let base = Attrs::new().family(Family::Monospace).color(text_color);
    let mut buf = Buffer::new(&mut fs, Metrics::new(font_size, row_h));
    buf.set_size(&mut fs, Some(w - pad), Some(h));
    buf.set_text(&mut fs, &joined, &base, Shaping::Advanced, None);
    for (bl, &kind) in buf.lines.iter_mut().zip(kinds.iter()) {
        let label = bl.text().to_string();
        bl.set_attrs_list(crate::input::popup_label_attrs(&label, kind, &base, dark));
    }
    buf.shape_until_scroll(&mut fs, false);

    p.viewport.update(&g.queue, Resolution { width: p.w, height: p.h });
    p.quads.prepare(&g.device, &g.queue, (w, h), &quads);
    let bounds = TextBounds { left: 0, top: 0, right: p.w as i32, bottom: p.h as i32 };
    p.text
        .prepare(
            &g.device,
            &g.queue,
            &mut fs,
            &mut p.atlas,
            &p.viewport,
            [TextArea {
                buffer: &buf,
                left: pad * 0.5,
                top: pad * 0.5,
                scale: 1.0,
                bounds,
                default_color: text_color,
                custom_glyphs: &[],
            }],
            &mut p.swash,
        )
        .ok();

    // On Windows the layered HWND can't show per-pixel alpha, so clear to the opaque
    // background; on macOS clear transparent and let the rounded quad show through.
    let load = if cfg!(windows) {
        wgpu::LoadOp::Clear(wgpu::Color { r: clear.r, g: clear.g, b: clear.b, a: 1.0 })
    } else {
        wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
    };

    use wgpu::CurrentSurfaceTexture as Cst;
    let frame = match p.surface.get_current_texture() {
        Cst::Success(t) | Cst::Suboptimal(t) => t,
        Cst::Outdated | Cst::Lost => {
            log::debug!("popup acquire: outdated/lost -> reconfigure+retry");
            p.reconfigure();
            match p.surface.get_current_texture() {
                Cst::Success(t) | Cst::Suboptimal(t) => t,
                _ => {
                    log::debug!("popup acquire: retry failed -> SKIP (stale frame)");
                    return false;
                }
            }
        }
        Cst::Occluded => {
            log::debug!("popup acquire: OCCLUDED -> SKIP (stale frame)");
            return false;
        }
        Cst::Timeout => {
            log::debug!("popup acquire: TIMEOUT -> SKIP (stale frame)");
            return false;
        }
        _ => return false,
    };
    let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut enc = g
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("unterm-popup") });
    {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("unterm-popup-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load, store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        p.quads.render(&mut pass);
        let _ = p.text.render(&p.atlas, &p.viewport, &mut pass);
    }
    g.queue.submit([enc.finish()]);
    let _ = g.device.poll(wgpu::PollType::wait_indefinitely());
    frame.present();
    true
}
