//! Gamepad input (`gilrs`): USB/Bluetooth controllers -> `JoypadState`.
//!
//! Three layers, split so everything but the device access itself is testable
//! without hardware:
//!   * `PadInput` + `PadState` — pure mapping logic. A `gilrs::EventType` is
//!     reduced to a `PadInput` (button up/down, axis value), accumulated in a
//!     `PadState`, and read back as a `JoypadState`. No I/O, no `Gilrs`.
//!   * `Slots` — which physical controller drives which SNES port. Generic over
//!     the id type, since `gilrs::GamepadId` cannot be constructed outside
//!     `gilrs` (its field is private), so the tests use plain integers.
//!   * `Pads` — the only part that talks to `gilrs`: opens the context, drains
//!     its event queue without blocking, and keeps the two `PadState`s and the
//!     `Slots` up to date. Exercised only with a real controller attached.
//!
//! Keyboard and gamepad coexist: `video::App` merges the keyboard's
//! `JoypadState` with player 1's pad state button by button (`merge`), so
//! neither input can cancel the other.
//!
//! Remapping follows the same rule as the keyboard (`crate::input`): a SNES
//! button named by `prefs.pad_map` uses that binding, a button the file says
//! nothing about falls back to `DEFAULT_PAD_MAP`.

use std::collections::BTreeMap;

use gilrs::{Axis, EventType, GamepadId, Gilrs};
/// Re-exported so the rest of the frontend can name a physical button (the
/// remapping capture) without depending on `gilrs` directly.
pub use gilrs::Button;
use snes_core::JoypadState;

use crate::i18n::{Lang, Msg};
use crate::input;

/// Controller ports the console exposes (`Snes::run_frame` takes
/// `[JoypadState; 2]`). Pad 1 drives player 1, pad 2 player 2; the keyboard
/// always stays on player 1.
pub const PLAYERS: usize = 2;

/// Deflection past which an analog stick counts as a D-pad press, on a
/// -1.0..=1.0 axis. Half travel is far above any resting noise (no extra
/// dead-zone filter is needed) while still letting a light push register, and
/// it keeps the two diagonals symmetrical: pushing into a corner passes the
/// threshold on both axes at once.
pub const STICK_THRESHOLD: f32 = 0.5;

/// Default gamepad mapping: SNES button name (the names `--script` and
/// `input::set_button` use) -> `gilrs` button. Several physical buttons may
/// drive the same SNES button — they are OR'ed (see `PadState::joypad`), which
/// is how both shoulder buttons and both triggers can act as L/R.
///
/// **Face buttons are mapped by position, not by label.** `gilrs` names the
/// four face buttons by geometry (South = bottom, East = right, North = top,
/// West = left), and the SNES diamond is B at the bottom, A on the right, Y on
/// the left, X on top. Mapping South->B / East->A / West->Y / North->X
/// therefore puts every SNES button exactly where it sits on a real SNES pad.
/// On an Xbox-style controller that means the button *labelled* A (bottom)
/// acts as SNES **B** and the one labelled B (right) acts as SNES **A**: the
/// labels disagree, but the geometry and the ergonomics agree — the primary
/// action of nearly every SNES game (B: jump/confirm) stays under the thumb's
/// resting position, and the cancel/secondary button (A) stays to its right.
/// Matching the *labels* instead (A->A, B->B) would mirror the diamond, put
/// jump on the right-hand button and confirm at the bottom, and break the
/// muscle memory of both the SNES and every modern pad at once.
///
/// The D-pad is listed here as buttons; controllers that report their hat as
/// an axis pair instead (`Axis::DPadX`/`DPadY`) and the left stick are handled
/// separately by `PadState::joypad`, and all three sources are OR'ed.
pub const DEFAULT_PAD_MAP: &[(&str, Button)] = &[
    ("B", Button::South),
    ("A", Button::East),
    ("Y", Button::West),
    ("X", Button::North),
    // Shoulder buttons (LB/RB) and triggers (LT/RT) both act as L/R: the SNES
    // has only two shoulder buttons, so binding both physical pairs costs
    // nothing and lets the player use whichever they prefer. `gilrs` turns an
    // analog trigger into `ButtonPressed`/`ButtonReleased` events with its own
    // hysteresis, so the digital events below are enough for both.
    ("L", Button::LeftTrigger),
    ("L", Button::LeftTrigger2),
    ("R", Button::RightTrigger),
    ("R", Button::RightTrigger2),
    ("Start", Button::Start),
    ("Select", Button::Select),
    ("Up", Button::DPadUp),
    ("Down", Button::DPadDown),
    ("Left", Button::DPadLeft),
    ("Right", Button::DPadRight),
];

/// Every `gilrs` button, with the name `prefs.pad_map` stores it under. The
/// stored name is the `gilrs` variant name (a test pins that against its own
/// `Debug`), so a preferences file written here stays readable by anything
/// else that knows `gilrs`; the *displayed* name is `pad_label`'s, which says
/// where the button sits on a modern controller.
pub const PAD_BUTTONS: &[(&str, Button)] = &[
    ("South", Button::South),
    ("East", Button::East),
    ("North", Button::North),
    ("West", Button::West),
    ("C", Button::C),
    ("Z", Button::Z),
    ("LeftTrigger", Button::LeftTrigger),
    ("LeftTrigger2", Button::LeftTrigger2),
    ("RightTrigger", Button::RightTrigger),
    ("RightTrigger2", Button::RightTrigger2),
    ("Select", Button::Select),
    ("Start", Button::Start),
    ("Mode", Button::Mode),
    ("LeftThumb", Button::LeftThumb),
    ("RightThumb", Button::RightThumb),
    ("DPadUp", Button::DPadUp),
    ("DPadDown", Button::DPadDown),
    ("DPadLeft", Button::DPadLeft),
    ("DPadRight", Button::DPadRight),
    ("Unknown", Button::Unknown),
];

/// Name `button` is stored under in the preferences.
pub fn pad_button_name(button: Button) -> &'static str {
    PAD_BUTTONS.iter().find(|&&(_, b)| b == button).map(|&(name, _)| name).unwrap_or("Unknown")
}

/// Physical button a stored name refers to, or `None` for a name no `gilrs`
/// version here knows (hand-edited file, newer build): the binding is then
/// ignored and the built-in default applies, the same lenient rule
/// `prefs::de_keymap` uses for keyboard keys.
pub fn pad_button_from_name(name: &str) -> Option<Button> {
    PAD_BUTTONS.iter().find(|&&(n, _)| n == name).map(|&(_, b)| b)
}

/// What the settings panel shows for a physical button: where it sits on a
/// modern controller, since `gilrs` names faces by geometry and few players
/// think of their pad in those terms.
pub fn pad_label(lang: Lang, button: Button) -> &'static str {
    // `C`, `Z`, `Select`, `Start` and `Mode` are printed on the pad itself and
    // are left alone, exactly like the SNES letters.
    match button {
        Button::South => Msg::PadSouth.text(lang),
        Button::East => Msg::PadEast.text(lang),
        Button::North => Msg::PadNorth.text(lang),
        Button::West => Msg::PadWest.text(lang),
        Button::C => "C",
        Button::Z => "Z",
        Button::LeftTrigger => Msg::PadLeftTrigger.text(lang),
        Button::LeftTrigger2 => Msg::PadLeftTrigger2.text(lang),
        Button::RightTrigger => Msg::PadRightTrigger.text(lang),
        Button::RightTrigger2 => Msg::PadRightTrigger2.text(lang),
        Button::Select => "Select",
        Button::Start => "Start",
        Button::Mode => Msg::PadMode.text(lang),
        Button::LeftThumb => Msg::PadLeftThumb.text(lang),
        Button::RightThumb => Msg::PadRightThumb.text(lang),
        Button::DPadUp => Msg::PadDPadUp.text(lang),
        Button::DPadDown => Msg::PadDPadDown.text(lang),
        Button::DPadLeft => Msg::PadDPadLeft.text(lang),
        Button::DPadRight => Msg::PadDPadRight.text(lang),
        Button::Unknown => Msg::PadUnknown.text(lang),
    }
}

/// Physical button bound to the SNES button `name` by the player, if the
/// preferences name one this build understands.
pub fn override_button(map: &BTreeMap<String, String>, name: &str) -> Option<Button> {
    map.get(name).and_then(|stored| pad_button_from_name(stored))
}

/// SNES button `button` currently drives, or `None`. Explicit bindings are
/// searched before defaults, exactly like `input::resolve_key`: a controller
/// button moved onto another SNES button stops driving the one that held it by
/// default.
pub fn resolve_button(map: &BTreeMap<String, String>, button: Button) -> Option<&'static str> {
    for name in input::BUTTONS {
        if override_button(map, name) == Some(button) {
            return Some(name);
        }
    }
    for name in input::BUTTONS {
        // An entry naming a button this build doesn't know counts as absent,
        // exactly like in `current_buttons`: the default binding applies.
        if override_button(map, name).is_none()
            && DEFAULT_PAD_MAP.iter().any(|&(n, b)| n == name && b == button)
        {
            return Some(name);
        }
    }
    None
}

/// Assign `button` to the SNES button `name`, swapping with whatever SNES
/// button already used it — same rule as `input::bind_key`, so no button is
/// ever silently left unbound.
///
/// An override binds exactly **one** physical button: a SNES button that had
/// two by default (L/R, bound to both the shoulder and the trigger) keeps only
/// the one the player pressed, and the swapped-with button receives the first
/// of the previous binding's physical buttons.
///
/// Like `input::bind_key`, every decision is taken on `resolve_button` — what
/// the physical button actually drives — never on what a SNES button claims: a
/// partial `pad_map` can have two SNES buttons claiming one physical button,
/// and deciding on the claim would leave the losing one dead while reporting a
/// swap that did not happen.
pub fn bind_button(
    map: &mut BTreeMap<String, String>,
    name: &str,
    button: Button,
) -> input::BindResult {
    let Some(name) = input::button_name(name) else { return input::BindResult::Unchanged };
    if resolve_button(map, button) == Some(name) {
        // Nothing to write: the button already drives this one. Left on its
        // default on purpose, so L/R keep *both* their default bindings
        // (shoulder and trigger) instead of being narrowed to one.
        return input::BindResult::Unchanged;
    }
    // What this SNES button hands over in a swap. `None` when it was already
    // claiming `button`: handing that back would put both on it again.
    let previous = current_buttons(map, name).first().copied().filter(|&b| b != button);
    let conflict = resolve_button(map, button).filter(|&other| other != name);
    map.insert(name.to_string(), pad_button_name(button).to_string());
    match (conflict, previous) {
        (Some(other), Some(previous)) => {
            map.insert(other.to_string(), pad_button_name(previous).to_string());
            input::BindResult::Swapped(other)
        }
        (Some(other), None) => {
            // Nothing to hand over: drop the other button's entry so it falls
            // back to its built-in binding rather than keeping one that no
            // longer reaches it.
            map.remove(other);
            input::BindResult::Reverted(other)
        }
        (None, _) => input::BindResult::Bound,
    }
}

/// Physical buttons driving the SNES button `name`, in display order: the
/// single override, or every default entry for that name.
pub fn current_buttons(map: &BTreeMap<String, String>, name: &str) -> Vec<Button> {
    match override_button(map, name) {
        Some(button) => vec![button],
        None => DEFAULT_PAD_MAP
            .iter()
            .filter(|&&(n, _)| n == name)
            .map(|&(_, button)| button)
            .collect(),
    }
}

/// What the panel prints for a SNES button's controller binding: the bound
/// button's label, the two defaults joined for L/R, or a dash when nothing
/// drives it.
///
/// Only the physical buttons that `resolve_button` actually routes back to
/// `name` are listed: a hand-edited `pad_map` can leave a SNES button claiming
/// a physical button another one won, and printing it would say the button
/// responds when it does not.
pub fn binding_label(lang: Lang, map: &BTreeMap<String, String>, name: &str) -> String {
    let buttons: Vec<Button> = current_buttons(map, name)
        .into_iter()
        .filter(|&b| resolve_button(map, b) == Some(name))
        .collect();
    if buttons.is_empty() {
        return "—".to_string();
    }
    buttons.iter().map(|&b| pad_label(lang, b)).collect::<Vec<_>>().join(" / ")
}

/// One meaningful change of a controller, decoupled from `gilrs::EventType`.
///
/// The indirection exists so the mapping can be unit-tested: every
/// button/axis variant of `EventType` carries a `gilrs::ev::Code`, whose field
/// is private to `gilrs`, so an `EventType` cannot be built outside that crate.
/// `PadInput` can, and `from_event` (a plain `match`, no construction) is the
/// only piece that needs a real device to be exercised.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PadInput {
    Button { button: Button, pressed: bool },
    Axis { axis: Axis, value: f32 },
}

impl PadInput {
    /// Reduce a `gilrs` event to the change it describes, or `None` for the
    /// events that carry no input (`Connected`/`Disconnected` are handled by
    /// `Pads::poll` itself, `Dropped` is a filtered event `gilrs` asks callers
    /// to ignore, `ButtonRepeated` only exists with the `Repeat` filter and
    /// would re-assert a state already held).
    ///
    /// `ButtonChanged` is deliberately ignored: `gilrs` already turns
    /// axis-backed buttons (analog triggers) into `ButtonPressed`/
    /// `ButtonReleased` with its own hysteresis, so acting on the analog value
    /// too would only add a second, differently-thresholded source for the
    /// same button.
    pub fn from_event(event: &EventType) -> Option<Self> {
        match *event {
            EventType::ButtonPressed(button, _) => Some(Self::Button { button, pressed: true }),
            EventType::ButtonReleased(button, _) => Some(Self::Button { button, pressed: false }),
            EventType::AxisChanged(axis, value, _) => Some(Self::Axis { axis, value }),
            _ => None,
        }
    }
}

/// Live state of one controller: which physical buttons are held, plus the
/// axes that can also drive the D-pad. Reset to this default whenever the
/// controller is (dis)connected, so nothing can stay stuck.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PadState {
    /// One flag per `PAD_BUTTONS` entry, i.e. per *physical* button rather
    /// than per SNES button: the mapping is applied when the state is read
    /// (`joypad`), so rebinding a button mid-session takes effect on the next
    /// frame without anything staying held. Two physical buttons driving the
    /// same SNES button stay independent, so releasing LT while LB is still
    /// held keeps L pressed.
    held: [bool; PAD_BUTTONS.len()],
    /// Hat reported as an axis pair (`Axis::DPadX`/`DPadY`) instead of
    /// buttons, as some drivers do.
    hat_x: f32,
    hat_y: f32,
    /// Left stick, which drives the D-pad too.
    stick_x: f32,
    stick_y: f32,
}

impl Default for PadState {
    fn default() -> Self {
        Self {
            held: [false; PAD_BUTTONS.len()],
            hat_x: 0.0,
            hat_y: 0.0,
            stick_x: 0.0,
            stick_y: 0.0,
        }
    }
}

impl PadState {
    /// Fold one input change into the state. Axes other than the hat and the
    /// left stick (right stick, analog triggers) are ignored; every button is
    /// recorded, mapped or not, since the mapping is only applied on the way
    /// out (`joypad`).
    pub fn apply(&mut self, input: PadInput) {
        match input {
            PadInput::Button { button, pressed } => {
                if let Some(i) = PAD_BUTTONS.iter().position(|&(_, b)| b == button) {
                    self.held[i] = pressed;
                }
            }
            PadInput::Axis { axis, value } => match axis {
                Axis::DPadX => self.hat_x = value,
                Axis::DPadY => self.hat_y = value,
                Axis::LeftStickX => self.stick_x = value,
                Axis::LeftStickY => self.stick_y = value,
                _ => {}
            },
        }
    }

    /// The SNES pad this controller is currently holding down, resolved
    /// through `map` (`prefs.pad_map`: SNES button -> stored `gilrs` button
    /// name; an empty map is the built-in mapping).
    ///
    /// The three direction sources (D-pad buttons, hat axes, left stick) are
    /// OR'ed. `gilrs` normalizes the vertical axes so that **positive is up**
    /// on every platform (`gilrs_core::IS_Y_AXIS_REVERSED` flips the ones that
    /// report up as negative), hence `+y` -> Up. The stick and hat axes always
    /// drive the directions, whatever the button bindings are: they are not
    /// remappable, since a stick is not a button.
    ///
    /// Opposite directions are not filtered out: they can only occur by
    /// combining two sources (stick left + hat right), and the keyboard path
    /// doesn't filter them either — both arrow keys can be held at once.
    pub fn joypad(&self, map: &BTreeMap<String, String>) -> JoypadState {
        let mut state = JoypadState::default();
        for (i, &(_, button)) in PAD_BUTTONS.iter().enumerate() {
            if !self.held[i] {
                continue;
            }
            if let Some(name) = resolve_button(map, button) {
                // Every name in `input::BUTTONS` is one `set_button` accepts;
                // the cross-check test in `input` guarantees it.
                let _ = input::set_button(&mut state, name, true);
            }
        }
        for &(x, y) in &[(self.hat_x, self.hat_y), (self.stick_x, self.stick_y)] {
            state.left |= x <= -STICK_THRESHOLD;
            state.right |= x >= STICK_THRESHOLD;
            state.down |= y <= -STICK_THRESHOLD;
            state.up |= y >= STICK_THRESHOLD;
        }
        state
    }
}

/// Per-button OR of two pad states. Used to let the keyboard and player 1's
/// controller drive the same port without either cancelling the other.
pub fn merge(a: JoypadState, b: JoypadState) -> JoypadState {
    JoypadState {
        a: a.a | b.a,
        b: a.b | b.b,
        x: a.x | b.x,
        y: a.y | b.y,
        l: a.l | b.l,
        r: a.r | b.r,
        start: a.start | b.start,
        select: a.select | b.select,
        up: a.up | b.up,
        down: a.down | b.down,
        left: a.left | b.left,
        right: a.right | b.right,
    }
}

/// Which physical controller owns which player slot.
///
/// A controller keeps its slot for as long as it stays connected: unplugging
/// player 1's pad frees slot 0 but does **not** promote player 2's pad to it,
/// so a mid-session cable knock cannot silently swap the two players. The
/// freed slot is handed to the next controller that connects (which may be the
/// same one coming back). Controllers beyond the second are tracked by `gilrs`
/// but drive nothing — the console has two ports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slots<T> {
    ids: [Option<T>; PLAYERS],
}

impl<T> Default for Slots<T> {
    fn default() -> Self {
        Self { ids: [const { None }; PLAYERS] }
    }
}

impl<T: Copy + PartialEq> Slots<T> {
    /// Player slot `id` already owns, if any.
    pub fn player_of(&self, id: T) -> Option<usize> {
        self.ids.iter().position(|slot| *slot == Some(id))
    }

    /// Give `id` the lowest free slot. `None` when it already has one (a
    /// controller present at startup also produces a `Connected` event on some
    /// platforms, so this must be idempotent) or when both ports are taken.
    pub fn connect(&mut self, id: T) -> Option<usize> {
        if self.player_of(id).is_some() {
            return None;
        }
        let slot = self.ids.iter().position(|s| s.is_none())?;
        self.ids[slot] = Some(id);
        Some(slot)
    }

    /// Release the slot `id` owned, if it owned one.
    pub fn disconnect(&mut self, id: T) -> Option<usize> {
        let slot = self.player_of(id)?;
        self.ids[slot] = None;
        Some(slot)
    }

    /// Number of ports currently driven by a controller.
    pub fn connected(&self) -> usize {
        self.ids.iter().filter(|s| s.is_some()).count()
    }
}

/// A controller was plugged in or unplugged, for the discreet on-screen notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PadNotice {
    /// 0-based player slot.
    pub player: usize,
    pub connected: bool,
    /// Controller name as the platform reports it; for the log only.
    pub name: String,
}

impl PadNotice {
    /// Text for the status overlay. Uppercase and unaccented: the overlay font
    /// (`video::glyph`) has no lowercase and no accented glyphs.
    pub fn status(&self, lang: Lang) -> String {
        crate::i18n::status_pad(lang, self.player + 1, self.connected)
    }
}

/// One drain of the controllers' event queue (`Pads::poll`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Polled {
    /// Controllers plugged in or unplugged during this drain.
    pub notices: Vec<PadNotice>,
    /// Buttons pressed during this drain, in order, from any controller that
    /// owns a port. Only the remapping capture reads them — play input goes
    /// through `Pads::player`, which reads the accumulated state instead.
    pub pressed: Vec<Button>,
}

/// The gamepad subsystem: a `gilrs` context plus the state of the two ports.
///
/// Best-effort, exactly like the audio output: if `gilrs` cannot start (no
/// permission, unsupported platform), the emulator runs on the keyboard alone
/// and only warns on stderr.
pub struct Pads {
    /// `None` when `gilrs` could not be initialised.
    gilrs: Option<Gilrs>,
    slots: Slots<GamepadId>,
    states: [PadState; PLAYERS],
}

impl Pads {
    /// Open the gamepad context and adopt the controllers already plugged in.
    pub fn new() -> Self {
        let mut pads =
            Self { gilrs: None, slots: Slots::default(), states: [PadState::default(); PLAYERS] };
        match Gilrs::new() {
            Ok(gilrs) => {
                // Controllers present before the app started are listed here;
                // depending on the platform they may *also* produce a
                // `Connected` event, which `Slots::connect` then ignores.
                let existing: Vec<(GamepadId, String)> = gilrs
                    .gamepads()
                    .map(|(id, gamepad)| (id, gamepad.name().to_string()))
                    .collect();
                pads.gilrs = Some(gilrs);
                for (id, name) in existing {
                    if let Some(player) = pads.slots.connect(id) {
                        eprintln!("pad: player {} = {name}", player + 1);
                    } else {
                        eprintln!("pad: {name} ignored (both ports already taken)");
                    }
                }
            }
            // `gilrs::Error::NotImplemented` also lands here: the platform has
            // no gamepad backend, which is not a reason to fail the run.
            Err(e) => eprintln!("pad: gamepad support unavailable ({e})"),
        }
        pads
    }

    /// Drain every event `gilrs` has queued and update the two ports. Never
    /// blocks (`Gilrs::next_event` returns `None` on an empty queue), so this
    /// is called once per frame from the event loop.
    ///
    /// Returns the hot-plug changes that happened and the buttons pressed, for
    /// the caller to show and to feed the remapping capture.
    pub fn poll(&mut self) -> Polled {
        let Self { gilrs, slots, states } = self;
        let mut polled = Polled::default();
        let Some(gilrs) = gilrs.as_mut() else { return polled };
        while let Some(event) = gilrs.next_event() {
            match event.event {
                EventType::Connected => {
                    match slots.connect(event.id) {
                        Some(player) => {
                            // A slot is always entered clean: a controller that
                            // comes back must not inherit whatever was held
                            // when it went away.
                            states[player] = PadState::default();
                            polled.notices.push(PadNotice {
                                player,
                                connected: true,
                                name: gilrs.gamepad(event.id).name().to_string(),
                            });
                        }
                        // The console has two ports; a third controller drives
                        // nothing. Said out loud (like `Pads::new` does for the
                        // ones already plugged in at startup) rather than
                        // ignored in silence — it is adopted as soon as a port
                        // frees up, see the `Disconnected` arm.
                        None => eprintln!(
                            "pad: {} ignored (both ports already taken)",
                            gilrs.gamepad(event.id).name()
                        ),
                    }
                }
                EventType::Disconnected => {
                    if let Some(player) = slots.disconnect(event.id) {
                        states[player] = PadState::default();
                        polled.notices.push(PadNotice {
                            player,
                            connected: false,
                            name: gilrs.gamepad(event.id).name().to_string(),
                        });
                    }
                    // A controller that was plugged in while both ports were
                    // taken owns no slot, and `gilrs` will not announce it a
                    // second time: the port just freed is handed to it here,
                    // instead of making the player unplug and replug it.
                    let orphan = gilrs
                        .gamepads()
                        .find(|&(id, gamepad)| {
                            id != event.id && gamepad.is_connected() && slots.player_of(id).is_none()
                        })
                        .map(|(id, gamepad)| (id, gamepad.name().to_string()));
                    if let Some((id, name)) = orphan {
                        if let Some(player) = slots.connect(id) {
                            states[player] = PadState::default();
                            polled.notices.push(PadNotice { player, connected: true, name });
                        }
                    }
                }
                ref other => {
                    if let (Some(player), Some(input)) =
                        (slots.player_of(event.id), PadInput::from_event(other))
                    {
                        states[player].apply(input);
                        if let PadInput::Button { button, pressed: true } = input {
                            polled.pressed.push(button);
                        }
                    }
                }
            }
        }
        polled
    }

    /// What player `player` (0-based) is holding down, resolved through `map`
    /// (`prefs.pad_map`). `JoypadState::default()` for a port with no
    /// controller, and for any index beyond `PLAYERS`.
    pub fn player(&self, player: usize, map: &BTreeMap<String, String>) -> JoypadState {
        match self.states.get(player) {
            Some(state) => state.joypad(map),
            None => JoypadState::default(),
        }
    }

    /// Ports currently driven by a controller (0..=`PLAYERS`).
    pub fn connected(&self) -> usize {
        self.slots.connected()
    }
}

impl Default for Pads {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No override: the built-in mapping, which is what `prefs.pad_map`
    /// carries by default (an empty map).
    fn no_map() -> BTreeMap<String, String> {
        BTreeMap::new()
    }


    /// Held state for one mapped button, by its `DEFAULT_PAD_MAP` index.
    fn press(state: &mut PadState, button: Button) {
        state.apply(PadInput::Button { button, pressed: true });
    }

    fn release(state: &mut PadState, button: Button) {
        state.apply(PadInput::Button { button, pressed: false });
    }

    #[test]
    fn every_mapped_name_is_a_button_set_button_knows() {
        for &(name, _) in DEFAULT_PAD_MAP {
            let mut state = JoypadState::default();
            assert!(input::set_button(&mut state, name, true).is_ok(), "button name {name:?}");
        }
        // Every SNES button reachable from the keyboard is reachable from a
        // controller too — a pad that could not press Select would be useless.
        for &(name, _) in input::DEFAULT_KEYMAP {
            assert!(
                DEFAULT_PAD_MAP.iter().any(|&(mapped, _)| mapped == name),
                "no gamepad binding for {name}"
            );
        }
    }

    /// The mapping decision documented on `DEFAULT_PAD_MAP`: face buttons go by
    /// position, so on an Xbox-style pad the bottom button (labelled A) is SNES
    /// B and the right one (labelled B) is SNES A.
    #[test]
    fn face_buttons_are_mapped_by_position_not_by_label() {
        let mut state = PadState::default();
        press(&mut state, Button::South);
        let pad = state.joypad(&no_map());
        assert!(pad.b, "bottom face button must be SNES B");
        assert!(!pad.a);

        let mut state = PadState::default();
        press(&mut state, Button::East);
        assert!(state.joypad(&no_map()).a, "right face button must be SNES A");

        let mut state = PadState::default();
        press(&mut state, Button::West);
        assert!(state.joypad(&no_map()).y, "left face button must be SNES Y");

        let mut state = PadState::default();
        press(&mut state, Button::North);
        assert!(state.joypad(&no_map()).x, "top face button must be SNES X");
    }

    #[test]
    fn press_and_release_toggle_exactly_one_button() {
        let mut state = PadState::default();
        press(&mut state, Button::Start);
        assert_eq!(state.joypad(&no_map()), JoypadState { start: true, ..Default::default() });
        release(&mut state, Button::Start);
        assert_eq!(state.joypad(&no_map()), JoypadState::default());

        press(&mut state, Button::Select);
        assert!(state.joypad(&no_map()).select);
        release(&mut state, Button::Select);
        assert!(!state.joypad(&no_map()).select);
    }

    #[test]
    fn unmapped_buttons_and_axes_change_nothing() {
        let mut state = PadState::default();
        press(&mut state, Button::Mode);
        press(&mut state, Button::LeftThumb);
        press(&mut state, Button::Unknown);
        state.apply(PadInput::Axis { axis: Axis::RightStickX, value: 1.0 });
        state.apply(PadInput::Axis { axis: Axis::RightStickY, value: -1.0 });
        state.apply(PadInput::Axis { axis: Axis::LeftZ, value: 1.0 });
        assert_eq!(state.joypad(&no_map()), JoypadState::default());
    }

    /// Both shoulder buttons and both triggers drive L/R, and they are OR'ed:
    /// releasing one while the other is held must not release the SNES button.
    #[test]
    fn shoulders_and_triggers_both_drive_l_and_r_and_are_or_ed() {
        let mut state = PadState::default();
        press(&mut state, Button::LeftTrigger);
        assert!(state.joypad(&no_map()).l);
        press(&mut state, Button::LeftTrigger2);
        assert!(state.joypad(&no_map()).l);
        release(&mut state, Button::LeftTrigger2);
        assert!(state.joypad(&no_map()).l, "LB is still held");
        release(&mut state, Button::LeftTrigger);
        assert!(!state.joypad(&no_map()).l);

        press(&mut state, Button::RightTrigger2);
        assert!(state.joypad(&no_map()).r);
        release(&mut state, Button::RightTrigger2);
        assert!(!state.joypad(&no_map()).r);
    }

    #[test]
    fn dpad_buttons_drive_the_directions() {
        let mut state = PadState::default();
        press(&mut state, Button::DPadUp);
        press(&mut state, Button::DPadRight);
        let pad = state.joypad(&no_map());
        assert!(pad.up && pad.right);
        assert!(!pad.down && !pad.left);
    }

    /// `gilrs` normalizes the vertical axes to "positive is up" on every
    /// platform; a stick pushed up must therefore press Up, not Down.
    #[test]
    fn left_stick_maps_to_the_dpad_with_positive_y_up() {
        let mut state = PadState::default();
        state.apply(PadInput::Axis { axis: Axis::LeftStickY, value: 1.0 });
        assert!(state.joypad(&no_map()).up);
        assert!(!state.joypad(&no_map()).down);
        state.apply(PadInput::Axis { axis: Axis::LeftStickY, value: -1.0 });
        assert!(state.joypad(&no_map()).down);
        assert!(!state.joypad(&no_map()).up);

        state.apply(PadInput::Axis { axis: Axis::LeftStickX, value: -1.0 });
        assert!(state.joypad(&no_map()).left);
        state.apply(PadInput::Axis { axis: Axis::LeftStickX, value: 1.0 });
        assert!(state.joypad(&no_map()).right);
    }

    #[test]
    fn a_stick_inside_the_threshold_presses_nothing() {
        let mut state = PadState::default();
        for value in [0.0, 0.1, -0.2, STICK_THRESHOLD - 0.01, -(STICK_THRESHOLD - 0.01)] {
            state.apply(PadInput::Axis { axis: Axis::LeftStickX, value });
            state.apply(PadInput::Axis { axis: Axis::LeftStickY, value });
            assert_eq!(state.joypad(&no_map()), JoypadState::default(), "value {value}");
        }
        // Exactly at the threshold counts as pressed.
        state.apply(PadInput::Axis { axis: Axis::LeftStickX, value: STICK_THRESHOLD });
        assert!(state.joypad(&no_map()).right);
    }

    /// A stick pushed into a corner must press both directions, which is what
    /// makes diagonals work in-game.
    #[test]
    fn a_diagonal_stick_presses_both_directions() {
        let mut state = PadState::default();
        state.apply(PadInput::Axis { axis: Axis::LeftStickX, value: 0.7 });
        state.apply(PadInput::Axis { axis: Axis::LeftStickY, value: 0.7 });
        let pad = state.joypad(&no_map());
        assert!(pad.up && pad.right);
        assert!(!pad.down && !pad.left);
    }

    /// Some drivers report the hat as an axis pair rather than four buttons.
    #[test]
    fn hat_axes_drive_the_dpad_too() {
        let mut state = PadState::default();
        state.apply(PadInput::Axis { axis: Axis::DPadY, value: 1.0 });
        assert!(state.joypad(&no_map()).up);
        state.apply(PadInput::Axis { axis: Axis::DPadY, value: 0.0 });
        state.apply(PadInput::Axis { axis: Axis::DPadX, value: -1.0 });
        let pad = state.joypad(&no_map());
        assert!(pad.left && !pad.up);
    }

    /// The three direction sources are OR'ed: a hat press and a stick push
    /// coexist instead of one clearing the other.
    #[test]
    fn dpad_buttons_hat_and_stick_are_or_ed() {
        let mut state = PadState::default();
        press(&mut state, Button::DPadLeft);
        state.apply(PadInput::Axis { axis: Axis::LeftStickY, value: 1.0 });
        state.apply(PadInput::Axis { axis: Axis::DPadX, value: 1.0 });
        let pad = state.joypad(&no_map());
        assert!(pad.left, "D-pad button still held");
        assert!(pad.right, "hat axis pushed right");
        assert!(pad.up, "stick pushed up");
    }

    #[test]
    fn a_fresh_state_presses_nothing() {
        assert_eq!(PadState::default().joypad(&no_map()), JoypadState::default());
    }

    #[test]
    fn merge_is_a_per_button_or() {
        let keyboard = JoypadState { a: true, up: true, ..Default::default() };
        let gamepad = JoypadState { b: true, up: true, left: true, ..Default::default() };
        let merged = merge(keyboard, gamepad);
        assert_eq!(
            merged,
            JoypadState { a: true, b: true, up: true, left: true, ..Default::default() }
        );
        // Neither side can clear the other's buttons.
        assert_eq!(merge(keyboard, JoypadState::default()), keyboard);
        assert_eq!(merge(JoypadState::default(), gamepad), gamepad);
        assert_eq!(merge(JoypadState::default(), JoypadState::default()), JoypadState::default());
    }

    #[test]
    fn merge_keeps_every_button_of_the_pad() {
        // A full house on one side must survive the merge untouched.
        let all = JoypadState {
            a: true,
            b: true,
            x: true,
            y: true,
            l: true,
            r: true,
            start: true,
            select: true,
            up: true,
            down: true,
            left: true,
            right: true,
        };
        assert_eq!(merge(all, JoypadState::default()), all);
        assert_eq!(merge(JoypadState::default(), all), all);
    }

    #[test]
    fn the_first_two_controllers_take_the_two_ports() {
        let mut slots = Slots::<u32>::default();
        assert_eq!(slots.connected(), 0);
        assert_eq!(slots.connect(10), Some(0));
        assert_eq!(slots.connect(20), Some(1));
        assert_eq!(slots.connected(), 2);
        assert_eq!(slots.player_of(10), Some(0));
        assert_eq!(slots.player_of(20), Some(1));
        assert_eq!(slots.player_of(30), None);
        // The console has two ports: a third controller drives nothing.
        assert_eq!(slots.connect(30), None);
        assert_eq!(slots.connected(), 2);
    }

    #[test]
    fn connecting_the_same_controller_twice_is_a_no_op() {
        // A controller plugged in before launch is both listed by
        // `Gilrs::gamepads` and (on some platforms) announced by a `Connected`
        // event; the second one must not take the other port.
        let mut slots = Slots::<u32>::default();
        assert_eq!(slots.connect(10), Some(0));
        assert_eq!(slots.connect(10), None);
        assert_eq!(slots.connected(), 1);
        assert_eq!(slots.player_of(10), Some(0));
    }

    #[test]
    fn unplugging_player_one_does_not_promote_player_two() {
        let mut slots = Slots::<u32>::default();
        slots.connect(10);
        slots.connect(20);
        assert_eq!(slots.disconnect(10), Some(0));
        assert_eq!(slots.player_of(20), Some(1), "player 2 must keep its port");
        assert_eq!(slots.connected(), 1);
        // The freed port goes to the next controller to arrive — including the
        // same one coming back.
        assert_eq!(slots.connect(10), Some(0));
        assert_eq!(slots.connect(30), None);
    }

    #[test]
    fn disconnecting_an_unknown_controller_is_harmless() {
        let mut slots = Slots::<u32>::default();
        assert_eq!(slots.disconnect(99), None);
        slots.connect(10);
        assert_eq!(slots.disconnect(99), None);
        assert_eq!(slots.player_of(10), Some(0));
        assert_eq!(slots.disconnect(10), Some(0));
        assert_eq!(slots.disconnect(10), None);
        assert_eq!(slots.connected(), 0);
    }

    #[test]
    fn hot_plug_notices_are_written_for_the_overlay_font() {
        let plugged = PadNotice { player: 0, connected: true, name: "Pad".to_string() };
        assert_eq!(plugged.status(Lang::Fr), "MANETTE 1 CONNECTEE");
        assert_eq!(plugged.status(Lang::En), "CONTROLLER 1 CONNECTED");
        let gone = PadNotice { player: 1, connected: false, name: "Pad".to_string() };
        assert_eq!(gone.status(Lang::Fr), "MANETTE 2 DECONNECTEE");
        assert_eq!(gone.status(Lang::En), "CONTROLLER 2 DISCONNECTED");
        // The overlay font is uppercase, unaccented ASCII only.
        for notice in [plugged, gone] {
            for lang in Lang::ALL {
                let text = notice.status(lang);
                assert!(
                    text.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == ' '),
                    "{text:?} cannot be drawn by the overlay font"
                );
            }
        }
    }

    /// The events that carry no input must be rejected by `from_event`, so
    /// `Pads::poll` can handle them itself.
    #[test]
    fn events_without_input_produce_no_pad_input() {
        assert_eq!(PadInput::from_event(&EventType::Connected), None);
        assert_eq!(PadInput::from_event(&EventType::Disconnected), None);
        assert_eq!(PadInput::from_event(&EventType::Dropped), None);
        assert_eq!(PadInput::from_event(&EventType::ForceFeedbackEffectCompleted), None);
    }

    // --- remapping (`prefs.pad_map`) ------------------------------------

    /// The stored name of a button must be its `gilrs` variant name, and every
    /// variant must be listed exactly once — a missing one could not be bound
    /// at all, and a wrong name would not survive a round trip through the
    /// preferences file.
    #[test]
    fn every_gilrs_button_is_listed_under_its_own_name() {
        let mut names: Vec<&str> = PAD_BUTTONS.iter().map(|&(name, _)| name).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "duplicate name in PAD_BUTTONS");
        for &(name, button) in PAD_BUTTONS {
            assert_eq!(name, format!("{button:?}"), "stored name must be the gilrs one");
            assert_eq!(pad_button_name(button), name);
            assert_eq!(pad_button_from_name(name), Some(button));
            for lang in Lang::ALL {
                assert!(!pad_label(lang, button).is_empty());
            }
        }
        // Every default binding names a button the preferences can store.
        for &(_, button) in DEFAULT_PAD_MAP {
            assert_eq!(pad_button_from_name(pad_button_name(button)), Some(button));
        }
        // A name from a hand-edited file that no `gilrs` version here knows.
        assert_eq!(pad_button_from_name("Paddle"), None);
    }

    #[test]
    fn an_empty_map_resolves_exactly_like_the_built_in_mapping() {
        let map = no_map();
        for &(name, button) in DEFAULT_PAD_MAP {
            assert_eq!(resolve_button(&map, button), Some(name), "{name} <- {button:?}");
            assert!(current_buttons(&map, name).contains(&button));
        }
        assert_eq!(resolve_button(&map, Button::Mode), None);
        assert_eq!(current_buttons(&map, "L"), vec![Button::LeftTrigger, Button::LeftTrigger2]);
        assert_eq!(current_buttons(&map, "A"), vec![Button::East]);
    }

    #[test]
    fn a_rebound_pad_button_drives_its_new_snes_button_only() {
        let mut map = no_map();
        map.insert("A".to_string(), "North".to_string());
        let mut state = PadState::default();
        press(&mut state, Button::North);
        let joypad = state.joypad(&map);
        assert!(joypad.a, "the bound button presses A");
        assert!(!joypad.x, "X no longer answers to its default button");
        // A's own default is free again.
        let mut state = PadState::default();
        press(&mut state, Button::East);
        assert_eq!(state.joypad(&map), JoypadState::default());
        // Buttons the map says nothing about keep their default.
        let mut state = PadState::default();
        press(&mut state, Button::South);
        assert!(state.joypad(&map).b);
    }

    /// An override binds exactly one physical button: L rebound to the trigger
    /// alone stops answering to the shoulder button.
    #[test]
    fn an_override_replaces_every_default_binding_of_that_button() {
        let mut map = no_map();
        map.insert("L".to_string(), "LeftTrigger2".to_string());
        assert_eq!(current_buttons(&map, "L"), vec![Button::LeftTrigger2]);
        let mut state = PadState::default();
        press(&mut state, Button::LeftTrigger);
        assert!(!state.joypad(&map).l);
        press(&mut state, Button::LeftTrigger2);
        assert!(state.joypad(&map).l);
    }

    /// A stored name this build doesn't know must not leave the button dead:
    /// the built-in binding applies, like `prefs::de_keymap` for the keyboard.
    #[test]
    fn an_unknown_stored_name_falls_back_to_the_default_binding() {
        let mut map = no_map();
        map.insert("A".to_string(), "Paddle".to_string());
        assert_eq!(override_button(&map, "A"), None);
        assert_eq!(current_buttons(&map, "A"), vec![Button::East]);
        let mut state = PadState::default();
        press(&mut state, Button::East);
        assert!(state.joypad(&map).a);
    }

    #[test]
    fn binding_a_free_pad_button_touches_only_that_snes_button() {
        let mut map = no_map();
        assert_eq!(bind_button(&mut map, "Start", Button::Mode), input::BindResult::Bound);
        assert_eq!(map.get("Start"), Some(&"Mode".to_string()));
        assert_eq!(resolve_button(&map, Button::Mode), Some("Start"));
        assert_eq!(resolve_button(&map, Button::Start), None, "the old button is free again");
        assert_eq!(resolve_button(&map, Button::Select), Some("Select"));
        // Re-binding the same button changes nothing.
        assert_eq!(bind_button(&mut map, "Start", Button::Mode), input::BindResult::Unchanged);
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn binding_a_pad_button_already_in_use_swaps_the_two_snes_buttons() {
        let mut map = no_map();
        assert_eq!(bind_button(&mut map, "A", Button::South), input::BindResult::Swapped("B"));
        assert_eq!(current_buttons(&map, "A"), vec![Button::South]);
        assert_eq!(current_buttons(&map, "B"), vec![Button::East], "B takes A's former button");
        let mut state = PadState::default();
        press(&mut state, Button::South);
        let joypad = state.joypad(&map);
        assert!(joypad.a && !joypad.b);
        // An unknown SNES button name writes nothing.
        assert_eq!(bind_button(&mut map, "Turbo", Button::Z), input::BindResult::Unchanged);
        assert_eq!(map.get("Turbo"), None);
    }

    /// A partial `pad_map` (hand-edited file, prefs from another build) must
    /// not end with two SNES buttons on one physical button: the second one
    /// would be dead and the panel would still show it bound.
    #[test]
    fn a_masked_pad_binding_is_repaired_instead_of_doubled() {
        // X explicitly holds South, which B holds by default: B is dead.
        let mut map = no_map();
        map.insert("X".to_string(), "South".to_string());
        assert_eq!(resolve_button(&map, Button::South), Some("X"));
        assert_eq!(binding_label(Lang::Fr, &map, "B"), "—", "B's claim is masked by X");

        assert_eq!(
            bind_button(&mut map, "B", Button::South),
            input::BindResult::Reverted("X")
        );
        assert_eq!(map.get("B"), Some(&"South".to_string()));
        assert_eq!(map.get("X"), None, "X goes back to its built-in button");
        assert_eq!(resolve_button(&map, Button::South), Some("B"));
        assert_eq!(resolve_button(&map, Button::North), Some("X"));
        assert_eq!(binding_label(Lang::Fr, &map, "B"), pad_label(Lang::Fr, Button::South));
        assert_eq!(binding_label(Lang::Fr, &map, "X"), pad_label(Lang::Fr, Button::North));
        // The console really sees B now, and nothing else.
        let mut state = PadState::default();
        press(&mut state, Button::South);
        let joypad = state.joypad(&map);
        assert!(joypad.b && !joypad.x);
    }

    /// Re-assigning a button that already drives that SNES button changes
    /// nothing — and in particular does not narrow L/R's two default bindings
    /// down to the one that was pressed.
    #[test]
    fn rebinding_a_default_button_to_its_own_snes_button_writes_nothing() {
        let mut map = no_map();
        assert_eq!(
            bind_button(&mut map, "L", Button::LeftTrigger),
            input::BindResult::Unchanged
        );
        assert!(map.is_empty(), "a default binding must not be narrowed: {map:?}");
        assert_eq!(current_buttons(&map, "L"), vec![Button::LeftTrigger, Button::LeftTrigger2]);
    }

    /// Swapping with a button that had two defaults hands over the first of
    /// them, and the swapped-to button ends up with exactly one binding.
    #[test]
    fn swapping_with_a_double_bound_button_hands_over_its_first_button() {
        let mut map = no_map();
        assert_eq!(
            bind_button(&mut map, "X", Button::LeftTrigger),
            input::BindResult::Swapped("L")
        );
        assert_eq!(current_buttons(&map, "X"), vec![Button::LeftTrigger]);
        assert_eq!(current_buttons(&map, "L"), vec![Button::North]);
        assert_eq!(resolve_button(&map, Button::LeftTrigger2), None);
    }

    #[test]
    fn the_binding_label_names_every_button_that_drives_it() {
        let map = no_map();
        assert_eq!(binding_label(Lang::Fr, &map, "A"), pad_label(Lang::Fr, Button::East));
        assert_eq!(
            binding_label(Lang::Fr, &map, "R"),
            format!("{} / {}", pad_label(Lang::Fr, Button::RightTrigger), pad_label(Lang::Fr, Button::RightTrigger2))
        );
        for name in input::BUTTONS {
            assert_ne!(binding_label(Lang::Fr, &map, name), "—", "{name} has no default binding");
        }
    }
}
