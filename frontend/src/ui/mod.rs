//! Application shell: the egui layer, the screen state machine, the Prisme
//! visual identity and the screens themselves.
//!
//! Split so the parts that need no display can be unit-tested: `app_state`
//! (transitions, Escape semantics), `theme` (palette, embedded typefaces, the
//! product mark and the spectral rule, all applied on a headless
//! `egui::Context`), `icons` (the painter-drawn icon set), `tabs` (the tab bar
//! and its spectral underline) and the decision
//! helpers of `library_view` /
//! `game_sheet` / `settings` (card subtitle, sheet facts, offered choices,
//! Escape precedence) are pure logic; `pad_art` draws the SNES controller of
//! the `Entrées` section and maps a point back to the button under it; `egui_layer` is the only module that
//! touches wgpu; `home`, `library_view`, `game_sheet` and `settings` only
//! build widgets, and `textures` caches the decoded pictures they draw.
//! `shot` renders those same screens to a PNG with no window at all
//! (`--ui-shot`), which is how the interface is looked at on a machine that
//! has no display.

pub mod app_state;
pub mod confirm;
pub mod egui_layer;
pub mod game;
pub mod game_sheet;
pub mod home;
pub mod icons;
pub mod library_view;
pub mod pad_art;
pub mod settings;
pub mod shot;
pub mod tabs;
pub mod textures;
pub mod theme;

pub use app_state::{escape_action, Action, AppState, EscapeAction, Screen, Setting};
pub use egui_layer::EguiLayer;
pub use library_view::LibraryUi;
pub use settings::SettingsUi;
pub use tabs::Tab;
pub use textures::TextureStore;
