//! `--ui-shot` — render one of the shell's screens to a PNG without a window.
//!
//! The application's interface is built on a machine with no display, so the
//! only way to *judge* it is to render it offscreen and look at the file. This
//! module does exactly that, and it does it through the very same drawing code
//! the windowed application runs: `ui::home::show` and `ui::settings::show` are
//! called here with the same models `video::App::redraw` builds, so a capture
//! cannot show a layout the application does not have.
//!
//! Two halves, split so the first one is testable with no GPU:
//! * `Fixture` — a realistic fake library (a dozen games, long and short
//!   titles, missing thumbnails, favourites, the four coprocessors, save slots)
//!   and the `build` pass that lays a screen out on a plain `egui::Context`.
//!   Pure CPU: `egui::Context` needs no window.
//! * `paint` — `egui-wgpu` drawing the tessellated output into an offscreen
//!   `wgpu` texture, which is then copied back to memory and encoded as a PNG.
//!   No surface and no window is created; on macOS Metal this works with no
//!   display attached.
//!
//! Nothing here reads the player's `prefs.json` or their library: the fixture
//! is built in a scratch directory of its own and deleted afterwards, so two
//! runs (and two tests) produce the same picture and never touch user data.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use egui_wgpu::wgpu;

use crate::library::{GameEntry, StateFile};
use crate::prefs::{GameStats, Prefs};

use super::game_sheet::SheetData;
use super::home::HomeModel;
use super::library_view::{LibraryModel, LibraryUi};
use super::settings::{Section, SettingsModel, SettingsUi};
use super::tabs::Tab;
use super::textures::TextureStore;
use super::theme;

/// Which screen a capture shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// Home screen with the library grid.
    Library,
    /// The same grid on the `Favoris` tab, which is also how the tab bar's
    /// other states are looked at.
    Favorites,
    /// Home screen with a game's sheet open.
    GameSheet,
    /// The settings view, which owns the whole window as the library does,
    /// opened on one section. One view per section: it shows a single section
    /// at a time, so a capture of only the first one leaves the five others —
    /// and the controller drawing of `Entrées` — impossible to look at.
    Settings(Section),
    /// The empty screen: a library folder with no game in it, and the call to
    /// action that goes with it.
    Empty,
    /// The library with the pointer resting on one tile: the hover state
    /// (elevation, brightened picture, `Jouer` and the favourite star), which
    /// is otherwise impossible to look at on a machine with no pointer.
    Hover,
}

/// Left/right margin of the home screen's central panel (`ui::home`), and the
/// spacing/geometry the grid is laid out with. Used only to aim the pointer of
/// `View::Hover` at a card.
const CONTENT_MARGIN: f32 = 24.0;
const GRID_SPACING: f32 = 10.0;
const SCROLL_BAR_W: f32 = 16.0;
/// Vertical offset of the first grid row at the default capture size, measured
/// on a capture; the pointer only has to land somewhere on the first row.
const GRID_TOP: f32 = 200.0;
/// Height of a card's text block at the shell's type scale.
const CARD_TEXT_H: f32 = 60.0;

impl View {
    /// Names accepted on the command line, in the order `--help` lists them.
    /// One name per screen the shell can show, the settings panel counting one
    /// per section since that is what it draws at a time.
    pub const ALL: [(&'static str, View); 11] = [
        ("library", View::Library),
        ("favorites", View::Favorites),
        ("game-sheet", View::GameSheet),
        ("settings-display", View::Settings(Section::Display)),
        ("settings-audio", View::Settings(Section::Audio)),
        ("settings-emulation", View::Settings(Section::Emulation)),
        ("settings-inputs", View::Settings(Section::Inputs)),
        ("settings-folders", View::Settings(Section::Folders)),
        ("settings-about", View::Settings(Section::About)),
        ("empty", View::Empty),
        ("library-hover", View::Hover),
    ];

    /// Names that are not a view of their own but another spelling of one.
    /// `settings` predates the per-section names and is the panel's own default
    /// section, so a command line written before them keeps working.
    pub const ALIASES: [(&'static str, View); 1] =
        [("settings", View::Settings(Section::Display))];

    pub fn parse(name: &str) -> Result<View, String> {
        Self::ALL
            .iter()
            .chain(Self::ALIASES.iter())
            .find(|(n, _)| *n == name)
            .map(|(_, v)| *v)
            .ok_or_else(|| {
                let names: Vec<&str> = Self::ALL.iter().map(|(n, _)| *n).collect();
                format!("unknown --ui-shot view: {name} ({})", names.join(", "))
            })
    }

    /// Where the mouse cursor is during a capture of this view, in points.
    /// Only `library-hover` has one: it exists to show the state of a tile
    /// under the pointer, which no other capture can reach.
    pub fn pointer(self, size: (u32, u32)) -> Option<egui::Pos2> {
        // Middle of the second card of the first row, computed the way the
        // grid lays it out rather than read off a picture, so the pointer
        // keeps landing on a card when the geometry is tuned.
        (self == View::Hover).then(|| {
            let metrics = super::library_view::grid_metrics(
                size.0 as f32 - 2.0 * CONTENT_MARGIN - SCROLL_BAR_W,
                GRID_SPACING,
                0.0,
                CARD_TEXT_H,
            );
            let column = 1.min(metrics.columns.saturating_sub(1)) as f32;
            egui::pos2(
                CONTENT_MARGIN + column * (metrics.outer_w + GRID_SPACING) + metrics.outer_w / 2.0,
                GRID_TOP + metrics.row_h * 0.35,
            )
        })
    }

    pub fn name(self) -> &'static str {
        Self::ALL.iter().find(|(_, v)| *v == self).map(|(n, _)| *n).unwrap_or("library")
    }
}

/// Default capture size in points, a common laptop window size.
pub const DEFAULT_SIZE: (u32, u32) = (1280, 800);
/// Bounds of `--ui-shot-size`: below the first the shell has nothing to lay
/// out, above the second a single PNG would run into hundreds of megabytes.
const MIN_SIDE: u32 = 320;
const MAX_SIDE: u32 = 4096;

/// Parse `WIDTHxHEIGHT` (`1280x800`).
pub fn parse_size(text: &str) -> Result<(u32, u32), String> {
    let (w, h) = text
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("expected WIDTHxHEIGHT, got {text}"))?;
    let parse = |s: &str| -> Result<u32, String> {
        s.parse::<u32>().map_err(|_| format!("invalid size: {text}"))
    };
    let (w, h) = (parse(w)?, parse(h)?);
    for side in [w, h] {
        if !(MIN_SIDE..=MAX_SIDE).contains(&side) {
            return Err(format!("size {text} is outside {MIN_SIDE}..={MAX_SIDE}"));
        }
    }
    Ok((w, h))
}

/// How many UI frames are built before the one that is painted. egui is
/// immediate mode and several things it lays out are known only from the
/// previous pass: a `Modal`'s auto-size, a `ScrollArea`'s content extent, the
/// pictures a `TextureStore` uploaded while drawing. Painting the first pass
/// would capture a half-measured screen.
const WARMUP_PASSES: u32 = 3;

/// Time each pass claims to happen at. `egui::Area` fades in over
/// `style.animation_time` seconds, so passes at the same instant would paint a
/// half-transparent settings panel; a step far longer than any egui animation
/// settles every transition and captures the resting state of the screen.
const PASS_SECONDS: f64 = 0.5;

/// `RawInput` for pass `pass` of a capture at `size` points. `pointer` places
/// the mouse cursor, which is the only way a capture can show a hover state:
/// there is no pointer on a machine with no display, so the view that shows
/// one hands it a position (`View::pointer`).
fn pass_input(size: (u32, u32), pass: u32, pointer: Option<egui::Pos2>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(size.0 as f32, size.1 as f32),
        )),
        time: Some(pass as f64 * PASS_SECONDS),
        events: pointer.map(egui::Event::PointerMoved).into_iter().collect(),
        ..Default::default()
    }
}

/// Render `view` at `size` points and write it to `out` as an RGBA PNG.
pub fn capture(view: View, size: (u32, u32), out: &Path) -> Result<(), String> {
    let mut fixture = Fixture::new(view)?;
    let ctx = egui::Context::default();
    theme::apply(&ctx);

    // Texture uploads are reported in the pass that created them, so the
    // deltas of every pass are accumulated: dropping the warm-up ones would
    // leave the painted frame referring to textures the renderer never got.
    let mut deltas = egui::TexturesDelta::default();
    let mut jobs = Vec::new();
    let mut pixels_per_point = 1.0;
    for pass in 0..=WARMUP_PASSES {
        let output =
            ctx.run(pass_input(size, pass, view.pointer(size)), |ctx| fixture.build(ctx));
        deltas.append(output.textures_delta);
        pixels_per_point = output.pixels_per_point;
        jobs = ctx.tessellate(output.shapes, pixels_per_point);
    }

    let rgba = paint(&jobs, &deltas, size, pixels_per_point)?;
    let png = crate::encode_rgba_png(&rgba, size.0, size.1)?;
    crate::write_new_file(out, &png)
}

/// Paint tessellated egui output into an offscreen texture and read it back as
/// RGBA8. The target format is sRGB, like the window surface, so the bytes come
/// back already encoded and go straight into a PNG.
fn paint(
    jobs: &[egui::ClippedPrimitive],
    deltas: &egui::TexturesDelta,
    size: (u32, u32),
    pixels_per_point: f32,
) -> Result<Vec<u8>, String> {
    let (width, height) = size;
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::default(),
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .map_err(|e| format!("no GPU adapter for the offscreen renderer: {e}"))?;
    let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("prisme-ui-shot"),
        ..Default::default()
    }))
    .map_err(|e| format!("no GPU device for the offscreen renderer: {e}"))?;

    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("prisme-ui-shot-target"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target = texture.create_view(&wgpu::TextureViewDescriptor::default());

    let mut renderer = egui_wgpu::Renderer::new(
        &device,
        format,
        egui_wgpu::RendererOptions {
            msaa_samples: 1,
            depth_stencil_format: None,
            dithering: false,
            ..Default::default()
        },
    );
    for (id, delta) in &deltas.set {
        renderer.update_texture(&device, &queue, *id, delta);
    }
    let screen = egui_wgpu::ScreenDescriptor {
        size_in_pixels: [width, height],
        pixels_per_point,
    };
    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("prisme-ui-shot") });
    let uploads = renderer.update_buffers(&device, &queue, &mut encoder, jobs, &screen);
    if !uploads.is_empty() {
        queue.submit(uploads);
    }
    {
        let [r, g, b, a] = theme::clear_color();
        let mut pass = encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("prisme-ui-shot"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r, g, b, a }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            })
            .forget_lifetime();
        renderer.render(&mut pass, jobs, &screen);
    }

    // `copy_texture_to_buffer` requires the row pitch to be a multiple of 256
    // bytes, so the read-back buffer is padded and the padding dropped below.
    let unpadded = width as usize * 4;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
    let padded = unpadded.div_ceil(align) * align;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("prisme-ui-shot-readback"),
        size: (padded * height as usize) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded as u32),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
    );
    queue.submit(Some(encoder.finish()));

    let slice = readback.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device
        .poll(wgpu::PollType::Wait { submission_index: None, timeout: None })
        .map_err(|e| format!("wait for the offscreen read-back: {e}"))?;
    rx.recv()
        .map_err(|e| format!("offscreen read-back was never answered: {e}"))?
        .map_err(|e| format!("map the offscreen read-back: {e}"))?;

    let mapped = slice.get_mapped_range();
    let mut rgba = Vec::with_capacity(unpadded * height as usize);
    for row in 0..height as usize {
        rgba.extend_from_slice(&mapped[row * padded..row * padded + unpadded]);
    }
    drop(mapped);
    readback.unmap();
    Ok(rgba)
}

/// Drive a future to completion on this thread. wgpu's `request_adapter` /
/// `request_device` are async on every backend; the native ones resolve
/// without ever yielding, and parking covers the case where they do.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    struct ThreadWaker(std::thread::Thread);
    impl std::task::Wake for ThreadWaker {
        fn wake(self: std::sync::Arc<Self>) {
            self.0.unpark();
        }
        fn wake_by_ref(self: &std::sync::Arc<Self>) {
            self.0.unpark();
        }
    }
    let mut future = std::pin::pin!(future);
    let waker =
        std::task::Waker::from(std::sync::Arc::new(ThreadWaker(std::thread::current())));
    let mut cx = std::task::Context::from_waker(&waker);
    loop {
        match future.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(value) => return value,
            std::task::Poll::Pending => std::thread::park(),
        }
    }
}

// --- fake library ---------------------------------------------------------

/// Serial number of the scratch directory, so two captures (or two tests) in
/// one process never share a path.
static SCRATCH_SERIAL: AtomicU64 = AtomicU64::new(0);

/// One fake game of the fixture.
struct FakeGame {
    /// Header title; empty means the card falls back to the file name, which
    /// is how a long title reaches the grid in practice (a header title is
    /// capped at 21 characters, a file name is not).
    title: &'static str,
    file: &'static str,
    mapping: &'static str,
    region: &'static str,
    rom_bytes: u64,
    sram_bytes: u64,
    coprocessor: Option<&'static str>,
    fastrom: bool,
    checksum_valid: bool,
    favorite: bool,
    play_seconds: u64,
    /// Days since the fixture's reference instant, `None` = never launched.
    played_days_ago: Option<i64>,
    /// A generated thumbnail exists for this game.
    thumbnail: bool,
    /// …or is still being generated (the grid shows a different placeholder).
    pending: bool,
}

/// The library a capture shows: a dozen games covering what actually breaks a
/// layout — a 40-character name next to a 6-character one, the four
/// coprocessors, favourites, a cart with no battery, an invalid checksum, two
/// games with no picture at all and one whose picture is still being made.
const FAKE_GAMES: &[FakeGame] = &[
    FakeGame {
        title: "SUPER MARIOWORLD",
        file: "Super Mario All-Stars + Super Mario World (E) [!].zip",
        mapping: "LoROM",
        region: "PAL",
        rom_bytes: 2_621_440,
        sram_bytes: 8192,
        coprocessor: None,
        fastrom: true,
        checksum_valid: true,
        favorite: true,
        play_seconds: 12 * 3600 + 25 * 60,
        played_days_ago: Some(1),
        thumbnail: true,
        pending: false,
    },
    FakeGame {
        title: "SECRET OF MANA",
        file: "Secret of Mana (F).zip",
        mapping: "HiROM",
        region: "PAL",
        rom_bytes: 2_097_152,
        sram_bytes: 8192,
        coprocessor: None,
        fastrom: true,
        checksum_valid: true,
        favorite: true,
        play_seconds: 41 * 3600,
        played_days_ago: Some(3),
        thumbnail: true,
        pending: false,
    },
    FakeGame {
        title: "STARWING",
        file: "Starwing (E).sfc",
        mapping: "LoROM",
        region: "PAL",
        rom_bytes: 1_048_576,
        sram_bytes: 0,
        coprocessor: Some("SuperFX"),
        fastrom: false,
        checksum_valid: true,
        favorite: false,
        play_seconds: 2 * 3600 + 10 * 60,
        played_days_ago: Some(9),
        thumbnail: true,
        pending: false,
    },
    FakeGame {
        title: "SUPER MARIO KART",
        file: "Super Mario Kart (E).sfc",
        mapping: "LoROM",
        region: "PAL",
        rom_bytes: 524_288,
        sram_bytes: 8192,
        coprocessor: Some("DSP-1"),
        fastrom: false,
        checksum_valid: true,
        favorite: false,
        play_seconds: 7 * 3600 + 45 * 60,
        played_days_ago: Some(2),
        thumbnail: true,
        pending: false,
    },
    FakeGame {
        title: "SUPER MARIO RPG",
        file: "Super Mario RPG - Legend of the Seven Stars (U).sfc",
        mapping: "LoROM",
        region: "NTSC",
        rom_bytes: 4_194_304,
        sram_bytes: 32768,
        coprocessor: Some("SA-1"),
        fastrom: true,
        checksum_valid: true,
        favorite: false,
        play_seconds: 0,
        played_days_ago: None,
        thumbnail: true,
        pending: false,
    },
    FakeGame {
        title: "MEGAMAN X3",
        file: "Mega Man X3 (E).sfc",
        mapping: "LoROM",
        region: "PAL",
        rom_bytes: 2_097_152,
        sram_bytes: 8192,
        coprocessor: Some("CX4"),
        fastrom: true,
        checksum_valid: true,
        favorite: false,
        play_seconds: 55 * 60,
        played_days_ago: Some(14),
        thumbnail: true,
        pending: false,
    },
    FakeGame {
        // No header title: the card shows this (long) file name instead.
        title: "",
        file: "The Legend of Zelda - A Link to the Past (Europe) (Rev 1) [!].sfc",
        mapping: "LoROM",
        region: "PAL",
        rom_bytes: 1_048_576,
        sram_bytes: 8192,
        coprocessor: None,
        fastrom: false,
        checksum_valid: true,
        favorite: true,
        play_seconds: 3 * 3600 + 5 * 60,
        played_days_ago: Some(5),
        thumbnail: true,
        pending: false,
    },
    FakeGame {
        title: "F-ZERO",
        file: "F-Zero (E).sfc",
        mapping: "LoROM",
        region: "PAL",
        rom_bytes: 524_288,
        sram_bytes: 0,
        coprocessor: None,
        fastrom: false,
        checksum_valid: true,
        favorite: false,
        play_seconds: 20 * 60,
        played_days_ago: Some(30),
        thumbnail: true,
        pending: false,
    },
    FakeGame {
        title: "SUPER METROID",
        file: "Super Metroid (E) [!].zip",
        mapping: "LoROM",
        region: "PAL",
        rom_bytes: 3_145_728,
        sram_bytes: 8192,
        coprocessor: None,
        fastrom: true,
        checksum_valid: true,
        favorite: false,
        play_seconds: 9 * 3600 + 50 * 60,
        played_days_ago: Some(7),
        thumbnail: true,
        pending: false,
    },
    FakeGame {
        title: "DONKEY KONG COUNTRY 2",
        file: "Donkey Kong Country 2 - Diddy's Kong Quest (E).sfc",
        mapping: "LoROM",
        region: "PAL",
        rom_bytes: 4_194_304,
        sram_bytes: 8192,
        coprocessor: None,
        fastrom: true,
        checksum_valid: true,
        favorite: false,
        play_seconds: 0,
        played_days_ago: None,
        thumbnail: true,
        pending: false,
    },
    FakeGame {
        // Freshly dropped in the folder: its picture is being emulated now.
        title: "TERRANIGMA",
        file: "Terranigma (E).sfc",
        mapping: "HiROM",
        region: "PAL",
        rom_bytes: 4_194_304,
        sram_bytes: 8192,
        coprocessor: None,
        fastrom: true,
        checksum_valid: true,
        favorite: false,
        play_seconds: 0,
        played_days_ago: None,
        thumbnail: false,
        pending: true,
    },
    FakeGame {
        // No picture and none coming: the permanent placeholder case.
        title: "ACTRAISER",
        file: "ActRaiser (E).sfc",
        mapping: "LoROM",
        region: "PAL",
        rom_bytes: 1_048_576,
        sram_bytes: 8192,
        coprocessor: None,
        fastrom: false,
        checksum_valid: false,
        favorite: false,
        play_seconds: 0,
        played_days_ago: None,
        thumbnail: false,
        pending: false,
    },
    FakeGame {
        title: "PILOTWINGS",
        file: "Pilotwings (E).sfc",
        mapping: "LoROM",
        region: "PAL",
        rom_bytes: 524_288,
        sram_bytes: 0,
        coprocessor: Some("DSP-1"),
        fastrom: false,
        checksum_valid: true,
        favorite: false,
        play_seconds: 35 * 60,
        played_days_ago: Some(21),
        thumbnail: true,
        pending: false,
    },
    FakeGame {
        title: "YOSHI'S ISLAND",
        file: "Super Mario World 2 - Yoshi's Island (E) [!].zip",
        mapping: "LoROM",
        region: "PAL",
        rom_bytes: 2_097_152,
        sram_bytes: 8192,
        coprocessor: Some("SuperFX"),
        fastrom: true,
        checksum_valid: true,
        favorite: false,
        play_seconds: 6 * 3600,
        played_days_ago: Some(4),
        thumbnail: true,
        pending: false,
    },
];

/// Reference instant of the fixture (2026-01-15 12:00:00 UTC), so the dates a
/// capture shows are stable from one run to the next.
const FIXTURE_NOW: i64 = 1_768_478_400;
/// Game whose sheet the `game-sheet` view opens: the one with no header title,
/// i.e. the longest name the grid can produce.
const SHEET_GAME: usize = 6;

/// The fake state a capture is drawn from, plus the scratch directory holding
/// the PNGs it points at (deleted on drop).
pub struct Fixture {
    view: View,
    dir: PathBuf,
    entries: Vec<GameEntry>,
    games: BTreeMap<String, GameStats>,
    thumbs: HashMap<String, PathBuf>,
    pending: HashSet<String>,
    /// Empty set the `empty` view borrows: with no game in the library there
    /// is no thumbnail in flight either.
    no_pending: HashSet<String>,
    sheet: SheetData,
    library_ui: LibraryUi,
    settings_ui: SettingsUi,
    textures: TextureStore,
    prefs: Prefs,
    rom_dir: PathBuf,
    config_dir: PathBuf,
}

impl Fixture {
    pub fn new(view: View) -> Result<Self, String> {
        let serial = SCRATCH_SERIAL.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir()
            .join(format!("prisme-ui-shot-{}-{serial}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;

        let rom_dir = PathBuf::from("/Users/vous/Jeux/Super Nintendo");
        let mut entries = Vec::new();
        let mut games = BTreeMap::new();
        let mut thumbs = HashMap::new();
        let mut pending = HashSet::new();
        for (i, game) in FAKE_GAMES.iter().enumerate() {
            let id = format!("{}-{:04X}", crate::sanitize_file_stem(game.file), 0x1000 + i * 7);
            entries.push(GameEntry {
                id: id.clone(),
                path: rom_dir.join(game.file),
                file_size: game.rom_bytes,
                modified: FIXTURE_NOW - (i as i64 + 1) * 86_400,
                title: game.title.to_string(),
                mapping: game.mapping.to_string(),
                region: game.region.to_string(),
                rom_bytes: game.rom_bytes,
                sram_bytes: game.sram_bytes,
                coprocessor: game.coprocessor.map(str::to_string),
                fastrom: game.fastrom,
                checksum: 0x1234u16.wrapping_add((i as u16) << 8),
                checksum_valid: game.checksum_valid,
            });
            games.insert(
                id.clone(),
                GameStats {
                    favorite: game.favorite,
                    play_seconds: game.play_seconds,
                    last_played: game.played_days_ago.map(|d| FIXTURE_NOW - d * 86_400),
                    thumbnail: None,
                },
            );
            if game.thumbnail {
                let path = dir.join(format!("thumb-{i:02}.png"));
                write_fake_picture(&path, i)?;
                thumbs.insert(id.clone(), path);
            }
            if game.pending {
                pending.insert(id);
            }
        }

        // Save slots and screenshots of the game the sheet opens on. Three of
        // the four states carry the picture written beside them when they were
        // saved; the fourth has none, which is the case a state written by an
        // older build produces.
        let sheet_id = entries[SHEET_GAME].id.clone();
        let mut states = Vec::new();
        let slot_picture = |name: &str, seed: usize| -> Result<Option<PathBuf>, String> {
            let path = dir.join(format!("{name}.png"));
            write_fake_picture(&path, seed)?;
            Ok(Some(path))
        };
        states.push(StateFile {
            slot: None,
            path: rom_dir.join("zelda.resume"),
            size: 541_312,
            modified: FIXTURE_NOW - 3_600,
            preview: slot_picture("resume", 2)?,
        });
        for (i, (n, hours)) in [(1u8, 2i64), (3, 26), (7, 240)].into_iter().enumerate() {
            states.push(StateFile {
                slot: Some(n),
                path: rom_dir.join(format!("zelda.state{n}")),
                size: 541_312,
                modified: FIXTURE_NOW - hours * 3_600,
                preview: if n == 7 { None } else { slot_picture(&format!("state{n}"), i + 5)? },
            });
        }
        let mut screenshots = Vec::new();
        for i in 0..5 {
            let path = dir.join(format!("Zelda_2026011{i}-2035{i}0.png"));
            write_fake_picture(&path, FAKE_GAMES.len() + i)?;
            screenshots.push(path);
        }

        Ok(Self {
            view,
            dir,
            entries,
            games,
            thumbs,
            pending,
            no_pending: HashSet::new(),
            sheet: SheetData { id: sheet_id.clone(), states, screenshots },
            library_ui: LibraryUi {
                selected: (view == View::GameSheet).then_some(sheet_id),
                tab: if view == View::Favorites { Tab::Favorites } else { Tab::Library },
                ..Default::default()
            },
            settings_ui: SettingsUi {
                open: matches!(view, View::Settings(_)),
                section: match view {
                    View::Settings(section) => section,
                    _ => Section::default(),
                },
                ..Default::default()
            },
            textures: TextureStore::new(),
            prefs: Prefs::default(),
            rom_dir,
            config_dir: PathBuf::from("/Users/vous/Library/Application Support/Prisme"),
        })
    }

    /// One UI pass, through the application's own screens. `video::App::redraw`
    /// composes exactly this way: one screen owns the window, and the settings
    /// view is one of them (a full-width view, not a panel over the library).
    pub fn build(&mut self, ctx: &egui::Context) {
        if matches!(self.view, View::Settings(_)) {
            super::settings::show(
                ctx,
                &mut SettingsModel {
                    app_name: crate::APP_NAME,
                    version: crate::VERSION,
                    prefs: &self.prefs,
                    fullscreen: false,
                    // What a fresh install resolves to with no monitor to
                    // measure, so the capture shows a real selected step.
                    zoom: crate::render::FALLBACK_ZOOM,
                    library_dir: &self.rom_dir,
                    config_dir: Some(&self.config_dir),
                    state: &mut self.settings_ui,
                },
            );
            return;
        }
        // The empty view is the same screen with nothing to show: an empty
        // library, not a different code path.
        let empty = self.view == View::Empty;
        let entries: &[GameEntry] = if empty { &[] } else { self.entries.as_slice() };
        let pending: &HashSet<String> = if empty { &self.no_pending } else { &self.pending };
        super::home::show(
            ctx,
            &mut HomeModel {
                app_name: crate::APP_NAME,
                version: crate::VERSION,
                game_title: Some("SUPER MARIOWORLD"),
                rom_path: Some(&self.entries[0].path),
                library: LibraryModel {
                    entries,
                    games: &self.games,
                    dir: &self.rom_dir,
                    thumbs: &self.thumbs,
                    pending,
                    state: &mut self.library_ui,
                    textures: &mut self.textures,
                },
                sheet: &self.sheet,
            },
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Write a 256x224 stand-in for an emulated screenshot: a two-tone gradient
/// with a horizon band, distinct per `seed`. Not art — its only job is to look
/// like a game picture so the layout is judged with the pictures in place, and
/// to differ from its neighbours so a mis-sized card is visible.
fn write_fake_picture(path: &Path, seed: usize) -> Result<(), String> {
    const W: u32 = 256;
    const H: u32 = 224;
    // The four prism accents, rotated per game, as the two tones of the plate.
    let top = theme::accent(seed);
    let bottom = theme::accent(seed + 1 + seed / 4);
    let mut rgba = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        let horizon = 96 + (seed as u32 * 13) % 64;
        for x in 0..W {
            let (base, shade) = if y < horizon {
                (top, y as f32 / horizon as f32)
            } else {
                (bottom, (H - y) as f32 / (H - horizon) as f32)
            };
            // Checkerboard on the lower half, so the pictures do not read as
            // flat colour and downscaling artifacts are visible.
            let checker = y >= horizon && ((x / 16 + y / 16) % 2 == 0);
            let k = 0.25 + 0.75 * shade * if checker { 0.7 } else { 1.0 };
            rgba.extend_from_slice(&[
                (base.r() as f32 * k) as u8,
                (base.g() as f32 * k) as u8,
                (base.b() as f32 * k) as u8,
                255,
            ]);
        }
    }
    let png = crate::encode_rgba_png(&rgba, W, H)?;
    crate::write_new_file(path, &png)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_view_is_named_on_the_command_line() {
        assert_eq!(View::parse("library"), Ok(View::Library));
        assert_eq!(View::parse("favorites"), Ok(View::Favorites));
        assert_eq!(View::parse("game-sheet"), Ok(View::GameSheet));
        assert_eq!(View::parse("empty"), Ok(View::Empty));
        assert_eq!(View::parse("library-hover"), Ok(View::Hover));
        // Only that view carries a pointer, and it lands inside the window.
        for (_, view) in View::ALL {
            match view.pointer(DEFAULT_SIZE) {
                Some(p) => {
                    assert_eq!(view, View::Hover);
                    assert!(p.x > 0.0 && p.x < DEFAULT_SIZE.0 as f32, "{p:?}");
                    assert!(p.y > 0.0 && p.y < DEFAULT_SIZE.1 as f32, "{p:?}");
                }
                None => assert_ne!(view, View::Hover),
            }
        }
        assert!(View::parse("Library").is_err(), "the names are case-sensitive");
        assert!(View::parse("grid").is_err());
        for (name, view) in View::ALL {
            assert_eq!(view.name(), name);
        }
    }

    /// Every section of the settings panel has a capture of its own: the panel
    /// draws one section at a time, so a single `settings` view could only ever
    /// show `Affichage` and left the five others unjudged.
    #[test]
    fn each_settings_section_has_its_own_view() {
        for section in Section::ALL {
            let view = View::Settings(section);
            let name = view.name();
            assert!(name.starts_with("settings-"), "{name}");
            assert_eq!(View::parse(name), Ok(view));
        }
        assert_eq!(
            View::ALL.iter().filter(|(_, v)| matches!(v, View::Settings(_))).count(),
            Section::ALL.len()
        );
        // The name used before the sections were split still resolves, to the
        // section the panel opens on.
        assert_eq!(View::parse("settings"), Ok(View::Settings(Section::default())));
        assert_eq!(View::Settings(Section::Display).name(), "settings-display");
    }

    #[test]
    fn the_capture_size_is_parsed_and_bounded() {
        assert_eq!(parse_size("1280x800"), Ok((1280, 800)));
        assert_eq!(parse_size("1024X768"), Ok((1024, 768)));
        assert_eq!(parse_size(&format!("{MIN_SIDE}x{MIN_SIDE}")), Ok((MIN_SIDE, MIN_SIDE)));
        for bad in ["1280", "1280x", "x800", "abcxdef", "0x0", "1280x-4", "8000x600", "300x300"] {
            assert!(parse_size(bad).is_err(), "{bad} must be rejected");
        }
        assert_eq!(DEFAULT_SIZE, (1280, 800));
    }

    /// The fixture exists to break layouts, not to look tidy: the defects the
    /// brief lists (uneven cards, missing pictures, long titles) can only show
    /// up on a library that contains those cases.
    #[test]
    fn the_fixture_covers_what_actually_breaks_a_layout() {
        assert!(FAKE_GAMES.len() >= 12, "{} games is too few", FAKE_GAMES.len());
        let chips: std::collections::BTreeSet<_> =
            FAKE_GAMES.iter().filter_map(|g| g.coprocessor).collect();
        for chip in ["SuperFX", "SA-1", "DSP-1", "CX4"] {
            assert!(chips.contains(chip), "no {chip} game in the fixture");
        }
        assert!(FAKE_GAMES.iter().any(|g| g.favorite));
        assert!(FAKE_GAMES.iter().any(|g| !g.thumbnail && !g.pending), "no missing thumbnail");
        assert!(FAKE_GAMES.iter().any(|g| g.pending), "no thumbnail in flight");
        assert!(FAKE_GAMES.iter().any(|g| !g.checksum_valid));
        assert!(FAKE_GAMES.iter().any(|g| g.sram_bytes == 0));
        // A very long name and a very short one, side by side in the grid.
        let longest = FAKE_GAMES.iter().map(|g| g.file.chars().count()).max().unwrap();
        assert!(longest >= 40, "longest name is only {longest} characters");
        assert!(FAKE_GAMES.iter().any(|g| g.title.chars().count() <= 6));
        assert!(FAKE_GAMES[SHEET_GAME].title.is_empty(), "the sheet game exercises the fallback");
    }

    /// Every view must lay out with no window and no GPU — this is the half of
    /// `capture` that can be tested anywhere, and it is also the half that
    /// would panic on a bad model.
    #[test]
    fn every_view_lays_out_headless() {
        for (name, view) in View::ALL {
            let mut fixture = Fixture::new(view).expect("fixture");
            let ctx = egui::Context::default();
            theme::apply(&ctx);
            let mut shapes = 0;
            let mut textures = 0;
            for pass in 0..=WARMUP_PASSES {
                let output = ctx.run(pass_input(DEFAULT_SIZE, pass, view.pointer(DEFAULT_SIZE)), |ctx| {
                    fixture.build(ctx)
                });
                textures += output.textures_delta.set.len();
                shapes = output.shapes.len();
            }
            assert!(shapes > 0, "{name} drew nothing");
            // The fixture's pictures are real PNGs on disk: they must decode
            // and reach the texture store, or the capture would show nothing
            // but placeholders. The empty view has no game and therefore no
            // picture — that is the point of it — and the settings view shows
            // no library at all.
            if !matches!(view, View::Empty | View::Settings(_)) {
                assert!(textures > 1, "{name} uploaded no picture ({textures} textures)");
            }
        }
    }

    /// The scratch directory is per fixture and goes away with it: a capture
    /// must leave nothing behind, and two of them must never share a path.
    #[test]
    fn each_fixture_owns_its_scratch_directory_and_removes_it() {
        let (a, b) = (Fixture::new(View::Library).unwrap(), Fixture::new(View::Library).unwrap());
        assert_ne!(a.dir, b.dir);
        assert!(a.dir.is_dir() && b.dir.is_dir());
        let path = a.dir.clone();
        assert!(a.thumbs.values().all(|p| p.exists()));
        drop(a);
        assert!(!path.exists(), "{} was left behind", path.display());
    }
}
