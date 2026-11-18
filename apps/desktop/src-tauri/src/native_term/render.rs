use super::screen::{Cursor, Frame, Preedit, Span};
use super::style::{FaceEm, FontSource, Style, Theme, cell_px};
use alacritty_terminal::vte::ansi::{CursorShape, Rgb};
use glyphon::cosmic_text::{FeatureTag, FontFeatures};
use glyphon::{
    Attrs, Buffer, Cache, Color as TextColor, Family, FontSystem, Metrics, Resolution, Shaping,
    Style as FontStyle, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
    Weight, Wrap, fontdb,
};
use std::collections::HashMap;
use std::ffi::c_void;
use std::path::Path;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The code a view ends with when the GPU refused its work: a wgpu error the device's
/// handler recorded, or a surface or device that could not be made.
pub const GPU_FAULT: &str = "native_terminal_gpu";

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

/// The device every native view draws with, made with the first view's surface: the
/// largest texture side it allows and whether its error handler recorded a wgpu error
/// since the last render call took it.
struct Device {
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    format: wgpu::TextureFormat,
    rects: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    cache: Cache,
    side_max: u32,
    faulted: Arc<AtomicBool>,
}

impl Device {
    /// Takes the wgpu error the handler recorded since the last call. wgpu reports an
    /// error inside the call that caused it, so the render call that asks owns it.
    /// Output: ok, or `native_terminal_gpu`.
    fn fault(&self) -> Result<(), &'static str> {
        if self.faulted.swap(false, Ordering::AcqRel) {
            assert!(!self.faulted.load(Ordering::Acquire));
            return Err(GPU_FAULT);
        }
        Ok(())
    }

    /// Holds a surface side to what the device can make: at least 1, at most its largest
    /// texture side, so a view wider than the GPU allows is drawn smaller, never handed to
    /// wgpu to refuse. Input: the side in pixels. Output: the side to configure.
    fn fit(&self, side: u32) -> u32 {
        assert!(self.side_max >= 1);
        let fitted = side.clamp(1, self.side_max);
        assert!((1..=self.side_max).contains(&fitted));
        fitted
    }
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
    advance_px: f32,
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

/// The upright face of a loaded family whose weight is nearest regular. Inputs: the font
/// database and the family. Output: the face id, or none when the family is not loaded.
fn face_for(db: &fontdb::Database, family: &str) -> Option<fontdb::ID> {
    let face = db
        .faces()
        .filter(|face| face.style == fontdb::Style::Normal)
        .filter(|face| face.families.iter().any(|(name, _)| name == family))
        .min_by_key(|face| face.weight.0.abs_diff(fontdb::Weight::NORMAL.0))?;
    assert!(face.families.iter().any(|(name, _)| name == family));
    Some(face.id)
}

/// Loads a family's faces into the database: a system file, or the faces built into the
/// app, read in place. Inputs: the database and the source. Output: ok, or
/// `native_terminal_style_invalid` when a face does not load.
fn load(db: &mut fontdb::Database, source: FontSource) -> Result<(), &'static str> {
    let invalid = "native_terminal_style_invalid";
    let before = db.len();
    match source {
        FontSource::File(path) => db.load_font_file(path).map_err(|_| invalid)?,
        FontSource::Built(faces) => {
            for face in faces {
                let face: &'static [u8] = face;
                if db
                    .load_font_source(fontdb::Source::Binary(Arc::new(face)))
                    .is_empty()
                {
                    return Err(invalid);
                }
            }
        }
    }
    if db.len() == before {
        return Err(invalid);
    }
    assert!(db.len() > before);
    Ok(())
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
    if em <= 0.0 || metrics.advance <= 0.0 || metrics.ascent <= 0.0 || metrics.descent < 0.0 {
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

/// Makes the device for the first surface: the low-power adapter that can present to it,
/// with the adapter's own limits instead of wgpu's defaults (a 2D texture side of 8192),
/// and an error handler that logs a wgpu error and records it instead of panicking.
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
        required_limits: adapter.limits(),
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        ..Default::default()
    }))
    .map_err(|e| e.to_string())?;
    let side_max = device.limits().max_texture_dimension_2d;
    assert_eq!(side_max, adapter.limits().max_texture_dimension_2d);
    let side_floor = wgpu::Limits::downlevel_defaults().max_texture_dimension_2d;
    if side_max < side_floor {
        return Err(format!(
            "{GPU_FAULT}: largest texture side {side_max} < {side_floor}"
        ));
    }
    let faulted = Arc::new(AtomicBool::new(false));
    let sink = Arc::clone(&faulted);
    device.on_uncaptured_error(Box::new(move |error| {
        eprintln!("{GPU_FAULT}: {error}");
        sink.store(true, Ordering::Release);
    }));
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
        side_max,
        faulted,
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
            advance_px: 0.0,
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
        if face_for(self.fonts.db(), style.family).is_none() {
            load(self.fonts.db_mut(), style.source)?;
        }
        let id = face_for(self.fonts.db(), style.family).ok_or(invalid)?;
        let face = face_em(&mut self.fonts, id)?;
        let (width, height) = cell_px(face, style.font_px, f64::from(self.scale));
        self.family = style.family.to_owned();
        self.font_px = (style.font_px * f64::from(self.scale)) as f32;
        self.advance_px = (face.advance * style.font_px * f64::from(self.scale)) as f32;
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
        self.surface_on(target, width, height)
    }

    /// Makes one view's surface on a surface target that outlives it: the device on the
    /// first, then the configure and the view's drawing state. Inputs: the target, width
    /// and height in physical pixels. Output: the surface, or the setup error.
    fn surface_on(
        &mut self,
        target: wgpu::SurfaceTargetUnsafe,
        width: u32,
        height: u32,
    ) -> Result<Surface, String> {
        let surface =
            unsafe { self.instance.create_surface_unsafe(target) }.map_err(|e| e.to_string())?;
        if self.device.is_none() {
            self.device = Some(device_for(&self.instance, &surface)?);
        }
        let set = self.device.as_ref().ok_or("no device")?;
        let mut config = surface
            .get_default_config(&set.adapter, set.fit(width), set.fit(height))
            .ok_or("surface not supported")?;
        config.format = set.format;
        config.present_mode = wgpu::PresentMode::Fifo;
        config.desired_maximum_frame_latency = 1;
        surface.configure(&set.device, &config);
        assert!(config.width <= set.side_max);
        assert!(config.height <= set.side_max);
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
        set.fault()?;
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

    /// Reconfigures a surface for a new size in physical pixels, held to the device's
    /// largest texture side. Inputs: the surface, width and height; a zero side is ignored.
    /// Output: ok, or `native_terminal_gpu` when wgpu refused the configure.
    pub fn resize(
        &self,
        surface: &mut Surface,
        width: u32,
        height: u32,
    ) -> Result<(), &'static str> {
        let Some(set) = self.device.as_ref() else {
            return Ok(());
        };
        if width == 0 || height == 0 {
            return Ok(());
        }
        let (width, height) = (set.fit(width), set.fit(height));
        if (width, height) == (surface.config.width, surface.config.height) {
            return Ok(());
        }
        surface.config.width = width;
        surface.config.height = height;
        surface.surface.configure(&set.device, &surface.config);
        assert_eq!(surface.config.width, width);
        set.fault()
    }

    fn row_buffer(&mut self) -> Buffer {
        let metrics = Metrics::new(self.font_px, self.cell.1);
        let mut buffer = Buffer::new(&mut self.fonts, metrics);
        buffer.set_wrap(&mut self.fonts, Wrap::None);
        buffer.set_size(&mut self.fonts, None, Some(self.cell.1));
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
        for ((slot, key), held) in next.iter_mut().zip(&frame.keys).zip(old.iter_mut()) {
            if held.as_ref().is_some_and(|row| row.key == *key) {
                *slot = held.take();
            }
        }
        let mut by_key: HashMap<u64, Vec<usize>> = HashMap::with_capacity(old.len());
        for (index, row) in old.iter().enumerate() {
            if let Some(row) = row {
                by_key.entry(row.key).or_default().push(index);
            }
        }
        for (slot, key) in next.iter_mut().zip(&frame.keys) {
            if slot.is_none()
                && let Some(found) = by_key.get_mut(key).and_then(Vec::pop)
            {
                *slot = old[found].take();
            }
        }
        let mut spare: Vec<Row> = old.into_iter().flatten().collect();
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
            self.shape_row(&mut row.buffer, spans);
            row.key = frame.keys[index];
            next[index] = Some(row);
            shaped += 1;
        }
        surface.rows = next.into_iter().flatten().collect();
        assert_eq!(surface.rows.len(), lines, "every row has a buffer");
        assert!(shaped <= lines);
        shaped
    }

    /// The letter spacing, in em, that makes a glyph `natural` device pixels wide fill
    /// `cells` cells.
    fn spacing(&self, cells: usize, natural: f32) -> f32 {
        assert!(cells == 1 || cells == 2);
        assert!(self.font_px > 0.0);
        (cells as f32 * self.cell.0 - natural) / self.font_px
    }

    /// Sets a row's rich text with ligatures off: each run of characters of one span that
    /// share a letter spacing is one piece. Inputs: the buffer, the spans, their characters
    /// and each character's spacing.
    fn set_row(&mut self, buffer: &mut Buffer, spans: &[Span], places: &[Place], spacing: &[f32]) {
        assert_eq!(places.len(), spacing.len());
        let family = self.family.clone();
        let base = Attrs::new().family(Family::Name(&family));
        let mut features = FontFeatures::new();
        features
            .disable(FeatureTag::STANDARD_LIGATURES)
            .disable(FeatureTag::CONTEXTUAL_LIGATURES)
            .disable(FeatureTag::CONTEXTUAL_ALTERNATES);
        let mut pieces: Vec<(&str, Attrs<'_>)> = Vec::new();
        let mut first = 0;
        for last in 0..places.len() {
            let ends = places.get(last + 1).is_none_or(|next| {
                next.span != places[first].span || spacing[last + 1] != spacing[first]
            });
            if !ends {
                continue;
            }
            let span = &spans[places[first].span];
            let end = places[last].local + places[last].len;
            let attrs = span_attrs(span, &family)
                .letter_spacing(spacing[first])
                .font_features(features.clone());
            pieces.push((&span.text[places[first].local..end], attrs));
            first = last + 1;
        }
        assert!(pieces.len() <= places.len());
        buffer.set_rich_text(&mut self.fonts, pieces, &base, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.fonts, false);
    }

    /// Shapes one row so each glyph starts on its own cell and fills it, as xterm draws:
    /// every character first gets the spacing that fits the family's advance to its cells;
    /// a character whose glyphs still miss (a fallback font, a wide character) is spaced
    /// again from its measured width. Inputs: the buffer and the row's spans. Output: whether
    /// the second pass ran.
    fn shape_row(&mut self, buffer: &mut Buffer, spans: &[Span]) -> bool {
        let places = places(spans);
        let mut spacing: Vec<f32> = places
            .iter()
            .map(|p| self.spacing(p.cells, self.advance_px))
            .collect();
        self.set_row(buffer, spans, &places, &spacing);
        let widths = glyph_widths(buffer);
        let mut refit = false;
        for (place, space) in places.iter().zip(spacing.iter_mut()) {
            let Some(&(width, glyphs)) = widths.get(&place.byte) else {
                continue;
            };
            let target = place.cells as f32 * self.cell.0;
            if (width - target).abs() > 0.01 {
                *space += (target - width) / (glyphs as f32 * self.font_px);
                refit = true;
            }
        }
        if refit {
            self.set_row(buffer, spans, &places, &spacing);
        }
        assert!(places.len() >= spans.len());
        refit
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

    /// Draws one frame on a surface and takes any wgpu error the frame raised, on every
    /// exit, so the error is reported for this view and never for the next one drawn.
    /// Inputs: the surface and the frame. Output: ok, why it was skipped, or
    /// `native_terminal_gpu` when wgpu refused the frame's work.
    pub fn draw(&mut self, surface: &mut Surface, frame: &Frame) -> Result<(), String> {
        let painted = self.paint(surface, frame);
        let Some(set) = self.device.as_ref() else {
            return painted;
        };
        set.fault()?;
        assert!(surface.config.width <= set.side_max);
        assert!(surface.config.height <= set.side_max);
        painted
    }

    /// Paints one frame: reshapes changed rows, uploads rectangles and glyphs, renders and
    /// presents; a lost surface is reconfigured at its held size. Inputs: the surface and
    /// the frame. Output: ok, or why it was skipped.
    fn paint(&mut self, surface: &mut Surface, frame: &Frame) -> Result<(), String> {
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
        let output = match surface.surface.get_current_texture() {
            Ok(output) => output,
            Err(error) => {
                surface.surface.configure(&set.device, &surface.config);
                return Err(error.to_string());
            }
        };
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

/// One cell group of a row: its byte in the row's text, its span, its byte and length in
/// that span's text, and the cells it covers. A plain character is one place; a wide or
/// clustered span is one place, so zero-width characters take no cell of their own.
struct Place {
    byte: usize,
    span: usize,
    local: usize,
    len: usize,
    cells: usize,
}

fn places(spans: &[Span]) -> Vec<Place> {
    let mut out = Vec::new();
    let mut byte = 0;
    for (index, span) in spans.iter().enumerate() {
        if span.wide || span.cluster {
            let len = span.text.len();
            let cells = span.cells;
            out.push(Place {
                byte,
                span: index,
                local: 0,
                len,
                cells,
            });
            byte += len;
            continue;
        }
        for (local, c) in span.text.char_indices() {
            let len = c.len_utf8();
            out.push(Place {
                byte,
                span: index,
                local,
                len,
                cells: 1,
            });
            byte += len;
        }
    }
    assert_eq!(byte, spans.iter().map(|s| s.text.len()).sum::<usize>());
    assert!(out.len() >= spans.len());
    out
}

/// The width in device pixels and the glyph count of each character's glyphs in a shaped
/// row, by the character's byte in the row.
fn glyph_widths(buffer: &Buffer) -> HashMap<usize, (f32, usize)> {
    let mut out: HashMap<usize, (f32, usize)> = HashMap::new();
    for run in buffer.layout_runs() {
        for glyph in run.glyphs {
            assert!(glyph.w.is_finite());
            let entry = out.entry(glyph.start).or_insert((0.0, 0));
            entry.0 += glyph.w;
            entry.1 += 1;
        }
    }
    assert!(out.values().all(|&(_, glyphs)| glyphs > 0));
    out
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
    use crate::native_term::codec::grid_for;
    use crate::native_term::style::font_for;

    fn metrics_of(list: &str) -> FaceEm {
        let (family, source) = font_for(list).expect("a known font");
        let db = fontdb::Database::new();
        let mut fonts = FontSystem::new_with_locale_and_db("en-US".into(), db);
        load(fonts.db_mut(), source).expect("faces load");
        let id = face_for(fonts.db(), family).expect("an upright face");
        face_em(&mut fonts, id).expect("face metrics")
    }

    fn shared_at(scale: f32) -> Shared {
        let (family, source) = font_for("JetBrains Mono").expect("built in");
        let theme = Theme::default();
        let style = Style {
            family,
            source,
            font_px: 13.0,
            blink: true,
            theme,
        };
        Shared::new(scale, &style).expect("shared state")
    }

    fn span(text: &str, column: usize, wide: bool) -> Span {
        let cells = if wide { 2 } else { text.chars().count() };
        let fg = Theme::default().foreground;
        Span {
            text: text.into(),
            column,
            cells,
            wide,
            cluster: false,
            fg,
            bold: false,
            italic: false,
        }
    }

    /// A bare CAMetalLayer: a surface target a row can draw on off the main thread, where
    /// an NSView target cannot be made. Output: the layer, which must outlive its surface.
    fn metal_layer() -> objc2::rc::Retained<objc2::runtime::AnyObject> {
        let class = objc2::runtime::AnyClass::get(c"CAMetalLayer").expect("QuartzCore loaded");
        let layer: objc2::rc::Retained<objc2::runtime::AnyObject> =
            unsafe { objc2::msg_send![class, new] };
        layer
    }

    /// The surface target for a layer. Input: the layer. Output: the target.
    fn on_layer(layer: &objc2::runtime::AnyObject) -> wgpu::SurfaceTargetUnsafe {
        let pointer: *const objc2::runtime::AnyObject = layer;
        wgpu::SurfaceTargetUnsafe::CoreAnimationLayer(pointer.cast_mut().cast())
    }

    /// A one-row frame of text. Input: the text. Output: the frame.
    fn frame_of(text: &str) -> Frame {
        Frame {
            rows: vec![vec![span(text, 0, false)]],
            keys: vec![1],
            fills: Vec::new(),
            cursor: None,
            preedit: None,
        }
    }

    /// A view past wgpu's default texture side (8192) but within the adapter's, as a window
    /// across two 6K displays (12032 px), configures at its full width and draws: the device
    /// asks the adapter's own limits (#569).
    #[test]
    fn a_surface_past_the_default_texture_limit_configures_at_full_width_and_draws() {
        let mut shared = shared_at(2.0);
        let layer = metal_layer();
        let mut surface = shared
            .surface_on(on_layer(&layer), 64, 64)
            .expect("a surface");
        let set = shared.device.as_ref().expect("a device");
        let adapter_max = set.adapter.limits().max_texture_dimension_2d;
        assert!(
            adapter_max >= 12032,
            "this GPU's largest side is {adapter_max}"
        );
        shared.resize(&mut surface, 12032, 64).expect("a resize");
        assert_eq!((surface.config.width, surface.config.height), (12032, 64));
        shared
            .draw(&mut surface, &frame_of("wide"))
            .expect("drawn at its full width");
        drop(surface);
        drop(layer);
    }

    /// A view past the device's own largest texture side is held to it, made and resized
    /// alike, and still draws with no GPU fault, never handed to wgpu to panic on (#569).
    #[test]
    fn a_surface_past_the_device_texture_limit_is_held_to_it_and_still_draws() {
        let mut shared = shared_at(2.0);
        let layer = metal_layer();
        let mut surface = shared
            .surface_on(on_layer(&layer), 64, 64)
            .expect("a surface");
        let side_max = shared
            .device
            .as_ref()
            .expect("a device")
            .device
            .limits()
            .max_texture_dimension_2d;
        shared
            .resize(&mut surface, side_max + 1, 64)
            .expect("a resize past the limit");
        assert_eq!(
            (surface.config.width, surface.config.height),
            (side_max, 64)
        );
        shared
            .draw(&mut surface, &frame_of("held"))
            .expect("drawn at the held size, no fault");
        let tall = metal_layer();
        let made = shared
            .surface_on(on_layer(&tall), 64, side_max * 2)
            .expect("a surface made past the limit");
        assert_eq!((made.config.width, made.config.height), (64, side_max));
        drop((surface, made));
        drop((layer, tall));
    }

    /// A GPU validation error never panics: the device's handler records it, and the next
    /// render call for a view reports `native_terminal_gpu` so the host ends that view; the
    /// call after it draws again (#569).
    #[test]
    fn a_gpu_validation_error_is_reported_as_the_gpu_code_not_a_panic() {
        let mut shared = shared_at(2.0);
        let layer = metal_layer();
        let mut surface = shared
            .surface_on(on_layer(&layer), 64, 64)
            .expect("a surface");
        let set = shared.device.as_ref().expect("a device");
        let side = set.device.limits().max_texture_dimension_2d + 1;
        let _texture = set.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("past the limit"),
            size: wgpu::Extent3d {
                width: side,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let faulted = shared.draw(&mut surface, &frame_of("fault"));
        assert_eq!(faulted, Err("native_terminal_gpu".to_owned()));
        shared
            .draw(&mut surface, &frame_of("after"))
            .expect("the fault is reported once");
        drop(surface);
        drop(layer);
    }

    fn cluster(text: &str, column: usize) -> Span {
        Span {
            cells: 1,
            cluster: true,
            ..span(text, column, false)
        }
    }

    fn glyphs(buffer: &Buffer) -> Vec<(usize, f32, u16)> {
        let runs = buffer.layout_runs();
        runs.flat_map(|run| run.glyphs.iter().map(|g| (g.start, g.x, g.glyph_id)))
            .collect()
    }

    #[test]
    fn every_glyph_starts_on_its_cell() {
        for scale in [1.0, 2.0] {
            let mut shared = shared_at(scale);
            let cell = shared.cell.0;
            let mut buffer = shared.row_buffer();
            assert!(!shared.shape_row(&mut buffer, &[span("WWWW", 0, false)]));
            let xs: Vec<f32> = glyphs(&buffer).iter().map(|g| g.1).collect();
            assert_eq!(xs.len(), 4);
            for (k, x) in xs.iter().enumerate() {
                assert!(
                    (x - k as f32 * cell).abs() < 0.05,
                    "{scale}x glyph {k} at {x}"
                );
            }
            let row = [
                span("a ", 0, false),
                span("\u{4e2d}", 2, true),
                span("b", 4, false),
            ];
            shared.shape_row(&mut buffer, &row);
            let xs: Vec<f32> = glyphs(&buffer).iter().map(|g| g.1).collect();
            assert_eq!(xs.len(), 4);
            for (x, column) in xs.iter().zip([0.0, 1.0, 2.0, 4.0]) {
                assert!((x - column * cell).abs() < 0.05, "{scale}x glyph at {x}");
            }
        }
    }

    #[test]
    fn zero_width_characters_draw_in_their_cell_and_shift_nothing() {
        for scale in [1.0, 2.0] {
            let mut shared = shared_at(scale);
            let cell = shared.cell.0;
            let mut buffer = shared.row_buffer();
            for first in ["e\u{301}", "\u{2764}\u{fe0f}"] {
                let row = [cluster(first, 0), span("xy", 1, false)];
                assert_eq!(places(&row).len(), 3);
                shared.shape_row(&mut buffer, &row);
                let all = glyphs(&buffer);
                assert!(all.iter().any(|g| g.0 == 0), "{first:?} draws");
                let after: Vec<f32> = all
                    .iter()
                    .filter(|g| g.0 >= first.len())
                    .map(|g| g.1)
                    .collect();
                assert_eq!(after.len(), 2, "{first:?}");
                for (x, column) in after.iter().zip([1.0, 2.0]) {
                    assert!((x - column * cell).abs() < 0.05, "{scale}x {first:?} {x}");
                }
            }
        }
    }

    #[test]
    fn ligatures_are_off() {
        let mut shared = shared_at(2.0);
        let mut buffer = shared.row_buffer();
        shared.shape_row(&mut buffer, &[span("-", 0, false)]);
        let alone = glyphs(&buffer)[0].2;
        for pair in ["->", "-=", "--"] {
            shared.shape_row(&mut buffer, &[span(pair, 0, false)]);
            assert_eq!(glyphs(&buffer)[0].2, alone, "{pair}");
        }
    }

    #[test]
    fn the_built_in_font_gives_the_rig_xterm_grid() {
        let face = metrics_of("\"JetBrains Mono\", monospace");
        assert_eq!(
            face,
            FaceEm {
                advance: 0.6,
                ascent: 1.02,
                descent: 0.3
            }
        );
        let cell = cell_px(face, 13.0, 1.0);
        assert_eq!(cell, (7.0, 17.0));
        assert_eq!(grid_for(666.0, 362.0, cell, 1000), (93, 21));
        assert_eq!(cell_px(face, 13.0, 2.0), (15.0, 34.0));
    }

    #[test]
    fn menlo_loads_from_its_system_file() {
        let face = metrics_of("Menlo");
        assert_eq!(face.advance, 1233.0 / 2048.0);
        assert_eq!(cell_px(face, 13.0, 2.0), (15.0, 30.0));
    }

    #[test]
    fn built_in_faces_load_once_by_family() {
        let mut db = fontdb::Database::new();
        assert_eq!(face_for(&db, "JetBrains Mono"), None);
        let (_, source) = font_for("JetBrains Mono").expect("built in");
        load(&mut db, source).expect("faces load");
        assert_eq!(db.len(), 4);
        assert!(face_for(&db, "JetBrains Mono").is_some());
        assert_eq!(face_for(&db, "Menlo"), None);
    }

    #[test]
    fn a_font_file_with_no_face_is_refused_not_asserted() {
        let folder = tempfile::tempdir().expect("temp folder");
        let path = folder.path().join("Empty.ttc");
        std::fs::write(&path, b"not a font").expect("write");
        let path: &'static str = Box::leak(path.to_string_lossy().into_owned().into_boxed_str());
        let mut db = fontdb::Database::new();
        let loaded = load(&mut db, FontSource::File(path));
        assert_eq!(loaded, Err("native_terminal_style_invalid"));
        assert_eq!(db.len(), 0);
    }

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
