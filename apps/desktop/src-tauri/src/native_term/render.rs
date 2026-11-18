use super::screen::{Cursor, Frame, Preedit, Span};
use super::style::{FaceEm, Style, Theme, cell_px};
use alacritty_terminal::vte::ansi::{CursorShape, Rgb};
use glyphon::{
    Attrs, Buffer, Cache, Color as TextColor, Family, FontSystem, Metrics, Resolution, Shaping,
    Style as FontStyle, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
    Weight, Wrap, fontdb,
};
use std::collections::HashMap;
use std::ffi::c_void;
use std::path::Path;
use std::ptr::NonNull;

/// The fallback font files loaded beside the style's family, a fixed set instead of every
/// system font, to keep memory small; Apple Color Emoji is the only system font with
/// U+23FA, the bullet Claude Code prints. A missing fallback only leaves its glyphs blank.
const FALLBACK_PATHS: [&str; 3] = [
    "/System/Library/Fonts/Apple Symbols.ttf",
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    "/System/Library/Fonts/Apple Color Emoji.ttc",
];

/// The most background rectangles one frame draws.
const RECTS_MAX: usize = 1 << 16;

/// Bytes of one rectangle: position, size and colour as f32.
const RECT_BYTES: usize = 32;

struct Row {
    buffer: Buffer,
    key: u64,
}

/// The device every native view draws with, made with the first view's surface.
struct Device {
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    format: wgpu::TextureFormat,
    rects: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    cache: Cache,
}

/// What all native views share: one wgpu instance and device, one font system and glyph
/// rasterizer cache, and the style: the family drawn with, the font size and cell size in
/// device pixels and the colours. `generation` counts style changes, so a surface reshapes
/// its rows after one.
pub struct Shared {
    instance: wgpu::Instance,
    device: Option<Device>,
    fonts: FontSystem,
    swash: SwashCache,
    scale: f32,
    family: String,
    font_px: f32,
    cell: (f32, f32),
    theme: Theme,
    generation: u64,
}

/// One native view's own drawing state: its Metal surface, glyph atlas, shaped rows (of the
/// style generation they were shaped in) and rectangle buffers.
pub struct Surface {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    viewport: Viewport,
    atlas: TextAtlas,
    text: TextRenderer,
    rows: Vec<Row>,
    generation: u64,
    rect_buffer: wgpu::Buffer,
    screen_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

/// The underline under marked text: one line as tall as a cursor's underline, along the
/// bottom of its cells. Inputs: the marked cells, the cell size and the line height in
/// pixels. Output: x, y, width and height in pixels.
fn preedit_rect(preedit: Preedit, cell: (f32, f32), line: f32) -> [f32; 4] {
    let (cw, ch) = cell;
    assert!(cw > 0.0);
    assert!(ch > 0.0);
    assert!(line >= 1.0);
    assert!(line <= ch);
    assert!(preedit.cells > 0);
    let x = preedit.column as f32 * cw;
    let y = (preedit.row + 1) as f32 * ch - line;
    [x, y, preedit.cells as f32 * cw, line]
}

fn linear(channel: u8) -> f64 {
    let c = f64::from(channel) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_rgba(color: Rgb, alpha: f32) -> [f32; 4] {
    [
        linear(color.r) as f32,
        linear(color.g) as f32,
        linear(color.b) as f32,
        alpha,
    ]
}

fn load_fallbacks() -> FontSystem {
    let mut db = fontdb::Database::new();
    for path in FALLBACK_PATHS {
        if Path::new(path).is_file() && db.load_font_file(path).is_err() {
            continue;
        }
    }
    FontSystem::new_with_locale_and_db("en-US".into(), db)
}

/// The upright face loaded from a file whose weight is nearest regular, and its family.
/// Inputs: the font database and the file. Output: the face id and family, or none.
fn face_in(db: &fontdb::Database, file: &Path) -> Option<(fontdb::ID, String)> {
    let from_file = |source: &fontdb::Source| match source {
        fontdb::Source::File(path) | fontdb::Source::SharedFile(path, _) => path == file,
        fontdb::Source::Binary(_) => false,
    };
    let face = db
        .faces()
        .filter(|face| face.style == fontdb::Style::Normal && from_file(&face.source))
        .min_by_key(|face| face.weight.0.abs_diff(fontdb::Weight::NORMAL.0))?;
    let family = face.families.first()?.0.clone();
    assert!(from_file(&face.source));
    Some((face.id, family))
}

/// A face's metrics in em, measured on 'W' as xterm measures its cell. Inputs: the font
/// system and the face. Output: the metrics, or `native_terminal_style_invalid`.
fn face_em(fonts: &mut FontSystem, id: fontdb::ID) -> Result<FaceEm, &'static str> {
    let invalid = "native_terminal_style_invalid";
    let font = fonts.get_font(id).ok_or(invalid)?;
    let face = font.rustybuzz();
    let em = f64::from(face.units_per_em());
    let glyph = face.glyph_index('W').ok_or(invalid)?;
    let advance = f64::from(face.glyph_hor_advance(glyph).ok_or(invalid)?);
    let metrics = FaceEm {
        advance: advance / em,
        ascent: f64::from(face.ascender()) / em,
        descent: -f64::from(face.descender()) / em,
    };
    if em <= 0.0 || metrics.advance <= 0.0 || metrics.ascent + metrics.descent <= 0.0 {
        return Err(invalid);
    }
    assert!(metrics.advance.is_finite() && metrics.ascent.is_finite());
    Ok(metrics)
}

const RECT_SHADER: &str = "
struct Screen { size: vec4<f32> };
@group(0) @binding(0) var<uniform> screen: Screen;
struct Out { @builtin(position) p: vec4<f32>, @location(0) color: vec4<f32> };
@vertex fn vs(@builtin(vertex_index) v: u32, @location(0) pos: vec2<f32>,
    @location(1) size: vec2<f32>, @location(2) color: vec4<f32>) -> Out {
  let corner = vec2<f32>(f32(v & 1u), f32((v >> 1u) & 1u));
  let px = pos + corner * size;
  var o: Out;
  o.p = vec4<f32>(px.x / screen.size.x * 2.0 - 1.0, 1.0 - px.y / screen.size.y * 2.0, 0.0, 1.0);
  o.color = color;
  return o;
}
@fragment fn fs(o: Out) -> @location(0) vec4<f32> { return o.color; }
";

fn rect_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout) {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("rects"),
        source: wgpu::ShaderSource::Wgsl(RECT_SHADER.into()),
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("screen"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("rects"),
        bind_group_layouts: &[&layout],
        push_constant_ranges: &[],
    });
    let attributes = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4];
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("rects"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: RECT_BYTES as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &attributes,
            }],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview: None,
        cache: None,
    });
    (pipeline, layout)
}

/// Makes the device for the first surface: the low-power adapter that can present to it.
fn device_for(
    instance: &wgpu::Instance,
    surface: &wgpu::Surface<'static>,
) -> Result<Device, String> {
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: Some(surface),
        force_fallback_adapter: false,
    }))
    .map_err(|e| e.to_string())?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("native-terminal"),
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        ..Default::default()
    }))
    .map_err(|e| e.to_string())?;
    let format = surface
        .get_capabilities(&adapter)
        .formats
        .first()
        .copied()
        .ok_or("surface has no format")?;
    let (rects, layout) = rect_pipeline(&device, format);
    let cache = Cache::new(&device);
    Ok(Device {
        adapter,
        device,
        queue,
        format,
        rects,
        layout,
        cache,
    })
}

impl Shared {
    /// Loads the fallback fonts and applies the first style; no GPU work runs until the first
    /// surface. Inputs: the display's backing scale and the style. Output: the shared state,
    /// or `native_terminal_style_invalid` when the style's font cannot be loaded.
    pub fn new(scale: f32, style: &Style) -> Result<Shared, &'static str> {
        assert!((1.0..=4.0).contains(&scale), "a Mac backing scale");
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::METAL,
            ..Default::default()
        });
        let mut shared = Shared {
            instance,
            device: None,
            fonts: load_fallbacks(),
            swash: SwashCache::new(),
            scale,
            family: String::new(),
            font_px: 0.0,
            cell: (0.0, 0.0),
            theme: style.theme,
            generation: 0,
        };
        shared.set_style(style)?;
        assert!(shared.cell.0 >= 1.0 && shared.cell.1 >= 1.0);
        Ok(shared)
    }

    /// Applies a style to every view: loads its font file once, measures the cell by
    /// xterm's rule and takes its colours; each surface reshapes its rows on its next draw.
    /// Input: the style. Output: ok, or `native_terminal_style_invalid`, leaving the old
    /// style in place.
    pub fn set_style(&mut self, style: &Style) -> Result<(), &'static str> {
        let invalid = "native_terminal_style_invalid";
        if face_in(self.fonts.db(), style.file).is_none() {
            self.fonts
                .db_mut()
                .load_font_file(style.file)
                .map_err(|_| invalid)?;
        }
        let (id, family) = face_in(self.fonts.db(), style.file).ok_or(invalid)?;
        let face = face_em(&mut self.fonts, id)?;
        let (width, height) = cell_px(face, style.font_px, f64::from(self.scale));
        self.family = family;
        self.font_px = (style.font_px * f64::from(self.scale)) as f32;
        self.cell = (width as f32, height as f32);
        self.theme = style.theme;
        self.generation += 1;
        assert!(!self.family.is_empty() && self.font_px > 0.0);
        Ok(())
    }

    /// The cell size in the view's points (CSS pixels), for the grid a host rect gives.
    pub fn cell_points(&self) -> (f64, f64) {
        let scale = f64::from(self.scale);
        (
            f64::from(self.cell.0) / scale,
            f64::from(self.cell.1) / scale,
        )
    }

    /// Makes one view's surface on its NSView, sized in physical pixels, Fifo present mode and
    /// a frame latency of one. Inputs: the NSView pointer, width and height. Output: the
    /// surface, or the setup error.
    pub fn surface(
        &mut self,
        view: *mut c_void,
        width: u32,
        height: u32,
    ) -> Result<Surface, String> {
        let pointer = NonNull::new(view).ok_or("null NSView")?;
        let handle = raw_window_handle::AppKitWindowHandle::new(pointer);
        let target = wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: raw_window_handle::RawDisplayHandle::AppKit(
                raw_window_handle::AppKitDisplayHandle::new(),
            ),
            raw_window_handle: raw_window_handle::RawWindowHandle::AppKit(handle),
        };
        let surface =
            unsafe { self.instance.create_surface_unsafe(target) }.map_err(|e| e.to_string())?;
        if self.device.is_none() {
            self.device = Some(device_for(&self.instance, &surface)?);
        }
        let set = self.device.as_ref().ok_or("no device")?;
        let mut config = surface
            .get_default_config(&set.adapter, width.max(1), height.max(1))
            .ok_or("surface not supported")?;
        config.format = set.format;
        config.present_mode = wgpu::PresentMode::Fifo;
        config.desired_maximum_frame_latency = 1;
        surface.configure(&set.device, &config);
        let viewport = Viewport::new(&set.device, &set.cache);
        let mut atlas = TextAtlas::new(&set.device, &set.queue, &set.cache, set.format);
        let text = TextRenderer::new(
            &mut atlas,
            &set.device,
            wgpu::MultisampleState::default(),
            None,
        );
        let rect_buffer = set.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rects"),
            size: (RECTS_MAX * RECT_BYTES) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let screen_buffer = set.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("screen"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = set.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("screen"),
            layout: &set.layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: screen_buffer.as_entire_binding(),
            }],
        });
        Ok(Surface {
            surface,
            config,
            viewport,
            atlas,
            text,
            rows: Vec::new(),
            generation: self.generation,
            rect_buffer,
            screen_buffer,
            bind_group,
        })
    }

    /// Reconfigures a surface for a new size in physical pixels. Inputs: the surface, width
    /// and height; a zero side is ignored.
    pub fn resize(&self, surface: &mut Surface, width: u32, height: u32) {
        let Some(set) = self.device.as_ref() else {
            return;
        };
        if width == 0
            || height == 0
            || (width, height) == (surface.config.width, surface.config.height)
        {
            return;
        }
        surface.config.width = width;
        surface.config.height = height;
        surface.surface.configure(&set.device, &surface.config);
        assert_eq!(surface.config.width, width);
    }

    fn row_buffer(&mut self) -> Buffer {
        let metrics = Metrics::new(self.font_px, self.cell.1);
        let mut buffer = Buffer::new(&mut self.fonts, metrics);
        buffer.set_wrap(&mut self.fonts, Wrap::None);
        buffer.set_size(&mut self.fonts, None, Some(self.cell.1));
        buffer.set_monospace_width(&mut self.fonts, Some(self.cell.0));
        assert_eq!(buffer.metrics(), metrics);
        buffer
    }

    /// Shapes only the rows whose content is new: a row whose hash matches an old row, at
    /// any index, reuses that row's shaped buffer, so a scroll by n lines reshapes n rows.
    /// Inputs: the surface and the frame. Output: how many rows were shaped.
    fn shape_rows(&mut self, surface: &mut Surface, frame: &Frame) -> usize {
        let lines = frame.rows.len();
        assert_eq!(frame.keys.len(), lines);
        if surface.generation != self.generation {
            surface.rows.clear();
            surface.generation = self.generation;
        }
        let mut old: Vec<Option<Row>> = std::mem::take(&mut surface.rows)
            .into_iter()
            .map(Some)
            .collect();
        let mut next: Vec<Option<Row>> = (0..lines).map(|_| None).collect();
        for index in 0..lines.min(old.len()) {
            if old[index]
                .as_ref()
                .is_some_and(|row| row.key == frame.keys[index])
            {
                next[index] = old[index].take();
            }
        }
        let mut by_key: HashMap<u64, Vec<usize>> = HashMap::with_capacity(old.len());
        for (index, row) in old.iter().enumerate() {
            if let Some(row) = row {
                by_key.entry(row.key).or_default().push(index);
            }
        }
        for index in 0..lines {
            if next[index].is_none()
                && let Some(found) = by_key.get_mut(&frame.keys[index]).and_then(Vec::pop)
            {
                next[index] = old[found].take();
            }
        }
        let mut spare: Vec<Row> = old.into_iter().flatten().collect();
        let family = self.family.clone();
        let base = Attrs::new().family(Family::Name(&family));
        let mut shaped = 0;
        for (index, spans) in frame.rows.iter().enumerate() {
            if next[index].is_some() {
                continue;
            }
            let mut row = match spare.pop() {
                Some(row) => row,
                None => Row {
                    buffer: self.row_buffer(),
                    key: 0,
                },
            };
            let pieces = spans
                .iter()
                .map(|s: &Span| (s.text.as_str(), span_attrs(s, &family)));
            row.buffer
                .set_rich_text(&mut self.fonts, pieces, &base, Shaping::Advanced, None);
            row.buffer.shape_until_scroll(&mut self.fonts, false);
            row.key = frame.keys[index];
            next[index] = Some(row);
            shaped += 1;
        }
        surface.rows = next.into_iter().flatten().collect();
        assert_eq!(surface.rows.len(), lines, "every row has a buffer");
        assert!(shaped <= lines);
        shaped
    }

    /// The rectangle of a cursor drawn over its cell: a beam 1 point wide, an underline 1
    /// point tall, or the 4 edges of a hollow block; a block is drawn in the cell colours.
    fn cursor_rects(&self, cursor: Cursor) -> Vec<[f32; 4]> {
        let (cw, ch) = self.cell;
        let (x, y) = (cursor.column as f32 * cw, cursor.row as f32 * ch);
        let line = self.scale.round().max(1.0);
        let rects = match cursor.shape {
            CursorShape::Beam => vec![[x, y, line, ch]],
            CursorShape::Underline => vec![[x, y + ch - line, cw, line]],
            CursorShape::HollowBlock => vec![
                [x, y, cw, line],
                [x, y + ch - line, cw, line],
                [x, y, line, ch],
                [x + cw - line, y, line, ch],
            ],
            CursorShape::Block | CursorShape::Hidden => Vec::new(),
        };
        assert!(rects.len() <= 4);
        rects
    }

    fn rect_bytes(&self, frame: &Frame) -> Vec<f32> {
        let (cw, ch) = self.cell;
        let mut out = Vec::with_capacity((frame.fills.len() + 4) * 8);
        for fill in frame.fills.iter().take(RECTS_MAX - 4) {
            let x = fill.column as f32 * cw;
            let y = fill.row as f32 * ch;
            out.extend_from_slice(&[x, y, fill.width as f32 * cw, ch]);
            out.extend_from_slice(&linear_rgba(fill.color, 1.0));
        }
        assert!(frame.preedit.is_none() || frame.cursor.is_none());
        if let Some(preedit) = frame.preedit {
            let line = self.scale.round().max(1.0);
            out.extend_from_slice(&preedit_rect(preedit, self.cell, line));
            out.extend_from_slice(&linear_rgba(self.theme.foreground, 1.0));
        }
        for rect in frame
            .cursor
            .map(|c| self.cursor_rects(c))
            .unwrap_or_default()
        {
            out.extend_from_slice(&rect);
            out.extend_from_slice(&linear_rgba(self.theme.cursor, 1.0));
        }
        assert_eq!(out.len() % 8, 0);
        assert!(out.len() / 8 <= RECTS_MAX);
        out
    }

    /// Draws one frame on a surface: reshapes changed rows, uploads rectangles and glyphs,
    /// renders and presents. Inputs: the surface and the frame. Output: ok, or why it was
    /// skipped.
    pub fn draw(&mut self, surface: &mut Surface, frame: &Frame) -> Result<(), String> {
        self.shape_rows(surface, frame);
        let rects = self.rect_bytes(frame);
        let set = self.device.as_ref().ok_or("no device")?;
        let (width, height) = (surface.config.width, surface.config.height);
        let screen = [width as f32, height as f32, 0.0, 0.0];
        set.queue
            .write_buffer(&surface.screen_buffer, 0, bytemuck::cast_slice(&screen));
        set.queue
            .write_buffer(&surface.rect_buffer, 0, bytemuck::cast_slice(&rects));
        surface
            .viewport
            .update(&set.queue, Resolution { width, height });
        let bounds = TextBounds {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        };
        let areas = surface
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| TextArea {
                buffer: &row.buffer,
                left: 0.0,
                top: index as f32 * self.cell.1,
                scale: 1.0,
                bounds,
                default_color: text_color(self.theme.foreground),
                custom_glyphs: &[],
            });
        surface
            .text
            .prepare(
                &set.device,
                &set.queue,
                &mut self.fonts,
                &mut surface.atlas,
                &surface.viewport,
                areas,
                &mut self.swash,
            )
            .map_err(|e| e.to_string())?;
        let output = surface.surface.get_current_texture().map_err(|e| {
            surface.surface.configure(&set.device, &surface.config);
            e.to_string()
        })?;
        let clear = linear_rgba(self.theme.background, 1.0);
        encode(set, surface, &output, rects.len() / 8, clear)?;
        output.present();
        surface.atlas.trim();
        assert_eq!(surface.generation, self.generation);
        Ok(())
    }
}

fn encode(
    set: &Device,
    surface: &Surface,
    output: &wgpu::SurfaceTexture,
    rect_count: usize,
    clear: [f32; 4],
) -> Result<(), String> {
    let view = output
        .texture
        .create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = set.device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("frame"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: f64::from(clear[0]),
                        g: f64::from(clear[1]),
                        b: f64::from(clear[2]),
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        if rect_count > 0 {
            pass.set_pipeline(&set.rects);
            pass.set_bind_group(0, &surface.bind_group, &[]);
            pass.set_vertex_buffer(0, surface.rect_buffer.slice(..));
            pass.draw(0..4, 0..rect_count as u32);
        }
        surface
            .text
            .render(&surface.atlas, &surface.viewport, &mut pass)
            .map_err(|e| e.to_string())?;
    }
    set.queue.submit(Some(encoder.finish()));
    Ok(())
}

fn text_color(color: Rgb) -> TextColor {
    TextColor::rgb(color.r, color.g, color.b)
}

fn span_attrs<'a>(span: &Span, family: &'a str) -> Attrs<'a> {
    Attrs::new()
        .family(Family::Name(family))
        .color(text_color(span.fg))
        .weight(if span.bold {
            Weight::BOLD
        } else {
            Weight::NORMAL
        })
        .style(if span.italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marked_text_is_underlined_along_the_bottom_of_its_cells() {
        let wide = Preedit {
            row: 2,
            column: 5,
            cells: 3,
        };
        assert_eq!(
            preedit_rect(wide, (8.0, 17.0), 2.0),
            [40.0, 49.0, 24.0, 2.0]
        );
        let one = Preedit {
            row: 0,
            column: 0,
            cells: 1,
        };
        assert_eq!(preedit_rect(one, (7.0, 15.0), 1.0), [0.0, 14.0, 7.0, 1.0]);
    }
}
