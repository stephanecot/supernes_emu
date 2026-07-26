//! Screen state machine of the application shell.
//!
//! The window hosts two screens: `Accueil` (the library / landing screen) and
//! `Jeu` (a cartridge running). Only one of them owns the window at a time.
//! Leaving the game for the home screen never tears the session down — the
//! console keeps its state, emulation is only suspended — so coming back
//! resumes exactly where it stopped, including the pause state the player had
//! set themselves.
//!
//! No I/O and no winit/egui types here on purpose: every transition is a pure
//! function of the current state, which is what makes it testable on a machine
//! with no display.

/// Which screen currently owns the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// Landing screen / library. No emulation runs while it is shown.
    Home,
    /// A cartridge is loaded and (unless paused) running.
    Game,
}

/// One persisted option the settings panel can change. Applied by the event
/// loop through the very same `App::set_*` methods the keyboard hotkeys and
/// the native menu use, so the three entry points can never write different
/// values into `prefs`.
///
/// Not `Copy`: one variant carries a path. Cloning a settings change once per
/// click is not a cost worth designing around.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Setting {
    /// Interface language, or `None` to follow the host's
    /// (`i18n::system_lang`). Applied on the next frame — the shell is rebuilt
    /// from `prefs` every time, so there is nothing to restart.
    Language(Option<crate::i18n::Lang>),
    /// Window size step of `ui::settings::ZOOM_CHOICES`, 1..=5.
    Zoom(u8),
    Filter(crate::render::Filter),
    Aspect(crate::render::Aspect),
    /// Window fullscreen state. Deliberately *not* persisted (see
    /// `video::App::set_fullscreen`), but it belongs to the display section.
    Fullscreen(bool),
    ShowFps(bool),
    Mute(bool),
    /// Output gain, 0..=100 percent.
    Volume(u8),
    /// Held-Tab speed multiplier (`prefs::FAST_FORWARD_FACTORS`).
    FastForward(u8),
    ResumeOnLaunch(bool),
    ConfirmOnQuit(bool),
    /// Save-state slot F5/F9 act on, 0..=9.
    Slot(u8),
    /// Let the assistant be summoned. Only settable when `claude` was actually
    /// found: the row is inert otherwise, so this can never be turned on for a
    /// feature the machine cannot run.
    Assistant(bool),
    /// Where the assistant's tool lives, typed or picked by the player. Empty
    /// goes back to looking on the `PATH`.
    AssistantPath(String),
    /// Drop every keyboard and controller binding the player made, back to the
    /// built-in `input::DEFAULT_KEYMAP` / `pad::DEFAULT_PAD_MAP`.
    ResetInputs,
}

/// A request produced by the UI, applied by the event loop once the egui
/// layer is no longer borrowed. Kept separate from the state machine itself so
/// building the UI stays a read-only operation over the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    /// Return to the suspended session.
    ResumeGame,
    /// Ask for a ROM with the native file dialog, then start it.
    PickRom,
    /// Leave the application (still subject to `prefs.confirm_on_quit`, which
    /// raises the `ui::confirm` modal instead of exiting straight away).
    Quit,
    /// The quit confirmation was answered yes: leave now.
    ConfirmQuit,
    /// …or no: dismiss the modal and restore the previous pause state.
    CancelQuit,
    /// Start the library game at this path (the `Jouer` button / a double
    /// click on a grid card). `resume` picks up the automatic session state
    /// when there is one; `false` starts the cartridge from power-on without
    /// touching it, so the suspended session is still there afterwards.
    Launch { path: std::path::PathBuf, resume: bool },
    /// Pin or unpin a game, by `library::game_id`.
    ToggleFavorite(String),
    /// Promote one of the game's own screenshots as its thumbnail, replacing
    /// the generated one.
    SetThumbnail { id: String, source: std::path::PathBuf },
    /// Go back to the generated thumbnail.
    ClearThumbnail(String),
    /// Delete one save state of the open game sheet, and the preview picture
    /// written beside it (`state::preview_path`). Confirmed in place by the
    /// sheet before it is produced.
    DeleteState(std::path::PathBuf),
    /// Rescan the library folder.
    Rescan,
    /// Add one game from anywhere on disk, with the native file dialog.
    /// `replacing` names a game whose file moved: the found file takes its
    /// place rather than joining it.
    AddGame { replacing: Option<std::path::PathBuf> },
    /// Drop an individually added game from the library. The file is never
    /// touched — forgetting a game must not be a way to delete it.
    ForgetGame(std::path::PathBuf),
    /// Choose the library folder with the native folder dialog, then rescan.
    ChooseLibraryDir,
    /// Back to the default library folder (`prefs.library_dir = None`), then
    /// rescan.
    ResetLibraryDir,
    /// Show the settings view (`ui::settings`).
    OpenSettings,
    /// Leave it for whatever it was opened from: the library tab that was
    /// showing, or the suspended game.
    CloseSettings,
    /// Leave it for one named library tab, chosen on the settings view's own
    /// tab bar. From the game screen this also steps back to the home screen —
    /// the tab bar belongs to it.
    ShowLibrary(crate::ui::Tab),
    /// Apply and persist one option of the settings panel.
    Set(Setting),
    /// Choose the screenshot folder with the native folder dialog.
    ChooseScreenshotDir,
    /// Back to the default screenshot folder (beside the ROM).
    ResetScreenshotDir,
    /// Choose the folder battery saves and save states go to.
    ChooseSaveDir,
    /// Back to the default save location (beside the ROM).
    ResetSaveDir,
    /// Turn one of a game's cheats on or off. `id` is the `library::game_id`
    /// the sheet is showing, `name` the cheat's own identity in its sidecar.
    /// Applies to the running console immediately when that game is the one
    /// loaded.
    ToggleCheat { id: String, name: String, enabled: bool },
    /// Drop one cheat from a game's sidecar. Nothing else is touched — a cheat
    /// is a note about an address, not a change to the save.
    RemoveCheat { id: String, name: String },
    /// Name the assistant's tool with the native file dialog.
    ChooseAssistantTool,
    /// Open the pedagogical PDF in the platform's document reader.
    OpenGuide,
    /// Fill one game's sheet in from the catalogues (`metadata`). One of the
    /// **two** requests in the whole application that reach the network, and
    /// both are a button the player pressed — nothing at scan time, nothing at
    /// startup. A failure leaves the sheet exactly as it was.
    FillSheet(String),
    /// The same for every game of the library that has no sheet yet.
    FillLibrary,
    /// Open a page in the platform's browser: the Wikipedia article a
    /// description was taken from, which the licence requires be reachable.
    OpenUrl(String),
}

/// What the Escape key does, resolved from the current context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscapeAction {
    /// Fullscreen is the first thing Escape backs out of, on either screen —
    /// the behavior every desktop OS expects from a fullscreen application.
    LeaveFullscreen,
    /// Windowed game: Escape steps back to the home screen instead of
    /// quitting. Quitting from the game is still reachable through the window
    /// close button, `Fichier > Quitter` and Cmd+Q, all of which keep going
    /// through `prefs.confirm_on_quit`.
    GoHome,
    /// Home screen with a suspended session: Escape is the way back in.
    ResumeGame,
    /// Home screen with nothing loaded: Escape leaves the application (through
    /// the quit confirmation, as before).
    Quit,
}

/// Resolve the Escape key. `fullscreen` is queried from the window itself, not
/// mirrored state, so this stays a pure decision function.
pub fn escape_action(screen: Screen, fullscreen: bool, has_session: bool) -> EscapeAction {
    if fullscreen {
        return EscapeAction::LeaveFullscreen;
    }
    match screen {
        Screen::Game => EscapeAction::GoHome,
        Screen::Home if has_session => EscapeAction::ResumeGame,
        Screen::Home => EscapeAction::Quit,
    }
}

/// Current screen plus the bit of game state that must survive a round trip
/// through the home screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppState {
    screen: Screen,
    has_session: bool,
    /// The game screen's own `paused` flag as it was when the home screen took
    /// over; restored verbatim on the way back so a player who had paused by
    /// hand does not silently get an unpaused game.
    saved_pause: bool,
}

impl AppState {
    /// `with_session` is true when the process was launched with a ROM path:
    /// that case must keep starting straight into the game.
    pub fn new(with_session: bool) -> Self {
        Self {
            screen: if with_session { Screen::Game } else { Screen::Home },
            has_session: with_session,
            saved_pause: false,
        }
    }

    pub fn screen(&self) -> Screen {
        self.screen
    }

    pub fn is_home(&self) -> bool {
        self.screen == Screen::Home
    }

    /// True once a cartridge has been loaded in this run; it stays true while
    /// the home screen is shown, since the session is only suspended.
    pub fn has_session(&self) -> bool {
        self.has_session
    }

    /// Suspend the game for the home screen, remembering `game_paused`.
    /// Returns false (and changes nothing) when the home screen is already up.
    pub fn go_home(&mut self, game_paused: bool) -> bool {
        if self.screen == Screen::Home {
            return false;
        }
        self.saved_pause = game_paused;
        self.screen = Screen::Home;
        true
    }

    /// Return to the suspended session. `Some(paused)` carries the pause flag
    /// the game screen must be restored to; `None` when there is nothing to
    /// return to (no cartridge loaded, or the game is already showing).
    pub fn resume_game(&mut self) -> Option<bool> {
        if self.screen == Screen::Game || !self.has_session {
            return None;
        }
        self.screen = Screen::Game;
        Some(self.saved_pause)
    }

    /// A cartridge was just loaded: the game screen takes over, unpaused.
    pub fn start_session(&mut self) {
        self.has_session = true;
        self.saved_pause = false;
        self.screen = Screen::Game;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launching_with_a_rom_starts_on_the_game_screen() {
        let s = AppState::new(true);
        assert_eq!(s.screen(), Screen::Game);
        assert!(s.has_session());
    }

    #[test]
    fn launching_without_a_rom_starts_on_the_home_screen() {
        let s = AppState::new(false);
        assert_eq!(s.screen(), Screen::Home);
        assert!(!s.has_session());
        assert!(s.is_home());
    }

    #[test]
    fn going_home_keeps_the_session_and_restores_the_pause_flag() {
        let mut s = AppState::new(true);
        assert!(s.go_home(true));
        assert_eq!(s.screen(), Screen::Home);
        assert!(s.has_session(), "the session is suspended, not dropped");
        // A second request while already home changes nothing.
        assert!(!s.go_home(false));
        assert_eq!(s.resume_game(), Some(true));
        assert_eq!(s.screen(), Screen::Game);

        // A player who was *not* paused gets back an unpaused game.
        assert!(s.go_home(false));
        assert_eq!(s.resume_game(), Some(false));
    }

    #[test]
    fn resuming_is_impossible_without_a_session() {
        let mut s = AppState::new(false);
        assert_eq!(s.resume_game(), None);
        assert_eq!(s.screen(), Screen::Home);
        // And is a no-op when the game is already showing.
        let mut s = AppState::new(true);
        assert_eq!(s.resume_game(), None);
    }

    #[test]
    fn starting_a_session_from_the_home_screen_switches_to_the_game() {
        let mut s = AppState::new(false);
        s.start_session();
        assert_eq!(s.screen(), Screen::Game);
        assert!(s.has_session());
        // A freshly started game is never paused, whatever the previous
        // session's pause flag was.
        assert!(s.go_home(true));
        s.start_session();
        assert!(s.go_home(false));
        assert_eq!(s.resume_game(), Some(false));
    }

    #[test]
    fn escape_backs_out_of_fullscreen_before_anything_else() {
        for screen in [Screen::Home, Screen::Game] {
            for has_session in [false, true] {
                assert_eq!(
                    escape_action(screen, true, has_session),
                    EscapeAction::LeaveFullscreen,
                    "{screen:?} session={has_session}"
                );
            }
        }
    }

    #[test]
    fn escape_in_a_windowed_game_returns_to_the_home_screen() {
        assert_eq!(escape_action(Screen::Game, false, true), EscapeAction::GoHome);
    }

    #[test]
    fn escape_on_the_home_screen_resumes_or_quits() {
        assert_eq!(escape_action(Screen::Home, false, true), EscapeAction::ResumeGame);
        assert_eq!(escape_action(Screen::Home, false, false), EscapeAction::Quit);
    }

    #[test]
    fn a_full_round_trip_never_loses_the_session() {
        let mut s = AppState::new(false);
        assert_eq!(escape_action(s.screen(), false, s.has_session()), EscapeAction::Quit);
        s.start_session();
        assert_eq!(escape_action(s.screen(), false, s.has_session()), EscapeAction::GoHome);
        s.go_home(false);
        assert_eq!(escape_action(s.screen(), false, s.has_session()), EscapeAction::ResumeGame);
        s.resume_game();
        assert_eq!(s.screen(), Screen::Game);
        assert!(s.has_session());
    }
}
