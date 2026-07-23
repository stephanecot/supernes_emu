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

use crate::audio::AudioOutput;
use crate::input;
#[cfg(target_os = "macos")]
use crate::menu::{self, AppMenu};
use crate::picker;
use crate::save;

/// Integer upscale factor for the 256x224 native framebuffer.
pub const WINDOW_SCALE: u32 = 3;

/// Wall-clock slack reserved for the spin-wait tail of each frame's pacing
/// deadline (see module docs).
const SPIN_SLACK: Duration = Duration::from_micros(1200);

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
pub fn run(
    rom_path: PathBuf,
    cart: Cartridge,
    save_path: PathBuf,
    sram_baseline: Vec<u8>,
) -> Result<(), String> {
    let title = format!("snes-frontend - {}", cart.title.trim());
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
        audio,
        audio_scratch: Vec::new(),
        fps_counter: FpsCounter::new(),
        show_fps: false,
        #[cfg(target_os = "macos")]
        menu: None,
    };
    let result = event_loop.run_app(&mut app).map_err(|e| format!("event loop: {e}"));
    save::save_if_dirty(&app.snes.bus.cart, &app.save_path, &app.sram_baseline);
    result
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
    /// cpal output; `None` when no audio device was available.
    audio: Option<AudioOutput>,
    /// Reused per-frame drain buffer to avoid re-allocating each frame.
    audio_scratch: Vec<(i16, i16)>,
    /// Rolling wall-clock rate of frames actually drawn (see `FpsCounter`).
    fps_counter: FpsCounter,
    /// `View > Show FPS` (macOS) / `F` hotkey: overlays the measured
    /// display FPS on the presented frame. Default off — see `App::resumed`
    /// and the menu's default unchecked `CheckMenuItem`.
    show_fps: bool,
    /// Menu bar handles, installed once in `resumed` (needs `NSApp` to
    /// exist first); `None` until then.
    #[cfg(target_os = "macos")]
    menu: Option<AppMenu>,
}

/// Wall-clock window over which `FpsCounter` averages; short enough to react
/// to a slowdown within about half a second, long enough that the on-screen
/// digits don't flicker frame to frame.
const FPS_WINDOW: Duration = Duration::from_millis(500);

/// Rolling display-FPS counter: records the `Instant` each frame is drawn
/// (i.e. each time the framebuffer is converted to RGBA for `pixels`, in
/// `about_to_wait`) and reports the average rate over the trailing
/// `FPS_WINDOW`. This measures real presented frames per wall-second, not
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
        #[cfg(target_os = "macos")]
        {
            self.menu = Some(menu::install());
        }
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

        if !self.paused || self.frame_advance {
            self.snes.run_frame([self.pad, JoypadState::default()]);
            self.frame_advance = false;
            if let Some(pixels) = &mut self.pixels {
                let frame = pixels.frame_mut();
                self.snes.framebuffer.to_rgba(frame);
                self.fps_counter.tick();
                // Overlay drawn only on the windowed present path, after the
                // core's own (pure) RGBA conversion — never touches
                // `snes.framebuffer` itself, so headless `--dump-frame`
                // output is unaffected.
                if self.show_fps {
                    let measured = self.fps_counter.fps();
                    let target = 1.0 / self.frame_duration.as_secs_f64();
                    // Green once the measured rate is within 5% of the
                    // cartridge region's native field rate (50/60 Hz); red
                    // if the emulator is falling behind it.
                    let color = if measured <= 0.0 || measured >= target * 0.95 {
                        [80, 255, 80, 255]
                    } else {
                        [255, 70, 70, 255]
                    };
                    let text = format!("FPS{:.0}/{:.0}", measured, target);
                    draw_overlay_text(frame, &text, color);
                }
            }
            // Feed this frame's audio into the ring; the callback's rate control
            // absorbs the emulator/host clock drift.
            if let Some(audio) = &mut self.audio {
                self.audio_scratch.clear();
                self.snes.drain_audio(&mut self.audio_scratch);
                audio.push(&self.audio_scratch);
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
        // Hotkeys act on the initial press only (ignore key-repeat).
        if pressed && !repeat {
            match code {
                KeyCode::Escape => {
                    event_loop.exit();
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
                KeyCode::F9 => {
                    self.load_state();
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

    /// `O` hotkey: open the native ROM picker and, if a file was chosen,
    /// tear down the running game (saving its SRAM first) and start the
    /// picked one. Cancelling the dialog or a load error leaves the current
    /// game running untouched.
    fn open_rom_dialog(&mut self) {
        let Some(path) = picker::pick_rom() else {
            return; // Cancelled: keep playing the current game.
        };
        if let Err(e) = self.switch_rom(&path) {
            eprintln!("error: could not load {}: {e}", path.display());
        }
    }

    /// F5 / `Emulation > Save State` (Cmd+S): snapshot the whole console
    /// (`Snes::save_state`) to the `<rom>.state` sidecar (slot 0) next to the
    /// currently loaded ROM. Never fails the run: an I/O error is reported and
    /// emulation continues.
    fn save_state(&mut self) {
        let path = crate::state::state_path(&self.current_rom_path, 0);
        let bytes = self.snes.save_state();
        match std::fs::write(&path, &bytes) {
            Ok(()) => eprintln!("state: saved {} ({} bytes)", path.display(), bytes.len()),
            Err(e) => eprintln!("state: could not write {}: {e}", path.display()),
        }
    }

    /// F9 / `Emulation > Load State` (Cmd+L): restore the console from the
    /// `<rom>.state` sidecar (slot 0). The blob carries no ROM image;
    /// `Snes::load_state` reattaches the live ROM and rejects a state saved
    /// from a different game. Any error (missing file, wrong ROM, corrupt
    /// blob) is reported and the running game is left untouched.
    fn load_state(&mut self) {
        let path = crate::state::state_path(&self.current_rom_path, 0);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("state: could not read {}: {e}", path.display());
                return;
            }
        };
        match self.snes.load_state(&bytes) {
            Ok(()) => eprintln!("state: loaded {}", path.display()),
            Err(e) => eprintln!("state: load failed ({}): {e}", path.display()),
        }
    }

    /// `F` hotkey / `View > Show FPS` (Cmd+F): toggles the on-screen FPS
    /// overlay (see `draw_overlay_text`). Also keeps the macOS menu's
    /// checkbox in sync when triggered from the keyboard, since AppKit only
    /// updates the checkmark itself when the *menu* item is clicked (see
    /// `poll_menu_events`, which does the reverse sync).
    fn toggle_show_fps(&mut self) {
        self.show_fps = !self.show_fps;
        #[cfg(target_os = "macos")]
        if let Some(menu) = &self.menu {
            menu.show_fps.set_checked(self.show_fps);
        }
    }

    /// Replace `self.snes` with a freshly constructed console for the ROM at
    /// `path`. Persists the outgoing cart's SRAM (via its own `save_path`)
    /// before replacing it, then loads the new cart's `.srm` sidecar the
    /// same way startup does, resets pad/pause/frame-advance state, and
    /// retargets pacing at the new cart's region field rate (a game switch
    /// can cross the PAL/NTSC line).
    fn switch_rom(&mut self, path: &Path) -> Result<(), String> {
        save::save_if_dirty(&self.snes.bus.cart, &self.save_path, &self.sram_baseline);

        let bytes = crate::load_rom_bytes(path)?;
        let mut cart = Cartridge::from_bytes(bytes)?;
        let save_path = save::default_save_path(path);
        let sram_baseline = save::load_sram(&mut cart, &save_path);

        self.title = format!("snes-frontend - {}", cart.title.trim());
        self.frame_duration = Duration::from_secs_f64(1.0 / cart.region.frames_per_second());
        self.snes = Snes::new(cart);
        self.save_path = save_path;
        self.sram_baseline = sram_baseline;
        self.pad = JoypadState::default();
        self.paused = false;
        self.frame_advance = false;
        self.next_deadline = Instant::now() + self.frame_duration;
        if let Some(window) = &self.window {
            window.set_title(&self.title);
        }
        self.current_rom_path = path.to_path_buf();
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
        if let Err(e) = self.switch_rom(&path) {
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
            // Quit shares one id across the app-menu and File-menu items.
            if event.id == menu.quit.id() || event.id == menu.quit_file.id() {
                event_loop.exit();
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
            } else if event.id == menu.show_fps.id() {
                // AppKit already flipped the CheckMenuItem's own checked
                // state before sending this click event (muda macOS impl);
                // mirror it rather than toggling again.
                self.show_fps = menu.show_fps.is_checked();
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
/// 0 = rightmost; set = lit). Only the characters the overlay ever prints
/// ("FPS<n>/<n>") are defined; anything else renders as a blank cell (still
/// advances the cursor, like a space).
fn glyph(c: char) -> [u8; GLYPH_H] {
    match c {
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
        'F' => [0b111, 0b100, 0b111, 0b100, 0b100],
        'P' => [0b111, 0b101, 0b111, 0b100, 0b100],
        'S' => [0b111, 0b100, 0b111, 0b001, 0b111],
        '/' => [0b001, 0b001, 0b010, 0b100, 0b100],
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
    let text_w = text.chars().count() * CHAR_ADVANCE;
    let box_w = text_w + OVERLAY_PAD * 2;
    let box_h = GLYPH_H * FONT_SCALE + OVERLAY_PAD * 2;
    if box_w > SCREEN_WIDTH || box_h > SCREEN_HEIGHT {
        return; // pathological (shouldn't happen for the overlay's own text): skip rather than panic on OOB math
    }
    let x0 = SCREEN_WIDTH - OVERLAY_MARGIN - box_w;
    let y0 = OVERLAY_MARGIN;

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
