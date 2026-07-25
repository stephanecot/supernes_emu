//! Windowed video output: winit window + pixels framebuffer, 256x224
//! BGR555 -> RGBA upload, 50.007/60.0988 fps pacing at absolute deadlines.
//!
//! Frame cadence is paced by wall-clock deadlines rather than vsync (which is
//! disabled): each `about_to_wait` computes the next presentation deadline,
//! sleeps for the bulk of the remaining time, then spin-waits the last
//! `SPIN_SLACK` for sub-millisecond accuracy (OS `sleep()` granularity is
//! coarse — a few ms on some hosts — so a plain sleep-to-deadline would
//! frequently overshoot).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use pixels::{Pixels, SurfaceTexture};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use snes_core::{Cartridge, JoypadState, Snes, SCREEN_HEIGHT, SCREEN_WIDTH};

use crate::audio::{self, AudioOutput};
use crate::dialog;
use crate::input;
use crate::library::{self, GameEntry, PlayClock, SortMode};
#[cfg(target_os = "macos")]
use crate::menu::{self, AppMenu};
use crate::pad;
use crate::prefs::Prefs;
use crate::render::{self, Aspect, Filter};
use crate::save;
use crate::thumbs;
use crate::ui::game_sheet::SheetData;
use crate::ui::home::HomeModel;
use crate::ui::library_view::LibraryModel;
use crate::ui::settings::SettingsModel;
use crate::ui::{
    self, Action, AppState, EguiLayer, EscapeAction, LibraryUi, Screen, SettingsUi, Setting,
    TextureStore,
};
use crate::{APP_NAME, VERSION};

/// Integer upscale factor for the 256x224 native framebuffer, and the
/// `zoom` preference's default value (see `prefs::Prefs::default`). The
/// window's *actual* size at any given moment is computed from
/// `prefs.zoom`/`prefs.aspect` (see `render::zoomed_dims`), clamped to fit
/// the screen (`render::clamp_to_available`) — this constant only fixes what
/// a fresh preferences file starts at.
pub const WINDOW_SCALE: u32 = 3;

/// Shrinks the target window size requested from the primary/current monitor
/// so it never asks for the literal full screen — leaves headroom for the
/// menu bar, Dock and window chrome the OS reserves around it.
const MONITOR_FIT_MARGIN: f64 = 0.92;

/// Wall-clock slack reserved for the spin-wait tail of each frame's pacing
/// deadline (see module docs).
const SPIN_SLACK: Duration = Duration::from_micros(1200);

/// How long a status message (screenshot taken, slot saved…) stays on screen.
const STATUS_DURATION: Duration = Duration::from_millis(1800);
/// Status messages are drawn white, unlike the FPS readout whose color encodes
/// whether the emulator is keeping up.
const STATUS_COLOR: [u8; 4] = [255, 255, 255, 255];

/// `<dir of rom>/<name>`, or `<name>` in the working directory when the ROM
/// path has no directory component. Used for the default screenshot and SPC
/// export folders.
fn sibling_dir(rom_path: &Path, name: &str) -> PathBuf {
    match rom_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => PathBuf::from(name),
    }
}

/// Last-modified time of `path`, or `None` if it doesn't exist or the
/// platform/filesystem can't report one. Used by `try_resume` to compare a
/// `.srm` sidecar against a `.resume` snapshot (review point A).
fn mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

/// A cartridge to start the window on, as prepared by `main::run`.
/// `paths` resolves this session's sidecar files (already honoring `--save`
/// and `prefs.save_dir`) and `sram_baseline` comes from `save::load_sram`,
/// which the caller already applied to `cart.sram`.
pub struct Launch {
    pub rom_path: PathBuf,
    pub cart: Cartridge,
    pub paths: crate::paths::GamePaths,
    pub sram_baseline: Vec<u8>,
}

/// Frame pacing used while the home screen owns the window: no cartridge means
/// no field rate to follow, so the UI is refreshed at a plain 60 Hz.
const HOME_FRAME_DURATION: Duration = Duration::from_nanos(16_666_667);

/// How much play time may accumulate before `prefs.json` is rewritten. The
/// counter itself is updated every second in memory; writing the file that
/// often would be pointless I/O, and every exit path flushes it anyway
/// (`persist_all`), so at most this many seconds of a session can be lost to a
/// hard crash.
const PLAY_TIME_FLUSH_SECS: u64 = 60;

/// Runtime state of the game library shown on the home screen. The heavy work
/// (folder scan, header parsing, thumbnail generation) happens on
/// `library::Worker`'s own thread; this is only what the UI reads plus the
/// bookkeeping needed to feed that thread.
struct LibraryState {
    /// Folder currently scanned (`library::library_dir`).
    dir: PathBuf,
    entries: Vec<GameEntry>,
    /// `None` until the home screen is shown for the first time: launching
    /// straight into a game must not pay for a scan nobody asked for.
    worker: Option<library::Worker>,
    /// Search text, sort order, open sheet (see `ui::library_view`).
    ui: LibraryUi,
    textures: TextureStore,
    /// Picture to show per game id: the promoted screenshot when the player
    /// chose one, else the generated thumbnail. Missing = placeholder.
    thumbs: HashMap<String, PathBuf>,
    /// Games whose thumbnail is queued on the worker.
    pending: HashSet<String>,
    /// Files listed by the open sheet, refreshed when the selection changes.
    sheet: SheetData,
}

impl LibraryState {
    fn new() -> Self {
        Self {
            dir: PathBuf::new(),
            entries: Vec::new(),
            worker: None,
            ui: LibraryUi::default(),
            textures: TextureStore::new(),
            thumbs: HashMap::new(),
            pending: HashSet::new(),
            sheet: SheetData::default(),
        }
    }
}

/// Run the windowed frontend: winit event loop + pixels present + egui shell,
/// paced to the cartridge region's native field rate (PAL 50.007 Hz / NTSC
/// 60.0988 Hz, from `Region::frames_per_second`) via an absolute deadline.
///
/// `launch` is `None` when the process was started with no ROM argument: the
/// window then opens on the home screen (`ui::Screen::Home`) instead of the
/// native file dialog it used to show, and a cartridge is loaded later through
/// `Action::PickRom`/`switch_rom`.
///
/// Battery SRAM is written back to the current session's `.srm` once the event
/// loop exits, however it exits (window close, quit, or a fatal window/surface
/// creation error), since `app` is still owned here after `run_app` returns.
/// The `O` hotkey and the home screen can swap in a different ROM mid-session
/// (see `App::open_rom_dialog`); `App` owns its own current
/// `paths`/`sram_baseline` so that exit-time save always targets whichever
/// game is loaded when the window closes.
///
/// `prefs` carries the persisted user options (loaded by `main`); it is stored
/// on `App`, written back after every option change and once more on exit.
pub fn run(launch: Option<Launch>, prefs: Prefs) -> Result<(), String> {
    let (title, snes, current_rom_path, game_paths, sram_baseline, frame_duration, game_id) =
        match launch {
            Some(l) => {
                let region = l.cart.region;
                let game_id =
                    library::game_id(l.cart.title.trim(), l.cart.header_checksum, &l.cart.rom);
                (
                    window_title(&l.cart.title),
                    Some(Snes::new(l.cart)),
                    l.rom_path,
                    l.paths,
                    l.sram_baseline,
                    Duration::from_secs_f64(1.0 / region.frames_per_second()),
                    Some(game_id),
                )
            }
            None => (
                home_window_title(),
                None,
                PathBuf::new(),
                crate::paths::GamePaths::default(),
                Vec::new(),
                HOME_FRAME_DURATION,
                None,
            ),
        };
    let app_state = AppState::new(snes.is_some());

    let mut event_loop_builder = EventLoop::builder();
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::EventLoopBuilderExtMacOS;
        // winit creates its own default NSApp main menu unless told not to;
        // left enabled it would duplicate (and fight over) the muda-built
        // menu bar installed in `App::resumed`.
        event_loop_builder.with_default_menu(false);
    }
    let event_loop = event_loop_builder.build().map_err(|e| format!("create event loop: {e}"))?;
    event_loop.set_control_flow(ControlFlow::Poll);

    // Audio is best-effort: a missing device must never fail the emulator.
    let audio = AudioOutput::new();

    let mut app = App {
        title,
        snes,
        current_rom_path,
        paths: game_paths,
        sram_baseline,
        frame_duration,
        next_deadline: Instant::now() + frame_duration,
        window: None,
        pixels: None,
        out_w: 0,
        out_h: 0,
        native_buf: vec![0u8; SCREEN_WIDTH * SCREEN_HEIGHT * 4],
        pad: JoypadState::default(),
        pads: pad::Pads::new(),
        focused: true,
        paused: false,
        frame_advance: false,
        fast_forward: false,
        audio,
        audio_scratch: Vec::new(),
        fps_counter: FpsCounter::new(),
        status: None,
        prefs,
        state: app_state,
        ui: None,
        settings: SettingsUi::default(),
        library: LibraryState::new(),
        game_id: None,
        play_clock: PlayClock::default(),
        play_tick: Instant::now(),
        play_unsaved: 0,
        dialogs: dialog::Dialogs::new(),
        quit_confirm: false,
        quit_saved_pause: false,
        #[cfg(target_os = "macos")]
        menu: None,
    };
    // Apply the restored mute/volume before the first sample is produced.
    app.apply_audio_gain();
    // Instant resume: pick the session state up before the first frame runs.
    // A no-op when the window opens on the home screen (no cartridge yet).
    app.try_resume();
    match game_id {
        // Launched with a ROM: the play-time counter starts on frame 0.
        Some(id) => app.start_play_session(id),
        // Launched bare: the home screen is up, so the library is scanned
        // right away instead of waiting for the first Escape.
        None => app.ensure_library(),
    }
    let result = event_loop.run_app(&mut app).map_err(|e| format!("event loop: {e}"));
    // `ApplicationHandler::exiting` has normally already flushed everything;
    // `persist_all` is idempotent and this second call covers the paths where
    // `run_app` returns without that event (e.g. a fatal window/surface error).
    app.persist_all();
    result
}

/// Window title: product name, version, then the cartridge's own title.
fn window_title(cart_title: &str) -> String {
    format!("{APP_NAME} {VERSION} - {}", cart_title.trim())
}

/// Window title while the home screen owns the window (no cartridge to name).
fn home_window_title() -> String {
    format!("{APP_NAME} {VERSION}")
}

/// A monitor's usable logical size for `render::clamp_to_available`: its
/// physical size converted through its own scale factor, shrunk by
/// `MONITOR_FIT_MARGIN` so the computed window target never asks for the
/// literal full screen (menu bar/Dock/window chrome need room too).
fn logical_monitor_size(monitor: &winit::monitor::MonitorHandle) -> (u32, u32) {
    let logical: LogicalSize<u32> = monitor.size().to_logical(monitor.scale_factor());
    (
        (logical.width as f64 * MONITOR_FIT_MARGIN) as u32,
        (logical.height as f64 * MONITOR_FIT_MARGIN) as u32,
    )
}

struct App {
    title: String,
    /// The running console, or `None` while the shell has no cartridge loaded
    /// (window opened on the home screen with no ROM argument). Every path
    /// that needs a console guards on it rather than assuming one exists.
    snes: Option<Snes>,
    /// Path of the currently loaded ROM (updated by `switch_rom`); used by
    /// `Emulation > Reset` to reload the same cart. Empty while `snes` is
    /// `None`.
    current_rom_path: PathBuf,
    /// Sidecar file locations (`.srm`, `.state`/`.stateN`, `.resume`) of the
    /// currently loaded cart, resolved once when it was loaded (rebuilt by
    /// `switch_rom`). Snapshotting `prefs.save_dir` there rather than reading
    /// it per write is deliberate: a folder chosen mid-session applies to the
    /// *next* game loaded, so this session can never overwrite a save the new
    /// folder already holds for the same game (see `paths::GamePaths`).
    paths: crate::paths::GamePaths,
    /// Post-load SRAM snapshot for the currently loaded cart; see
    /// `save::load_sram`/`save::save_if_dirty`.
    sram_baseline: Vec<u8>,
    frame_duration: Duration,
    /// Absolute wall-clock time the next emulated frame should be presented at.
    next_deadline: Instant,
    window: Option<Arc<Window>>,
    /// `pixels`' buffer and surface are always the same size — the window's
    /// current physical size — so its own (nearest-neighbor-only) scaling
    /// pass is a 1:1 copy; `render::compose_frame` does all zoom/filter/PAR
    /// work in `about_to_wait` before that copy (see `render` module docs).
    pixels: Option<Pixels<'static>>,
    /// Current `pixels` buffer/surface size in physical pixels, tracked
    /// alongside `pixels` itself since `Pixels` exposes no getter for it;
    /// updated by `apply_resize`.
    out_w: u32,
    out_h: u32,
    /// Reused native `SCREEN_WIDTH`x`SCREEN_HEIGHT` RGBA8 scratch buffer:
    /// `Snes::framebuffer` converted to RGBA, composed into `pixels`' frame
    /// by `render::compose_frame` every presented frame. The FPS/status
    /// overlays are drawn separately, *after* that composition, straight onto
    /// `pixels`' own (already-scaled) frame — see the "FPS overlay" module
    /// comment near `draw_overlay_text` for why. Never touched by the
    /// headless dump paths or the F12 screenshot, which both read
    /// `snes.framebuffer` directly.
    native_buf: Vec<u8>,
    /// Player-1 pad state accumulated from keyboard events. Merged with
    /// player 1's controller (`pads`) button by button — see `pad::merge`: the
    /// keyboard never cancels the controller and vice versa.
    pad: JoypadState,
    /// Controllers (`gilrs`): controller 1 drives player 1 alongside the
    /// keyboard, controller 2 drives player 2 on its own. Polled once per
    /// `about_to_wait`; absent hardware or a failed `gilrs` init simply leaves
    /// both ports at rest.
    pads: pad::Pads,
    /// Window focus, tracked from `WindowEvent::Focused`. A window is focused
    /// when it opens; controllers only drive the console while this holds (the
    /// keyboard cannot reach an unfocused window at all).
    focused: bool,
    paused: bool,
    /// Set by `N` while paused: step exactly one frame, then cleared.
    frame_advance: bool,
    /// True while the turbo key (Tab) is held: `about_to_wait` then runs
    /// `prefs.fast_forward_factor` emulated frames per presented frame and
    /// silences the audio output.
    fast_forward: bool,
    /// cpal output; `None` when no audio device was available.
    audio: Option<AudioOutput>,
    /// Reused per-frame drain buffer to avoid re-allocating each frame.
    audio_scratch: Vec<(i16, i16)>,
    /// Rolling wall-clock rate of frames actually drawn (see `FpsCounter`).
    fps_counter: FpsCounter,
    /// Transient bottom-left status message (`STATUS_DURATION`), shown after a
    /// screenshot, a slot save/load or an SPC export.
    status: Option<(String, Instant)>,
    /// Persisted user options; the single source of truth for anything the
    /// user can toggle (`show_fps`, `mute`, `volume`, `fast_forward_factor`,
    /// `confirm_on_quit`). Every change is written back immediately so a crash
    /// cannot lose it.
    prefs: Prefs,
    /// Which screen owns the window (`Accueil` / `Jeu`) and the pause flag the
    /// game screen must be restored to. See `ui::app_state`.
    state: AppState,
    /// egui shell drawn into `pixels`' own surface; created in `resumed`
    /// alongside `pixels`, since it needs that wgpu device and surface format.
    ui: Option<EguiLayer>,
    /// Settings panel: whether it is up, which section is selected. The values
    /// it shows are read from `prefs` every frame, never mirrored here (see
    /// `ui::settings`).
    settings: SettingsUi,
    /// The game library of the home screen (Phase 8 step 2).
    library: LibraryState,
    /// `library::game_id` of the running game, the key play time and
    /// `last_played` are recorded under. `None` with no cartridge loaded.
    game_id: Option<String>,
    /// Sub-second remainder of the play-time accounting.
    play_clock: PlayClock,
    /// Wall clock of the last `about_to_wait`, whatever the screen; the
    /// difference is credited to the game only while it is actually running.
    play_tick: Instant,
    /// Seconds credited but not yet written to `prefs.json` (see
    /// `PLAY_TIME_FLUSH_SECS`).
    play_unsaved: u64,
    /// Native file/folder dialogs, opened off the winit callback stack (see
    /// `crate::dialog`: a native modal opened from a callback re-enters winit's
    /// event handler, which panics on purpose).
    dialogs: dialog::Dialogs,
    /// The quit confirmation modal (`ui::confirm`) is up. It replaces the
    /// native alert this used to show, for the same reentrancy reason.
    quit_confirm: bool,
    /// Pause flag to restore when the quit confirmation is dismissed.
    quit_saved_pause: bool,
    /// Menu bar handles, installed once in `resumed` (needs `NSApp` to
    /// exist first); `None` until then.
    #[cfg(target_os = "macos")]
    menu: Option<AppMenu>,
}

/// Wall-clock window over which `FpsCounter` averages; short enough to react
/// to a slowdown within about half a second, long enough that the on-screen
/// digits don't flicker frame to frame.
const FPS_WINDOW: Duration = Duration::from_millis(500);

/// Rolling display-FPS counter: records the `Instant` of each `about_to_wait`
/// pass that actually emulated a frame (a paused pass still re-uploads the
/// framebuffer but does not tick) and reports the average rate over the
/// trailing `FPS_WINDOW`. This measures real presented frames per wall-second, not
/// the emulator's internal frame count, so a stall (GC pause, slow host,
/// window occlusion) is visible even though `Snes::run_frame` always
/// advances exactly one emulated frame per call.
struct FpsCounter {
    samples: std::collections::VecDeque<Instant>,
}

impl FpsCounter {
    fn new() -> Self {
        Self { samples: std::collections::VecDeque::new() }
    }

    /// Record a frame drawn "now"; drop samples older than `FPS_WINDOW`.
    fn tick(&mut self) {
        let now = Instant::now();
        self.samples.push_back(now);
        while let Some(&front) = self.samples.front() {
            if now.duration_since(front) > FPS_WINDOW {
                self.samples.pop_front();
            } else {
                break;
            }
        }
    }

    /// Average frames/second over the trailing window; `0.0` until at least
    /// two samples have been recorded (first tick after start/resume).
    fn fps(&self) -> f64 {
        if self.samples.len() < 2 {
            return 0.0;
        }
        let span =
            self.samples.back().unwrap().duration_since(*self.samples.front().unwrap());
        if span.as_secs_f64() <= 0.0 {
            return 0.0;
        }
        (self.samples.len() - 1) as f64 / span.as_secs_f64()
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return; // Already initialized; e.g. a redundant resume on some platforms.
        }
        let aspect = Aspect::from_pref(&self.prefs.aspect);
        let target = render::zoomed_dims(self.prefs.zoom, aspect);
        // Bound the initial window to the primary monitor so a large restored
        // `zoom` (or a screen smaller than the one the preference was saved
        // on) can never request an unusable, off-screen-sized window.
        let max = event_loop.primary_monitor().map(|m| logical_monitor_size(&m));
        let (w, h) = match max {
            Some(max) => render::clamp_to_available(target, max),
            None => target,
        };
        let size = LogicalSize::new(w, h);
        // `with_resizable(true)` is winit's own default; set explicitly since
        // free mouse-drag resizing is a functional requirement here (the
        // window is *not* fixed to the zoom presets — see `render` module
        // docs and `WindowEvent::Resized` -> `apply_resize`).
        let attrs = Window::default_attributes()
            .with_title(self.title.clone())
            .with_inner_size(size)
            .with_resizable(true);
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("error: create window: {e}");
                event_loop.exit();
                return;
            }
        };
        let phys = window.inner_size();
        let surface_texture = SurfaceTexture::new(phys.width, phys.height, Arc::clone(&window));
        // Buffer size == surface size (both the window's physical size): see
        // the `pixels` field doc and `render` module docs for why all
        // zoom/filter/aspect scaling is done by this crate's own CPU code
        // instead of `pixels`' built-in (nearest-neighbor-only) scaler.
        let mut pixels = match Pixels::new(phys.width, phys.height, surface_texture) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("error: create pixels surface: {e}");
                event_loop.exit();
                return;
            }
        };
        // Frame pacing is done manually against a wall-clock deadline; vsync
        // would additionally block on the compositor's own refresh cycle.
        pixels.enable_vsync(false);
        // egui shares `pixels`' device, queue and swap-chain view (see
        // `ui::egui_layer` module docs) — no second wgpu surface exists.
        let egui_layer = EguiLayer::new(
            &window,
            pixels.device(),
            pixels.surface_texture_format(),
            (phys.width, phys.height),
            window.scale_factor() as f32,
        );
        self.out_w = phys.width;
        self.out_h = phys.height;
        self.window = Some(window);
        self.pixels = Some(pixels);
        self.ui = Some(egui_layer);
        self.next_deadline = Instant::now() + self.frame_duration;

        // NSApp only exists once winit has resumed at least once; installing
        // the menu bar any earlier is a silent no-op on macOS (see `menu`
        // module docs). The menu carries actions only — every setting lives in
        // the egui panel — so it needs no restored state.
        #[cfg(target_os = "macos")]
        {
            self.menu = Some(menu::install());
        }
    }

    /// Dispatched on `Event::LoopExiting`, which winit emits both for
    /// `event_loop.exit()` (Esc-confirmed quit, window close, our custom Quit
    /// menu item) and for AppKit's `applicationWillTerminate:` — the path a
    /// Dock/`terminate:` quit takes, where `run_app` never returns normally.
    /// This is therefore the one hook that covers every exit route, so the
    /// battery SRAM flush lives here.
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.persist_all();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _window_id: WindowId, event: WindowEvent) {
        // egui sees every window event so it can track pointer position,
        // modifiers and focus even while the game screen owns the input. Its
        // `consumed` verdict is only honored on the home screen and while the
        // settings panel is up: on the game screen the emulated pad must never
        // lose a key to an overlay.
        // A pending remapping capture is the one exception: the key that ends
        // it must reach `handle_key` even if a focused egui widget claims it
        // (arrows, Tab, Enter and Space are all keys egui consumes and all
        // legitimate pad bindings). This holds for a *controller* capture too,
        // where the only key that counts is Escape — the announced way out —
        // which a focused widget would otherwise swallow; `apply_capture_key`
        // ignores every other key of a controller capture on its own.
        let capturing = self.settings.open && self.settings.capture.is_active();
        if let (Some(window), Some(ui)) = (&self.window, &mut self.ui) {
            let response = ui.on_window_event(window, &event);
            if (self.state.is_home() || self.settings.open || self.quit_confirm)
                && response.consumed
                && !matches!(event, WindowEvent::CloseRequested | WindowEvent::RedrawRequested)
                && !(capturing && matches!(event, WindowEvent::KeyboardInput { .. }))
            {
                return;
            }
        }
        match event {
            // Routed through the same confirmation path as Esc/menu-Quit
            // (rather than a bare `event_loop.exit()`) so clicking the
            // window's close button can't skip `prefs.confirm_on_quit`.
            WindowEvent::CloseRequested => self.request_quit(event_loop),
            WindowEvent::Resized(size) => self.apply_resize(size),
            WindowEvent::KeyboardInput {
                event: KeyEvent { physical_key: PhysicalKey::Code(code), state, repeat, .. },
                ..
            } => self.handle_key(event_loop, code, state, repeat),
            // Key releases are not delivered once the window loses focus, so
            // anything held (pad buttons, the turbo key) would stay stuck.
            // Controllers are a different matter: the OS delivers their events
            // to every process regardless of focus, so `pads` keeps tracking
            // them (nothing gets stuck there) and `current_pads` simply stops
            // feeding them to the console while another application is in
            // front — playing the emulated game from the background would be
            // surprising.
            WindowEvent::Focused(focused) => {
                self.focused = focused;
                if !focused {
                    self.pad = JoypadState::default();
                    self.set_fast_forward(false);
                }
            }
            WindowEvent::RedrawRequested => {
                let action = self.redraw();
                // The UI mutates its own view state in place (search text,
                // sort order, opened sheet); those follow-ups run once the
                // egui borrow is over, like `Action` itself.
                self.sync_library_view();
                self.apply_action(event_loop, action);
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            return; // Not yet resumed on this platform.
        }
        #[cfg(target_os = "macos")]
        self.poll_menu_events(event_loop);
        // A native dialog asked for anywhere in the shell is opened here, and
        // its answer applied here, never from the callback that requested it
        // (see `crate::dialog`).
        self.pump_dialogs();
        // Scan results and finished thumbnails arrive here, one channel drain
        // per frame; the library thread never touches the UI itself.
        self.poll_library();
        // Controllers, same rule: one non-blocking drain per frame, on every
        // screen, so a hot-plug is noticed even while the game is suspended.
        self.poll_pads();
        pace(&mut self.next_deadline, self.frame_duration);
        if self.status.as_ref().is_some_and(|(_, until)| Instant::now() >= *until) {
            self.status = None;
        }

        // The home screen suspends emulation: no frame is run, no audio is
        // produced, and the console keeps every byte of its state until the
        // player comes back (`ui::AppState`). A native dialog on screen does
        // the same: it owns the keyboard and the player is not looking at the
        // game.
        let running = self.state.screen() == Screen::Game
            && self.snes.is_some()
            && !self.dialogs.is_busy();
        // Play time only advances while the console actually runs: the home
        // screen and a pause stop the counter, as does an open native dialog.
        // The settings panel deliberately does not: the game
        // keeps running behind it so a change of filter, ratio, window size or
        // volume is seen and heard immediately. Its keyboard is taken by the
        // panel and the pad was released on opening, so nothing is *played*
        // behind it.
        self.credit_play_time(running && !self.paused);

        if running && (!self.paused || self.frame_advance) {
            // Fast-forward runs `factor` emulated frames per presented frame;
            // only the last one is uploaded, so the extra frames cost no
            // presentation work. `frame_advance` always steps exactly one.
            let factor = if self.fast_forward && !self.paused {
                self.prefs.fast_forward_factor.max(1) as u32
            } else {
                1
            };
            let mut frames_run = 0u32;
            let pads = self.current_pads();
            for i in 0..factor {
                if let Some(snes) = &mut self.snes {
                    snes.run_frame(pads);
                }
                frames_run += 1;
                // Silent degradation: `next_deadline` is already the *next*
                // presentation time (advanced by `pace` above), so passing it
                // means the host cannot sustain the requested factor. Stop
                // here rather than build a backlog and stall the event loop.
                if i + 1 < factor && Instant::now() >= self.next_deadline {
                    break;
                }
            }
            self.frame_advance = false;
            self.fps_counter.tick();
            // Feed this frame's audio into the ring; the callback's rate control
            // absorbs the emulator/host clock drift. The APU is always drained,
            // including while muted or accelerating, so it never runs against a
            // full internal buffer and unmuting resumes mid-note.
            if let (Some(audio), Some(snes)) = (&mut self.audio, &mut self.snes) {
                self.audio_scratch.clear();
                snes.drain_audio(&mut self.audio_scratch);
                // An accelerated pass produced `frames_run` frames' worth of
                // samples for one frame of wall time; pushing all of them would
                // overrun the ring, so only a real-time-rate slice goes in (at
                // gain 0, see `apply_audio_gain`) to keep the consumer fed with
                // silence instead of holding its last sample.
                let take = self.audio_scratch.len() / frames_run.max(1) as usize;
                audio.push(&self.audio_scratch[..take]);
            }
        }
        // Re-uploaded every iteration, including while paused, so a status
        // message triggered from a pause (save/load state, screenshot) is
        // still shown and then disappears on expiry. Only the FPS counter is
        // gated on an emulated frame, since it measures emulation rate.
        // Native 256x224 conversion happens on `native_buf`, not on `pixels`'
        // own frame buffer, which is sized to the window instead (see the
        // `pixels`/`native_buf` field docs). `render::compose_frame` then
        // does the zoom/filter/PAR scaling into `pixels`' actual frame; the
        // FPS/status overlays are drawn *after* that, straight onto the
        // scaled output, so their on-screen size stays constant regardless of
        // zoom/window size instead of growing with it (see the "FPS overlay"
        // module comment near `draw_overlay_text`). None of this touches
        // `snes.framebuffer` itself, so headless `--dump-frame` output and
        // the F12 screenshot (which both read the core) are unaffected —
        // this whole block only ever runs on the windowed present path.
        // Skipped entirely on the home screen, where egui owns the surface and
        // clears it itself (`ui::egui_layer::EguiLayer::render`).
        if running {
            if let Some(snes) = &self.snes {
                snes.framebuffer.to_rgba(&mut self.native_buf);
            }
        }
        if let (true, Some(pixels)) = (running, &mut self.pixels) {
            let filter = Filter::from_pref(&self.prefs.filter);
            let aspect = Aspect::from_pref(&self.prefs.aspect);
            render::compose_frame(
                &self.native_buf,
                pixels.frame_mut(),
                self.out_w,
                self.out_h,
                filter,
                aspect,
            );
            let (buf_w, buf_h) = (self.out_w as usize, self.out_h as usize);
            let frame = pixels.frame_mut();
            if self.prefs.show_fps {
                let measured = self.fps_counter.fps();
                let target = 1.0 / self.frame_duration.as_secs_f64();
                // Green once the measured rate is within 5% of the cartridge
                // region's native field rate (50/60 Hz); red if the emulator
                // is falling behind it.
                let color = if measured <= 0.0 || measured >= target * 0.95 {
                    [80, 255, 80, 255]
                } else {
                    [255, 70, 70, 255]
                };
                // Space between the "FPS" label and the numbers so the
                // readout doesn't read as one run-together token.
                let text = format!("FPS {:.0}/{:.0}", measured, target);
                draw_overlay_text(frame, buf_w, buf_h, &text, color);
            }
            if let Some((text, _)) = &self.status {
                draw_status_text(frame, buf_w, buf_h, text, STATUS_COLOR);
            }
        }
        // Always request a redraw, even while paused, so the compositor keeps
        // presenting the last frame (e.g. after an expose/resize).
        if let Some(window) = &self.window {
            window.request_redraw();
        }
        event_loop.set_control_flow(ControlFlow::Poll);
    }
}

impl App {
    fn handle_key(&mut self, event_loop: &ActiveEventLoop, code: KeyCode, state: ElementState, repeat: bool) {
        let pressed = state == ElementState::Pressed;
        // The quit confirmation is the outermost modal: nothing else reacts
        // until it is answered.
        if self.quit_confirm {
            if pressed && !repeat {
                match code {
                    KeyCode::Escape => self.cancel_quit(),
                    KeyCode::Enter | KeyCode::NumpadEnter => {
                        self.quit_confirm = false;
                        event_loop.exit();
                    }
                    _ => {}
                }
            }
            return;
        }
        // The settings panel owns the keyboard while it is up, on either
        // screen: its widgets take the keys (egui already saw the event) and
        // the emulated pad must not move behind a modal. Escape is the way out.
        //
        // The window keys are the exception: at ×1/×2 the panel is wider than
        // the window and its own size buttons can be off screen, so F1-F4 and
        // F11 must stay live or the panel would have no way out but Escape.
        if self.settings.open {
            // A pending capture takes the keyboard before *everything* else,
            // application shortcuts included: F11 pressed here must be
            // assigned to the SNES button being remapped, not toggle
            // fullscreen. Releases are swallowed too, so the key that was just
            // assigned cannot also act on its way up.
            if self.settings.capture.is_active() {
                if pressed && !repeat {
                    self.apply_capture_key(code);
                }
                return;
            }
            if pressed && !repeat {
                match code {
                    KeyCode::Escape => self.handle_escape(event_loop),
                    KeyCode::F1 => self.set_zoom(1),
                    KeyCode::F2 => self.set_zoom(2),
                    KeyCode::F3 => self.set_zoom(3),
                    KeyCode::F4 => self.set_zoom(4),
                    KeyCode::F11 => self.set_fullscreen(!self.is_fullscreen()),
                    _ => {}
                }
            }
            return;
        }
        // The home screen owns the keyboard: no emulated pad, no in-game
        // hotkey (they all act on a console that is deliberately suspended).
        // Only the keys that are about the *window* or the shell stay live.
        if self.state.is_home() {
            if pressed && !repeat {
                match code {
                    KeyCode::Escape => self.handle_escape(event_loop),
                    KeyCode::KeyO => self.open_rom_dialog(),
                    KeyCode::Comma => self.open_settings(),
                    KeyCode::F11 => self.set_fullscreen(!self.is_fullscreen()),
                    _ => {}
                }
            }
            return;
        }
        // The turbo key is held, so it reacts to press *and* release; a
        // key-repeat press just re-asserts the state it is already in.
        if code == KeyCode::Tab {
            self.set_fast_forward(pressed);
            return;
        }
        // Hotkeys act on the initial press only (ignore key-repeat).
        if pressed && !repeat {
            match code {
                KeyCode::Escape => {
                    self.handle_escape(event_loop);
                    return;
                }
                KeyCode::KeyM => {
                    self.set_mute(!self.prefs.mute);
                    return;
                }
                KeyCode::Equal | KeyCode::NumpadAdd => {
                    self.adjust_volume(true);
                    return;
                }
                KeyCode::Minus | KeyCode::NumpadSubtract => {
                    self.adjust_volume(false);
                    return;
                }
                KeyCode::KeyP => {
                    self.paused = !self.paused;
                    return;
                }
                KeyCode::KeyN => {
                    if self.paused {
                        self.frame_advance = true;
                    }
                    return;
                }
                KeyCode::KeyO => {
                    self.open_rom_dialog();
                    return;
                }
                // App menu `Réglages…` (Cmd+, on macOS): the settings panel,
                // which is where every option now lives.
                KeyCode::Comma => {
                    self.open_settings();
                    return;
                }
                // `Réglages > Affichage > Taille de la fenêtre`: set the
                // window zoom directly, F1-F4 for x1-x4 — chosen over Ctrl+/-
                // so it doesn't collide with the existing volume hotkeys.
                KeyCode::F1 => {
                    self.set_zoom(1);
                    return;
                }
                KeyCode::F2 => {
                    self.set_zoom(2);
                    return;
                }
                KeyCode::F3 => {
                    self.set_zoom(3);
                    return;
                }
                KeyCode::F4 => {
                    self.set_zoom(4);
                    return;
                }
                KeyCode::F5 => {
                    self.save_state();
                    return;
                }
                KeyCode::F6 => {
                    self.reset();
                    return;
                }
                KeyCode::F7 => {
                    self.next_slot();
                    return;
                }
                KeyCode::F8 => {
                    self.export_spc();
                    return;
                }
                KeyCode::F9 => {
                    self.load_state();
                    return;
                }
                // `Réglages > Émulation > Reprise instantanée`.
                KeyCode::F10 => {
                    self.set_resume_on_launch(!self.prefs.resume_on_launch);
                    return;
                }
                // `Affichage > Plein écran`: F11 is the conventional
                // Windows/Linux fullscreen-toggle key; macOS additionally gets
                // Ctrl+Cmd+F as a menu accelerator (its own system
                // convention) — see `menu::install`.
                KeyCode::F11 => {
                    self.set_fullscreen(!self.is_fullscreen());
                    return;
                }
                // `Réglages > Émulation > Confirmation` (avant de quitter).
                KeyCode::KeyC => {
                    self.set_confirm_on_quit(!self.prefs.confirm_on_quit);
                    return;
                }
                KeyCode::F12 => {
                    self.take_screenshot();
                    return;
                }
                KeyCode::KeyF => {
                    self.toggle_show_fps();
                    return;
                }
                // `Réglages > Affichage > Filtre`: cycles Aucun -> Lissé ->
                // CRT -> Aucun (`render::Filter::next`).
                KeyCode::KeyV => {
                    self.cycle_filter();
                    return;
                }
                // `Réglages > Affichage > Ratio`: toggles Pixel-parfait <->
                // TV authentique (`render::Aspect::toggled`).
                KeyCode::KeyR => {
                    self.cycle_aspect();
                    return;
                }
                // `Réglages > Émulation > Accéléré`: steps through the
                // offered factors one at a time.
                KeyCode::BracketLeft => {
                    self.adjust_fast_forward_factor(false);
                    return;
                }
                KeyCode::BracketRight => {
                    self.adjust_fast_forward_factor(true);
                    return;
                }
                // `Réglages > Émulation > Slot de sauvegarde`: jump straight
                // to a slot instead of cycling with F7.
                other => {
                    if let Some(slot) = digit_to_slot(other) {
                        self.set_slot(slot);
                        return;
                    }
                }
            }
        }
        // Resolved through the player's own bindings (`prefs.keymap`), with
        // the built-in table as the fallback for every button they left alone
        // — see `input::resolve_key`.
        if let Some(name) = input::resolve_key(&self.prefs.keymap, code) {
            let _ = input::set_button(&mut self.pad, name, pressed);
        }
    }

    /// One key press routed to the settings panel's pending capture: assign
    /// it, refuse it (application shortcut), or cancel on Escape. The binding
    /// is persisted immediately and applies to the very next frame, since
    /// `handle_key` resolves through `prefs.keymap` every time.
    fn apply_capture_key(&mut self, code: KeyCode) {
        match self.settings.capture.on_key(code) {
            input::Captured::Key { button, key } => {
                let result = input::bind_key(&mut self.prefs.keymap, button, key);
                self.prefs.save();
                self.settings.capture.notice = bind_notice(result, &input::key_label(key));
            }
            // The refusal already wrote its own explanation, and the capture
            // stays pending so another key can be tried.
            input::Captured::Reserved(_) | input::Captured::Cancelled | input::Captured::Ignored => {
            }
        }
    }

    /// Same for a controller button: the first button pressed while the
    /// `Entrées` section waits for one is assigned to the SNES button that
    /// asked for it.
    fn apply_capture_pad(&mut self, button: pad::Button) {
        // `Unknown` is `gilrs`' catch-all for a control it could not identify;
        // storing it would bind every unidentified control of that pad at once.
        if button == pad::Button::Unknown {
            self.settings.capture.notice =
                Some("Ce bouton n'est pas reconnu par le système.".to_string());
            return;
        }
        let Some(name) = self.settings.capture.take_gamepad() else { return };
        let result = pad::bind_button(&mut self.prefs.pad_map, name, button);
        self.prefs.save();
        self.settings.capture.notice = bind_notice(result, pad::pad_label(button));
    }

    /// Drop every binding the player made, on both devices.
    fn reset_input_bindings(&mut self) {
        self.settings.capture.cancel();
        self.prefs.keymap = crate::prefs::default_keymap();
        self.prefs.pad_map.clear();
        self.prefs.save();
        self.settings.capture.notice = None;
    }

    /// Drain the controllers' event queue and report the hot-plug changes.
    /// Non-blocking (see `pad::Pads::poll`), so it costs nothing when no
    /// controller is attached; called on every screen so plugging a pad in
    /// from the home screen is noticed too.
    ///
    /// A controller appearing or disappearing is never fatal: the notice is a
    /// stderr line plus the same discreet bottom-left status message a
    /// screenshot or a slot save uses.
    fn poll_pads(&mut self) {
        let polled = self.pads.poll();
        for notice in polled.notices {
            let state = if notice.connected { "connected" } else { "disconnected" };
            eprintln!(
                "pad: player {} {state} ({}); {} controller(s) in use",
                notice.player + 1,
                notice.name,
                self.pads.connected()
            );
            self.set_status(notice.status());
        }
        // A controller button pressed while the `Entrées` section waits for
        // one is a binding, not a game input — and it cannot be one anyway,
        // since both ports are held at rest while the panel is up
        // (`current_pads`).
        if self.settings.open && self.settings.capture.waiting_for(input::Device::Gamepad).is_some()
        {
            if let Some(&button) = polled.pressed.first() {
                self.apply_capture_pad(button);
            }
        }
    }

    /// The two `JoypadState`s to feed the console this frame.
    ///
    /// Player 1 is the keyboard OR'ed with controller 1 (`pad::merge`), so
    /// both can be used at once — a second player on the keyboard is not
    /// possible, by design: the keyboard always stays on player 1. Player 2 is
    /// controller 2 alone.
    ///
    /// While the settings panel or the quit confirmation is up, both ports are
    /// held at rest: those overlays already take the keyboard (`self.pad` is
    /// cleared when they open), and the emulation keeps running behind them,
    /// so a controller left leaning on the desk must not play the game either.
    /// Same when the window is not focused (see `WindowEvent::Focused`).
    fn current_pads(&self) -> [JoypadState; 2] {
        if self.settings.open || self.quit_confirm || !self.focused {
            return [JoypadState::default(); 2];
        }
        [
            pad::merge(self.pad, self.pads.player(0, &self.prefs.pad_map)),
            self.pads.player(1, &self.prefs.pad_map),
        ]
    }

    /// Escape, resolved by `ui::escape_action`:
    ///   * fullscreen (either screen) -> leave fullscreen, nothing else;
    ///   * windowed game -> back to the home screen, emulation suspended;
    ///   * home screen with a suspended session -> back into the game;
    ///   * home screen with nothing loaded -> quit, still through
    ///     `prefs.confirm_on_quit`.
    ///
    /// This is the one behavior change of the shell: Escape used to quit
    /// straight from the game. Quitting from a game is still one action away —
    /// the window close button, `Fichier > Quitter`, Cmd+Q, or a second Escape
    /// once the home screen is up with no session.
    fn handle_escape(&mut self, event_loop: &ActiveEventLoop) {
        // The settings panel is the outermost overlay but the innermost thing
        // Escape backs out of, on either screen — except in fullscreen, which
        // keeps precedence (`ui::settings::escape_closes_settings`).
        if ui::settings::escape_closes_settings(self.settings.open, self.is_fullscreen()) {
            self.close_settings();
            return;
        }
        // On the home screen an open game sheet is the innermost thing Escape
        // backs out of, before the screen itself — the same "one step back per
        // press" rule `ui::escape_action` implements for the screens.
        if self.state.is_home() && !self.is_fullscreen() && self.library.ui.selected.is_some() {
            self.library.ui.selected = None;
            return;
        }
        match ui::escape_action(
            self.state.screen(),
            self.is_fullscreen(),
            self.state.has_session(),
        ) {
            EscapeAction::LeaveFullscreen => self.set_fullscreen(false),
            EscapeAction::GoHome => self.go_home(),
            EscapeAction::ResumeGame => self.resume_game(),
            EscapeAction::Quit => self.request_quit(event_loop),
        }
    }

    /// Suspend the session and show the home screen. The console is left
    /// untouched (no save-state round trip): only emulation stops. Held input
    /// is released so nothing is stuck when the game comes back, and the audio
    /// ring is flushed so the last frame's samples don't loop under the UI.
    fn go_home(&mut self) {
        if !self.state.go_home(self.paused) {
            return;
        }
        self.paused = true;
        self.frame_advance = false;
        self.pad = JoypadState::default();
        self.set_fast_forward(false);
        if let Some(audio) = &self.audio {
            audio.flush();
        }
        if let Some(window) = &self.window {
            window.set_title(&home_window_title());
        }
        // The home screen is where an application is left open for a long time,
        // so everything the session produced goes to disk now rather than
        // waiting for an exit that a crash, a `kill` or a Dock quit may never
        // reach: battery SRAM first, then the resume snapshot (same order as
        // `persist_all` — it keeps `.resume`'s mtime >= `.srm`'s, so
        // `try_resume`'s newer-`.srm` guard doesn't misfire), then the play
        // time with the rest of the preferences.
        if let Some(snes) = &self.snes {
            save::save_if_dirty(&snes.bus.cart, &self.paths.srm_write(), &self.sram_baseline);
            self.write_resume_state();
            self.resync_sram_baseline();
        }
        if self.play_unsaved > 0 {
            self.play_unsaved = 0;
            self.prefs.save();
        }
        // A game session can have added a save state or a screenshot; drop the
        // gathered lists so an open sheet is rebuilt from disk on the way back
        // in (`sync_library_view`).
        self.library.sheet = SheetData::default();
        self.ensure_library();
    }

    /// Return to the suspended session, restoring the pause flag the player
    /// had set. Does nothing when no cartridge is loaded.
    fn resume_game(&mut self) {
        let Some(was_paused) = self.state.resume_game() else { return };
        self.paused = was_paused;
        self.pad = JoypadState::default();
        // The home screen consumed an arbitrary amount of wall time; restart
        // pacing from now instead of catching up frames that never ran.
        self.next_deadline = Instant::now() + self.frame_duration;
        if let Some(window) = &self.window {
            window.set_title(&self.title);
        }
    }

    /// Apply what the UI (or a hotkey) asked for, once the borrow of the egui
    /// layer is over.
    fn apply_action(&mut self, event_loop: &ActiveEventLoop, action: Action) {
        match action {
            Action::None => {}
            Action::ResumeGame => self.resume_game(),
            Action::PickRom => self.open_rom_dialog(),
            Action::Quit => self.request_quit(event_loop),
            Action::ConfirmQuit => {
                self.quit_confirm = false;
                event_loop.exit();
            }
            Action::CancelQuit => self.cancel_quit(),
            Action::Launch(path) => {
                if let Err(e) = self.switch_rom(&path, true) {
                    eprintln!("error: could not load {}: {e}", path.display());
                    self.library.ui.error =
                        Some(format!("Impossible de charger {} : {e}", path.display()));
                }
            }
            Action::ToggleFavorite(id) => {
                let stats = self.prefs.games.entry(id).or_default();
                stats.favorite = !stats.favorite;
                self.prefs.save();
            }
            Action::SetThumbnail { id, source } => {
                self.prefs.games.entry(id).or_default().thumbnail = Some(source.clone());
                self.prefs.save();
                // The promoted file may already be cached under its own path
                // from the gallery; the picture itself has not changed, but
                // dropping it keeps the store from pinning a stale decode if
                // the player overwrites the capture later.
                self.library.textures.forget(&source);
                self.refresh_thumbnails();
            }
            Action::ClearThumbnail(id) => {
                self.prefs.games.entry(id).or_default().thumbnail = None;
                self.prefs.save();
                self.refresh_thumbnails();
            }
            Action::DeleteState(path) => self.delete_state(&path),
            Action::Rescan => self.rescan_library(),
            Action::ChooseLibraryDir => {
                // `library.dir` is still empty when the run went straight into
                // a game: resolve the folder the same way the scan would.
                let current = library::library_dir(&self.prefs);
                self.dialogs.request(dialog::Request::LibraryDir { current });
            }
            Action::ResetLibraryDir => {
                self.prefs.library_dir = None;
                self.prefs.save();
                self.rescan_library();
            }
            Action::OpenSettings => self.open_settings(),
            Action::CloseSettings => self.close_settings(),
            Action::ShowLibrary(tab) => self.show_library_tab(tab),
            Action::Set(setting) => self.apply_setting(setting),
            Action::ChooseScreenshotDir => {
                let current = self
                    .prefs
                    .screenshot_dir
                    .clone()
                    .unwrap_or_else(|| sibling_dir(&self.current_rom_path, "Screenshots"));
                self.dialogs.request(dialog::Request::ScreenshotDir { current });
            }
            Action::ResetScreenshotDir => {
                self.prefs.screenshot_dir = None;
                self.prefs.save();
                self.library.sheet = SheetData::default();
            }
            Action::ChooseSaveDir => {
                // Opens where saves go today: the configured folder, else the
                // ROM's own directory (which is where the sidecars are).
                let current = self.prefs.save_dir.clone().unwrap_or_else(|| {
                    match self.current_rom_path.parent() {
                        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
                        _ => library::library_dir(&self.prefs),
                    }
                });
                self.dialogs.request(dialog::Request::SaveDir { current });
            }
            Action::ResetSaveDir => self.set_save_dir(None),
            Action::OpenGuide => self.open_guide(),
        }
    }

    /// Apply one option of the settings panel. Every arm goes through the same
    /// `set_*` method the corresponding hotkey uses, so the panel can never
    /// write a value a hotkey could not, nor skip a side effect (window resize,
    /// audio gain, persistence).
    fn apply_setting(&mut self, setting: Setting) {
        match setting {
            Setting::Zoom(zoom) => self.set_zoom(zoom),
            Setting::Filter(filter) => self.set_filter(filter),
            Setting::Aspect(aspect) => self.set_aspect(aspect),
            Setting::Fullscreen(on) => self.set_fullscreen(on),
            Setting::ShowFps(on) => self.set_show_fps(on),
            Setting::Mute(on) => self.set_mute(on),
            Setting::Volume(volume) => self.set_volume(volume),
            Setting::FastForward(factor) => self.set_fast_forward_factor(factor),
            Setting::ResumeOnLaunch(on) => self.set_resume_on_launch(on),
            Setting::ConfirmOnQuit(on) => self.set_confirm_on_quit(on),
            Setting::Slot(slot) => self.set_slot(slot),
            Setting::ResetInputs => self.reset_input_bindings(),
        }
    }

    /// `,` hotkey / app menu `Réglages…` (Cmd+,) / the home screen's button.
    /// Held input is released: the panel takes the keyboard, so a key held
    /// when it opened would otherwise stay pressed on the emulated pad.
    fn open_settings(&mut self) {
        if self.settings.open {
            return;
        }
        self.settings.open = true;
        self.settings.notice = None;
        self.settings.folder_notice = None;
        self.settings.capture = input::Capture::default();
        // Resolved once per opening rather than per frame: this touches the
        // file system (`is_file`) and the answer cannot change mid-panel.
        self.settings.guide = crate::guide::find();
        self.pad = JoypadState::default();
        self.set_fast_forward(false);
    }

    /// `Réglages > Dossiers > Dossier des sauvegardes`: where `.srm`, save
    /// states and the session state are written from now on. The folder is
    /// created and probed first (`paths::prepare_dir`) — an unusable one is
    /// reported in the panel and the preference is left untouched, since a
    /// stored folder that cannot be written to would silently cost saves.
    ///
    /// Takes effect at the next ROM load: the running session keeps the files
    /// it read from (see the `paths` field), so this can never overwrite a save
    /// the new folder already holds for the same game.
    ///
    /// The folder being replaced is remembered in `prefs.previous_save_dir`:
    /// the saves it holds keep being *read* (see `paths::read_sidecar`), so
    /// clearing the setting cannot silently hand the player back an older file
    /// left beside the ROM. Nothing is ever written there again.
    fn set_save_dir(&mut self, dir: Option<PathBuf>) {
        if let Some(dir) = &dir {
            if let Err(e) = crate::paths::prepare_dir(dir) {
                eprintln!("save dir: {e}");
                self.settings.folder_notice = Some(ui::settings::FolderNotice::Error(format!(
                    "Dossier inutilisable, réglage inchangé : {e}"
                )));
                return;
            }
        }
        move_save_dir(&mut self.prefs.save_dir, &mut self.prefs.previous_save_dir, dir);
        self.prefs.save();
        self.settings.folder_notice = Some(ui::settings::FolderNotice::Info(
            save_dir_notice(self.snes.is_some(), self.prefs.previous_save_dir.as_deref()),
        ));
        // The sheet lists this game's save states from that folder.
        self.library.sheet = SheetData::default();
    }

    /// A library tab chosen on the settings view's own tab bar: leave the
    /// settings for that view of the library. From the game screen this steps
    /// back to the home screen too, since that is where the library lives; the
    /// session is only suspended, exactly as Escape would.
    fn show_library_tab(&mut self, tab: ui::Tab) {
        self.close_settings();
        if tab.is_view() {
            self.library.ui.tab = tab;
            // The sheet belongs to the game, not to the view (same rule as the
            // home screen's own bar).
            self.library.ui.selected = None;
            self.library.ui.confirm_delete = None;
        }
        if !self.state.is_home() {
            self.go_home();
        } else {
            self.ensure_library();
        }
    }

    fn close_settings(&mut self) {
        self.settings.open = false;
        self.settings.notice = None;
        self.settings.folder_notice = None;
        // Nothing may stay pending behind a closed panel: the next key would
        // otherwise be eaten as a binding.
        self.settings.capture = input::Capture::default();
        self.pad = JoypadState::default();
    }

    /// `Réglages > À propos > Ouvrir le PDF`: hand the pedagogical guide to
    /// the platform's document reader. A failure is reported in the panel
    /// itself rather than only on stderr, which nobody reading the panel sees.
    fn open_guide(&mut self) {
        let Some(path) = self.settings.guide.clone() else {
            self.settings.notice = Some("Le guide n'a pas été trouvé.".to_string());
            return;
        };
        match crate::guide::open(&path) {
            Ok(()) => self.settings.notice = None,
            Err(e) => {
                eprintln!("guide: {e}");
                self.settings.notice = Some(format!("Ouverture impossible : {e}"));
            }
        }
    }

    /// Spawn the library thread and run a first scan, once. Called the first
    /// time the home screen is shown (`go_home`, or startup with no ROM).
    fn ensure_library(&mut self) {
        if self.library.worker.is_some() {
            return;
        }
        self.library.ui.sort = SortMode::from_pref(&self.prefs.library_sort);
        self.library.ui.tab = ui::Tab::from_pref(&self.prefs.library_tab);
        match library::Worker::spawn() {
            Some(worker) => {
                self.library.worker = Some(worker);
                self.rescan_library();
            }
            // The OS refused a thread: the grid says so instead of the shell
            // dying, and the next visit to the home screen tries again.
            None => {
                self.library.ui.scanning = false;
                self.library.ui.error =
                    Some("La bibliothèque n'a pas pu démarrer (thread indisponible).".to_string());
            }
        }
    }

    /// Ask the library thread for a fresh scan of the configured folder. The
    /// thumbnails still queued are dropped by the worker (they belong to the
    /// folder being replaced), so `pending` is cleared here to match: the grid
    /// would otherwise keep counting generations that will never be answered.
    fn rescan_library(&mut self) {
        let dir = library::library_dir(&self.prefs);
        self.library.dir = dir.clone();
        let Some(worker) = &self.library.worker else {
            // Without a worker nothing will ever answer; leaving `scanning` set
            // would pin "Analyse du dossier…" on screen forever.
            self.library.ui.scanning = false;
            return;
        };
        self.library.ui.scanning = true;
        self.library.ui.error = None;
        self.library.pending.clear();
        worker.submit(library::Job::Scan(dir));
    }

    /// Drain the library thread's updates: scan results and finished
    /// thumbnails. Cheap enough to call every frame (a `try_recv` loop on an
    /// empty channel), and never blocks the event loop.
    fn poll_library(&mut self) {
        let Some(worker) = &self.library.worker else { return };
        let updates = worker.poll();
        if updates.is_empty() {
            return;
        }
        let mut rescanned = false;
        for update in updates {
            match update {
                library::Update::Scanned { dir, entries, error } => {
                    self.library.dir = dir;
                    self.library.entries = entries;
                    self.library.ui.scanning = false;
                    self.library.ui.error = error.map(|e| format!("Dossier illisible : {e}"));
                    rescanned = true;
                }
                library::Update::Thumb { id, path } => {
                    self.library.pending.remove(&id);
                    if let Some(path) = &path {
                        // The store may hold a memoized "missing file" for
                        // this path from before the picture existed.
                        self.library.textures.forget(path);
                    }
                }
            }
        }
        if rescanned {
            self.request_thumbnails();
        }
        self.refresh_thumbnails();
    }

    /// Queue a thumbnail for every game that has none yet, in the order the
    /// grid shows them, so the pictures fill in from the top. Games with a
    /// promoted screenshot or an already-generated file are skipped, which
    /// makes this incremental across runs: only genuinely new games cost an
    /// emulation run.
    fn request_thumbnails(&mut self) {
        let Some(worker) = &self.library.worker else { return };
        let order = library::arrange(
            &self.library.entries,
            "",
            self.library.ui.sort,
            &self.prefs.games,
        );
        let mut queued = Vec::new();
        for entry in order {
            if self.library.pending.contains(&entry.id) {
                continue;
            }
            if picture_for(&self.prefs, &entry.id).is_some() {
                continue; // already has a picture, generated or promoted
            }
            queued.push(entry.id.clone());
            worker.submit(library::Job::Thumb { id: entry.id.clone(), rom: entry.path.clone() });
        }
        self.library.pending.extend(queued);
    }

    /// Re-resolve the picture of every game: the promoted screenshot when the
    /// player chose one and it still exists, else the generated thumbnail when
    /// it has been produced.
    fn refresh_thumbnails(&mut self) {
        let mut map = HashMap::with_capacity(self.library.entries.len());
        for entry in &self.library.entries {
            if let Some(picture) = picture_for(&self.prefs, &entry.id) {
                map.insert(entry.id.clone(), picture);
            }
        }
        self.library.thumbs = map;
    }

    /// Follow-ups of the view state the UI mutated in place: persist a changed
    /// sort order and re-gather the sheet's file lists when another game was
    /// selected.
    fn sync_library_view(&mut self) {
        if self.library.worker.is_none() {
            // The library has never been opened, so its view state is still
            // the default one — writing that back would clobber the stored
            // sort order of a player who launched straight into a game.
            return;
        }
        let sort = self.library.ui.sort.as_pref();
        let tab = self.library.ui.tab.as_pref();
        if self.prefs.library_sort != sort || self.prefs.library_tab != tab {
            self.prefs.library_sort = sort.to_string();
            self.prefs.library_tab = tab.to_string();
            self.prefs.save();
        }
        let selected = self.library.ui.selected.clone();
        match selected {
            Some(id) if self.library.sheet.id != id => {
                let entry = self.library.entries.iter().find(|e| e.id == id).cloned();
                self.library.sheet = match entry {
                    Some(entry) => SheetData {
                        states: library::save_states(&self.sheet_paths(&entry)),
                        id,
                        screenshots: library::screenshots(
                            &library::screenshot_dir(&entry.path, &self.prefs),
                            &entry.title,
                        ),
                    },
                    None => SheetData::default(),
                };
            }
            None if !self.library.sheet.id.is_empty() => {
                self.library.sheet = SheetData::default();
            }
            _ => {}
        }
    }

    /// Sidecar layout the game sheet must list the save states of.
    ///
    /// For the game currently loaded this is the session's own frozen
    /// `GamePaths` — that is where F9 reads from, and it can differ from the
    /// current preference (the folder is read at load time and never
    /// retargeted mid-session). Every other entry of the library is resolved
    /// against the preferences as they stand, which is where its next load
    /// would look.
    fn sheet_paths(&self, entry: &GameEntry) -> crate::paths::GamePaths {
        if self.paths.id() == entry.id && self.current_rom_path == entry.path {
            return self.paths.clone();
        }
        crate::paths::GamePaths::new(
            &entry.path,
            &entry.id,
            self.prefs.save_dir.clone(),
            None,
        )
        .with_previous_dir(self.prefs.previous_save_dir.clone())
    }

    /// Start counting play time for `id`, and record the launch instant that
    /// drives the "recently played" sort order.
    fn start_play_session(&mut self, id: String) {
        self.play_clock.reset();
        self.play_tick = Instant::now();
        self.play_unsaved = 0;
        let stats = self.prefs.games.entry(id.clone()).or_default();
        stats.last_played = Some(library::now_unix());
        self.game_id = Some(id);
        self.prefs.save();
    }

    /// Credit the wall time since the previous pass to the running game.
    /// `running` false only advances the reference instant, so suspended time
    /// (home screen, pause, modal dialog) is never counted.
    fn credit_play_time(&mut self, running: bool) {
        let now = Instant::now();
        let dt = now.saturating_duration_since(self.play_tick);
        self.play_tick = now;
        if !running {
            self.play_clock.reset();
            return;
        }
        let seconds = self.play_clock.add(dt);
        if seconds == 0 {
            return;
        }
        let Some(id) = self.game_id.clone() else { return };
        self.prefs.games.entry(id).or_default().play_seconds += seconds;
        self.play_unsaved += seconds;
        if self.play_unsaved >= PLAY_TIME_FLUSH_SECS {
            self.play_unsaved = 0;
            self.prefs.save();
        }
    }

    /// Build one UI frame and present it. Returns what the UI asked for; the
    /// caller applies it once the egui layer is no longer borrowed.
    ///
    /// Presentation order on the game screen: `pixels`' scaling renderer
    /// blits the already-composed frame, then egui's pass draws over it
    /// (`LoadOp::Load`). On the home screen the scaling renderer is skipped
    /// and egui's pass clears the surface itself, so a stale emulated frame
    /// can never show through.
    fn redraw(&mut self) -> Action {
        let Some(window) = self.window.clone() else { return Action::None };
        let home = self.state.is_home();
        let paused = self.paused;
        // Only the home screen names the suspended session; cloning these two
        // on the game screen would allocate on every presented frame for
        // nothing.
        let (game_title, rom_path) = if home {
            (
                self.snes.as_ref().map(|s| s.bus.cart.title.trim().to_string()),
                (!self.current_rom_path.as_os_str().is_empty())
                    .then(|| self.current_rom_path.clone()),
            )
        } else {
            (None, None)
        };
        // Resolved for the settings panel only, and only while it is up: the
        // library's own `dir` is still empty when a run went straight into a
        // game without ever showing the home screen.
        let settings_open = self.settings.open;
        let quit_confirm = self.quit_confirm;
        let fullscreen = self.is_fullscreen();
        let (rom_dir, config_dir) = if settings_open {
            (library::library_dir(&self.prefs), crate::prefs::config_dir())
        } else {
            (PathBuf::new(), None)
        };
        let Self { ui, pixels, library, prefs, settings, .. } = self;
        let (Some(ui), Some(pixels)) = (ui.as_mut(), pixels.as_ref()) else {
            return Action::None;
        };
        let LibraryState { dir, entries, ui: view, textures, thumbs, pending, sheet, .. } = library;
        let action = ui.run(&window, |ctx| {
            // One screen owns the window: the settings are a full-width view
            // now, not a modal over the library or over the game.
            let mut action = if settings_open {
                crate::ui::settings::show(
                    ctx,
                    &mut SettingsModel {
                        app_name: APP_NAME,
                        version: VERSION,
                        prefs,
                        fullscreen,
                        library_dir: &rom_dir,
                        config_dir: config_dir.as_deref(),
                        state: settings,
                    },
                )
            } else if home {
                crate::ui::home::show(
                    ctx,
                    &mut HomeModel {
                        app_name: APP_NAME,
                        version: VERSION,
                        game_title: game_title.as_deref(),
                        rom_path: rom_path.as_deref(),
                        library: LibraryModel {
                            entries,
                            games: &prefs.games,
                            dir,
                            thumbs,
                            pending,
                            state: &mut *view,
                            textures: &mut *textures,
                        },
                        sheet,
                    },
                )
            } else {
                crate::ui::game::overlay(ctx, paused);
                Action::None
            };
            // The quit confirmation sits over everything, including the
            // settings view: nothing else can be acted on until it is
            // answered.
            if quit_confirm {
                let produced = crate::ui::confirm::show(ctx, APP_NAME);
                if produced != Action::None {
                    action = produced;
                }
            }
            action
        });
        window.pre_present_notify();
        // The emulated frame is only blitted when a shell screen is *not*
        // covering the window: the settings view fills it opaquely, so scaling
        // and presenting the last frame under it would be work nobody sees.
        let shell = home || settings_open;
        let clear = shell.then(crate::ui::theme::clear_color);
        if let Err(e) = pixels.render_with(|encoder, target, ctx| {
            if !shell {
                ctx.scaling_renderer.render(encoder, target);
            }
            ui.render(encoder, target, &ctx.device, &ctx.queue, clear);
            Ok(())
        }) {
            eprintln!("error: pixels render: {e}");
        }
        action
    }

    /// `[`/`]` hotkeys: step `prefs.fast_forward_factor` through
    /// `prefs::FAST_FORWARD_FACTORS` (the same list the macOS menu's
    /// `Accéléré` radio group offers), one entry per press.
    fn adjust_fast_forward_factor(&mut self, up: bool) {
        let factors = crate::prefs::FAST_FORWARD_FACTORS;
        let idx = factors.iter().position(|&f| f == self.prefs.fast_forward_factor).unwrap_or(0);
        let next = if up { (idx + 1).min(factors.len() - 1) } else { idx.saturating_sub(1) };
        self.set_fast_forward_factor(factors[next]);
    }

    /// Flush everything that must outlive the process: the battery SRAM of
    /// the currently loaded cart, the automatic session state (instant
    /// resume), then the preferences file. Idempotent — the SRAM baseline is
    /// re-synced after the write, so the second call (exit hook, then `run`)
    /// writes nothing new; rewriting the resume state is harmless since no
    /// emulation happened in between.
    ///
    /// SRAM is written *before* the resume snapshot on purpose (review point
    /// A): both are derived from the exact same in-memory state at this
    /// instant, so their content never disagrees, but `try_resume`'s
    /// newer-`.srm` mtime guard would otherwise misfire on every ordinary
    /// launch — `.srm` written second would always read as "newer" than
    /// `.resume`, even though nothing unusual happened. Writing `.srm` first
    /// means `.resume`'s mtime is always >= `.srm`'s in the normal case, so
    /// that guard only ever trips for the genuine out-of-band edit it exists
    /// to catch.
    ///
    /// With no cartridge loaded (home screen, nothing ever started) there is
    /// no SRAM and no session to snapshot: only the preferences are written.
    fn persist_all(&mut self) {
        if let Some(snes) = &self.snes {
            save::save_if_dirty(&snes.bus.cart, &self.paths.srm_write(), &self.sram_baseline);
            self.write_resume_state();
            self.resync_sram_baseline();
        }
        // Whatever play time was credited since the last flush goes out with
        // the rest of the preferences.
        self.play_unsaved = 0;
        self.prefs.save();
    }

    /// Re-derive `sram_baseline` from the SRAM currently held by `self.snes`.
    /// Must run after every successful `Snes::load_state` (instant resume,
    /// manual slot load, a ROM switch that resumes) as well as after a battery
    /// write, and nowhere else. `load_state` replaces the entire cart —
    /// including its SRAM — with whatever the blob held at the moment it was
    /// written; leaving the pre-load baseline in place would make the next
    /// `save_if_dirty` diff the *post-load* SRAM against *pre-load* bytes,
    /// see them differ (even though nothing has actually changed since the
    /// load), and rewrite `.srm` with the state-blob's SRAM copy — which can
    /// be older than the `.srm` already on disk (see `try_resume`'s
    /// newer-`.srm` guard for the case that matters most).
    fn resync_sram_baseline(&mut self) {
        if let Some(snes) = &self.snes {
            self.sram_baseline = snes.bus.cart.sram.as_bytes().to_vec();
        }
    }

    /// Esc / `Fichier > Quitter` / app-menu Quit (Cmd+Q) / the window's close
    /// button: leave through `event_loop.exit()` so the `exiting` hook's SRAM
    /// flush runs, after the confirmation when `prefs.confirm_on_quit` is set.
    ///
    /// The confirmation is an in-app egui modal (`ui::confirm`), not a native
    /// `NSAlert`: a native modal opened from this callback re-enters winit's
    /// event handler, which panics on purpose (see `crate::dialog` and
    /// `docs/PUNCHLIST.md`). Emulation is paused while it is up, and the
    /// previous pause state restored if the player answers no.
    fn request_quit(&mut self, event_loop: &ActiveEventLoop) {
        if !self.prefs.confirm_on_quit {
            event_loop.exit();
            return;
        }
        if self.quit_confirm {
            return;
        }
        self.quit_confirm = true;
        // The confirmation is the outermost modal and answers to Enter/Escape:
        // a capture left pending under it would never see them.
        self.settings.capture.cancel();
        self.quit_saved_pause = self.paused;
        self.paused = true;
        self.pad = JoypadState::default();
        self.set_fast_forward(false);
    }

    /// The quit confirmation was answered no (or dismissed): back to where the
    /// player was, with the pause flag they had set.
    fn cancel_quit(&mut self) {
        if !self.quit_confirm {
            return;
        }
        self.quit_confirm = false;
        self.paused = self.quit_saved_pause;
        self.pad = JoypadState::default();
        self.next_deadline = Instant::now() + self.frame_duration;
    }

    /// Push the current mute/volume (and the fast-forward silence) to the
    /// output stage. The APU keeps running in every case — only the gain
    /// applied on the way into the ring changes, so audio resumes instantly
    /// and in the middle of the note it was playing.
    fn apply_audio_gain(&mut self) {
        let gain = if self.fast_forward {
            0.0
        } else {
            audio::gain_for(self.prefs.mute, self.prefs.volume)
        };
        if let Some(audio) = &mut self.audio {
            audio.set_gain(gain);
        }
    }

    /// `M` hotkey / `Réglages > Audio > Muet`.
    fn set_mute(&mut self, on: bool) {
        self.prefs.mute = on;
        self.prefs.save();
        self.apply_audio_gain();
    }

    /// `+`/`-` hotkeys: one 10-point step, clamped to 0..=100 and persisted.
    fn adjust_volume(&mut self, up: bool) {
        let volume = audio::step_volume(self.prefs.volume, up);
        if volume == self.prefs.volume {
            return; // already at 0 % or 100 %
        }
        self.set_volume(volume);
        eprintln!("audio: volume {volume} %");
    }

    /// `Réglages > Audio > Volume`: absolute setting, 0..=100 percent. The
    /// `+`/`-` hotkeys go through this too, so both write the same clamped
    /// value and push the same gain to the output stage.
    fn set_volume(&mut self, volume: u8) {
        let volume = volume.min(100);
        if self.prefs.volume == volume {
            return;
        }
        self.prefs.volume = volume;
        self.prefs.save();
        self.apply_audio_gain();
    }

    /// Tab pressed/released. Audio is silenced while accelerating (decided
    /// design); `prefs.mute`/`prefs.volume` are left untouched, so releasing
    /// the key restores exactly the previous state — a user who had already
    /// muted stays muted.
    fn set_fast_forward(&mut self, on: bool) {
        if self.fast_forward == on {
            return;
        }
        self.fast_forward = on;
        self.apply_audio_gain();
        if !on {
            // Turbo just released: every frame pushed into the ring while it
            // was held went in at gain 0 (see `apply_audio_gain`) so the
            // consumer never gapped to a stuck sample — but that leaves up to
            // ~256ms (`audio::RING_CAPACITY`'s worth in the worst case) of
            // enqueued silence that would otherwise have to play out before
            // real-time audio resumes. Drop it so unmuting is instant.
            if let Some(audio) = &self.audio {
                audio.flush();
            }
        }
        // Pacing restarts from now: an accelerated pass may have ended well
        // past the deadline it was aiming at, and the frames it ran ahead of
        // must not be counted as a backlog to catch up.
        self.next_deadline = Instant::now() + self.frame_duration;
    }

    /// `[`/`]` hotkeys / `Réglages > Émulation > Accéléré`: how many frames one
    /// Tab-held presentation runs. Clamped to the range the preferences file
    /// documents.
    fn set_fast_forward_factor(&mut self, factor: u8) {
        self.prefs.fast_forward_factor = factor.clamp(2, 4);
        self.prefs.save();
    }

    /// `C` hotkey / `Réglages > Émulation > Confirmation`.
    fn set_confirm_on_quit(&mut self, on: bool) {
        self.prefs.confirm_on_quit = on;
        self.prefs.save();
    }

    /// Whether the window is currently fullscreen (queried from winit itself
    /// rather than a mirrored bool on `App`, since `Window::set_fullscreen`
    /// can resolve asynchronously on some platforms — this always reflects
    /// what the platform actually reports right now).
    fn is_fullscreen(&self) -> bool {
        self.window.as_ref().is_some_and(|w| w.fullscreen().is_some())
    }

    /// `F11` hotkey / Ctrl+Cmd+F / `Affichage > Plein écran` (macOS-only
    /// menu): toggles borderless fullscreen on the window's current monitor.
    /// Borderless (not exclusive) fullscreen, matching winit's own "idiomatic
    /// way for fullscreen games to work on macOS" recommendation (see
    /// `winit::window::Window::set_fullscreen`'s doc) — no video-mode
    /// change, task switching/Spaces keep working.
    ///
    /// Deliberately **not** persisted in `prefs`: unlike zoom/filter/aspect,
    /// starting the next launch already fullscreen would be a surprising
    /// default for a window a debugger/agent needs to see, and the user
    /// re-enters it in one keypress anyway.
    ///
    /// Entering/leaving fullscreen changes the window's physical size, which
    /// fires `WindowEvent::Resized`; `apply_resize` (called from there)
    /// re-letterboxes the content for the new size, same as any other
    /// resize.
    fn set_fullscreen(&mut self, on: bool) {
        let Some(window) = &self.window else { return };
        if on {
            window.set_fullscreen(Some(winit::window::Fullscreen::Borderless(None)));
        } else {
            window.set_fullscreen(None);
        }
    }

    /// `O` hotkey / `Fichier > Ouvrir une ROM…` / the home screen's button:
    /// ask for the native ROM picker. It opens in the folder the library scans,
    /// so the two notions of "my ROM folder" agree. The dialog itself is shown
    /// by `pump_dialogs`, off this callback's stack (see `crate::dialog`).
    fn open_rom_dialog(&mut self) {
        let start = library::library_dir(&self.prefs);
        self.dialogs.request(dialog::Request::Rom { start });
    }

    /// Apply the answer of a finished native dialog, then open the next queued
    /// one. Called once per `about_to_wait` — never from `window_event`, whose
    /// stack a native modal must not be opened on.
    ///
    /// Cancelling a dialog or a load error leaves the current game untouched.
    fn pump_dialogs(&mut self) {
        while let Some(answer) = self.dialogs.poll() {
            // The panel held the screen for an arbitrary amount of wall time
            // and swallowed the key releases; drop held input and restart
            // pacing from now instead of catching up frames nobody ran.
            self.pad = JoypadState::default();
            self.set_fast_forward(false);
            self.next_deadline = Instant::now() + self.frame_duration;
            match answer {
                dialog::Answer::Rom(path) => {
                    // Remember where the player browses: it is the library's
                    // own fallback folder (`library::library_dir`).
                    if let Some(parent) =
                        path.parent().filter(|p| !p.as_os_str().is_empty())
                    {
                        self.prefs.last_rom_dir = Some(parent.to_path_buf());
                        self.prefs.save();
                    }
                    if let Err(e) = self.switch_rom(&path, true) {
                        eprintln!("error: could not load {}: {e}", path.display());
                        self.library.ui.error =
                            Some(format!("Impossible de charger {} : {e}", path.display()));
                    }
                }
                dialog::Answer::LibraryDir(dir) => {
                    self.prefs.library_dir = Some(dir);
                    self.prefs.save();
                    self.rescan_library();
                }
                dialog::Answer::ScreenshotDir(dir) => {
                    self.prefs.screenshot_dir = Some(dir);
                    self.prefs.save();
                    // The sheet's gallery reads that folder; rebuild it on the
                    // next frame instead of showing the old one's captures.
                    self.library.sheet = SheetData::default();
                }
                dialog::Answer::SaveDir(dir) => self.set_save_dir(Some(dir)),
                dialog::Answer::Cancelled => {}
            }
        }
        self.dialogs.pump();
    }

    /// F5 / `Émulation > Sauvegarder l'état` (Cmd+S): snapshot the whole
    /// console (`Snes::save_state`) into the current slot's sidecar — in
    /// `prefs.save_dir` when the session was started with one configured, else
    /// next to the loaded ROM (`paths::GamePaths`). Never fails the run: an I/O
    /// error is reported (missing folder created on the fly, unwritable one
    /// shown as `SLOT n ERREUR`) and emulation continues.
    fn save_state(&mut self) {
        let slot = self.prefs.save_slot;
        let path = self.paths.state_write(slot);
        let Some(bytes) = self.snes.as_mut().map(|s| s.save_state()) else { return };
        // Atomic (temp file + rename): a crash or power loss mid-write must
        // never leave a truncated `.state`/`.stateN`, which `Snes::load_state`
        // would then reject as a corrupt body, costing the whole slot instead
        // of just this save attempt.
        match crate::atomic::write(&path, &bytes) {
            Ok(()) => {
                eprintln!("state: saved {} ({} bytes)", path.display(), bytes.len());
                self.write_state_preview(&path);
                self.set_status(format!("SLOT {slot} SAUVE"));
            }
            Err(e) => {
                eprintln!("state: could not write {}: {e}", path.display());
                self.set_status(format!("SLOT {slot} ERREUR"));
            }
        }
    }

    /// Write the picture of a save state beside it (`<state>.png`, the raw
    /// 256x224 framebuffer, exactly what F12 and `--dump-frame` produce), so
    /// the game sheet can show what each slot holds.
    ///
    /// Atomic like the state itself, and **optional**: a failure is reported
    /// and nothing else happens — the state is already on disk and loads with
    /// or without its picture. Called after the state was written, so the two
    /// files always describe the same instant.
    fn write_state_preview(&mut self, state: &Path) {
        let Some(snes) = self.snes.as_ref() else { return };
        let mut rgba = vec![0u8; SCREEN_WIDTH * SCREEN_HEIGHT * 4];
        snes.framebuffer.to_rgba(&mut rgba);
        match crate::state::write_preview(
            state,
            &rgba,
            SCREEN_WIDTH as u32,
            SCREEN_HEIGHT as u32,
        ) {
            // The picture of this slot may already be on screen in the game
            // sheet: drop the cached texture so the new one is decoded.
            Ok(path) => self.library.textures.forget(&path),
            Err(e) => eprintln!("state: {e}"),
        }
    }

    /// Delete one save state and, with it, its preview picture: an orphaned
    /// picture would show the frame of a state that no longer exists. Asked for
    /// by the game sheet, which arms the request on a first click and sends it
    /// on the confirmation.
    fn delete_state(&mut self, path: &Path) {
        let preview = match crate::state::delete_with_preview(path) {
            Ok(preview) => {
                eprintln!("state: deleted {}", path.display());
                preview
            }
            Err(e) => {
                eprintln!("state: {e}");
                return;
            }
        };
        self.library.textures.forget(&preview);
        // Force `sync_library_view` to gather the sheet's file lists again on
        // the next frame, so the deleted slot leaves the list at once.
        self.library.sheet = SheetData::default();
    }

    /// F9 / `Émulation > Charger l'état` (Cmd+L): restore the console from the
    /// current slot's sidecar. The blob carries no ROM image;
    /// `Snes::load_state` reattaches the live ROM and rejects a state saved
    /// from a different game. Any error (missing file, wrong ROM, corrupt
    /// blob) is reported and the running game is left untouched.
    fn load_state(&mut self) {
        if self.snes.is_none() {
            return;
        }
        let slot = self.prefs.save_slot;
        // Read resolution, not write resolution: a slot saved before a save
        // folder was configured is still beside the ROM, and must keep loading.
        let path = self.paths.state_read(slot);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("state: no state in slot {slot} ({})", path.display());
                self.set_status(format!("SLOT {slot} VIDE"));
                return;
            }
            Err(e) => {
                eprintln!("state: could not read {}: {e}", path.display());
                self.set_status(format!("SLOT {slot} ERREUR"));
                return;
            }
        };
        match self.snes.as_mut().map(|s| s.load_state(&bytes)).unwrap_or(Ok(())) {
            Ok(()) => {
                eprintln!("state: loaded {}", path.display());
                // The slot's snapshot replaced `cart.sram` wholesale; see
                // `resync_sram_baseline` (review point A).
                self.resync_sram_baseline();
                self.set_status(format!("SLOT {slot} CHARGE"));
            }
            Err(e) => {
                eprintln!("state: load failed ({}): {e}", path.display());
                self.set_status(format!("SLOT {slot} ERREUR"));
            }
        }
    }

    /// F7 / `Émulation > Slot suivant`: cycle through the 10 slots.
    fn next_slot(&mut self) {
        let slot = (self.prefs.save_slot + 1) % crate::state::SLOT_COUNT;
        self.set_slot(slot);
    }

    /// Select the slot F5/F9 (and Cmd+S/Cmd+L) act on; persisted immediately.
    fn set_slot(&mut self, slot: u8) {
        self.prefs.save_slot = slot.min(crate::state::SLOT_COUNT - 1);
        self.prefs.save();
        let slot = self.prefs.save_slot;
        self.set_status(format!("SLOT {slot}"));
    }

    /// `F10` hotkey / `Réglages > Émulation > Reprise instantanée`: whether
    /// `<rom>.resume` is restored at launch. The session state is written on
    /// exit either way, so turning the option back on resumes from the last
    /// session.
    fn set_resume_on_launch(&mut self, on: bool) {
        self.prefs.resume_on_launch = on;
        self.prefs.save();
    }

    /// Write the automatic session state to `<rom>.resume`, a file outside the
    /// manual `.state`/`.stateN` series so it can never overwrite a slot. Runs
    /// on every exit path (see `persist_all`) and before a ROM switch.
    fn write_resume_state(&mut self) {
        let path = self.paths.resume_write();
        let Some(bytes) = self.snes.as_mut().map(|s| s.save_state()) else { return };
        // Atomic (temp file + rename): this runs unconditionally on every
        // exit path, so a crash or power loss mid-write must never corrupt
        // `.resume` — a truncated blob would make the *next* launch's
        // `try_resume` reject it and silently fall back to power-on, but a
        // half-written file could in principle also be misread as valid by
        // an unlucky byte pattern, which atomic replacement rules out.
        match crate::atomic::write(&path, &bytes) {
            Ok(()) => {
                eprintln!("resume: wrote {} ({} bytes)", path.display(), bytes.len());
                // Same picture-beside-the-state rule as the manual slots, so
                // the sheet's `Reprise` line shows where the session stopped.
                self.write_state_preview(&path);
            }
            Err(e) => eprintln!("resume: could not write {}: {e}", path.display()),
        }
    }

    /// Restore `<rom>.resume` for the currently loaded game if the option is
    /// on and the file exists. A state from another game, a truncated file or
    /// an incompatible format is reported and ignored — the game then simply
    /// starts from power-on, which is why `load_state`'s error is not fatal
    /// here (it leaves the console untouched).
    ///
    /// Design decision (review point A): `Snes::load_state` overwrites the
    /// whole cart, including SRAM, with whatever the `.resume` blob held at
    /// the moment it was written — which can predate the `.srm` sidecar
    /// already on disk (e.g. the player restored an older `.resume` from a
    /// backup, or something external touched the `.srm` after that snapshot
    /// was taken). Silently reverting to the older, embedded SRAM in that
    /// case would look like *losing* the more recent save. Since a resume can
    /// only make the game's *progress* older, not the *battery save* (those
    /// are conceptually independent even though a state blob bundles both),
    /// this picks the more recent of the two by file mtime: if `.srm` is
    /// strictly newer than `.resume`, the SRAM this launch already loaded
    /// from it (`sram_baseline`, captured by `save::load_sram` right before
    /// `Snes::new`) is re-applied over the resumed cart's SRAM.
    fn try_resume(&mut self) {
        if !self.prefs.resume_on_launch || self.snes.is_none() {
            return;
        }
        let path = self.paths.resume_read();
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                eprintln!("resume: could not read {}: {e}", path.display());
                return;
            }
        };
        let srm_path = self.paths.srm_read();
        let srm_is_newer =
            mtime(&srm_path).zip(mtime(&path)).is_some_and(|(srm, resume)| srm > resume);
        match self.snes.as_mut().map(|s| s.load_state(&bytes)).unwrap_or(Ok(())) {
            Ok(()) => {
                eprintln!("resume: restored {}", path.display());
                if srm_is_newer {
                    // `sram_baseline` is exactly `cart.sram.len()` bytes for
                    // this ROM (captured from the same cart, same session),
                    // so re-applying it here can never mis-size.
                    if let Some(snes) = &mut self.snes {
                        snes.bus.cart.sram.load(&self.sram_baseline);
                    }
                    eprintln!(
                        "resume: {} is newer than {}; keeping its SRAM instead of the resumed snapshot's",
                        srm_path.display(),
                        path.display()
                    );
                }
                self.set_status("REPRISE");
                self.resync_sram_baseline();
            }
            Err(e) => {
                eprintln!("resume: ignoring {} ({e})", path.display());
            }
        }
    }

    /// F12 / `Fichier > Capture d'écran`: write the raw 256x224 framebuffer as
    /// a PNG, straight from the core — no FPS/status overlay, no zoom, no
    /// filter (those live only in the windowed present path). Destination:
    /// `prefs.screenshot_dir` if set, else a `Screenshots` folder beside the
    /// ROM; the directory is created on demand.
    fn take_screenshot(&mut self) {
        let Some(cart_title) = self.snes.as_ref().map(|s| s.bus.cart.title.clone()) else {
            return; // No cartridge loaded: nothing to capture.
        };
        let dir = self
            .prefs
            .screenshot_dir
            .clone()
            .unwrap_or_else(|| sibling_dir(&self.current_rom_path, "Screenshots"));
        let stem = format!(
            "{}_{}",
            crate::sanitize_file_stem(&cart_title),
            crate::now_local().file_stamp()
        );
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("screenshot: could not create {}: {e}", dir.display());
            self.set_status("CAPTURE IMPOSSIBLE");
            return;
        }
        let path = crate::unique_path(&dir, &stem, "png");
        let result = match &self.snes {
            Some(snes) => crate::write_frame_png(snes, &path),
            None => return,
        };
        match result {
            Ok(()) => {
                eprintln!("screenshot: wrote {}", path.display());
                self.set_status("CAPTURE ECRAN");
            }
            Err(e) => {
                eprintln!("screenshot: {e}");
                self.set_status("CAPTURE IMPOSSIBLE");
            }
        }
    }

    /// `Fichier > Exporter la musique (.spc)`: dump the current APU state as a
    /// standard `.spc` file in an `SPC` folder beside the ROM.
    fn export_spc(&mut self) {
        let Some(title) = self.snes.as_ref().map(|s| s.bus.cart.title.trim().to_string()) else {
            return; // No cartridge loaded: no APU state to export.
        };
        let dir = sibling_dir(&self.current_rom_path, "SPC");
        let stem =
            format!("{}_{}", crate::sanitize_file_stem(&title), crate::now_local().file_stamp());
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("spc: could not create {}: {e}", dir.display());
            self.set_status("EXPORT SPC ERREUR");
            return;
        }
        let path = crate::unique_path(&dir, &stem, "spc");
        let result = match &self.snes {
            Some(snes) => crate::spc::write(snes, &path, &title),
            None => return,
        };
        match result {
            Ok(()) => {
                eprintln!("spc: wrote {} ({} bytes)", path.display(), crate::spc::FILE_SIZE);
                self.set_status("MUSIQUE SPC EXPORTEE");
            }
            Err(e) => {
                eprintln!("spc: {e}");
                self.set_status("EXPORT SPC ERREUR");
            }
        }
    }

    /// Show `text` in the bottom-left corner for `STATUS_DURATION`. The overlay
    /// font has uppercase letters, digits and a few separators only, so
    /// messages are written without accents.
    fn set_status(&mut self, text: impl Into<String>) {
        self.status = Some((text.into(), Instant::now() + STATUS_DURATION));
    }

    /// `F` hotkey / `Réglages > Affichage > Afficher les FPS`: toggles the
    /// on-screen FPS overlay (see `draw_overlay_text`).
    fn toggle_show_fps(&mut self) {
        self.set_show_fps(!self.prefs.show_fps);
    }

    /// Applies and persists the FPS-overlay setting; restored on the next
    /// launch by `Prefs::load`.
    fn set_show_fps(&mut self, on: bool) {
        self.prefs.show_fps = on;
        self.prefs.save();
    }

    /// F1-F4 hotkeys / `Réglages > Affichage > Taille de la fenêtre`: sets the
    /// window's integer upscale factor and resizes the window to match.
    /// Clamped to 1..=4 (the range the panel/hotkeys offer; `Prefs::sanitize`
    /// separately allows up to 8 for a hand-edited file, which this cannot
    /// reach through the UI).
    fn set_zoom(&mut self, zoom: u8) {
        let zoom = zoom.clamp(1, 4);
        if self.prefs.zoom == zoom {
            return;
        }
        self.prefs.zoom = zoom;
        self.prefs.save();
        self.resize_window_for_display_prefs();
    }

    /// `V` hotkey / `Réglages > Affichage > Filtre`: applies and persists the
    /// presentation filter. Purely a rendering setting — no resize needed,
    /// `render::compose_frame` reads it every presented frame.
    fn set_filter(&mut self, filter: Filter) {
        let value = filter.as_pref();
        if self.prefs.filter == value {
            return;
        }
        self.prefs.filter = value.to_string();
        self.prefs.save();
    }

    /// `V` hotkey: steps `Aucun -> Lissé -> CRT -> Aucun`
    /// (`render::Filter::next`), independent of the current window size.
    fn cycle_filter(&mut self) {
        let current = Filter::from_pref(&self.prefs.filter);
        self.set_filter(current.next());
    }

    /// `R` hotkey / `Réglages > Affichage > Ratio`: applies and persists the
    /// pixel-aspect-ratio mode, then resizes the window since the content's
    /// native (pre-zoom) size depends on it (`Aspect::Tv` stretches the width
    /// — see `render::content_dims`).
    fn set_aspect(&mut self, aspect: Aspect) {
        let value = aspect.as_pref();
        if self.prefs.aspect == value {
            return;
        }
        self.prefs.aspect = value.to_string();
        self.prefs.save();
        self.resize_window_for_display_prefs();
    }

    /// `R` hotkey: toggles Pixel-parfait <-> TV authentique
    /// (`render::Aspect::toggled`).
    fn cycle_aspect(&mut self) {
        let current = Aspect::from_pref(&self.prefs.aspect);
        self.set_aspect(current.toggled());
    }

    /// Resizes the window to `render::zoomed_dims(prefs.zoom, prefs.aspect)`,
    /// clamped to fit the window's current monitor (`render::
    /// clamp_to_available`) so a large zoom/TV-ratio combination can never
    /// request an unusable, off-screen-sized window. Called after `set_zoom`
    /// and `set_aspect`; `set_filter` never resizes, since a filter doesn't
    /// change the content's target size.
    fn resize_window_for_display_prefs(&mut self) {
        let Some(window) = &self.window else { return };
        let aspect = Aspect::from_pref(&self.prefs.aspect);
        let target = render::zoomed_dims(self.prefs.zoom, aspect);
        let max = window.current_monitor().map(|m| logical_monitor_size(&m));
        let (w, h) = match max {
            Some(max) => render::clamp_to_available(target, max),
            None => target,
        };
        // `request_inner_size` resolves synchronously on some platforms (no
        // `WindowEvent::Resized` follows in that case, per winit's own doc on
        // the method) and asynchronously on others (a `Resized` event follows
        // and `window_event` calls `apply_resize` from there) — handle both:
        // apply immediately when winit already computed the new size, do
        // nothing otherwise and let the event arrive.
        if let Some(new_size) = window.request_inner_size(LogicalSize::new(w, h)) {
            self.apply_resize(new_size);
        }
    }

    /// Applies a new physical window size to the `pixels` buffer/surface
    /// (both always equal — see the `pixels` field doc) and records it in
    /// `out_w`/`out_h`. Called from `WindowEvent::Resized` (any resize,
    /// including one the user drags) and, when winit resolves it
    /// synchronously, straight from `resize_window_for_display_prefs`.
    fn apply_resize(&mut self, size: winit::dpi::PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return; // Minimized/zero-size transient; nothing to present.
        }
        if let Some(pixels) = &mut self.pixels {
            if let Err(e) = pixels.resize_buffer(size.width, size.height) {
                eprintln!("error: resize pixel buffer: {e}");
                return;
            }
            if let Err(e) = pixels.resize_surface(size.width, size.height) {
                eprintln!("error: resize surface: {e}");
                return;
            }
        }
        if let Some(ui) = &mut self.ui {
            ui.resize(size.width, size.height);
        }
        self.out_w = size.width;
        self.out_h = size.height;
    }

    /// Replace `self.snes` with a freshly constructed console for the ROM at
    /// `path`. Persists the outgoing cart's SRAM (via its own `paths`)
    /// before replacing it, then loads the new cart's `.srm` sidecar the
    /// same way startup does, resets pad/pause/frame-advance state, and
    /// retargets pacing at the new cart's region field rate (a game switch
    /// can cross the PAL/NTSC line).
    /// `resume` asks for the new game's session state to be restored once it is
    /// loaded; `reset` passes false, since restoring the state it just wrote
    /// would undo the reset.
    ///
    /// This is also the path the home screen takes to start the *first* game
    /// of a run, in which case there is no outgoing console to flush.
    fn switch_rom(&mut self, path: &Path, resume: bool) -> Result<(), String> {
        // Leaving a game is an exit for that game: its session state and
        // battery SRAM are written before the console is replaced. SRAM
        // first, resume snapshot second — same reasoning as `persist_all`
        // (review point A: keeps `.resume`'s mtime >= `.srm`'s in the
        // ordinary case, so `try_resume`'s newer-`.srm` guard doesn't misfire
        // next time this ROM is loaded).
        if let Some(snes) = &self.snes {
            save::save_if_dirty(&snes.bus.cart, &self.paths.srm_write(), &self.sram_baseline);
            self.write_resume_state();
        }

        // The outgoing game's play time is final at this point.
        if self.play_unsaved > 0 {
            self.play_unsaved = 0;
            self.prefs.save();
        }

        let bytes = crate::load_rom_bytes(path)?;
        let mut cart = Cartridge::from_bytes(bytes)?;
        let game_id = library::game_id(cart.title.trim(), cart.header_checksum, &cart.rom);
        // The folder preference is read here — at load time — and then frozen
        // for the whole session (see the `paths` field): a game loaded now
        // follows whatever `Réglages > Dossiers` currently says. The game id is
        // what names its sidecars inside a shared folder, so two ROM files of
        // the same name keep separate saves.
        let game_paths =
            crate::paths::GamePaths::new(path, &game_id, self.prefs.save_dir.clone(), None)
                .with_previous_dir(self.prefs.previous_save_dir.clone());
        let sram_baseline = save::load_sram(&mut cart, &game_paths.srm_read());

        self.title = window_title(&cart.title);
        self.frame_duration = Duration::from_secs_f64(1.0 / cart.region.frames_per_second());
        self.snes = Some(Snes::new(cart));
        self.paths = game_paths;
        self.sram_baseline = sram_baseline;
        self.pad = JoypadState::default();
        self.paused = false;
        self.frame_advance = false;
        self.fast_forward = false;
        self.apply_audio_gain();
        self.next_deadline = Instant::now() + self.frame_duration;
        if let Some(window) = &self.window {
            window.set_title(&self.title);
        }
        self.current_rom_path = path.to_path_buf();
        // A loaded cartridge always takes the window: this is what turns the
        // home screen into the game screen.
        self.state.start_session();
        self.start_play_session(game_id);
        if resume {
            self.try_resume();
        }
        Ok(())
    }

    /// `F6` hotkey / `Emulation > Reset` menu item (Cmd+R, macOS only):
    /// reload the currently running ROM in place. Reuses `switch_rom` with the
    /// same path rather than rebuilding `Snes` from `self.snes.bus.cart`
    /// directly (`Cartridge` isn't `Clone`): `switch_rom` first flushes the
    /// live, possibly-dirty SRAM to its `.srm`, then reloads that same file
    /// into the fresh cart, so the net effect is a power-on reset of
    /// CPU/PPU/APU state that preserves the current battery save — matching
    /// the SNES's physical reset button, which restarts execution but never
    /// erases cartridge SRAM.
    fn reset(&mut self) {
        if self.snes.is_none() {
            return;
        }
        let path = self.current_rom_path.clone();
        if let Err(e) = self.switch_rom(&path, false) {
            eprintln!("error: reset failed to reload {}: {e}", path.display());
        }
    }

    /// Drains muda's global menu-click channel (populated on the main
    /// thread by AppKit when a menu item is activated — either by mouse or
    /// by its accelerator) and dispatches each click. Called once per
    /// `about_to_wait` so a menu action lands before that iteration's
    /// pacing/frame-run, the same way keyboard hotkeys are handled
    /// synchronously in `window_event`. The menu carries no predefined item but
    /// separators — AppKit's own about panel opens a nested run loop that
    /// crashes winit, so it was removed (`menu::install`).
    /// `Quit` is a *custom* item (not `PredefinedMenuItem::quit`) so
    /// it routes here and we exit the winit loop the same way `Esc`/window-close
    /// do, which triggers the exit-time battery-SRAM flush in `run` — AppKit's
    /// `terminate:` would kill the process before that save could run.
    #[cfg(target_os = "macos")]
    fn poll_menu_events(&mut self, event_loop: &ActiveEventLoop) {
        while let Ok(event) = muda::MenuEvent::receiver().try_recv() {
            let Some(menu) = &self.menu else { continue };
            // Actions only — the menu holds no setting any more, so no branch
            // here reads back a checkmark AppKit flipped on its own, and no
            // menu state has to be re-derived afterwards. Every option is
            // changed through `ui::settings` -> `Action::Set` -> the same
            // `set_*` methods the hotkeys call.
            let id = event.id.clone();
            // Quit shares one id across the app-menu and File-menu items; it
            // goes through the same confirmation as Esc.
            if id == menu.quit.id() || id == menu.quit_file.id() {
                self.request_quit(event_loop);
            } else if id == menu.home.id() {
                self.go_home();
            } else if id == menu.settings.id() {
                self.open_settings();
            } else if id == menu.open_rom.id() {
                self.open_rom_dialog();
            } else if id == menu.pause_resume.id() {
                self.paused = !self.paused;
            } else if id == menu.reset.id() {
                self.reset();
            } else if id == menu.save_state.id() {
                self.save_state();
            } else if id == menu.load_state.id() {
                self.load_state();
            } else if id == menu.next_slot.id() {
                self.next_slot();
            } else if id == menu.screenshot.id() {
                self.take_screenshot();
            } else if id == menu.export_spc.id() {
                self.export_spc();
            } else if id == menu.fullscreen.id() {
                self.set_fullscreen(!self.is_fullscreen());
            }
        }
    }
}

/// Picture to show for `id`, or `None` when the game still needs one (see
/// `library::resolve_picture`). Both the display map and the generation queue
/// go through this, so they can never disagree.
fn picture_for(prefs: &Prefs, id: &str) -> Option<PathBuf> {
    let custom = prefs.games.get(id).and_then(|s| s.thumbnail.clone());
    library::resolve_picture(custom.as_deref(), thumbs::thumb_path(id).as_deref())
}

/// What the `Entrées` section says after a binding was written. A plain
/// assignment needs no comment; a swap does, since the *other* button changed
/// too and the player did not ask for that directly.
fn bind_notice(result: input::BindResult, label: &str) -> Option<String> {
    match result {
        input::BindResult::Swapped(other) => Some(format!(
            "{label} servait déjà pour {} : les deux boutons ont été échangés.",
            ui::settings::button_label(other)
        )),
        // The other button had nothing to receive in exchange (it was claiming
        // the same key/button, which is why one of the two was mute): it goes
        // back to its default rather than keeping a binding it no longer has.
        input::BindResult::Reverted(other) => Some(format!(
            "{label} servait déjà pour {} : ce bouton revient à son réglage par défaut.",
            ui::settings::button_label(other)
        )),
        input::BindResult::Bound | input::BindResult::Unchanged => None,
    }
}

/// Point `save_dir` at `dir`, keeping the folder being replaced in `previous`.
///
/// `previous` is a read-only fallback (`paths::GamePaths::with_previous_dir`):
/// the saves left in a folder the player stops using must keep being found, or
/// clearing the setting would hand back whatever older file sits beside the
/// ROM and look like lost progress. Only a folder actually being left is
/// recorded — re-picking the same one changes nothing — and the folder now in
/// use is never also the "previous" one.
fn move_save_dir(
    save_dir: &mut Option<PathBuf>,
    previous: &mut Option<PathBuf>,
    dir: Option<PathBuf>,
) {
    let left_behind = save_dir.take().filter(|old| Some(old) != dir.as_ref());
    if left_behind.is_some() {
        *previous = left_behind;
    }
    if dir.is_some() && *previous == dir {
        *previous = None;
    }
    *save_dir = dir;
}

/// What the `Dossiers` section says after the save folder changed: when the new
/// setting takes effect, and what became of the files in the folder being
/// replaced. Both facts matter — the running game keeps writing where it read
/// from, and a save left in the abandoned folder is still *read*
/// (`paths::read_sidecar`), which is what stops the change from looking like
/// lost progress.
fn save_dir_notice(game_running: bool, previous: Option<&Path>) -> String {
    let mut text = String::from("Pris en compte au prochain chargement de jeu");
    if game_running {
        text.push_str(" : la partie en cours garde ses fichiers actuels.");
    } else {
        text.push('.');
    }
    if let Some(previous) = previous {
        text.push_str(&format!(
            " Les sauvegardes restées dans {} sont toujours relues ; rien n'a été déplacé ni \
             supprimé.",
            previous.display()
        ));
    }
    text
}

/// Top-row digit key -> save-state slot number (`Digit0` = slot 0 ... `Digit9`
/// = slot 9), or `None` for any other key. Numpad digits are deliberately not
/// aliased: they're needed unmodified for a future 2-player/gamepad-less
/// numpad layout, and a single unambiguous row is easier to document.
fn digit_to_slot(code: KeyCode) -> Option<u8> {
    match code {
        KeyCode::Digit0 => Some(0),
        KeyCode::Digit1 => Some(1),
        KeyCode::Digit2 => Some(2),
        KeyCode::Digit3 => Some(3),
        KeyCode::Digit4 => Some(4),
        KeyCode::Digit5 => Some(5),
        KeyCode::Digit6 => Some(6),
        KeyCode::Digit7 => Some(7),
        KeyCode::Digit8 => Some(8),
        KeyCode::Digit9 => Some(9),
        _ => None,
    }
}

/// Sleep for the bulk of the remaining time until `deadline`, then spin the
/// last `SPIN_SLACK` for accuracy; advances `deadline` by one
/// `frame_duration`. If wall clock has drifted more than 4 frames past the
/// deadline (long pause, breakpoint, laptop sleep), resync instead of
/// fast-forwarding a backlog of frames.
fn pace(deadline: &mut Instant, frame_duration: Duration) {
    let now = Instant::now();
    if now < *deadline {
        let remaining = *deadline - now;
        if remaining > SPIN_SLACK {
            std::thread::sleep(remaining - SPIN_SLACK);
        }
        while Instant::now() < *deadline {
            std::hint::spin_loop();
        }
    }
    *deadline += frame_duration;
    if Instant::now() > *deadline + frame_duration * 4 {
        *deadline = Instant::now() + frame_duration;
    }
}

// --- FPS overlay: tiny built-in bitmap font, windowed-present-only ---------
//
// Deliberately not a font asset: the overlay only ever needs digits, F/P/S,
// a space and '/', so a hand-encoded 3x5 glyph table avoids pulling in a
// font dependency for a handful of on-screen characters. Drawn directly into
// `pixels`' own RGBA8 frame buffer (`out_w`x`out_h`, the window's physical
// size) from `about_to_wait`, *after* `render::compose_frame` has already
// scaled/filtered/letterboxed the emulated picture into it — never onto
// `native_buf` or `snes.framebuffer` (the core's own pixel data), so this has
// no effect on headless `--dump-frame`/`--dump-frame-every` output, which
// reads straight from the core, nor on the F12 screenshot.
//
// Drawing after scaling (not before) is deliberate: at a `FONT_SCALE` fixed
// in *output* pixels, the overlay's on-screen size stays constant regardless
// of the window's zoom/size — a glyph drawn into the native 256x224 buffer
// before scaling would instead have grown proportionally with zoom (e.g. 4x
// too big at zoom x4, worse in a maximized/fullscreen window), which is what
// made the previous native-resolution placement look oversized.

/// Glyph cell size before scaling: 3 columns x 5 rows.
const GLYPH_W: usize = 3;
const GLYPH_H: usize = 5;
/// Each on-screen glyph pixel is drawn as a `FONT_SCALE`x`FONT_SCALE` block
/// of *output* pixels (the overlay is drawn post-scale — see the module
/// comment above), so 1x keeps the whole overlay small and unobtrusive at
/// any window size instead of scaling up with zoom.
const FONT_SCALE: usize = 1;
/// Horizontal distance (in output pixels) from one glyph's left edge to the
/// next: glyph width + 1 column of inter-glyph spacing, both scaled.
const CHAR_ADVANCE: usize = (GLYPH_W + 1) * FONT_SCALE;
/// Gap between the framebuffer edge and the overlay's background box.
const OVERLAY_MARGIN: usize = 3;
/// Gap between the background box edge and the glyphs it contains.
const OVERLAY_PAD: usize = 2;

/// 3x5 bitmap glyph for one overlay character. Each row is a `u8` using its
/// low 3 bits as the left/middle/right pixel columns (bit 2 = leftmost, bit
/// 0 = rightmost; set = lit). Digits, uppercase letters and a few separators
/// are defined (enough for the FPS readout and the status messages); lowercase
/// is folded to uppercase and anything else renders as a blank cell (still
/// advances the cursor, like a space).
fn glyph(c: char) -> [u8; GLYPH_H] {
    match c.to_ascii_uppercase() {
        '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        '2' => [0b111, 0b001, 0b111, 0b100, 0b111],
        '3' => [0b111, 0b001, 0b111, 0b001, 0b111],
        '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        '5' => [0b111, 0b100, 0b111, 0b001, 0b111],
        '6' => [0b111, 0b100, 0b111, 0b101, 0b111],
        '7' => [0b111, 0b001, 0b001, 0b001, 0b001],
        '8' => [0b111, 0b101, 0b111, 0b101, 0b111],
        '9' => [0b111, 0b101, 0b111, 0b001, 0b111],
        'A' => [0b010, 0b101, 0b111, 0b101, 0b101],
        'B' => [0b110, 0b101, 0b110, 0b101, 0b110],
        'C' => [0b011, 0b100, 0b100, 0b100, 0b011],
        'D' => [0b110, 0b101, 0b101, 0b101, 0b110],
        'E' => [0b111, 0b100, 0b110, 0b100, 0b111],
        'F' => [0b111, 0b100, 0b111, 0b100, 0b100],
        'G' => [0b011, 0b100, 0b101, 0b101, 0b011],
        'H' => [0b101, 0b101, 0b111, 0b101, 0b101],
        'I' => [0b111, 0b010, 0b010, 0b010, 0b111],
        'J' => [0b001, 0b001, 0b001, 0b101, 0b010],
        'K' => [0b101, 0b101, 0b110, 0b101, 0b101],
        'L' => [0b100, 0b100, 0b100, 0b100, 0b111],
        'M' => [0b101, 0b111, 0b111, 0b101, 0b101],
        'N' => [0b110, 0b101, 0b101, 0b101, 0b101],
        'O' => [0b010, 0b101, 0b101, 0b101, 0b010],
        'P' => [0b111, 0b101, 0b111, 0b100, 0b100],
        'Q' => [0b010, 0b101, 0b101, 0b111, 0b011],
        'R' => [0b110, 0b101, 0b110, 0b101, 0b101],
        'S' => [0b111, 0b100, 0b111, 0b001, 0b111],
        'T' => [0b111, 0b010, 0b010, 0b010, 0b010],
        'U' => [0b101, 0b101, 0b101, 0b101, 0b111],
        'V' => [0b101, 0b101, 0b101, 0b101, 0b010],
        'W' => [0b101, 0b101, 0b111, 0b111, 0b101],
        'X' => [0b101, 0b101, 0b010, 0b101, 0b101],
        'Y' => [0b101, 0b101, 0b010, 0b010, 0b010],
        'Z' => [0b111, 0b001, 0b010, 0b100, 0b111],
        '/' => [0b001, 0b001, 0b010, 0b100, 0b100],
        '-' => [0b000, 0b000, 0b111, 0b000, 0b000],
        '.' => [0b000, 0b000, 0b000, 0b000, 0b010],
        ':' => [0b000, 0b010, 0b000, 0b010, 0b000],
        _ => [0; GLYPH_H],
    }
}

/// Blits a solid `w`x`h` RGBA rectangle at `(x,y)` into a `buf_w`x`buf_h`
/// RGBA8 buffer, clipped to its bounds. `buf_w`/`buf_h` are the *caller's*
/// buffer dimensions (the window's current physical size, not a fixed
/// constant — see the module comment above), so the same drawing code works
/// at any window size.
fn fill_rect(frame: &mut [u8], buf_w: usize, buf_h: usize, x: usize, y: usize, w: usize, h: usize, color: [u8; 4]) {
    for row in y..(y + h).min(buf_h) {
        let row_base = row * buf_w * 4;
        for col in x..(x + w).min(buf_w) {
            let i = row_base + col * 4;
            frame[i..i + 4].copy_from_slice(&color);
        }
    }
}

/// Paints `text` into the top-right corner of a `buf_w`x`buf_h` RGBA8 buffer
/// over a solid black background box, so the overlay stays legible against
/// any game content behind it.
fn draw_overlay_text(frame: &mut [u8], buf_w: usize, buf_h: usize, text: &str, color: [u8; 4]) {
    let Some((box_w, _)) = text_box_size(text, buf_w, buf_h) else { return };
    draw_text_box(frame, buf_w, buf_h, buf_w.saturating_sub(OVERLAY_MARGIN + box_w), OVERLAY_MARGIN, text, color);
}

/// Paints `text` in the bottom-left corner: the transient status messages
/// (screenshot taken, slot saved/loaded, SPC exported) go there so they never
/// collide with the FPS readout in the opposite corner.
fn draw_status_text(frame: &mut [u8], buf_w: usize, buf_h: usize, text: &str, color: [u8; 4]) {
    let Some((_, box_h)) = text_box_size(text, buf_w, buf_h) else { return };
    draw_text_box(frame, buf_w, buf_h, OVERLAY_MARGIN, buf_h.saturating_sub(OVERLAY_MARGIN + box_h), text, color);
}

/// Background-box size for `text` in a `buf_w`x`buf_h` buffer, or `None` when
/// it cannot fit (the caller then skips drawing rather than doing
/// out-of-bounds math). Independent of `buf_w`/`buf_h` except for that fit
/// check: the box itself is always the same size in output pixels, at any
/// window size (see the module comment above) — only whether it *fits* can
/// depend on the buffer.
fn text_box_size(text: &str, buf_w: usize, buf_h: usize) -> Option<(usize, usize)> {
    let box_w = text.chars().count() * CHAR_ADVANCE + OVERLAY_PAD * 2;
    let box_h = GLYPH_H * FONT_SCALE + OVERLAY_PAD * 2;
    if box_w > buf_w || box_h > buf_h {
        return None;
    }
    Some((box_w, box_h))
}

/// Blits `text` at `(x0, y0)` over a solid black background box.
fn draw_text_box(frame: &mut [u8], buf_w: usize, buf_h: usize, x0: usize, y0: usize, text: &str, color: [u8; 4]) {
    let Some((box_w, box_h)) = text_box_size(text, buf_w, buf_h) else { return };

    fill_rect(frame, buf_w, buf_h, x0, y0, box_w, box_h, [0, 0, 0, 255]);

    let mut cx = x0 + OVERLAY_PAD;
    let cy = y0 + OVERLAY_PAD;
    for ch in text.chars() {
        let rows = glyph(ch);
        for (row, bits) in rows.iter().enumerate() {
            for col in 0..GLYPH_W {
                if bits & (1 << (GLYPH_W - 1 - col)) != 0 {
                    let px = cx + col * FONT_SCALE;
                    let py = cy + row * FONT_SCALE;
                    fill_rect(frame, buf_w, buf_h, px, py, FONT_SCALE, FONT_SCALE, color);
                }
            }
        }
        cx += CHAR_ADVANCE;
    }
}

#[cfg(test)]
mod overlay_tests {
    use super::*;

    #[test]
    fn glyph_digits_and_symbols_match_hand_encoded_bitmap() {
        assert_eq!(glyph('0'), [0b111, 0b101, 0b101, 0b101, 0b111]);
        assert_eq!(glyph('1'), [0b010, 0b110, 0b010, 0b010, 0b111]);
        assert_eq!(glyph('8'), [0b111, 0b101, 0b111, 0b101, 0b111]);
        assert_eq!(glyph('F'), [0b111, 0b100, 0b111, 0b100, 0b100]);
        assert_eq!(glyph('/'), [0b001, 0b001, 0b010, 0b100, 0b100]);
        // Unknown/space characters render as a blank cell (still advances
        // the cursor in draw_overlay_text) rather than panicking.
        assert_eq!(glyph(' '), [0; GLYPH_H]);
    }

    #[test]
    fn every_character_used_by_a_status_message_has_a_glyph() {
        // The messages `set_status` can produce, plus the FPS readout (now
        // "FPS 60/50" — a space separates the label from the numbers).
        let messages = [
            "FPS 60/50",
            "SLOT 9 SAUVE",
            "SLOT 0 CHARGE",
            "SLOT 3 VIDE",
            "SLOT 7 ERREUR",
            "SLOT 4",
            "REPRISE",
            "CAPTURE ECRAN",
            "CAPTURE IMPOSSIBLE",
            "MUSIQUE SPC EXPORTEE",
            "EXPORT SPC ERREUR",
            "MANETTE 1 CONNECTEE",
            "MANETTE 1 DECONNECTEE",
            "MANETTE 2 CONNECTEE",
            "MANETTE 2 DECONNECTEE",
        ];
        for msg in messages {
            for c in msg.chars().filter(|c| *c != ' ') {
                assert_ne!(glyph(c), [0; GLYPH_H], "no glyph for {c:?} in {msg:?}");
            }
            assert!(
                text_box_size(msg, SCREEN_WIDTH, SCREEN_HEIGHT).is_some(),
                "{msg:?} does not fit in a 256x224 buffer"
            );
        }
        // The hot-plug notices are built by `pad::PadNotice::status`, not
        // written out here: check the real strings, for every port and both
        // directions, so the two can't drift apart.
        for player in 0..pad::PLAYERS {
            for connected in [true, false] {
                let text = pad::PadNotice { player, connected, name: String::new() }.status();
                assert!(messages.contains(&text.as_str()), "unlisted status {text:?}");
                for c in text.chars().filter(|c| *c != ' ') {
                    assert_ne!(glyph(c), [0; GLYPH_H], "no glyph for {c:?} in {text:?}");
                }
            }
        }
        // Lowercase folds to uppercase rather than rendering blank.
        assert_eq!(glyph('a'), glyph('A'));
        assert_eq!(glyph(' '), [0; GLYPH_H]);
    }

    #[test]
    fn status_text_is_drawn_bottom_left_and_fps_top_right() {
        let mut frame = vec![0u8; SCREEN_WIDTH * SCREEN_HEIGHT * 4];
        draw_status_text(&mut frame, SCREEN_WIDTH, SCREEN_HEIGHT, "SLOT 3 SAUVE", STATUS_COLOR);
        let (_, box_h) = text_box_size("SLOT 3 SAUVE", SCREEN_WIDTH, SCREEN_HEIGHT).expect("fits");
        // Bottom-left corner of the box is the black background fill.
        let y = SCREEN_HEIGHT - OVERLAY_MARGIN - box_h;
        let idx = (y * SCREEN_WIDTH + OVERLAY_MARGIN) * 4;
        assert_eq!(&frame[idx..idx + 4], &[0, 0, 0, 255]);
        // The opposite corner (where the FPS overlay lives) is untouched.
        let top_right = (OVERLAY_MARGIN * SCREEN_WIDTH + (SCREEN_WIDTH - OVERLAY_MARGIN - 1)) * 4;
        assert_eq!(&frame[top_right..top_right + 4], &[0, 0, 0, 0]);
        assert!(frame.chunks_exact(4).any(|p| p == STATUS_COLOR));
    }

    #[test]
    fn text_too_wide_for_the_buffer_is_skipped_instead_of_drawn() {
        assert_eq!(text_box_size(&"W".repeat(64), SCREEN_WIDTH, SCREEN_HEIGHT), None);
        let mut frame = vec![7u8; SCREEN_WIDTH * SCREEN_HEIGHT * 4];
        draw_status_text(&mut frame, SCREEN_WIDTH, SCREEN_HEIGHT, &"W".repeat(64), STATUS_COLOR);
        assert!(frame.iter().all(|&b| b == 7), "nothing should have been drawn");
    }

    #[test]
    fn fill_rect_paints_only_the_target_region() {
        let mut frame = vec![9u8; SCREEN_WIDTH * SCREEN_HEIGHT * 4];
        fill_rect(&mut frame, SCREEN_WIDTH, SCREEN_HEIGHT, 2, 3, 4, 2, [255, 0, 0, 255]);
        let idx = |x: usize, y: usize| (y * SCREEN_WIDTH + x) * 4;
        // Inside the 4x2 rect at (2,3): painted red.
        assert_eq!(&frame[idx(2, 3)..idx(2, 3) + 4], &[255, 0, 0, 255]);
        assert_eq!(&frame[idx(5, 4)..idx(5, 4) + 4], &[255, 0, 0, 255]);
        // One row above / one column right of the rect: untouched sentinel.
        assert_eq!(&frame[idx(2, 2)..idx(2, 2) + 4], &[9, 9, 9, 9]);
        assert_eq!(&frame[idx(6, 3)..idx(6, 3) + 4], &[9, 9, 9, 9]);
    }

    #[test]
    fn fill_rect_clips_to_frame_bounds_without_panicking() {
        // A rect straddling the bottom-right edge must clip, not index OOB.
        let mut frame = vec![0u8; SCREEN_WIDTH * SCREEN_HEIGHT * 4];
        fill_rect(
            &mut frame,
            SCREEN_WIDTH,
            SCREEN_HEIGHT,
            SCREEN_WIDTH - 2,
            SCREEN_HEIGHT - 2,
            10,
            10,
            [1, 2, 3, 4],
        );
        let last = ((SCREEN_HEIGHT - 1) * SCREEN_WIDTH + (SCREEN_WIDTH - 1)) * 4;
        assert_eq!(&frame[last..last + 4], &[1, 2, 3, 4]);
    }

    #[test]
    fn draw_overlay_text_paints_top_right_box_and_leaves_rest_untouched() {
        let mut frame = vec![0u8; SCREEN_WIDTH * SCREEN_HEIGHT * 4];
        let text_color = [80, 255, 80, 255];
        draw_overlay_text(&mut frame, SCREEN_WIDTH, SCREEN_HEIGHT, "FPS 60/50", text_color);
        // Background box corner near the top-right edge is the black box fill.
        let idx = (OVERLAY_MARGIN * SCREEN_WIDTH + (SCREEN_WIDTH - OVERLAY_MARGIN - 1)) * 4;
        assert_eq!(&frame[idx..idx + 4], &[0, 0, 0, 255]);
        // Top-left corner of the buffer is untouched by a top-right overlay.
        assert_eq!(&frame[0..4], &[0, 0, 0, 0]);
        // At least one glyph pixel was actually lit in the requested color.
        assert!(
            frame.chunks_exact(4).any(|p| p == text_color),
            "expected at least one lit glyph pixel in the overlay text color"
        );
    }

    /// The whole point of drawing the overlay *after* scaling (see the "FPS
    /// overlay" module comment): its on-screen size must not depend on the
    /// buffer/window size — a zoom x1 window and a maximized/fullscreen one
    /// get the exact same box in output pixels, not a scaled-up one.
    #[test]
    fn overlay_box_size_is_independent_of_the_buffer_size() {
        let native = text_box_size("FPS 60/50", SCREEN_WIDTH, SCREEN_HEIGHT).expect("fits");
        // A window several times larger than the native picture (e.g. zoom
        // x8 or a maximized 4K display).
        let large = text_box_size("FPS 60/50", 3840, 2160).expect("fits");
        assert_eq!(native, large);
    }

    #[test]
    fn font_scale_is_1x_so_the_overlay_stays_small_in_output_pixels() {
        // Regression guard for the "l'affichage des FPS est trop gros"
        // report: each glyph pixel must draw as a single output pixel, not a
        // multi-pixel block (drawing after scaling — see the module comment —
        // already keeps the *apparent* size constant across window sizes;
        // this keeps the *absolute* size small to begin with).
        assert_eq!(FONT_SCALE, 1);
    }
}

#[cfg(test)]
mod path_tests {
    use super::*;

    #[test]
    fn mtime_reports_none_for_a_missing_file_and_some_for_an_existing_one() {
        let path = std::env::temp_dir()
            .join(format!("prisme_mtime_test_{}.tmp", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert_eq!(mtime(&path), None);
        std::fs::write(&path, b"x").expect("write fixture");
        assert!(mtime(&path).is_some());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn default_capture_folders_sit_next_to_the_rom() {
        assert_eq!(
            sibling_dir(Path::new("/roms/game.sfc"), "Screenshots"),
            PathBuf::from("/roms/Screenshots")
        );
        assert_eq!(sibling_dir(Path::new("/roms/game.zip"), "SPC"), PathBuf::from("/roms/SPC"));
        // A bare file name has no parent directory: fall back to the CWD.
        assert_eq!(sibling_dir(Path::new("game.sfc"), "SPC"), PathBuf::from("SPC"));
    }

    #[test]
    fn digit_keys_map_straight_to_their_slot_number() {
        assert_eq!(digit_to_slot(KeyCode::Digit0), Some(0));
        assert_eq!(digit_to_slot(KeyCode::Digit9), Some(9));
        for (n, code) in [
            KeyCode::Digit0,
            KeyCode::Digit1,
            KeyCode::Digit2,
            KeyCode::Digit3,
            KeyCode::Digit4,
            KeyCode::Digit5,
            KeyCode::Digit6,
            KeyCode::Digit7,
            KeyCode::Digit8,
            KeyCode::Digit9,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(digit_to_slot(code), Some(n as u8));
        }
        // Non-digit keys (including numpad digits, deliberately not aliased)
        // and existing hotkeys must not be misread as a slot number.
        assert_eq!(digit_to_slot(KeyCode::Numpad5), None);
        assert_eq!(digit_to_slot(KeyCode::F5), None);
        assert_eq!(digit_to_slot(KeyCode::KeyA), None);
    }

    #[test]
    fn the_home_screen_titles_the_window_with_the_product_only() {
        let home = home_window_title();
        assert_eq!(home, format!("{APP_NAME} {VERSION}"));
        // A cartridge title is appended after a separator, so the two forms
        // can never be confused.
        let game = window_title("  MARIO_ALLSTARS+WORLD  ");
        assert_eq!(game, format!("{home} - MARIO_ALLSTARS+WORLD"));
        assert_ne!(home, game);
    }

    #[test]
    fn the_home_screen_is_paced_at_60_hz() {
        // No cartridge means no field rate to follow; the UI refresh rate is
        // a plain 60 Hz, within a microsecond of 1/60 s.
        let hz = 1.0 / HOME_FRAME_DURATION.as_secs_f64();
        assert!((hz - 60.0).abs() < 1e-3, "{hz}");
    }

    /// Every key `handle_key` consumes as an application hotkey on the game
    /// screen must be refused by the remapping capture: it acts and `return`s
    /// before the pad mapping is reached, so a button bound to it would never
    /// press anything. This list mirrors that dispatch — a hotkey added there
    /// without being added to `input::RESERVED_KEYS` fails here.
    #[test]
    fn every_game_screen_hotkey_is_refused_as_a_pad_binding() {
        let hotkeys = [
            KeyCode::Tab,
            KeyCode::KeyM,
            KeyCode::Equal,
            KeyCode::NumpadAdd,
            KeyCode::Minus,
            KeyCode::NumpadSubtract,
            KeyCode::KeyP,
            KeyCode::KeyN,
            KeyCode::KeyO,
            KeyCode::Comma,
            KeyCode::KeyC,
            KeyCode::KeyF,
            KeyCode::KeyV,
            KeyCode::KeyR,
            KeyCode::BracketLeft,
            KeyCode::BracketRight,
            KeyCode::F1,
            KeyCode::F2,
            KeyCode::F3,
            KeyCode::F4,
            KeyCode::F5,
            KeyCode::F6,
            KeyCode::F7,
            KeyCode::F8,
            KeyCode::F9,
            KeyCode::F10,
            KeyCode::F11,
            KeyCode::F12,
        ];
        for code in hotkeys {
            assert!(input::reserved_for(code).is_some(), "{code:?} is a hotkey but bindable");
        }
        // The slot digits go through `digit_to_slot`, so the two lists are
        // cross-checked instead of restated.
        for &(code, _) in input::RESERVED_KEYS {
            if let Some(slot) = digit_to_slot(code) {
                assert!(slot < crate::state::SLOT_COUNT);
            }
        }
        for code in [KeyCode::Digit0, KeyCode::Digit5, KeyCode::Digit9] {
            assert!(digit_to_slot(code).is_some());
            assert!(input::reserved_for(code).is_some(), "{code:?}");
        }
        // Escape is handled by `handle_escape` and is what cancels a capture,
        // so it can never be assigned either — and must not be listed as a
        // refusal, which would make the capture answer with a message instead
        // of backing out.
        assert_eq!(input::reserved_for(KeyCode::Escape), None);
    }

    #[test]
    fn a_binding_is_only_commented_when_it_took_one_from_another_button() {
        assert_eq!(bind_notice(input::BindResult::Bound, "Espace"), None);
        assert_eq!(bind_notice(input::BindResult::Unchanged, "Espace"), None);
        let notice = bind_notice(input::BindResult::Swapped("B"), "Z").expect("a swap is explained");
        assert!(notice.contains('Z'), "{notice}");
        assert!(notice.contains("échangés"), "{notice}");
        // A button that had nothing to receive back is told apart from a swap:
        // it went back to its default instead of taking the other's binding.
        let notice =
            bind_notice(input::BindResult::Reverted("X"), "Z").expect("a takeover is explained");
        assert!(notice.contains("par défaut"), "{notice}");
        assert!(notice.contains("X"), "{notice}");
    }

    /// The folder a player stops using has to stay known, or the saves it holds
    /// become invisible the moment the setting changes.
    #[test]
    fn changing_the_save_folder_remembers_the_one_it_replaces() {
        let a = PathBuf::from("/a");
        let b = PathBuf::from("/b");
        let (mut dir, mut previous) = (None, None);

        // First folder: nothing was left behind yet.
        move_save_dir(&mut dir, &mut previous, Some(a.clone()));
        assert_eq!((dir.clone(), previous.clone()), (Some(a.clone()), None));

        // Re-picking the same folder changes nothing.
        move_save_dir(&mut dir, &mut previous, Some(a.clone()));
        assert_eq!((dir.clone(), previous.clone()), (Some(a.clone()), None));

        // Moving to another one: the first is the fallback.
        move_save_dir(&mut dir, &mut previous, Some(b.clone()));
        assert_eq!((dir.clone(), previous.clone()), (Some(b.clone()), Some(a.clone())));

        // "Par défaut": beside the ROM again, and the folder just left is the
        // fallback — this is what stops the recent saves from disappearing.
        move_save_dir(&mut dir, &mut previous, None);
        assert_eq!((dir.clone(), previous.clone()), (None, Some(b.clone())));

        // Clearing twice keeps the last real folder rather than losing it.
        move_save_dir(&mut dir, &mut previous, None);
        assert_eq!((dir.clone(), previous.clone()), (None, Some(b.clone())));

        // Picking the fallback back means there is no fallback any more.
        move_save_dir(&mut dir, &mut previous, Some(b.clone()));
        assert_eq!((dir, previous), (Some(b), None));
    }

    /// Changing the save folder must always say when it takes effect, and name
    /// the folder left behind when there is one — its saves are still read, and
    /// silence there is what would look like lost progress.
    #[test]
    fn changing_the_save_folder_says_when_it_applies_and_what_was_left_behind() {
        let running = save_dir_notice(true, None);
        assert!(running.contains("prochain chargement"), "{running}");
        assert!(running.contains("partie en cours"), "{running}");

        let idle = save_dir_notice(false, None);
        assert!(idle.contains("prochain chargement"), "{idle}");
        assert!(!idle.contains("partie en cours"), "{idle}");

        let left = save_dir_notice(false, Some(Path::new("/old-saves")));
        assert!(left.contains("/old-saves"), "{left}");
        assert!(left.contains("toujours relues"), "{left}");
        assert!(left.contains("rien n'a été déplacé"), "{left}");
    }

    #[test]
    fn slot_cycle_wraps_after_the_last_slot() {
        // Mirrors `App::next_slot` without needing a console.
        let next = |s: u8| (s + 1) % crate::state::SLOT_COUNT;
        assert_eq!(next(0), 1);
        assert_eq!(next(8), 9);
        assert_eq!(next(9), 0);
    }
}

#[cfg(test)]
mod fps_counter_tests {
    use super::*;

    #[test]
    fn reports_zero_before_two_samples() {
        let mut c = FpsCounter::new();
        assert_eq!(c.fps(), 0.0);
        c.tick();
        assert_eq!(c.fps(), 0.0);
    }

    #[test]
    fn averages_synthetic_60fps_samples() {
        let mut c = FpsCounter::new();
        // Synthesize 10 samples 16.667ms apart (60 Hz) without any real
        // sleeping, so the test is deterministic and instant.
        let base = Instant::now();
        for i in 0..10u32 {
            c.samples.push_back(base + Duration::from_micros(16_667) * i);
        }
        let fps = c.fps();
        assert!((fps - 60.0).abs() < 1.0, "expected ~60 fps, got {fps}");
    }

    #[test]
    fn drops_samples_older_than_the_window() {
        let mut c = FpsCounter::new();
        let now = Instant::now();
        // A stale sample from well before FPS_WINDOW must be evicted by the
        // next tick(), which stamps "now" internally.
        c.samples.push_back(now - FPS_WINDOW * 4);
        c.tick();
        assert_eq!(c.samples.len(), 1, "stale sample should have been evicted");
    }
}
