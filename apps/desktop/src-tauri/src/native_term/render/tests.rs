use super::*;

/// A panic inside a guarded call once the call marked the device lost is the GPU code,
/// so the render thread lives: wgpu 25 does that when DX12's present finds the loss
/// itself. Synthetic: the call marks the flag and panics as wgpu's fatal handler does.
#[test]
fn a_panic_inside_a_guarded_call_that_lost_the_device_is_the_gpu_code() {
    let lost = AtomicBool::new(false);
    let caught = guarded(&lost, || -> u8 {
        lost.store(true, Ordering::Release);
        panic!("Error in Surface::present: Parent device is lost")
    });
    assert_eq!(caught, Err(GPU_FAULT));
}

/// A guarded call that returns gives its value, and a panic inside one on a live device
/// goes on: only a loss is caught.
#[test]
#[should_panic(expected = "not a loss")]
fn a_panic_inside_a_guarded_call_on_a_live_device_goes_on() {
    let lost = AtomicBool::new(false);
    assert_eq!(guarded(&lost, || 7_u8), Ok(7));
    let _ = guarded(&lost, || -> u8 { panic!("not a loss") });
}
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
#[cfg(target_os = "macos")]
fn metal_layer() -> objc2::rc::Retained<objc2::runtime::AnyObject> {
    let class = objc2::runtime::AnyClass::get(c"CAMetalLayer").expect("QuartzCore loaded");
    let layer: objc2::rc::Retained<objc2::runtime::AnyObject> =
        unsafe { objc2::msg_send![class, new] };
    layer
}

/// The surface target for a layer. Input: the layer. Output: the target.
#[cfg(target_os = "macos")]
fn on_layer(layer: &objc2::runtime::AnyObject) -> wgpu::SurfaceTargetUnsafe {
    let pointer: *const objc2::runtime::AnyObject = layer;
    wgpu::SurfaceTargetUnsafe::CoreAnimationLayer(pointer.cast_mut().cast())
}

/// A one-row frame of text. Input: the text. Output: the frame.
#[cfg(target_os = "macos")]
fn frame_of(text: &str) -> Frame {
    Frame {
        rows: vec![vec![span(text, 0, false)]],
        keys: vec![1],
        fills: Vec::new(),
        cursor: None,
        preedit: None,
    }
}

/// Loses a shared renderer's device as a GPU reset would: destroys it, then polls so
/// wgpu runs its device-lost callback (wgpu-core resource.rs:561-575). Input: the
/// renderer, with a device.
#[cfg(target_os = "macos")]
fn lose(shared: &Shared) {
    let set = shared.device.as_ref().expect("a device");
    set.device.destroy();
    set.device
        .poll(wgpu::PollType::Wait)
        .expect("a poll after the destroy");
}

/// A frame drawn after the device is lost reports `native_terminal_gpu` before any GPU
/// work. Its 200 glyphs overflow glyphon's first 4096-byte vertex buffer, so without the
/// guard glyphon maps a new buffer on the lost device, which wgpu 25 panics on
/// (V243-NATIVE-PRESENT-FATAL).
#[cfg(target_os = "macos")]
#[test]
fn a_frame_on_a_lost_device_reports_the_gpu_code_before_glyphon_maps_a_buffer() {
    let mut shared = shared_at(2.0);
    let layer = metal_layer();
    let mut surface = unsafe { shared.surface_on(on_layer(&layer), 4096, 128) }.expect("a surface");
    shared
        .draw(&mut surface, &frame_of("before"))
        .expect("drawn before the loss");
    lose(&shared);
    let wide = "x".repeat(200);
    let grown = Frame {
        rows: vec![vec![span(&wide, 0, false)]],
        keys: vec![2],
        fills: Vec::new(),
        cursor: None,
        preedit: None,
    };
    assert_eq!(shared.draw(&mut surface, &grown), Err(GPU_FAULT.to_owned()));
    drop(surface);
    drop(layer);
}

/// A frame acquired before the device is lost is never presented, since present on a
/// lost device panics in wgpu 25: the call reports `native_terminal_gpu`. Synthetic: no
/// Metal call loses a device between acquire and present.
#[cfg(target_os = "macos")]
#[test]
fn a_frame_acquired_before_a_loss_is_not_presented() {
    let mut shared = shared_at(2.0);
    let layer = metal_layer();
    let surface = unsafe { shared.surface_on(on_layer(&layer), 64, 64) }.expect("a surface");
    let output = surface.surface.get_current_texture().expect("a frame");
    lose(&shared);
    let set = shared.device.as_ref().expect("a device");
    assert_eq!(hand_back(set, output, Ok(())), Err(GPU_FAULT.to_owned()));
    drop(surface);
    drop(layer);
}

/// A frame whose encode failed after the device was lost is never dropped as is, since
/// the drop discards it and that panics on a lost device in wgpu 25: the call reports
/// `native_terminal_gpu`. Synthetic, as above.
#[cfg(target_os = "macos")]
#[test]
fn a_frame_whose_encode_failed_on_a_lost_device_is_not_discarded() {
    let mut shared = shared_at(2.0);
    let layer = metal_layer();
    let surface = unsafe { shared.surface_on(on_layer(&layer), 64, 64) }.expect("a surface");
    let output = surface.surface.get_current_texture().expect("a frame");
    lose(&shared);
    let set = shared.device.as_ref().expect("a device");
    let refused = Err("encode refused".to_owned());
    assert_eq!(hand_back(set, output, refused), Err(GPU_FAULT.to_owned()));
    drop(surface);
    drop(layer);
}

/// A frame the caller acquired before the device was lost (the Windows render thread
/// waits for the swap chain first) is never dropped when the paint stops at the loss,
/// since the drop discards it and that panics on a lost device in wgpu 25: the paint
/// reports `native_terminal_gpu`. Synthetic, as above.
#[cfg(target_os = "macos")]
#[test]
fn a_frame_acquired_before_a_loss_and_painted_after_it_is_not_dropped() {
    let mut shared = shared_at(2.0);
    let layer = metal_layer();
    let mut surface = unsafe { shared.surface_on(on_layer(&layer), 64, 64) }.expect("a surface");
    let output = shared.acquire(&mut surface).expect("a frame");
    lose(&shared);
    let painted = shared.paint(&mut surface, &frame_of("after"), Some(output));
    assert_eq!(painted, Err(GPU_FAULT.to_owned()));
    drop(surface);
    drop(layer);
}

/// A view made after the device is lost draws on a fresh device, so Open again brings
/// the terminal back (V243-NATIVE-DEVICE-REPLACE). With `--nocapture` it prints how
/// long making that view took, the fresh device included.
#[cfg(target_os = "macos")]
#[test]
fn a_view_made_after_a_loss_draws_on_a_fresh_device() {
    let mut shared = shared_at(2.0);
    let layer = metal_layer();
    let mut old = unsafe { shared.surface_on(on_layer(&layer), 64, 64) }.expect("a surface");
    lose(&shared);
    assert_eq!(
        shared.draw(&mut old, &frame_of("a")),
        Err(GPU_FAULT.to_owned())
    );
    let fresh = metal_layer();
    let started = std::time::Instant::now();
    let made = unsafe { shared.surface_on(on_layer(&fresh), 64, 64) };
    eprintln!("a view after a loss made in {:?}", started.elapsed());
    let mut new = made.expect("a surface on a fresh device");
    assert_eq!(shared.draw(&mut new, &frame_of("b")), Ok(()));
    assert_eq!(shared.draw(&mut new, &frame_of("c")), Ok(()));
    drop((old, new));
    drop((layer, fresh));
}

/// A view from the lost device never moves onto the fresh one: its resize and draw
/// report `native_terminal_gpu` with no GPU work, and dropping it leaves the fresh view
/// drawing. Without its own lost flag, the resize configures it onto the fresh device
/// and its draw raises device-mismatch errors there (probe 5aedcf53d).
#[cfg(target_os = "macos")]
#[test]
fn a_view_from_the_lost_device_never_moves_to_the_new_one() {
    let mut shared = shared_at(2.0);
    let layer = metal_layer();
    let mut old = unsafe { shared.surface_on(on_layer(&layer), 64, 64) }.expect("a surface");
    lose(&shared);
    let fresh = metal_layer();
    let mut new = unsafe { shared.surface_on(on_layer(&fresh), 64, 64) }
        .expect("a surface on a fresh device");
    assert_eq!(shared.resize(&mut old, 128, 96), Err(GPU_FAULT));
    assert_eq!((old.config.width, old.config.height), (64, 64));
    assert_eq!(
        shared.draw(&mut old, &frame_of("a")),
        Err(GPU_FAULT.to_owned())
    );
    drop(old);
    assert_eq!(shared.draw(&mut new, &frame_of("b")), Ok(()));
    drop(new);
    drop((layer, fresh));
}

/// Device attempts after a loss stop at the budget: each loss within it gets a fresh
/// device, and the next one ends the new view with `native_terminal_gpu` without
/// asking wgpu for another device.
#[cfg(target_os = "macos")]
#[test]
fn device_attempts_after_a_loss_stop_at_the_budget() {
    let mut shared = shared_at(2.0);
    let rounds = DEVICE_REPLACE_MAX as usize;
    let layers: Vec<_> = (0..rounds + 2).map(|_| metal_layer()).collect();
    let first = unsafe { shared.surface_on(on_layer(&layers[0]), 64, 64) };
    let mut views = vec![first.expect("a surface")];
    for layer in &layers[1..=rounds] {
        lose(&shared);
        let made = unsafe { shared.surface_on(on_layer(layer), 64, 64) };
        views.push(made.expect("a fresh device within the budget"));
    }
    lose(&shared);
    let refused = unsafe { shared.surface_on(on_layer(&layers[rounds + 1]), 64, 64) };
    assert_eq!(refused.err(), Some(GPU_FAULT.to_owned()));
    assert!(shared.device.is_none());
    drop(views);
    drop(layers);
}

/// A surface drawn by a renderer other than the one whose device made it is a
/// programmer error: the host draws a view with the renderer of the scale it was made
/// at (host:1148-1163, :1216), so the draw asserts it instead of mixing two devices.
#[cfg(target_os = "macos")]
#[test]
#[should_panic(expected = "a surface drawn by the renderer that made it")]
fn a_surface_drawn_by_another_renderer_trips_the_device_assert() {
    let mut maker = shared_at(2.0);
    let mut other = shared_at(2.0);
    let (layer, second) = (metal_layer(), metal_layer());
    let mut surface = unsafe { maker.surface_on(on_layer(&layer), 64, 64) }.expect("a surface");
    let _held = unsafe { other.surface_on(on_layer(&second), 64, 64) }
        .expect("a device for the other renderer");
    let _ = other.draw(&mut surface, &frame_of("a"));
}

/// A view as wide as the adapter's own largest texture side, past wgpu's default 8192
/// (a window across two 6K displays is 12032 px; Apple GPUs allow 16384), configures
/// at that full width and draws: the device asks the adapter's limits (#569). A GPU
/// whose side is not past 8192 prints why the row skipped and passes.
#[cfg(target_os = "macos")]
#[test]
fn a_surface_past_the_default_texture_limit_configures_at_full_width_and_draws() {
    let mut shared = shared_at(2.0);
    let layer = metal_layer();
    let mut surface = unsafe { shared.surface_on(on_layer(&layer), 64, 64) }.expect("a surface");
    let default_max = wgpu::Limits::default().max_texture_dimension_2d;
    assert_eq!(default_max, 8192, "wgpu's default largest texture side");
    let set = shared.device.as_ref().expect("a device");
    let adapter_max = set.adapter.limits().max_texture_dimension_2d;
    if adapter_max <= default_max {
        eprintln!("skipped: this GPU's largest texture side {adapter_max} is not past 8192");
        return;
    }
    shared
        .resize(&mut surface, adapter_max, 64)
        .expect("a resize");
    assert_eq!(
        (surface.config.width, surface.config.height),
        (adapter_max, 64)
    );
    shared
        .draw(&mut surface, &frame_of("wide"))
        .expect("drawn at its full width");
    drop(surface);
    drop(layer);
}

/// A view past the device's own largest texture side is held to it, made and resized
/// alike, and still draws with no GPU fault, never handed to wgpu to panic on (#569).
#[cfg(target_os = "macos")]
#[test]
fn a_surface_past_the_device_texture_limit_is_held_to_it_and_still_draws() {
    let mut shared = shared_at(2.0);
    let layer = metal_layer();
    let mut surface = unsafe { shared.surface_on(on_layer(&layer), 64, 64) }.expect("a surface");
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
    let made = unsafe { shared.surface_on(on_layer(&tall), 64, side_max * 2) }
        .expect("a surface made past the limit");
    assert_eq!((made.config.width, made.config.height), (64, side_max));
    drop((surface, made));
    drop((layer, tall));
}

/// A GPU validation error never panics: the device's handler records it, and the next
/// render call for a view reports `native_terminal_gpu` so the host ends that view; the
/// call after it draws again (#569).
#[cfg(target_os = "macos")]
#[test]
fn a_gpu_validation_error_is_reported_as_the_gpu_code_not_a_panic() {
    let mut shared = shared_at(2.0);
    let layer = metal_layer();
    let mut surface = unsafe { shared.surface_on(on_layer(&layer), 64, 64) }.expect("a surface");
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

#[cfg(target_os = "macos")]
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
#[cfg(not(windows))]
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
fn each_platform_draws_with_its_own_backends() {
    #[cfg(target_os = "macos")]
    assert_eq!(BACKENDS, wgpu::Backends::METAL);
    #[cfg(target_os = "linux")]
    assert_eq!(BACKENDS, wgpu::Backends::VULKAN | wgpu::Backends::GL);
    #[cfg(windows)]
    assert_eq!(BACKENDS, wgpu::Backends::DX12);
}

/// With no system font at all, an upright, bold, italic and bold italic span each find a
/// face of their own style and weight; with none, cosmic-text panics "no default font
/// found" while shaping, as an italic span did on DejaVu Sans Mono with no emoji font.
#[test]
fn every_style_finds_its_own_face_with_no_system_font() {
    let mut fonts = load_fallbacks(&[]);
    let styles = [FontStyle::Normal, FontStyle::Italic];
    let weights = [Weight::NORMAL, Weight::BOLD];
    let attrs = |style, weight| {
        Attrs::new()
            .family(Family::Name("DejaVu Sans Mono"))
            .style(style)
            .weight(weight)
    };
    for (style, weight) in styles.into_iter().flat_map(|s| weights.map(|w| (s, w))) {
        assert!(
            !fonts.get_font_matches(&attrs(style, weight)).is_empty(),
            "a {style:?} {weight:?} span has a face"
        );
    }
    let mut buffer = Buffer::new(&mut fonts, Metrics::new(13.0, 17.0));
    for (style, weight) in styles.into_iter().flat_map(|s| weights.map(|w| (s, w))) {
        buffer.set_text(&mut fonts, "ab", &attrs(style, weight), Shaping::Advanced);
        buffer.shape_until_scroll(&mut fonts, false);
        let ids: Vec<fontdb::ID> = buffer
            .layout_runs()
            .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.font_id))
            .collect();
        assert_eq!(ids.len(), 2, "{style:?} {weight:?}");
        for id in ids {
            let face = fonts.db().face(id).expect("a loaded face");
            assert_eq!((face.style, face.weight), (style, weight));
        }
    }
}

#[test]
fn every_present_fallback_file_loads_its_faces() {
    let fonts = load_fallbacks(&FALLBACKS);
    let present: Vec<std::path::PathBuf> = FALLBACKS
        .iter()
        .filter_map(FontSource::file)
        .filter(|path| path.is_file())
        .collect();
    for path in &present {
        let faces = fonts
            .db()
            .faces()
            .filter(|face| match &face.source {
                fontdb::Source::File(file) | fontdb::Source::SharedFile(file, _) => file == path,
                fontdb::Source::Binary(_) => false,
            })
            .count();
        assert!(faces > 0, "{} loaded no face", path.display());
    }
    assert!(present.len() <= FALLBACKS.len());
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
