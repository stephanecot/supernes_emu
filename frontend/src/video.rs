//! Windowed video output: winit window + pixels framebuffer, 256x224
//! BGR555 -> RGBA upload, 50.007/60.0988 fps pacing at absolute deadlines.
//!
//! Frame cadence is paced by wall-clock deadlines rather than vsync (which is
//! disabled): each `about_to_wait` computes the next presentation deadline,
//! sleeps for the bulk of the remaining time, then spin-waits the last
//! `SPIN_SLACK` for sub-millisecond accuracy (OS `sleep()` granularity is
//! coarse — a few ms on some hosts — so a plain sleep-to-deadline would
//! frequently overshoot).

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
use crate::input;
#[cfg(target_os = "macos")]
use crate::menu::{self, AppMenu};
use crate::picker;
use crate::prefs::Prefs;
use crate::save;
use crate::{APP_NAME, VERSION};

/// Integer upscale factor for the 256x224 native framebuffer.
pub const WINDOW_SCALE: u32 = 3;

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

/// Run the windowed frontend (M5): winit event loop + pixels present, paced
/// to the cartridge region's native field rate (PAL 50.007 Hz / NTSC
/// 60.0988 Hz, from `Region::frames_per_second`) via an absolute deadline.
///
/// `save_path`/`sram_baseline` come from `save::load_sram` (already applied
/// to `cart.sram` by the caller); battery SRAM is written back to
/// `save_path` once the event loop exits, however it exits (window close,
/// Esc, or a fatal window/surface creation error), since `app` is still
/// owned here after `run_app` returns. The `O` hotkey can swap in a
/// different ROM mid-session (see `App::open_rom_dialog`); `App` then owns
/// its own current `save_path`/`sram_baseline` so the exit-time save below
/// always targets whichever game is loaded when the window closes.
///
/// `prefs` carries the persisted user options (loaded by `main`); it is stored
/// on `App`, written back after every option change and once more on exit.
pub fn run(
    rom_path: PathBuf,
    cart: Cartridge,
    save_path: PathBuf,
    sram_baseline: Vec<u8>,
    prefs: Prefs,
) -> Result<(), String> {
    let title = window_title(&cart.title);
    let region = cart.region;
    let snes = Snes::new(cart);
    let frame_duration = Duration::from_secs_f64(1.0 / region.frames_per_second());

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
        current_rom_path: rom_path,
        save_path,
        sram_baseline,
        frame_duration,
        next_deadline: Instant::now() + frame_duration,
        window: None,
        pixels: None,
        pad: JoypadState::default(),
        paused: false,
        frame_advance: false,
        fast_forward: false,
        audio,
        audio_scratch: Vec::new(),
        fps_counter: FpsCounter::new(),
        status: None,
        prefs,
        #[cfg(target_os = "macos")]
        menu: None,
    };
    // Apply the restored mute/volume before the first sample is produced.
    app.apply_audio_gain();
    // Instant resume: pick the session state up before the first frame runs.
    app.try_resume();
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

struct App {
    title: String,
    snes: Snes,
    /// Path of the currently loaded ROM (updated by `switch_rom`); used by
    /// `Emulation > Reset` to reload the same cart.
    current_rom_path: PathBuf,
    /// Sidecar `.srm` path for the currently loaded cart (updated by the `O`
    /// hotkey when the ROM is switched).
    save_path: PathBuf,
    /// Post-load SRAM snapshot for the currently loaded cart; see
    /// `save::load_sram`/`save::save_if_dirty`.
    sram_baseline: Vec<u8>,
    frame_duration: Duration,
    /// Absolute wall-clock time the next emulated frame should be presented at.
    next_deadline: Instant,
    window: Option<Arc<Window>>,
    pixels: Option<Pixels<'static>>,
    /// Player-1 pad state accumulated from keyboard events; player 2 is
    /// unconnected (frontend has no multi-controller UI yet).
    pad: JoypadState,
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
        let size = LogicalSize::new(
            SCREEN_WIDTH as u32 * WINDOW_SCALE,
            SCREEN_HEIGHT as u32 * WINDOW_SCALE,
        );
        let attrs = Window::default_attributes().with_title(self.title.clone()).with_inner_size(size);
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
        let mut pixels =
            match Pixels::new(SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32, surface_texture) {
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
        self.window = Some(window);
        self.pixels = Some(pixels);
        self.next_deadline = Instant::now() + self.frame_duration;

        // NSApp only exists once winit has resumed at least once; installing
        // the menu bar any earlier is a silent no-op on macOS (see `menu`
        // module docs).
        // Checkable items are created with their restored state (FPS overlay,
        // mute, confirm-on-quit, fast-forward factor), since AppKit owns the
        // checkmark state.
        #[cfg(target_os = "macos")]
        {
            self.menu = Some(menu::install(&self.prefs));
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
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if size.width > 0 && size.height > 0 {
                    if let Some(pixels) = &mut self.pixels {
                        // pixels' scaling renderer keeps the 256x224 buffer at
                        // the nearest integer scale, letterboxing any remainder.
                        let _ = pixels.resize_surface(size.width, size.height);
                    }
                }
            }
            WindowEvent::KeyboardInput {
                event: KeyEvent { physical_key: PhysicalKey::Code(code), state, repeat, .. },
                ..
            } => self.handle_key(event_loop, code, state, repeat),
            // Key releases are not delivered once the window loses focus, so
            // anything held (pad buttons, the turbo key) would stay stuck.
            WindowEvent::Focused(false) => {
                self.pad = JoypadState::default();
                self.set_fast_forward(false);
            }
            WindowEvent::RedrawRequested => {
                if let (Some(window), Some(pixels)) = (&self.window, &self.pixels) {
                    window.pre_present_notify();
                    if let Err(e) = pixels.render() {
                        eprintln!("error: pixels render: {e}");
                    }
                }
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
        pace(&mut self.next_deadline, self.frame_duration);
        if self.status.as_ref().is_some_and(|(_, until)| Instant::now() >= *until) {
            self.status = None;
        }

        if !self.paused || self.frame_advance {
            // Fast-forward runs `factor` emulated frames per presented frame;
            // only the last one is uploaded, so the extra frames cost no
            // presentation work. `frame_advance` always steps exactly one.
            let factor = if self.fast_forward && !self.paused {
                self.prefs.fast_forward_factor.max(1) as u32
            } else {
                1
            };
            let mut frames_run = 0u32;
            for i in 0..factor {
                self.snes.run_frame([self.pad, JoypadState::default()]);
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
            if let Some(audio) = &mut self.audio {
                self.audio_scratch.clear();
                self.snes.drain_audio(&mut self.audio_scratch);
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
        if let Some(pixels) = &mut self.pixels {
            let frame = pixels.frame_mut();
            self.snes.framebuffer.to_rgba(frame);
            // Overlays are drawn only on the windowed present path, after the
            // core's own (pure) RGBA conversion — they never touch
            // `snes.framebuffer` itself, so headless `--dump-frame` output and
            // the F12 screenshot (which both read the core) are unaffected.
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
                let text = format!("FPS{:.0}/{:.0}", measured, target);
                draw_overlay_text(frame, &text, color);
            }
            if let Some((text, _)) = &self.status {
                draw_status_text(frame, text, STATUS_COLOR);
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
                    self.request_quit(event_loop);
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
                KeyCode::F5 => {
                    self.save_state();
                    return;
                }
                KeyCode::F7 => {
                    self.next_slot();
                    return;
                }
                KeyCode::F9 => {
                    self.load_state();
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
                _ => {}
            }
        }
        if let Some(name) = input::keycode_to_button(code) {
            let _ = input::set_button(&mut self.pad, name, pressed);
        }
    }

    /// Flush everything that must outlive the process: the automatic session
    /// state (instant resume), the battery SRAM of the currently loaded cart,
    /// then the preferences file. Idempotent — the SRAM baseline is re-synced
    /// after the write, so the second call (exit hook, then `run`) writes
    /// nothing new; rewriting the resume state is harmless since no emulation
    /// happened in between.
    fn persist_all(&mut self) {
        self.write_resume_state();
        save::save_if_dirty(&self.snes.bus.cart, &self.save_path, &self.sram_baseline);
        self.sram_baseline = self.snes.bus.cart.sram.as_bytes().to_vec();
        self.prefs.save();
    }

    /// Esc / `Fichier > Quitter` / app-menu Quit (Cmd+Q): confirm first when
    /// `prefs.confirm_on_quit` is set, then leave through `event_loop.exit()`
    /// so the `exiting` hook's SRAM flush runs.
    fn request_quit(&mut self, event_loop: &ActiveEventLoop) {
        if !self.prefs.confirm_on_quit || self.confirm_quit_dialog() {
            event_loop.exit();
        }
    }

    /// Modal quit confirmation. Emulation is paused for the dialog's whole
    /// lifetime: `MessageDialog::show` blocks this thread, and `paused` also
    /// keeps `about_to_wait` from advancing the console should the platform
    /// pump our loop from inside the modal session. Answering "Non" restores
    /// the previous pause state and returns to the game.
    ///
    /// Key releases are delivered to the dialog, not to the window, so the pad
    /// and the held turbo key are cleared on the way out to avoid stuck input.
    fn confirm_quit_dialog(&mut self) -> bool {
        let was_paused = self.paused;
        self.paused = true;
        let answer = rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Warning)
            .set_title(format!("Quitter {APP_NAME} ?"))
            .set_description("La sauvegarde de la cartouche sera écrite avant de quitter.")
            .set_buttons(rfd::MessageButtons::OkCancelCustom(
                "Oui".to_owned(),
                "Non".to_owned(),
            ))
            .show();
        // The custom buttons come back as `Custom(label)`; `Yes`/`Ok` are
        // accepted too so a backend that ignores custom labels still works.
        let quit = match &answer {
            rfd::MessageDialogResult::Custom(label) => label == "Oui",
            rfd::MessageDialogResult::Yes | rfd::MessageDialogResult::Ok => true,
            _ => false,
        };
        self.paused = was_paused;
        self.pad = JoypadState::default();
        self.set_fast_forward(false);
        // The dialog consumed an arbitrary amount of wall time; restart pacing
        // from now instead of catching up frames that were never emulated.
        self.next_deadline = Instant::now() + self.frame_duration;
        quit
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

    /// `M` hotkey / `Audio > Muet` (Cmd+M).
    fn set_mute(&mut self, on: bool) {
        self.prefs.mute = on;
        self.prefs.save();
        self.apply_audio_gain();
        #[cfg(target_os = "macos")]
        if let Some(menu) = &self.menu {
            menu.mute.set_checked(on);
        }
    }

    /// `+`/`-` hotkeys / `Audio > Volume +` / `Volume −`: one 10-point step,
    /// clamped to 0..=100 and persisted.
    fn adjust_volume(&mut self, up: bool) {
        let volume = audio::step_volume(self.prefs.volume, up);
        if volume == self.prefs.volume {
            return; // already at 0 % or 100 %
        }
        self.prefs.volume = volume;
        self.prefs.save();
        self.apply_audio_gain();
        #[cfg(target_os = "macos")]
        if let Some(menu) = &self.menu {
            menu.set_volume_label(volume);
        }
        eprintln!("audio: volume {volume} %");
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
        // Pacing restarts from now: an accelerated pass may have ended well
        // past the deadline it was aiming at, and the frames it ran ahead of
        // must not be counted as a backlog to catch up.
        self.next_deadline = Instant::now() + self.frame_duration;
    }

    /// `Émulation > Accéléré > ×N`: how many frames one Tab-held presentation
    /// runs. Clamped to the range the preferences file documents.
    #[cfg(target_os = "macos")]
    fn set_fast_forward_factor(&mut self, factor: u8) {
        self.prefs.fast_forward_factor = factor.clamp(2, 8);
        self.prefs.save();
        if let Some(menu) = &self.menu {
            menu.sync_fast_forward(self.prefs.fast_forward_factor);
        }
    }

    /// `Fichier > Demander confirmation avant de quitter`.
    #[cfg(target_os = "macos")]
    fn set_confirm_on_quit(&mut self, on: bool) {
        self.prefs.confirm_on_quit = on;
        self.prefs.save();
        if let Some(menu) = &self.menu {
            menu.confirm_quit.set_checked(on);
        }
    }

    /// `O` hotkey: open the native ROM picker and, if a file was chosen,
    /// tear down the running game (saving its SRAM first) and start the
    /// picked one. Cancelling the dialog or a load error leaves the current
    /// game running untouched.
    fn open_rom_dialog(&mut self) {
        let Some(path) = picker::pick_rom() else {
            return; // Cancelled: keep playing the current game.
        };
        if let Err(e) = self.switch_rom(&path, true) {
            eprintln!("error: could not load {}: {e}", path.display());
        }
    }

    /// F5 / `Émulation > Sauvegarder l'état` (Cmd+S): snapshot the whole
    /// console (`Snes::save_state`) into the current slot's sidecar next to the
    /// loaded ROM. Never fails the run: an I/O error is reported and emulation
    /// continues.
    fn save_state(&mut self) {
        let slot = self.prefs.save_slot;
        let path = crate::state::state_path(&self.current_rom_path, slot);
        let bytes = self.snes.save_state();
        match std::fs::write(&path, &bytes) {
            Ok(()) => {
                eprintln!("state: saved {} ({} bytes)", path.display(), bytes.len());
                self.set_status(format!("SLOT {slot} SAUVE"));
            }
            Err(e) => {
                eprintln!("state: could not write {}: {e}", path.display());
                self.set_status(format!("SLOT {slot} ERREUR"));
            }
        }
    }

    /// F9 / `Émulation > Charger l'état` (Cmd+L): restore the console from the
    /// current slot's sidecar. The blob carries no ROM image;
    /// `Snes::load_state` reattaches the live ROM and rejects a state saved
    /// from a different game. Any error (missing file, wrong ROM, corrupt
    /// blob) is reported and the running game is left untouched.
    fn load_state(&mut self) {
        let slot = self.prefs.save_slot;
        let path = crate::state::state_path(&self.current_rom_path, slot);
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
        match self.snes.load_state(&bytes) {
            Ok(()) => {
                eprintln!("state: loaded {}", path.display());
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
        #[cfg(target_os = "macos")]
        if let Some(menu) = &self.menu {
            menu.sync_slot(slot);
        }
        self.set_status(format!("SLOT {slot}"));
    }

    /// `Émulation > Reprise instantanée`: whether `<rom>.resume` is restored at
    /// launch. The session state is written on exit either way, so turning the
    /// option back on resumes from the last session.
    #[cfg(target_os = "macos")]
    fn set_resume_on_launch(&mut self, on: bool) {
        self.prefs.resume_on_launch = on;
        self.prefs.save();
        if let Some(menu) = &self.menu {
            menu.resume_on_launch.set_checked(on);
        }
    }

    /// Write the automatic session state to `<rom>.resume`, a file outside the
    /// manual `.state`/`.stateN` series so it can never overwrite a slot. Runs
    /// on every exit path (see `persist_all`) and before a ROM switch.
    fn write_resume_state(&mut self) {
        let path = crate::state::resume_path(&self.current_rom_path);
        let bytes = self.snes.save_state();
        match std::fs::write(&path, &bytes) {
            Ok(()) => eprintln!("resume: wrote {} ({} bytes)", path.display(), bytes.len()),
            Err(e) => eprintln!("resume: could not write {}: {e}", path.display()),
        }
    }

    /// Restore `<rom>.resume` for the currently loaded game if the option is
    /// on and the file exists. A state from another game, a truncated file or
    /// an incompatible format is reported and ignored — the game then simply
    /// starts from power-on, which is why `load_state`'s error is not fatal
    /// here (it leaves the console untouched).
    fn try_resume(&mut self) {
        if !self.prefs.resume_on_launch {
            return;
        }
        let path = crate::state::resume_path(&self.current_rom_path);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                eprintln!("resume: could not read {}: {e}", path.display());
                return;
            }
        };
        match self.snes.load_state(&bytes) {
            Ok(()) => {
                eprintln!("resume: restored {}", path.display());
                self.set_status("REPRISE");
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
        let dir = self
            .prefs
            .screenshot_dir
            .clone()
            .unwrap_or_else(|| sibling_dir(&self.current_rom_path, "Screenshots"));
        let stem = format!(
            "{}_{}",
            crate::sanitize_file_stem(&self.snes.bus.cart.title),
            crate::now_local().file_stamp()
        );
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("screenshot: could not create {}: {e}", dir.display());
            self.set_status("CAPTURE IMPOSSIBLE");
            return;
        }
        let path = crate::unique_path(&dir, &stem, "png");
        match crate::write_frame_png(&self.snes, &path) {
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
        let dir = sibling_dir(&self.current_rom_path, "SPC");
        let title = self.snes.bus.cart.title.trim().to_string();
        let stem =
            format!("{}_{}", crate::sanitize_file_stem(&title), crate::now_local().file_stamp());
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("spc: could not create {}: {e}", dir.display());
            self.set_status("EXPORT SPC ERREUR");
            return;
        }
        let path = crate::unique_path(&dir, &stem, "spc");
        match crate::spc::write(&self.snes, &path, &title) {
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

    /// `F` hotkey / `View > Show FPS` (Cmd+F): toggles the on-screen FPS
    /// overlay (see `draw_overlay_text`). Also keeps the macOS menu's
    /// checkbox in sync when triggered from the keyboard, since AppKit only
    /// updates the checkmark itself when the *menu* item is clicked (see
    /// `poll_menu_events`, which does the reverse sync).
    fn toggle_show_fps(&mut self) {
        self.set_show_fps(!self.prefs.show_fps);
    }

    /// Applies and persists the FPS-overlay setting; restored on the next
    /// launch by `Prefs::load`.
    fn set_show_fps(&mut self, on: bool) {
        self.prefs.show_fps = on;
        self.prefs.save();
        #[cfg(target_os = "macos")]
        if let Some(menu) = &self.menu {
            menu.show_fps.set_checked(on);
        }
    }

    /// Replace `self.snes` with a freshly constructed console for the ROM at
    /// `path`. Persists the outgoing cart's SRAM (via its own `save_path`)
    /// before replacing it, then loads the new cart's `.srm` sidecar the
    /// same way startup does, resets pad/pause/frame-advance state, and
    /// retargets pacing at the new cart's region field rate (a game switch
    /// can cross the PAL/NTSC line).
    /// `resume` asks for the new game's session state to be restored once it is
    /// loaded; `reset` passes false, since restoring the state it just wrote
    /// would undo the reset.
    fn switch_rom(&mut self, path: &Path, resume: bool) -> Result<(), String> {
        // Leaving a game is an exit for that game: its session state and
        // battery SRAM are written before the console is replaced.
        self.write_resume_state();
        save::save_if_dirty(&self.snes.bus.cart, &self.save_path, &self.sram_baseline);

        let bytes = crate::load_rom_bytes(path)?;
        let mut cart = Cartridge::from_bytes(bytes)?;
        let save_path = save::default_save_path(path);
        let sram_baseline = save::load_sram(&mut cart, &save_path);

        self.title = window_title(&cart.title);
        self.frame_duration = Duration::from_secs_f64(1.0 / cart.region.frames_per_second());
        self.snes = Snes::new(cart);
        self.save_path = save_path;
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
        if resume {
            self.try_resume();
        }
        Ok(())
    }

    /// `Emulation > Reset` menu item (Cmd+R): reload the currently running
    /// ROM in place. Reuses `switch_rom` with the same path rather than
    /// rebuilding `Snes` from `self.snes.bus.cart` directly (`Cartridge`
    /// isn't `Clone`): `switch_rom` first flushes the live, possibly-dirty
    /// SRAM to `save_path`, then reloads that same file into the fresh
    /// cart, so the net effect is a power-on reset of CPU/PPU/APU state
    /// that preserves the current battery save — matching the SNES's
    /// physical reset button, which restarts execution but never erases
    /// cartridge SRAM.
    #[cfg(target_os = "macos")]
    fn reset(&mut self) {
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
    /// synchronously in `window_event`. `About` is a muda `PredefinedMenuItem`
    /// that AppKit runs itself (standard about panel) and never appears on this
    /// channel. `Quit` is a *custom* item (not `PredefinedMenuItem::quit`) so
    /// it routes here and we exit the winit loop the same way `Esc`/window-close
    /// do, which triggers the exit-time battery-SRAM flush in `run` — AppKit's
    /// `terminate:` would kill the process before that save could run.
    #[cfg(target_os = "macos")]
    fn poll_menu_events(&mut self, event_loop: &ActiveEventLoop) {
        while let Ok(event) = muda::MenuEvent::receiver().try_recv() {
            let Some(menu) = &self.menu else { continue };
            // Resolved up front: the `menu` borrow must not still be live when
            // a branch below calls back into `&mut self`.
            let ff_click: Option<u8> = menu
                .ff_items
                .iter()
                .zip(menu::FF_FACTORS)
                .find(|(item, _)| event.id == *item.id())
                .map(|(_, &(factor, _))| factor);
            let slot_click: Option<u8> = menu
                .slot_items
                .iter()
                .position(|item| event.id == *item.id())
                .map(|i| i as u8);
            // Quit shares one id across the app-menu and File-menu items; it
            // goes through the same confirmation as Esc.
            if event.id == menu.quit.id() || event.id == menu.quit_file.id() {
                self.request_quit(event_loop);
            } else if event.id == menu.mute.id() {
                // AppKit already flipped the CheckMenuItem before sending the
                // click (muda macOS impl); mirror it rather than toggling again.
                let checked = menu.mute.is_checked();
                self.set_mute(checked);
            } else if event.id == menu.volume_up.id() {
                self.adjust_volume(true);
            } else if event.id == menu.volume_down.id() {
                self.adjust_volume(false);
            } else if event.id == menu.confirm_quit.id() {
                let checked = menu.confirm_quit.is_checked();
                self.set_confirm_on_quit(checked);
            } else if let Some(factor) = ff_click {
                // Radio group: `set_fast_forward_factor` re-derives every
                // checkmark, since AppKit only flipped the clicked one (and
                // would have *un*checked an already-selected factor).
                self.set_fast_forward_factor(factor);
            } else if event.id == menu.open_rom.id() {
                self.open_rom_dialog();
            } else if event.id == menu.pause_resume.id() {
                self.paused = !self.paused;
            } else if event.id == menu.reset.id() {
                self.reset();
            } else if event.id == menu.save_state.id() {
                self.save_state();
            } else if event.id == menu.load_state.id() {
                self.load_state();
            } else if let Some(slot) = slot_click {
                // Radio group, like the fast-forward factors: `set_slot`
                // re-derives every checkmark.
                self.set_slot(slot);
            } else if event.id == menu.next_slot.id() {
                self.next_slot();
            } else if event.id == menu.resume_on_launch.id() {
                let checked = menu.resume_on_launch.is_checked();
                self.set_resume_on_launch(checked);
            } else if event.id == menu.screenshot.id() {
                self.take_screenshot();
            } else if event.id == menu.export_spc.id() {
                self.export_spc();
            } else if event.id == menu.show_fps.id() {
                // AppKit already flipped the CheckMenuItem's own checked
                // state before sending this click event (muda macOS impl);
                // mirror it rather than toggling again.
                let checked = menu.show_fps.is_checked();
                self.set_show_fps(checked);
            }
        }
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
// Deliberately not a font asset: the overlay only ever needs digits, F/P/S
// and '/', so a hand-encoded 3x5 glyph table avoids pulling in a font
// dependency for six on-screen characters. Drawn directly into the `pixels`
// RGBA8 frame buffer from `about_to_wait`, after `FrameBuffer::to_rgba` —
// `snes.framebuffer` (the core's own pixel data) is never touched, so this
// has no effect on headless `--dump-frame`/`--dump-frame-every` output,
// which reads straight from the core.

/// Glyph cell size before scaling: 3 columns x 5 rows.
const GLYPH_W: usize = 3;
const GLYPH_H: usize = 5;
/// Each on-screen glyph pixel is drawn as a `FONT_SCALE`x`FONT_SCALE` block;
/// at 1x a 3px-wide digit would be nearly unreadable on the native 256x224
/// buffer.
const FONT_SCALE: usize = 2;
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

/// Blits a solid `w`x`h` RGBA rectangle at `(x,y)` into an RGBA8
/// `SCREEN_WIDTH`x`SCREEN_HEIGHT` frame buffer, clipped to its bounds.
fn fill_rect(frame: &mut [u8], x: usize, y: usize, w: usize, h: usize, color: [u8; 4]) {
    for row in y..(y + h).min(SCREEN_HEIGHT) {
        let row_base = row * SCREEN_WIDTH * 4;
        for col in x..(x + w).min(SCREEN_WIDTH) {
            let i = row_base + col * 4;
            frame[i..i + 4].copy_from_slice(&color);
        }
    }
}

/// Paints `text` into the top-right corner of an RGBA8
/// `SCREEN_WIDTH`x`SCREEN_HEIGHT` frame buffer over a solid black background
/// box, so the overlay stays legible against any game content behind it.
fn draw_overlay_text(frame: &mut [u8], text: &str, color: [u8; 4]) {
    let Some((box_w, _)) = text_box_size(text) else { return };
    draw_text_box(frame, SCREEN_WIDTH - OVERLAY_MARGIN - box_w, OVERLAY_MARGIN, text, color);
}

/// Paints `text` in the bottom-left corner: the transient status messages
/// (screenshot taken, slot saved/loaded, SPC exported) go there so they never
/// collide with the FPS readout in the opposite corner.
fn draw_status_text(frame: &mut [u8], text: &str, color: [u8; 4]) {
    let Some((_, box_h)) = text_box_size(text) else { return };
    draw_text_box(frame, OVERLAY_MARGIN, SCREEN_HEIGHT - OVERLAY_MARGIN - box_h, text, color);
}

/// Background-box size for `text`, or `None` when it cannot fit on screen (the
/// caller then skips drawing rather than doing out-of-bounds math).
fn text_box_size(text: &str) -> Option<(usize, usize)> {
    let box_w = text.chars().count() * CHAR_ADVANCE + OVERLAY_PAD * 2;
    let box_h = GLYPH_H * FONT_SCALE + OVERLAY_PAD * 2;
    if box_w > SCREEN_WIDTH || box_h > SCREEN_HEIGHT {
        return None;
    }
    Some((box_w, box_h))
}

/// Blits `text` at `(x0, y0)` over a solid black background box.
fn draw_text_box(frame: &mut [u8], x0: usize, y0: usize, text: &str, color: [u8; 4]) {
    let Some((box_w, box_h)) = text_box_size(text) else { return };

    fill_rect(frame, x0, y0, box_w, box_h, [0, 0, 0, 255]);

    let mut cx = x0 + OVERLAY_PAD;
    let cy = y0 + OVERLAY_PAD;
    for ch in text.chars() {
        let rows = glyph(ch);
        for (row, bits) in rows.iter().enumerate() {
            for col in 0..GLYPH_W {
                if bits & (1 << (GLYPH_W - 1 - col)) != 0 {
                    let px = cx + col * FONT_SCALE;
                    let py = cy + row * FONT_SCALE;
                    fill_rect(frame, px, py, FONT_SCALE, FONT_SCALE, color);
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
        // The messages `set_status` can produce, plus the FPS readout.
        let messages = [
            "FPS60/50",
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
        ];
        for msg in messages {
            for c in msg.chars().filter(|c| *c != ' ') {
                assert_ne!(glyph(c), [0; GLYPH_H], "no glyph for {c:?} in {msg:?}");
            }
            assert!(text_box_size(msg).is_some(), "{msg:?} does not fit on screen");
        }
        // Lowercase folds to uppercase rather than rendering blank.
        assert_eq!(glyph('a'), glyph('A'));
        assert_eq!(glyph(' '), [0; GLYPH_H]);
    }

    #[test]
    fn status_text_is_drawn_bottom_left_and_fps_top_right() {
        let mut frame = vec![0u8; SCREEN_WIDTH * SCREEN_HEIGHT * 4];
        draw_status_text(&mut frame, "SLOT 3 SAUVE", STATUS_COLOR);
        let (_, box_h) = text_box_size("SLOT 3 SAUVE").expect("fits");
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
    fn text_too_wide_for_the_screen_is_skipped_instead_of_drawn() {
        assert_eq!(text_box_size(&"W".repeat(64)), None);
        let mut frame = vec![7u8; SCREEN_WIDTH * SCREEN_HEIGHT * 4];
        draw_status_text(&mut frame, &"W".repeat(64), STATUS_COLOR);
        assert!(frame.iter().all(|&b| b == 7), "nothing should have been drawn");
    }

    #[test]
    fn fill_rect_paints_only_the_target_region() {
        let mut frame = vec![9u8; SCREEN_WIDTH * SCREEN_HEIGHT * 4];
        fill_rect(&mut frame, 2, 3, 4, 2, [255, 0, 0, 255]);
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
        fill_rect(&mut frame, SCREEN_WIDTH - 2, SCREEN_HEIGHT - 2, 10, 10, [1, 2, 3, 4]);
        let last = ((SCREEN_HEIGHT - 1) * SCREEN_WIDTH + (SCREEN_WIDTH - 1)) * 4;
        assert_eq!(&frame[last..last + 4], &[1, 2, 3, 4]);
    }

    #[test]
    fn draw_overlay_text_paints_top_right_box_and_leaves_rest_untouched() {
        let mut frame = vec![0u8; SCREEN_WIDTH * SCREEN_HEIGHT * 4];
        let text_color = [80, 255, 80, 255];
        draw_overlay_text(&mut frame, "FPS60/50", text_color);
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
}

#[cfg(test)]
mod path_tests {
    use super::*;

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
