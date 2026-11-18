use super::screen::{BACKGROUND, FOREGROUND, Frame, Preedit, Span};
use alacritty_terminal::vte::ansi::Rgb;
use glyphon::{
    Attrs, Buffer, Cache, Color as TextColor, Family, FontSystem, Metrics, Resolution, Shaping,
    Style, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight, Wrap,
    fontdb,
};
use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::time::Instant;

/// The font files loaded, a fixed set instead of every system font, to keep memory small;
/// Apple Color Emoji is the only system font with U+23FA, the bullet Claude Code prints.
const FONT_PATHS: [&str; 4] = [
    "/System/Library/Fonts/Menlo.ttc",
    "/System/Library/Fonts/Apple Symbols.ttf",
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    "/System/Library/Fonts/Apple Color Emoji.ttc",
];

/// The font size in points before the display scale.
const FONT_POINTS: f32 = 13.0;

/// The most background rectangles one frame draws.
const RECTS_MAX: usize = 1 << 16;

/// Bytes of one rectangle: position, size and colour as f32.
const RECT_BYTES: usize = 32;

/// The padding around the grid, in points.
const PAD_POINTS: f32 = 4.0;

/// The thickness of the line under marked text, in points.
const UNDERLINE_POINTS: f32 = 1.0;

/// Times of one drawn frame: CPU build (rows, rects, glyph prepare), the wait for a drawable
/// and the instant the frame was handed to the display.
pub struct Timing {
    pub build_ms: f64,
    pub acquire_ms: f64,
    pub submitted: Instant,
}

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
/// rasterizer cache, and the cell size at the display scale.
pub struct Shared {
    instance: wgpu::Instance,
    device: Option<Device>,
    fonts: FontSystem,
    swash: SwashCache,
    pub scale: f32,
    pub cell: (f32, f32),
    pub pad: f32,
}

/// One native view's own drawing state: its Metal surface, glyph atlas, shaped rows and
/// rectangle buffers.
pub struct Surface {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    viewport: Viewport,
    atlas: TextAtlas,
    text: TextRenderer,
    rows: Vec<Row>,
    rect_buffer: wgpu::Buffer,
    screen_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
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

fn load_fonts() -> FontSystem {
    let mut db = fontdb::Database::new();
    for path in FONT_PATHS {
        if std::path::Path::new(path).exists() {
            let _ = db.load_font_file(path);
        }
    }
    assert!(db.len() > 0, "at least one font loaded");
    db.set_monospace_family("Menlo");
    FontSystem::new_with_locale_and_db("en-US".into(), db)
}

fn cell_size(fonts: &mut FontSystem, font_px: f32) -> (f32, f32) {
    assert!(font_px > 0.0);
    let line = (font_px * 1.25).round();
    let mut probe = Buffer::new(fonts, Metrics::new(font_px, line));
    let attrs = Attrs::new().family(Family::Monospace);
    probe.set_text(fonts, "M", &attrs, Shaping::Basic);
    probe.shape_until_scroll(fonts, false);
    let width = probe
        .layout_runs()
        .next()
        .and_then(|r| r.glyphs.first())
        .map_or(0.0, |g| g.w);
    assert!(width > 0.0, "monospace cell has a width");
    (width, line)
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
    /// Loads the fonts and measures a cell at the display scale; no GPU work runs until the
    /// first surface. Input: the display's backing scale. Output: the shared state.
    pub fn new(scale: f32) -> Shared {
        assert!(scale >= 1.0 && scale <= 4.0, "a Mac backing scale");
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::METAL,
            ..Default::default()
        });
        let mut fonts = load_fonts();
        let cell = cell_size(&mut fonts, FONT_POINTS * scale);
        let pad = PAD_POINTS * scale;
        Shared {
            instance,
            device: None,
            fonts,
            swash: SwashCache::new(),
            scale,
            cell,
            pad,
        }
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
        let font_px = self.cell.1 / 1.25;
        let mut buffer = Buffer::new(&mut self.fonts, Metrics::new(font_px, self.cell.1));
        buffer.set_wrap(&mut self.fonts, Wrap::None);
        buffer.set_size(&mut self.fonts, None, Some(self.cell.1));
        buffer.set_monospace_width(&mut self.fonts, Some(self.cell.0));
        buffer
    }

    /// Shapes only the rows whose content is new: a row whose hash matches an old row, at
    /// any index, reuses that row's shaped buffer, so a scroll by n lines reshapes n rows.
    /// Inputs: the surface and the frame. Output: how many rows were shaped.
    fn shape_rows(&mut self, surface: &mut Surface, frame: &Frame) -> usize {
        let lines = frame.rows.len();
        assert_eq!(frame.keys.len(), lines);
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
        let base = Attrs::new().family(Family::Monospace);
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
                .map(|s: &Span| (s.text.as_str(), span_attrs(s)));
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

    fn rect_bytes(&self, frame: &Frame) -> Vec<f32> {
        let (cw, ch) = self.cell;
        let mut out = Vec::with_capacity((frame.fills.len() + 1) * 8);
        for fill in frame.fills.iter().take(RECTS_MAX - 1) {
            let x = self.pad + fill.column as f32 * cw;
            let y = self.pad + fill.row as f32 * ch;
            out.extend_from_slice(&[x, y, fill.width as f32 * cw, ch]);
            out.extend_from_slice(&linear_rgba(fill.color, 1.0));
        }
        if let Some((row, column)) = frame.cursor {
            let x = self.pad + column as f32 * cw;
            let y = self.pad + row as f32 * ch;
            out.extend_from_slice(&[x, y, cw, ch]);
            out.extend_from_slice(&linear_rgba(FOREGROUND, 0.45));
        }
        assert_eq!(out.len() % 8, 0);
        assert!(out.len() / 8 <= RECTS_MAX);
        out
    }

    /// Draws one frame on a surface: reshapes changed rows, uploads rectangles and glyphs,
    /// renders and presents. Inputs: the surface and the frame. Output: its timing, or why it
    /// was skipped.
    pub fn draw(&mut self, surface: &mut Surface, frame: &Frame) -> Result<Timing, String> {
        let start = Instant::now();
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
                left: self.pad,
                top: self.pad + index as f32 * self.cell.1,
                scale: 1.0,
                bounds,
                default_color: text_color(FOREGROUND),
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
        let built = Instant::now();
        let output = surface.surface.get_current_texture().map_err(|e| {
            surface.surface.configure(&set.device, &surface.config);
            e.to_string()
        })?;
        let acquired = Instant::now();
        encode(set, surface, &output, rects.len() / 8)?;
        output.present();
        let submitted = Instant::now();
        surface.atlas.trim();
        let build_ms = (built - start).as_secs_f64() * 1000.0;
        let acquire_ms = (acquired - built).as_secs_f64() * 1000.0;
        Ok(Timing {
            build_ms,
            acquire_ms,
            submitted,
        })
    }
}

fn encode(
    set: &Device,
    surface: &Surface,
    output: &wgpu::SurfaceTexture,
    rect_count: usize,
) -> Result<(), String> {
    let view = output
        .texture
        .create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = set.device.create_command_encoder(&Default::default());
    let clear = linear_rgba(BACKGROUND, 1.0);
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

fn span_attrs(span: &Span) -> Attrs<'static> {
    Attrs::new()
        .family(Family::Monospace)
        .color(text_color(span.fg))
        .weight(if span.bold {
            Weight::BOLD
        } else {
            Weight::NORMAL
        })
        .style(if span.italic {
            Style::Italic
        } else {
            Style::Normal
        })
}

/// The line under marked text, in physical pixels: the full width of its cells at the
/// bottom of its row, `UNDERLINE_POINTS` thick and never under one pixel. Inputs: the marked
/// cells, the cell size, the padding and the display scale. Output: x, y, width, height.
pub fn underline(preedit: Preedit, cell: (f32, f32), pad: f32, scale: f32) -> [f32; 4] {
    let _ = (preedit, cell, pad, scale, UNDERLINE_POINTS);
    [0.0; 4]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marked_text_is_underlined_at_the_bottom_of_its_cells() {
        let preedit = Preedit {
            row: 1,
            column: 2,
            cells: 3,
        };
        assert_eq!(
            underline(preedit, (16.0, 32.0), 8.0, 2.0),
            [40.0, 70.0, 48.0, 2.0]
        );
        let first = Preedit {
            row: 0,
            column: 0,
            cells: 1,
        };
        assert_eq!(
            underline(first, (8.0, 16.0), 4.0, 1.0),
            [4.0, 19.0, 8.0, 1.0]
        );
    }
}
