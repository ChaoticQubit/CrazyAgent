use super::screen::{Cursor, Frame, Preedit, Span};
use super::style::{FaceEm, FontSource, JETBRAINS_MONO, Style, Theme, cell_px};
use alacritty_terminal::vte::ansi::{CursorShape, Rgb};
use glyphon::cosmic_text::{FeatureTag, FontFeatures};
use glyphon::{
    Attrs, Buffer, Cache, Color as TextColor, Family, FontSystem, Metrics, Resolution, Shaping,
    Style as FontStyle, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
    Weight, Wrap, fontdb,
};
use raw_window_handle::{RawDisplayHandle, RawWindowHandle};
use std::collections::HashMap;
#[cfg(target_os = "macos")]
use std::ffi::c_void;
use std::panic::{self, AssertUnwindSafe};
#[cfg(target_os = "macos")]
use std::ptr::NonNull;
use std::sync::Arc;
#[cfg(all(windows, test))]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(windows)]
use std::time::{Duration, Instant};

/// The code a view ends with when the GPU refused its work: a wgpu error the device's
/// handler recorded, or a surface or device that could not be made.
pub const GPU_FAULT: &str = "native_terminal_gpu";

/// The longest releasing a lost device may hold the Windows render thread: wgpu-core 25's
/// queue drop waits on the last submission for 100, 200, 400, 800, 1600 and 3200 ms, then
/// panics. A removed D3D12 device's fence reads complete, so its release returns at once.
#[cfg(all(windows, test))]
pub const RELEASE_STALL_MAX: Duration = Duration::from_millis(6300);

/// The fallback font files loaded beside the style's family, a fixed set instead of every
/// system font, to keep memory small; Apple Color Emoji is the only system font with
/// U+23FA, the bullet Claude Code prints. A missing fallback only leaves its glyphs blank.
#[cfg(target_os = "macos")]
const FALLBACKS: [FontSource; 3] = [
    FontSource::File("/System/Library/Fonts/Apple Symbols.ttf"),
    FontSource::File("/System/Library/Fonts/Supplemental/Arial Unicode.ttf"),
    FontSource::File("/System/Library/Fonts/Apple Color Emoji.ttc"),
];

/// The fallback font files on Linux: DejaVu Sans for symbols, Noto Color Emoji and Noto
/// Sans CJK, each where Debian and Ubuntu, Fedora and Arch install it. A missing file only
/// leaves its glyphs blank.
#[cfg(target_os = "linux")]
const FALLBACKS: [FontSource; 9] = [
    FontSource::File("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"),
    FontSource::File("/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf"),
    FontSource::File("/usr/share/fonts/TTF/DejaVuSans.ttf"),
    FontSource::File("/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf"),
    FontSource::File("/usr/share/fonts/google-noto-color-emoji-fonts/Noto-COLRv1.ttf"),
    FontSource::File("/usr/share/fonts/noto/NotoColorEmoji.ttf"),
    FontSource::File("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"),
    FontSource::File("/usr/share/fonts/google-noto-sans-cjk-fonts/NotoSansCJK-Regular.ttc"),
    FontSource::File("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc"),
];

/// The fallback font files on Windows, in its fonts folder: Segoe UI Symbol and Segoe UI
/// Emoji. A missing file only leaves its glyphs blank.
#[cfg(windows)]
const FALLBACKS: [FontSource; 2] = [
    FontSource::Fonts("seguisym.ttf"),
    FontSource::Fonts("seguiemj.ttf"),
];

/// The wgpu backends the native views draw with on macOS.
#[cfg(target_os = "macos")]
const BACKENDS: wgpu::Backends = wgpu::Backends::METAL;

/// The wgpu backends on Linux: Vulkan and GL. wgpu ranks the adapters that can present by
/// device type, a hardware GPU before a software one, and Vulkan before GL on the same type.
#[cfg(target_os = "linux")]
const BACKENDS: wgpu::Backends = wgpu::Backends::VULKAN.union(wgpu::Backends::GL);

/// The wgpu backends on Windows: DX12, with the adapter left to wgpu.
#[cfg(windows)]
const BACKENDS: wgpu::Backends = wgpu::Backends::DX12;

/// The most background rectangles one frame draws.
const RECTS_MAX: usize = 1 << 16;

/// Device attempts each backing scale's renderer may make after its first lost device in
/// one app run, failed ones counted: a GPU lost more often keeps its views ended until a
/// restart, and each attempt blocks the main thread on an adapter and device request.
const DEVICE_REPLACE_MAX: u32 = 2;

/// Bytes of one rectangle: position, size and colour as f32.
const RECT_BYTES: usize = 32;

struct Row {
    buffer: Buffer,
    key: u64,
}

/// The device every native view draws with, made with the first view's surface, or after
/// a loss with the next one's: the largest texture side it allows, whether its error
/// handler recorded a wgpu error since the last render call took it, and whether wgpu
/// reported the device lost.
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
    lost: Arc<AtomicBool>,
}

impl Device {
    /// Whether wgpu reported this device lost. wgpu runs the lost callback inside the call
    /// that lost the device, and every wgpu call here runs on one thread (the main thread on
    /// macOS, the render thread on Windows), so a loss is seen by the next check on that
    /// thread. Output: the answer; it never clears.
    fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Acquire)
    }

    /// Takes the wgpu error the handler recorded since the last call. wgpu reports an
    /// error inside the call that caused it, so the render call that asks owns it.
    /// Output: ok, or `native_terminal_gpu`.
    fn fault(&self) -> Result<(), &'static str> {
        if self.faulted.swap(false, Ordering::AcqRel) {
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
/// its rows after one. `attempts_left` is none until a device is lost, then the device
/// attempts still allowed.
pub struct Shared {
    instance: wgpu::Instance,
    device: Option<Device>,
    attempts_left: Option<u32>,
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

/// One native view's own drawing state: its wgpu surface, glyph atlas, shaped rows (of the
/// style generation they were shaped in), rectangle buffers, and the lost flag of the
/// device they were all made on.
pub struct Surface {
    lost: Arc<AtomicBool>,
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

impl Surface {
    /// Whether the device this surface was made on is lost. A device is only replaced once
    /// lost, so a surface from an older device always answers yes. Output: the answer.
    fn device_lost(&self) -> bool {
        self.lost.load(Ordering::Acquire)
    }
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

/// Loads the fallback font files that are present, then the four built-in JetBrains Mono
/// faces last, so a span of any style and weight has a face even with no system font:
/// cosmic-text panics while shaping a span no loaded face matches. Input: the files.
/// Output: the font system.
fn load_fallbacks(files: &[FontSource]) -> FontSystem {
    let mut db = fontdb::Database::new();
    for path in files.iter().filter_map(FontSource::file) {
        if path.is_file() && db.load_font_file(&path).is_err() {
            continue;
        }
    }
    let before = db.len();
    load(&mut db, FontSource::Built(&JETBRAINS_MONO)).expect("the built-in faces load");
    assert_eq!(db.len(), before + JETBRAINS_MONO.len());
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
        system => {
            let path = system.file().ok_or(invalid)?;
            db.load_font_file(path).map_err(|_| invalid)?;
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

/// Makes a device for a surface when none is held (the first, or after a loss or a failed
/// attempt): the low-power adapter that can present to it, with the adapter's own limits
/// instead of wgpu's defaults (a 2D texture side of 8192), an error handler that logs a wgpu
/// error and records it instead of panicking, and a lost callback that logs the loss and
/// marks the device lost and faulted.
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
    let lost = Arc::new(AtomicBool::new(false));
    let (lost_mark, fault_mark) = (Arc::clone(&lost), Arc::clone(&faulted));
    device.set_device_lost_callback(move |reason, message| {
        eprintln!("{GPU_FAULT}: device lost ({reason:?}): {message}");
        lost_mark.store(true, Ordering::Release);
        fault_mark.store(true, Ordering::Release);
    });
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
        lost,
    })
}

impl Shared {
    /// Loads the fallback fonts and applies the first style; no GPU work runs until the first
    /// surface. Inputs: the display's backing scale and the style. Output: the shared state,
    /// or `native_terminal_style_invalid` when the style's font cannot be loaded.
    pub fn new(scale: f32, style: &Style) -> Result<Shared, &'static str> {
        assert!((1.0..=4.0).contains(&scale), "a display backing scale");
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: BACKENDS,
            ..Default::default()
        });
        let mut shared = Shared {
            instance,
            device: None,
            attempts_left: None,
            fonts: load_fallbacks(&FALLBACKS),
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

    /// Makes one view's surface on its NSView through `surface_raw`. Safety: `view` must
    /// point to a live NSView that outlives the returned surface. Inputs: the NSView pointer,
    /// width and height. Output: the surface, or the setup error.
    #[cfg(target_os = "macos")]
    pub unsafe fn surface(
        &mut self,
        view: *mut c_void,
        width: u32,
        height: u32,
    ) -> Result<Surface, String> {
        let pointer = NonNull::new(view).ok_or("null NSView")?;
        let display = RawDisplayHandle::AppKit(raw_window_handle::AppKitDisplayHandle::new());
        let window = RawWindowHandle::AppKit(raw_window_handle::AppKitWindowHandle::new(pointer));
        unsafe { self.surface_raw(display, window, width, height) }
    }

    /// Makes one view's surface on a native window, sized in physical pixels, Fifo present
    /// mode and a frame latency of one. Safety: both handles must name a live display and
    /// window that outlive the returned surface. Inputs: the handles, width and height.
    /// Output: the surface, or the setup error.
    pub unsafe fn surface_raw(
        &mut self,
        display: RawDisplayHandle,
        window: RawWindowHandle,
        width: u32,
        height: u32,
    ) -> Result<Surface, String> {
        let target = wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: display,
            raw_window_handle: window,
        };
        unsafe { self.surface_on(target, width, height) }
    }

    /// The device a new surface is made on: the held one, or after a loss a fresh one while
    /// attempts are left. On macOS and Linux a lost device is forgotten, never dropped: wgpu
    /// 25's queue drop waits up to 6.3 s on the main thread and can panic, so one device leaks
    /// per loss, at most `DEVICE_REPLACE_MAX` + 1 per scale per run. On Windows it goes
    /// through `release`. Input: the new surface. Output: the device, `native_terminal_gpu`
    /// once spent, or `device_for`'s error.
    fn device_on(&mut self, surface: &wgpu::Surface<'static>) -> Result<&Device, String> {
        if self.device.as_ref().is_some_and(Device::is_lost) {
            let lost = self.device.take();
            #[cfg(windows)]
            release(lost, Released::Device);
            #[cfg(not(windows))]
            std::mem::forget(lost);
            self.attempts_left.get_or_insert(DEVICE_REPLACE_MAX);
        }
        if self.device.is_none() {
            if let Some(left) = self.attempts_left.as_mut() {
                if *left == 0 {
                    return Err(GPU_FAULT.to_owned());
                }
                *left -= 1;
            }
            self.device = Some(device_for(&self.instance, surface)?);
        }
        assert!(
            self.attempts_left
                .is_none_or(|left| left < DEVICE_REPLACE_MAX)
        );
        Ok(self
            .device
            .as_ref()
            .expect("a device once device_for succeeded"))
    }

    /// Makes one view's surface on a surface target: the device on the first, or a fresh
    /// one after a loss, then the configure and the view's drawing state. Safety: the target
    /// must name a live layer or window that outlives the returned surface. Inputs: the
    /// target, width and height in physical pixels. Output: the surface, or the setup error.
    unsafe fn surface_on(
        &mut self,
        target: wgpu::SurfaceTargetUnsafe,
        width: u32,
        height: u32,
    ) -> Result<Surface, String> {
        let surface =
            unsafe { self.instance.create_surface_unsafe(target) }.map_err(|e| e.to_string())?;
        let set = self.device_on(&surface)?;
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
        let lost = Arc::clone(&set.lost);
        set.fault()?;
        Ok(Surface {
            lost,
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
    /// largest texture side; a surface whose device is lost is never moved onto another.
    /// Inputs: the surface, width and height; a zero side is ignored. Output: ok, or
    /// `native_terminal_gpu` when its device is lost or wgpu refused the configure.
    pub fn resize(
        &self,
        surface: &mut Surface,
        width: u32,
        height: u32,
    ) -> Result<(), &'static str> {
        if surface.device_lost() {
            return Err(GPU_FAULT);
        }
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
    /// exit, so the error is reported for this view and never for the next one drawn. A
    /// surface whose device is lost reports `native_terminal_gpu` and leaves the current
    /// device's errors to that device's views. Inputs: the surface and the frame. Output:
    /// ok, why it was skipped, or `native_terminal_gpu` when wgpu refused the frame's work.
    #[cfg(target_os = "macos")]
    pub fn draw(&mut self, surface: &mut Surface, frame: &Frame) -> Result<(), String> {
        let painted = self.paint(surface, frame, None);
        self.taken(surface, painted)
    }

    /// Waits for a surface's next texture; a failed wait reconfigures the surface. `draw`
    /// calls it after building the frame; a caller that wants output arriving during the
    /// wait drawn in that frame calls it first and builds with `draw_acquired`. Input: the
    /// surface. Output: the texture, or why there is none.
    pub fn acquire(&self, surface: &mut Surface) -> Result<wgpu::SurfaceTexture, String> {
        let set = self.device.as_ref().ok_or("no device")?;
        surface.surface.get_current_texture().map_err(|e| {
            surface.surface.configure(&set.device, &surface.config);
            e.to_string()
        })
    }

    /// Draws one frame into a texture `acquire` gave and takes any wgpu error as `draw`
    /// does. Inputs: the surface, the frame and the texture. Output: ok, why it was
    /// skipped, or `native_terminal_gpu`.
    #[cfg(windows)]
    pub fn draw_acquired(
        &mut self,
        surface: &mut Surface,
        frame: &Frame,
        output: wgpu::SurfaceTexture,
    ) -> Result<(), String> {
        let painted = self.paint(surface, frame, Some(output));
        self.taken(surface, painted)
    }

    /// Takes the wgpu error a frame raised, which wins over the paint's own result, and
    /// checks the surface stayed within the GPU's texture side. A surface whose device is
    /// lost reports `native_terminal_gpu` and takes none of the current device's errors.
    /// Inputs: the surface and the paint's result. Output: the frame's result.
    fn taken(&self, surface: &Surface, painted: Result<(), String>) -> Result<(), String> {
        if surface.device_lost() {
            return Err(GPU_FAULT.to_owned());
        }
        let Some(set) = self.device.as_ref() else {
            return painted;
        };
        set.fault()?;
        assert!(surface.config.width <= set.side_max);
        assert!(surface.config.height <= set.side_max);
        painted
    }

    /// Whether wgpu reported the device lost; false while no device is held, before the
    /// first surface makes one or after `retire_lost`. Output: the answer.
    #[cfg(windows)]
    pub fn lost(&self) -> bool {
        self.device.as_ref().is_some_and(Device::is_lost)
    }

    /// Waits for the device's work to finish so wgpu frees what a dropped surface left: a
    /// window takes one swap chain at a time, so its next surface, on another scale's
    /// device, needs the old one gone. A lost device is left to `retire_lost`. Output: ok,
    /// or `native_terminal_gpu` when the wait failed.
    #[cfg(windows)]
    pub fn settle(&self) -> Result<(), &'static str> {
        let Some(set) = self.device.as_ref().filter(|set| !set.is_lost()) else {
            return Ok(());
        };
        set.device
            .poll(wgpu::PollType::Wait)
            .map(|_| ())
            .map_err(|_| GPU_FAULT)
    }

    /// Whether a device is held, so a surface made now requests none. Output: the answer.
    #[cfg(windows)]
    pub fn has_device(&self) -> bool {
        self.device.is_some()
    }

    /// Releases a device wgpu reported lost through `release`, so D3D12 can make a new one
    /// on the same adapter. Only the device goes (its adapter, queue, pipeline and glyph
    /// cache); the instance and fonts stay, and the next surface requests a new device.
    /// Output: whether a lost device was released.
    #[cfg(windows)]
    pub fn retire_lost(&mut self) -> bool {
        let Some(lost) = self.device.take_if(|device| device.is_lost()) else {
            return false;
        };
        release(lost, Released::Device);
        true
    }

    /// Takes the wgpu error the device recorded since the last render call took it, as
    /// `taken` does after a frame; none before the first surface. Output: ok, or
    /// `native_terminal_gpu`.
    #[cfg(windows)]
    pub fn take_fault(&self) -> Result<(), &'static str> {
        self.device.as_ref().map_or(Ok(()), Device::fault)
    }

    /// Test builds only: records a wgpu error on the device, as the error handler does for
    /// a reconfigure wgpu refused; the next render call that takes errors meets it.
    #[cfg(all(windows, test))]
    pub fn trial_error(&self) {
        let set = self.device.as_ref().expect("a device to record on");
        set.faulted.store(true, Ordering::Release);
        assert!(set.faulted.load(Ordering::Acquire), "the error is recorded");
    }

    /// Test builds only: destroys the device, as a GPU reset would, and waits until wgpu
    /// reports it lost; the render thread's next GPU call then meets the loss.
    #[cfg(all(windows, test))]
    pub fn trial_lose(&self) {
        let set = self.device.as_ref().expect("a device to lose");
        set.device.destroy();
        let polled = set.device.poll(wgpu::PollType::Wait).is_ok();
        eprintln!("trial loss: poll ok {polled}");
        assert!(set.is_lost(), "wgpu reported the loss");
    }

    /// Test builds only: removes the D3D12 device right before this state's next present,
    /// as a driver reset that `Present` itself finds would, with wgpu not told first.
    #[cfg(all(windows, test))]
    pub fn trial_remove_at_present(&self) {
        assert!(self.device.is_some(), "a device to remove");
        let armed = REMOVE_AT_PRESENT.swap(true, Ordering::SeqCst);
        assert!(!armed, "one removal armed at a time");
    }

    /// Gives back a texture acquired for a frame that is not drawn: dropped, which discards
    /// it, or on a lost device, where the drop panics in wgpu 25, forgotten as in `hand_back`
    /// on macOS and Linux and released on Windows. Input: the texture. Output: ok, or
    /// `native_terminal_gpu` on a lost device.
    pub fn give_back(&self, output: wgpu::SurfaceTexture) -> Result<(), &'static str> {
        let set = self
            .device
            .as_ref()
            .expect("an acquired texture has a device");
        assert!(output.texture.width() <= set.side_max);
        if set.is_lost() {
            #[cfg(windows)]
            release(output, Released::Frame);
            #[cfg(not(windows))]
            std::mem::forget(output);
            return Err(GPU_FAULT);
        }
        drop(output);
        Ok(())
    }

    /// Paints one frame: builds it, then renders and presents into the texture given, else
    /// the surface's next one; a lost surface is reconfigured at its held size. A texture
    /// given for a frame that is not built goes back through `give_back`. Inputs: the
    /// surface, the frame and an acquired texture. Output: ok, why it was skipped, or
    /// `native_terminal_gpu`.
    fn paint(
        &mut self,
        surface: &mut Surface,
        frame: &Frame,
        acquired: Option<wgpu::SurfaceTexture>,
    ) -> Result<(), String> {
        let rects = match self.build(surface, frame) {
            Ok(rects) => rects,
            Err(error) => {
                if let Some(output) = acquired {
                    self.give_back(output)?;
                }
                return Err(error);
            }
        };
        let output = match acquired {
            Some(output) => output,
            None => self.acquire(surface)?,
        };
        let set = self.device.as_ref().expect("a built frame has a device");
        let clear = linear_rgba(self.theme.background, 1.0);
        let encoded = encode(set, surface, &output, rects.len() / 8, clear);
        hand_back(set, output, encoded)?;
        surface.atlas.trim();
        assert_eq!(surface.generation, self.generation);
        Ok(())
    }

    /// Builds one frame's GPU input: reshapes changed rows, uploads the rectangles, the
    /// screen size and the glyphs. When the surface's own device is lost it does no work,
    /// since glyphon's buffer map then panics in wgpu 25; a loss found inside glyphon's own
    /// buffer creation goes through `guarded`. Inputs: the surface and the frame. Output:
    /// the rectangle data, why the frame is skipped, or `native_terminal_gpu`.
    fn build(&mut self, surface: &mut Surface, frame: &Frame) -> Result<Vec<f32>, String> {
        if surface.device_lost() {
            return Err(GPU_FAULT.to_owned());
        }
        self.shape_rows(surface, frame);
        let rects = self.rect_bytes(frame);
        let set = self.device.as_ref().ok_or("no device")?;
        assert!(
            Arc::ptr_eq(&surface.lost, &set.lost),
            "a surface drawn by the renderer that made it"
        );
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
        let prepared = guarded(&set.lost, || {
            surface.text.prepare(
                &set.device,
                &set.queue,
                &mut self.fonts,
                &mut surface.atlas,
                &surface.viewport,
                areas,
                &mut self.swash,
            )
        });
        prepared?.map_err(|e| e.to_string())?;
        assert_eq!(rects.len() % 8, 0);
        Ok(rects)
    }
}

/// Hands an acquired frame back to wgpu: presents it once encoded, or lets it drop, which
/// discards it, when encoding failed. On a lost device both panic in wgpu 25, so the frame
/// is forgotten on macOS and Linux (one frame leaks) and released on Windows, and its view
/// ends. A loss that present itself finds, as DX12's does on a driver reset, goes through
/// `guarded`. Inputs: the device, the frame and the encode result. Output: ok, the encode
/// error, or `native_terminal_gpu`.
fn hand_back(
    set: &Device,
    output: wgpu::SurfaceTexture,
    encoded: Result<(), String>,
) -> Result<(), String> {
    assert!(output.texture.width() <= set.side_max);
    assert!(output.texture.height() <= set.side_max);
    if set.is_lost() {
        #[cfg(windows)]
        release(output, Released::Frame);
        #[cfg(not(windows))]
        std::mem::forget(output);
        return Err(GPU_FAULT.to_owned());
    }
    encoded?;
    at_present(set);
    guarded(&set.lost, || output.present()).map_err(str::to_owned)
}

#[cfg(panic = "abort")]
compile_error!("the native terminal turns wgpu's panic on a lost GPU into an end: it must unwind");

/// Runs one wgpu call that can find the device lost inside itself: wgpu 25 marks the device
/// lost, which runs its callback, then panics. That panic on a lost device becomes
/// `native_terminal_gpu`, so the render thread lives and the view ends; any other panic
/// goes on. Inputs: the device's lost flag and the call. Output: its value, or the code.
fn guarded<T>(lost: &AtomicBool, call: impl FnOnce() -> T) -> Result<T, &'static str> {
    match panic::catch_unwind(AssertUnwindSafe(call)) {
        Ok(value) => Ok(value),
        Err(_) if lost.load(Ordering::Acquire) => Err(GPU_FAULT),
        Err(raised) => panic::resume_unwind(raised),
    }
}

/// Drops a lost device, or a frame acquired on one, on the Windows render thread instead of
/// forgetting it: D3D12 keeps one device per adapter and refuses a new one while a removed
/// one is still held. It runs only after a reported loss, within `RELEASE_STALL_MAX`; a
/// panic inside wgpu's drop is caught and logged, so the render thread goes on. Inputs:
/// what to drop and which kind it is.
#[cfg(windows)]
fn release<T>(lost: T, kind: Released) {
    let started = Instant::now();
    let dropped = panic::catch_unwind(AssertUnwindSafe(move || drop(lost)));
    if dropped.is_err() {
        eprintln!("{GPU_FAULT}: releasing a lost device panicked");
    }
    released(kind, started.elapsed());
}

/// What `release` drops: a lost device, or a frame acquired on one.
#[cfg(windows)]
#[derive(Clone, Copy)]
enum Released {
    Device,
    Frame,
}

#[cfg(all(windows, test))]
static RELEASED_DEVICES: AtomicU64 = AtomicU64::new(0);
#[cfg(all(windows, test))]
static RELEASED_FRAMES: AtomicU64 = AtomicU64::new(0);
#[cfg(all(windows, test))]
static RELEASE_LONGEST_US: AtomicU64 = AtomicU64::new(0);

#[cfg(all(windows, test))]
fn released(kind: Released, took: Duration) {
    let micros = u64::try_from(took.as_micros()).unwrap_or(u64::MAX);
    let count = match kind {
        Released::Device => &RELEASED_DEVICES,
        Released::Frame => &RELEASED_FRAMES,
    };
    count.fetch_add(1, Ordering::SeqCst);
    RELEASE_LONGEST_US.fetch_max(micros, Ordering::SeqCst);
    assert!(RELEASE_LONGEST_US.load(Ordering::SeqCst) >= micros);
}

#[cfg(all(windows, not(test)))]
fn released(_: Released, _: Duration) {}

/// Test builds only: how many lost devices and frames the render thread released, and the
/// longest release. Output: the device count, the frame count and the time.
#[cfg(all(windows, test))]
pub fn trial_releases() -> (u64, u64, Duration) {
    let longest = Duration::from_micros(RELEASE_LONGEST_US.load(Ordering::SeqCst));
    let devices = RELEASED_DEVICES.load(Ordering::SeqCst);
    (devices, RELEASED_FRAMES.load(Ordering::SeqCst), longest)
}

#[cfg(all(windows, test))]
static REMOVE_AT_PRESENT: AtomicBool = AtomicBool::new(false);
#[cfg(all(windows, test))]
static REMOVALS: AtomicU64 = AtomicU64::new(0);

/// Test builds only: how many D3D12 devices the present seam removed. Output: the count.
#[cfg(all(windows, test))]
pub fn trial_removals() -> u64 {
    REMOVALS.load(Ordering::SeqCst)
}

#[cfg(all(windows, test))]
fn at_present(set: &Device) {
    if REMOVE_AT_PRESENT.swap(false, Ordering::SeqCst) {
        remove_device(set);
    }
}

#[cfg(not(all(windows, test)))]
fn at_present(_: &Device) {}

/// Test builds only: puts the D3D12 device in the removed state through
/// `ID3D12Device5::RemoveDevice`, Microsoft's call for testing removal, and prints what
/// the device then reports. wgpu-hal's device is a windows 0.58 type, one COM pointer, so
/// the pointer is borrowed as windows 0.62's. Input: the device.
#[cfg(all(windows, test))]
fn remove_device(set: &Device) {
    use windows::Win32::Graphics::Direct3D12::{ID3D12Device, ID3D12Device5};
    use windows::core::Interface;
    let reason = unsafe {
        set.device.as_hal::<wgpu::hal::api::Dx12, _, _>(|hal| {
            let raw = hal.expect("a DX12 device").raw_device();
            let pointer_size = size_of::<*mut std::ffi::c_void>();
            assert_eq!(size_of_val(raw), pointer_size, "one COM pointer");
            let pointer: *mut std::ffi::c_void = std::mem::transmute_copy(raw);
            assert!(!pointer.is_null(), "a live device");
            let device = ID3D12Device::from_raw_borrowed(&pointer).expect("a device");
            let five = device.cast::<ID3D12Device5>();
            println!("trial removal: ID3D12Device5 cast ok {}", five.is_ok());
            five.expect("ID3D12Device5").RemoveDevice();
            device.GetDeviceRemovedReason()
        })
    };
    println!("trial removal: removed reason {reason:?}");
    assert!(reason.is_err(), "the device reads removed");
    REMOVALS.fetch_add(1, Ordering::SeqCst);
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
mod tests;
