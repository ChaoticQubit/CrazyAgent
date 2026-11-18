use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, fontdb,
};
use std::num::NonZeroIsize;
use std::time::Instant;

/// Rows of the stand-in terminal screen each text frame draws.
const ROWS: usize = 24;

/// Columns of filler text per row, the width of a common terminal.
const COLUMNS: usize = 80;

/// The clear colour of every frame: pure green, so a screen pixel read at the view's centre
/// tells the native view (0, 255, 0) from the page's red terminal box behind it.
const CLEAR: wgpu::Color = wgpu::Color { r: 0.0, g: 1.0, b: 0.0, a: 1.0 };

/// Times of one drawn frame: CPU build (text shaping and glyph upload), the wait for a
/// drawable, and the instant the frame was handed to the display.
pub struct Timing {
    pub build_ms: f64,
    pub acquire_ms: f64,
    pub submitted: Instant,
}

/// Direct3D 12 through wgpu on one child window, with a glyph atlas drawing a 24 x 80 text
/// screen whose last row shows the typed text.
pub struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    fonts: FontSystem,
    swash: SwashCache,
    viewport: Viewport,
    atlas: TextAtlas,
    text: TextRenderer,
    rows: Vec<Buffer>,
    line_px: f32,
    pub adapter: String,
    pub adapters: Vec<String>,
}

/// Loads Consolas, the monospace font every Windows install carries.
/// Output: the font system, or the reason it has no font.
fn fonts() -> Result<FontSystem, String> {
    let mut db = fontdb::Database::new();
    db.load_font_file("C:\\Windows\\Fonts\\consola.ttf").map_err(|e| e.to_string())?;
    db.set_monospace_family("Consolas");
    Ok(FontSystem::new_with_locale_and_db("en-US".into(), db))
}

/// Names every Direct3D 12 adapter the instance sees, with its type and driver, so a run
/// says whether its frame times come from a real GPU or the software rasterizer.
/// Input: the instance. Output: one line per adapter.
fn adapters(instance: &wgpu::Instance) -> Vec<String> {
    instance
        .enumerate_adapters(wgpu::Backends::DX12)
        .iter()
        .map(|adapter| {
            let info = adapter.get_info();
            format!("{} | {:?} | {:?} | {}", info.name, info.device_type, info.backend, info.driver)
        })
        .collect()
}

impl Gpu {
    /// Sets up Direct3D 12 on the child window `hwnd`, sized in physical pixels, Fifo present
    /// mode and a frame latency of one, falling back to the software adapter when no
    /// hardware one answers. Inputs: the window and module handles, width, height and the
    /// scale. Output: the GPU state, or the setup error.
    pub fn new(hwnd: isize, hinstance: isize, width: u32, height: u32, scale: f32) -> Result<Gpu, String> {
        let window = NonZeroIsize::new(hwnd).ok_or("null HWND")?;
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::DX12,
            ..Default::default()
        });
        let seen = adapters(&instance);
        let mut handle = raw_window_handle::Win32WindowHandle::new(window);
        handle.hinstance = NonZeroIsize::new(hinstance);
        let target = wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: raw_window_handle::RawDisplayHandle::Windows(
                raw_window_handle::WindowsDisplayHandle::new(),
            ),
            raw_window_handle: raw_window_handle::RawWindowHandle::Win32(handle),
        };
        let surface = unsafe { instance.create_surface_unsafe(target) }.map_err(|e| e.to_string())?;
        let request = |fallback: bool| {
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: Some(&surface),
                force_fallback_adapter: fallback,
            }))
        };
        let adapter = request(false).or_else(|_| request(true)).map_err(|e| e.to_string())?;
        let info = adapter.get_info();
        let named = format!("{} | {:?} | {:?}", info.name, info.device_type, info.backend);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("native-windows-spike"),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            ..Default::default()
        }))
        .map_err(|e| e.to_string())?;
        let mut config = surface
            .get_default_config(&adapter, width.max(1), height.max(1))
            .ok_or("surface not supported")?;
        config.present_mode = wgpu::PresentMode::Fifo;
        config.desired_maximum_frame_latency = 1;
        surface.configure(&device, &config);
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, config.format);
        let text = TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);
        let mut fonts = fonts()?;
        let font_px = 13.0 * scale;
        let line_px = (font_px * 1.25).round();
        let attrs = Attrs::new().family(Family::Monospace);
        let rows = (0..ROWS)
            .map(|row| {
                let mut buffer = Buffer::new(&mut fonts, Metrics::new(font_px, line_px));
                let filler: String = (0..COLUMNS)
                    .map(|column| char::from(b'!' + ((row * 7 + column) % 90) as u8))
                    .collect();
                buffer.set_text(&mut fonts, &filler, &attrs, Shaping::Basic);
                buffer.shape_until_scroll(&mut fonts, false);
                buffer
            })
            .collect();
        assert!(line_px > 0.0, "a text row has height");
        Ok(Gpu {
            surface,
            device,
            queue,
            config,
            fonts,
            swash: SwashCache::new(),
            viewport,
            atlas,
            text,
            rows,
            line_px,
            adapter: named,
            adapters: seen,
        })
    }

    /// Reconfigures the surface for a new size in physical pixels; a zero or unchanged
    /// size is ignored. Inputs: width and height.
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 || (width == self.config.width && height == self.config.height) {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    /// The surface size in physical pixels. Output: width and height.
    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// Draws one frame: the green clear, and with `text` the 24 rows whose last one shows
    /// the typed text; uploads glyphs, renders and presents. Inputs: whether to draw text
    /// and the typed text. Output: its timing, or why it was skipped.
    pub fn draw(&mut self, text: bool, typed: &str) -> Result<Timing, String> {
        let start = Instant::now();
        let (width, height) = (self.config.width, self.config.height);
        if text {
            let attrs = Attrs::new().family(Family::Monospace);
            let shown: String = typed.chars().rev().take(COLUMNS).collect::<Vec<_>>().into_iter().rev().collect();
            if let Some(last) = self.rows.last_mut() {
                last.set_text(&mut self.fonts, &format!("$ {shown}_"), &attrs, Shaping::Basic);
                last.shape_until_scroll(&mut self.fonts, false);
            }
            self.viewport.update(&self.queue, Resolution { width, height });
            let bounds = TextBounds { left: 0, top: 0, right: width as i32, bottom: height as i32 };
            let line_px = self.line_px;
            let areas = self.rows.iter().enumerate().map(|(index, buffer)| TextArea {
                buffer,
                left: 8.0,
                top: 8.0 + index as f32 * line_px,
                scale: 1.0,
                bounds,
                default_color: Color::rgb(0x10, 0x10, 0x10),
                custom_glyphs: &[],
            });
            self.text
                .prepare(&self.device, &self.queue, &mut self.fonts, &mut self.atlas, &self.viewport, areas, &mut self.swash)
                .map_err(|e| e.to_string())?;
        }
        let built = Instant::now();
        let output = self.surface.get_current_texture().map_err(|e| {
            self.surface.configure(&self.device, &self.config);
            e.to_string()
        })?;
        let acquired = Instant::now();
        let view = output.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("frame"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(CLEAR), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if text {
                self.text.render(&self.atlas, &self.viewport, &mut pass).map_err(|e| e.to_string())?;
            }
        }
        self.queue.submit(Some(encoder.finish()));
        output.present();
        let submitted = Instant::now();
        self.atlas.trim();
        Ok(Timing {
            build_ms: (built - start).as_secs_f64() * 1000.0,
            acquire_ms: (acquired - built).as_secs_f64() * 1000.0,
            submitted,
        })
    }
}
