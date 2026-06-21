//! Offscreen wgpu renderer for the agent panel (the chat view).
//!
//! Renders the role-tagged transcript as stacked, optionally-carded message
//! blocks (Zed-like) plus a pinned row of action buttons (permission options),
//! into an IOSurface-backed `MTLTexture` handed to Unity zero-copy. Unlike the
//! terminal (a fixed grid), the panel flows wrapped text and supports mouse
//! selection and scrollback.
//!
//! It shares the one process-global wgpu device, queue, glyph cache, and font
//! database (see [`crate::gpu`]) with the terminal renderer, so opening a panel
//! alongside terminals stays cheap and the glyph atlas is warmed once. The
//! panel itself holds no durable state: the conversation lives in the agent
//! session (which survives domain reloads), so the host recreates the panel on
//! reload and re-renders from the session transcript.

use glyphon::{
    Attrs, Buffer, Color, Family, FontSystem, Metrics, Resolution, Shaping, Style, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight, Wrap,
};

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use crate::gpu::{self, FORMAT};
use crate::surface::{self, SharedSurface};
use crate::quads::{Quad, QuadRenderer};
use std::ffi::c_void;

/// Record/unit separators used to encode role-tagged blocks in `set_text`.
/// `set_text` content is `role\x1f text` blocks joined by `\x1e`. Plain text
/// with neither separator is treated as a single agent block. Mirrors the
/// transcript format produced by [`crate::acp_session`].
pub(crate) const RS: char = '\u{1e}';
pub(crate) const US: char = '\u{1f}';

#[derive(Clone, Copy, PartialEq)]
enum Role {
    User,
    Agent,
    Thought,
    Tool,
}

impl Role {
    fn from_tag(c: char) -> Role {
        match c {
            'u' => Role::User,
            't' => Role::Thought,
            'x' => Role::Tool,
            _ => Role::Agent,
        }
    }
    /// Whether the block gets a card background.
    fn carded(self) -> bool {
        matches!(self, Role::User | Role::Tool)
    }
}

struct Block {
    role: Role,
    text: String,
}

/// A shaped, measured render item: one card-able, optionally-indented buffer.
/// Non-agent blocks produce one; an agent block is expanded into several (one per
/// Markdown element).
struct Measured {
    buffer: Buffer,
    text: String, // visible text (must match the buffer, for selection)
    height: f32,
    card_alpha: f32, // 0 = no card background
    indent: f32,     // left indent in physical px (lists / quotes)
    code: bool,      // a code block: rendered unwrapped + horizontally scrollable
    natural_w: f32,  // unwrapped content width (code blocks only)
}
/// A laid-out block kept after render() so mouse hit-testing/selection works
/// between frames. `tx/ty` is the text top-left in physical px.
struct LaidBlock {
    buffer: Buffer,
    text: String,
    tx: f32,
    ty: f32,
    /// Horizontal scroll applied to a code block (0 for everything else); the
    /// buffer is drawn at `tx - hscroll` and clipped to `clip`.
    hscroll: f32,
    /// Clip rect (physical px x,y,w,h) for code blocks; None = full panel.
    clip: Option<[f32; 4]>,
    /// Content-hash key + max scroll, so the wheel handler can scroll this block.
    code_key: Option<u64>,
    max_hscroll: f32,
}

/// A caret position: byte offset into block `block`'s text.
#[derive(Clone, Copy, PartialEq)]
struct TextPos {
    block: usize,
    offset: usize,
}

impl TextPos {
    fn le(self, o: TextPos) -> bool {
        (self.block, self.offset) <= (o.block, o.offset)
    }
}

fn parse_blocks(text: &str) -> Vec<Block> {
    if !text.contains(RS) && !text.contains(US) {
        if text.is_empty() {
            return Vec::new();
        }
        return vec![Block {
            role: Role::Agent,
            text: text.to_string(),
        }];
    }
    text.split(RS)
        .filter(|s| !s.is_empty())
        .map(|chunk| {
            let mut it = chunk.splitn(2, US);
            let tag = it.next().unwrap_or("a").chars().next().unwrap_or('a');
            let body = it.next().unwrap_or("");
            Block {
                role: Role::from_tag(tag),
                text: body.to_string(),
            }
        })
        .collect()
}

pub struct PanelRenderer {
    width: u32,
    height: u32,
    shared: SharedSurface,

    clear: wgpu::Color,
    text_color: Color,
    /// Font family names per style (e.g. Unity's Inter Regular/SemiBold/Italic);
    /// None falls back to sans-serif. Heavier Inter weights use a distinct family
    /// name ("Inter SemiBold"), so Markdown bold/italic select the right face here
    /// rather than relying on synthesis. Missing glyphs (CJK, emoji) still resolve
    /// via cosmic-text's fallback.
    font_family: Option<String>,
    font_bold: Option<String>,
    font_italic: Option<String>,
    font_bold_italic: Option<String>,
    /// HiDPI factor: the panel renders at physical pixels and scales all sizes
    /// by this so text is crisp (no upscaling blur) on Retina displays.
    scale: f32,
    /// Vertical scroll offset in physical px (0 = bottom-anchored / latest).
    scroll: f32,
    /// Laid-out content height in physical px (for the host's scrollbar).
    content_h: f32,
    /// Action buttons (e.g. permission options) drawn pinned at the bottom.
    buttons: Vec<String>,
    /// Hit rects (physical px) for `buttons`, computed each render.
    button_rects: Vec<[f32; 4]>,
    /// Laid-out blocks from the last render, for hit-testing/selection.
    laid: Vec<LaidBlock>,
    /// Active text selection (anchor, focus), as block+offset positions.
    sel: Option<(TextPos, TextPos)>,
    /// Per-code-block horizontal scroll, keyed by the block's content hash so it
    /// survives re-layout as the transcript grows.
    hscroll: HashMap<u64, f32>,

    swash_cache: SwashCache,
    viewport: Viewport,
    atlas: TextAtlas,
    text_renderer: TextRenderer,
    quads: QuadRenderer,
}

impl PanelRenderer {
    pub fn new(width: u32, height: u32) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        let g = gpu::gpu();

        let shared = create_target(&g.device, width, height);

        let swash_cache = SwashCache::new();
        let viewport = Viewport::new(&g.device, &g.cache);
        let mut atlas = TextAtlas::new(&g.device, &g.queue, &g.cache, FORMAT);
        let text_renderer =
            TextRenderer::new(&mut atlas, &g.device, wgpu::MultisampleState::default(), None);
        let quads = QuadRenderer::new(&g.device, FORMAT);

        Self {
            width,
            height,
            shared,
            // Themed by the host (Unity editor colors); these are fallbacks.
            clear: wgpu::Color {
                r: 0.051,
                g: 0.051,
                b: 0.051,
                a: 1.0,
            },
            text_color: Color::rgb(210, 210, 214),
            font_family: None,
            font_bold: None,
            font_italic: None,
            font_bold_italic: None,
            scale: 1.0,
            scroll: 0.0,
            content_h: 0.0,
            buttons: Vec::new(),
            button_rects: Vec::new(),
            laid: Vec::new(),
            sel: None,
            hscroll: HashMap::new(),
            swash_cache,
            viewport,
            atlas,
            text_renderer,
            quads,
        }
    }

    /// Background clear color, in linear space (sRGB target encodes on store).
    pub fn set_clear_color(&mut self, r: f64, g: f64, b: f64, a: f64) {
        self.clear = wgpu::Color { r, g, b, a };
    }

    /// Default text color, as sRGB bytes (glyphon color space).
    pub fn set_text_color(&mut self, r: u8, g: u8, b: u8, a: u8) {
        self.text_color = Color::rgba(r, g, b, a);
    }

    /// Load Regular/Bold/Italic/BoldItalic faces (empty path = skip). Each is
    /// recorded by its own family name so Markdown selects the real face.
    pub fn set_fonts(&mut self, regular: &str, bold: &str, italic: &str, bold_italic: &str) {
        let mut fs = gpu::font_system().lock().unwrap();
        if !regular.is_empty() {
            self.font_family = load_face(&mut fs, regular);
        }
        if !bold.is_empty() {
            self.font_bold = load_face(&mut fs, bold);
        }
        if !italic.is_empty() {
            self.font_italic = load_face(&mut fs, italic);
        }
        if !bold_italic.is_empty() {
            self.font_bold_italic = load_face(&mut fs, bold_italic);
        }
    }

    /// HiDPI scale (pixels per point). Layout and font sizes scale by this.
    pub fn set_scale(&mut self, scale: f32) {
        self.scale = scale.max(0.5);
    }

    /// Scroll offset in physical px (0 = bottom). Clamped during layout.
    pub fn set_scroll(&mut self, scroll: f32) {
        self.scroll = scroll.max(0.0);
    }

    /// Total laid-out content height in physical px (from the last render).
    pub fn content_height(&self) -> f32 {
        self.content_h
    }

    /// Set the action buttons drawn pinned at the bottom (empty = none).
    pub fn set_buttons(&mut self, labels: Vec<String>) {
        self.buttons = labels;
    }

    /// Begin a selection at physical-px (x, y).
    pub fn selection_begin(&mut self, x: f32, y: f32) {
        if let Some(p) = self.hit_text(x, y) {
            self.sel = Some((p, p));
        } else {
            self.sel = None;
        }
    }

    /// Extend the active selection to (x, y).
    pub fn selection_update(&mut self, x: f32, y: f32) {
        if let (Some((a, _)), Some(p)) = (self.sel, self.hit_text(x, y)) {
            self.sel = Some((a, p));
        }
    }

    pub fn selection_clear(&mut self) {
        self.sel = None;
    }

    pub fn has_selection(&self) -> bool {
        matches!(self.sel, Some((a, b)) if a != b)
    }

    /// Select everything.
    pub fn select_all(&mut self) {
        if self.laid.is_empty() {
            self.sel = None;
            return;
        }
        let last = self.laid.len() - 1;
        self.sel = Some((
            TextPos { block: 0, offset: 0 },
            TextPos { block: last, offset: self.laid[last].text.len() },
        ));
    }

    /// The selected text (joining across blocks with newlines), or empty.
    pub fn selected_text(&self) -> String {
        let Some((a, b)) = self.sel else {
            return String::new();
        };
        let (lo, hi) = if a.le(b) { (a, b) } else { (b, a) };
        if lo == hi {
            return String::new();
        }
        let mut out = String::new();
        for bi in lo.block..=hi.block.min(self.laid.len().saturating_sub(1)) {
            let t = &self.laid[bi].text;
            let start = if bi == lo.block { lo.offset } else { 0 };
            let end = if bi == hi.block { hi.offset } else { t.len() };
            let start = clamp_boundary(t, start);
            let end = clamp_boundary(t, end.max(start));
            if bi != lo.block {
                out.push('\n');
            }
            out.push_str(&t[start..end]);
        }
        out
    }

    /// Map a physical-px point to a caret position.
    fn hit_text(&self, x: f32, y: f32) -> Option<TextPos> {
        if self.laid.is_empty() {
            return None;
        }
        // Pick the block whose vertical band contains y (clamp to ends).
        let mut block = 0usize;
        for (i, b) in self.laid.iter().enumerate() {
            if y >= b.ty {
                block = i;
            }
        }
        if y < self.laid[0].ty {
            block = 0;
        }
        let b = &self.laid[block];
        // Code blocks are drawn at `tx - hscroll`, so map back into buffer space.
        let cursor = b.buffer.hit(x - b.tx + b.hscroll, (y - b.ty).max(0.0));
        let offset = match cursor {
            Some(c) => cursor_to_offset(&b.buffer, c),
            None => {
                if x < b.tx - b.hscroll {
                    0
                } else {
                    b.text.len()
                }
            }
        };
        Some(TextPos {
            block,
            offset: offset.min(b.text.len()),
        })
    }

    /// Highlight quads for the current selection (physical px, overlay color).
    fn selection_quads(&self, overlay: f32) -> Vec<Quad> {
        let Some((a, b)) = self.sel else {
            return Vec::new();
        };
        let (lo, hi) = if a.le(b) { (a, b) } else { (b, a) };
        if lo == hi {
            return Vec::new();
        }
        let mut quads = Vec::new();
        for bi in lo.block..=hi.block.min(self.laid.len().saturating_sub(1)) {
            let blk = &self.laid[bi];
            let sel_start = if bi == lo.block { lo.offset } else { 0 };
            let sel_end = if bi == hi.block { hi.offset } else { blk.text.len() };
            let line_starts = line_starts(&blk.buffer);
            for run in blk.buffer.layout_runs() {
                let line_off = line_starts.get(run.line_i).copied().unwrap_or(0);
                let mut min_x = f32::MAX;
                let mut max_x = f32::MIN;
                for g in run.glyphs.iter() {
                    let gs = line_off + g.start;
                    let ge = line_off + g.end;
                    if ge > sel_start && gs < sel_end {
                        min_x = min_x.min(g.x);
                        max_x = max_x.max(g.x + g.w);
                    }
                }
                if max_x > min_x {
                    quads.push(Quad {
                        x: blk.tx - blk.hscroll + min_x,
                        y: blk.ty + run.line_top,
                        w: max_x - min_x,
                        h: run.line_height,
                        color: [overlay, overlay, overlay, 0.28],
                        radius: 0.0,
                    });
                }
            }
        }
        quads
    }

    /// Scroll the code block under (x, y) horizontally by `dx` physical px.
    /// Returns true if a code block consumed it (so the host can keep the event).
    pub fn scroll_h(&mut self, x: f32, y: f32, dx: f32) -> bool {
        for l in &self.laid {
            let (Some(key), Some(c)) = (l.code_key, l.clip) else {
                continue;
            };
            if x >= c[0] && x <= c[0] + c[2] && y >= c[1] && y <= c[1] + c[3] && l.max_hscroll > 0.5 {
                let cur = self.hscroll.get(&key).copied().unwrap_or(0.0);
                self.hscroll.insert(key, (cur + dx).clamp(0.0, l.max_hscroll));
                return true;
            }
        }
        false
    }

    /// Index of the button at physical-px point (x, y), or -1.
    pub fn hit_button(&self, x: f32, y: f32) -> i32 {
        for (i, r) in self.button_rects.iter().enumerate() {
            if x >= r[0] && x <= r[0] + r[2] && y >= r[1] && y <= r[1] + r[3] {
                return i as i32;
            }
        }
        -1
    }

    /// Raw `id<MTLTexture>` of the current render target.
    pub fn raw_texture(&self) -> *mut c_void {
        self.shared.raw_texture()
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);
        if width == self.width && height == self.height {
            return;
        }
        self.width = width;
        self.height = height;
        let shared = create_target(&gpu::gpu().device, width, height);
        self.shared = shared;
    }

    /// Render the role-tagged transcript as stacked, optionally-carded blocks
    /// (Zed-like). Newest content is bottom-anchored so it stays in view.
    pub fn render(&mut self, text: &str) {
        let g = gpu::gpu();
        let mut fs = gpu::font_system().lock().unwrap();

        let s = self.scale;
        let pad = 14.0 * s;
        let gap = 8.0 * s;
        let card_pad = 10.0 * s;
        let radius = 8.0 * s;
        let font_size = 14.0 * s;
        let line_height = 20.0 * s;

        let width = self.width as f32;
        let height = self.height as f32;
        let content_w = (width - pad * 2.0).max(1.0);
        let text_color = self.text_color;

        // Card overlay adapts to the theme: light wash on dark bg, dark on light.
        let lum = 0.2126 * self.clear.r + 0.7152 * self.clear.g + 0.0722 * self.clear.b;
        let overlay = if lum < 0.5 { 1.0_f32 } else { 0.0_f32 };

        let regular = self
            .font_family
            .as_deref()
            .map(Family::Name)
            .unwrap_or(Family::SansSerif);

        let blocks = parse_blocks(text);

        // First pass: shape + measure each block as plain text.
        let mut measured: Vec<Measured> = Vec::new();
        for b in &blocks {
            measured.push(build_plain(
                &mut fs, b, content_w, font_size, line_height, card_pad, regular, text_color,
            ));
        }

        // Measure the action-button labels and pack them into rows that fit the
        // width, so a narrow panel wraps the buttons instead of overflowing; the
        // reserved bottom strip grows with the number of rows.
        let btn_pad_x = 12.0 * s;
        let gap_b = 8.0 * s;
        let btn_h = line_height + card_pad;
        let avail = (width - pad * 2.0).max(1.0);
        let mut btn_buffers: Vec<Buffer> = Vec::new();
        let mut btn_w: Vec<f32> = Vec::new(); // full button width incl. h-padding
        for label in &self.buttons {
            let mut buf = Buffer::new(&mut fs, Metrics::new(font_size, line_height));
            buf.set_size(&mut fs, None, None);
            buf.set_text(
                &mut fs,
                label,
                Attrs::new().family(regular).color(text_color),
                Shaping::Advanced,
            );
            buf.shape_until_scroll(&mut fs, false);
            btn_w.push(measure_width(&buf) + btn_pad_x * 2.0);
            btn_buffers.push(buf);
        }
        let mut rows: Vec<Vec<usize>> = Vec::new();
        {
            let mut cur: Vec<usize> = Vec::new();
            let mut cur_w = 0.0_f32;
            for (i, &w) in btn_w.iter().enumerate() {
                let add = if cur.is_empty() { w } else { w + gap_b };
                if !cur.is_empty() && cur_w + add > avail {
                    rows.push(std::mem::take(&mut cur));
                    cur.push(i);
                    cur_w = w;
                } else {
                    cur.push(i);
                    cur_w += add;
                }
            }
            if !cur.is_empty() {
                rows.push(cur);
            }
        }
        let button_block_h = if rows.is_empty() {
            0.0
        } else {
            rows.len() as f32 * btn_h + (rows.len() as f32 - 1.0) * gap_b
        };
        // Buttons scroll inline with the transcript (not pinned to the bottom), so
        // no bottom strip is reserved; they're added to the content total below and
        // placed right after the last block, `gap` beneath it.
        let buttons_h = if rows.is_empty() { 0.0 } else { gap + button_block_h };
        let content_bottom = height - pad;

        // Bottom-anchor when the transcript overflows; `scroll` reveals older
        // content (clamped so 0 = latest, max = top).
        let total: f32 = measured.iter().map(|m| m.height).sum::<f32>()
            + gap * measured.len().saturating_sub(1) as f32
            + buttons_h;
        self.content_h = total + pad * 2.0;
        let viewport_h = content_bottom - pad;
        let mut y = if total <= viewport_h {
            pad
        } else {
            let max_scroll = total - viewport_h;
            let scroll = self.scroll.min(max_scroll);
            content_bottom - total + scroll
        };

        // Second pass: place each block, emitting a card quad where needed.
        let bounds = TextBounds {
            left: 0,
            top: 0,
            right: self.width as i32,
            bottom: self.height as i32,
        };
        let mut quads: Vec<Quad> = Vec::new();
        self.laid.clear();
        let mut live_keys: Vec<u64> = Vec::new();
        for m in measured {
            let card = m.card_alpha > 0.0;
            let x0 = pad + m.indent;
            if card {
                quads.push(Quad {
                    x: x0,
                    y,
                    w: (content_w - m.indent).max(1.0),
                    h: m.height,
                    color: [overlay, overlay, overlay, m.card_alpha],
                    radius,
                });
            }
            let (tx, ty) = if card {
                (x0 + card_pad, y + card_pad)
            } else {
                (x0, y)
            };
            // Code blocks: render unwrapped, clipped to the card, with a per-block
            // horizontal scroll (clamped to how far the longest line overflows).
            let (hscroll, clip, code_key, max_hscroll) = if m.code {
                let inner_w = (content_w - card_pad * 2.0).max(1.0);
                let max_h = (m.natural_w - inner_w).max(0.0);
                let key = hash_str(&m.text);
                let cur = self.hscroll.get(&key).copied().unwrap_or(0.0).clamp(0.0, max_h);
                self.hscroll.insert(key, cur);
                live_keys.push(key);
                let left = tx.max(0.0);
                let top = y.max(0.0);
                let right = (tx + inner_w).min(self.width as f32);
                let bottom = (y + m.height).min(self.height as f32);
                let clip = [left, top, (right - left).max(0.0), (bottom - top).max(0.0)];
                (cur, Some(clip), Some(key), max_h)
            } else {
                (0.0, None, None, 0.0)
            };
            self.laid.push(LaidBlock {
                buffer: m.buffer,
                text: m.text,
                tx,
                ty,
                hscroll,
                clip,
                code_key,
                max_hscroll,
            });
            y += m.height + gap;
        }
        self.hscroll.retain(|k, _| live_keys.contains(k));

        // Selection highlight (above the cards, below the text).
        quads.extend(self.selection_quads(overlay));

        // Action buttons: placed right after the last block (scrolling with the
        // transcript, not pinned), wrapped into rows (each row right-aligned).
        // Positions are indexed by button so hit-testing maps a click to its option.
        self.button_rects.clear();
        let mut btn_pos: Vec<(f32, f32)> = vec![(0.0, 0.0); self.buttons.len()];
        if !rows.is_empty() {
            let mut rects = vec![[0.0_f32; 4]; self.buttons.len()];
            let block_top = y;
            for (ri, row) in rows.iter().enumerate() {
                let row_w: f32 = row.iter().map(|&i| btn_w[i]).sum::<f32>()
                    + gap_b * row.len().saturating_sub(1) as f32;
                let mut bx = (width - pad - row_w).max(pad);
                let row_y = block_top + ri as f32 * (btn_h + gap_b);
                for &i in row {
                    let bw = btn_w[i];
                    quads.push(Quad {
                        x: bx,
                        y: row_y,
                        w: bw,
                        h: btn_h,
                        color: [overlay, overlay, overlay, 0.18],
                        radius,
                    });
                    btn_pos[i] = (bx + btn_pad_x, row_y + (btn_h - line_height) / 2.0);
                    rects[i] = [bx, row_y, bw, btn_h];
                    bx += bw + gap_b;
                }
            }
            self.button_rects = rects;
        }

        // Text areas: blocks (from self.laid) then button labels.
        let mut areas: Vec<TextArea> = Vec::with_capacity(self.laid.len() + btn_buffers.len());
        for l in &self.laid {
            let b = l.clip.map_or(bounds, |c| TextBounds {
                left: c[0] as i32,
                top: c[1] as i32,
                right: (c[0] + c[2]) as i32,
                bottom: (c[1] + c[3]) as i32,
            });
            areas.push(TextArea {
                buffer: &l.buffer,
                left: l.tx - l.hscroll,
                top: l.ty,
                scale: 1.0,
                bounds: b,
                default_color: text_color,
                custom_glyphs: &[],
            });
        }
        for (i, buf) in btn_buffers.iter().enumerate() {
            let (lx, ty) = btn_pos[i];
            areas.push(TextArea {
                buffer: buf,
                left: lx,
                top: ty,
                scale: 1.0,
                bounds,
                default_color: text_color,
                custom_glyphs: &[],
            });
        }

        self.viewport.update(
            &g.queue,
            Resolution {
                width: self.width,
                height: self.height,
            },
        );
        self.quads
            .prepare(&g.device, &g.queue, (width, height), &quads);
        self.text_renderer
            .prepare(
                &g.device,
                &g.queue,
                &mut fs,
                &mut self.atlas,
                &self.viewport,
                areas,
                &mut self.swash_cache,
            )
            .expect("unterm: panel glyphon prepare failed");

        let mut encoder = g
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("unterm-panel-encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("unterm-panel-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: self.shared.view(),
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.clear),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            self.quads.render(&mut pass);
            self.text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)
                .expect("unterm: panel glyphon render failed");
        }
        g.queue.submit([encoder.finish()]);
        // Wait for the GPU so the IOSurface holds a complete frame before Unity
        // samples it (the zero-copy path has no readback to force completion).
        g.device.poll(wgpu::Maintain::Wait);
        self.atlas.trim();
        // Blit into the presented texture (no-op on macOS; D3D copy on Windows).
        self.shared.present();
    }

}

/// Stable content hash, used to key a code block's horizontal scroll.
fn hash_str(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}




/// Load a font file and return its first family name (None on failure).
fn load_face(fs: &mut FontSystem, path: &str) -> Option<String> {
    let db = fs.db_mut();
    if let Err(e) = db.load_font_file(path) {
        log::warn!("unterm: failed to load font {path}: {e}");
        return None;
    }
    db.faces()
        .last()
        .and_then(|f| f.families.first())
        .map(|(name, _)| name.clone())
}
/// Build one plain (non-Markdown) block: user prompts, thoughts, tool lines.
fn build_plain(
    fs: &mut FontSystem,
    b: &Block,
    content_w: f32,
    font_size: f32,
    line_height: f32,
    card_pad: f32,
    family: Family<'_>,
    text_color: Color,
) -> Measured {
    let carded = b.role.carded();
    let inner_w = if carded { content_w - card_pad * 2.0 } else { content_w };
    let color = match b.role {
        Role::Thought => dim(text_color, 150),
        Role::Tool => dim(text_color, 205),
        _ => text_color,
    };
    let mut buffer = Buffer::new(fs, Metrics::new(font_size, line_height));
    buffer.set_size(fs, Some(inner_w.max(1.0)), None);
    buffer.set_wrap(fs, Wrap::WordOrGlyph);
    buffer.set_text(
        fs,
        &b.text,
        Attrs::new().family(family).color(color),
        Shaping::Advanced,
    );
    buffer.shape_until_scroll(fs, false);
    let text_h = measure_height(&buffer);
    let card_alpha = match b.role {
        Role::User => 0.10,
        Role::Tool => 0.06,
        _ => 0.0,
    };
    let height = if carded { text_h + card_pad * 2.0 } else { text_h };
    Measured {
        buffer,
        text: b.text.clone(),
        height,
        card_alpha,
        indent: 0.0,
        code: false,
        natural_w: 0.0,
    }
}




/// Scale a color's alpha (for dimmed thoughts / tool text).
fn dim(c: Color, alpha: u8) -> Color {
    Color::rgba(c.r(), c.g(), c.b(), alpha)
}

/// Byte offset in the buffer's full text where each BufferLine starts.
fn line_starts(buffer: &Buffer) -> Vec<usize> {
    let mut starts = Vec::with_capacity(buffer.lines.len());
    let mut off = 0;
    for line in &buffer.lines {
        starts.push(off);
        off += line.text().len() + 1; // +1 for the '\n' separator
    }
    starts
}

/// Convert a cosmic-text cursor to a byte offset in the buffer's full text.
fn cursor_to_offset(buffer: &Buffer, cursor: glyphon::cosmic_text::Cursor) -> usize {
    let starts = line_starts(buffer);
    starts.get(cursor.line).copied().unwrap_or(0) + cursor.index
}

/// Clamp a byte index to the nearest char boundary at or below it.
fn clamp_boundary(s: &str, mut i: usize) -> usize {
    if i > s.len() {
        i = s.len();
    }
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Widest laid-out line of a shaped buffer (physical px).
fn measure_width(buffer: &Buffer) -> f32 {
    buffer.layout_runs().map(|r| r.line_w).fold(0.0, f32::max)
}

/// Laid-out pixel height of a shaped buffer.
fn measure_height(buffer: &Buffer) -> f32 {
    let mut h = 0.0_f32;
    for run in buffer.layout_runs() {
        h = h.max(run.line_top + run.line_height);
    }
    if h <= 0.0 {
        20.0
    } else {
        h
    }
}

fn create_target(device: &wgpu::Device, width: u32, height: u32) -> SharedSurface {
    surface::create_shared_target(device, width, height, FORMAT)
}
