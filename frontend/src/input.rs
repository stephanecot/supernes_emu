//! Keyboard -> `JoypadState` mapping, and the remapping logic shared by the
//! keyboard and the gamepad (`crate::pad`).
//!
//! Three layers, all pure (no winit event loop, no `gilrs` context, no I/O), so
//! everything below is unit-tested without a screen:
//!   * the built-in mapping `DEFAULT_KEYMAP` (Z=B, X=A, A=Y, S=X, Q=L, W=R,
//!     arrows, Enter=Start, RShift=Select) and the resolution of a physical key
//!     through `prefs.keymap`;
//!   * `bind_key`, which assigns a key to a button and settles the conflict
//!     with whichever button already used it;
//!   * `Capture`, the state machine the settings panel drives while it waits
//!     for the player to press the key/button they want.
//!
//! Resolution rule, used by both devices: a button the preferences bind
//! explicitly uses that binding; a button they say nothing about falls back to
//! the built-in table. An explicit binding always wins over a default one, so
//! rebinding B to the key A used to hold cannot leave both buttons firing.

use std::collections::BTreeMap;

use snes_core::JoypadState;
use winit::keyboard::KeyCode;

/// Built-in button -> physical key mapping. Physical keys are
/// layout-independent scancode positions, so the mapping stays put on
/// non-QWERTY layouts. Button names are the ones the `--script` contract uses.
/// Single source of truth: `prefs::default_keymap` derives the persisted
/// defaults from this table, `BUTTONS` derives the button list from it, and
/// `effective_key` falls back to it for anything the player has not rebound.
pub const DEFAULT_KEYMAP: &[(&str, KeyCode)] = &[
    ("Up", KeyCode::ArrowUp),
    ("Down", KeyCode::ArrowDown),
    ("Left", KeyCode::ArrowLeft),
    ("Right", KeyCode::ArrowRight),
    ("B", KeyCode::KeyZ),
    ("A", KeyCode::KeyX),
    ("Y", KeyCode::KeyA),
    ("X", KeyCode::KeyS),
    ("L", KeyCode::KeyQ),
    ("R", KeyCode::KeyW),
    ("Start", KeyCode::Enter),
    ("Select", KeyCode::ShiftRight),
];

/// The twelve SNES buttons, in the order the settings panel lists them (the
/// D-pad first, then the diamond, the shoulders and the two menu buttons).
/// Kept as a constant rather than iterated from `DEFAULT_KEYMAP` so the names
/// are `&'static str` for the resolution functions; a test asserts the two
/// lists stay identical.
pub const BUTTONS: [&str; 12] =
    ["Up", "Down", "Left", "Right", "B", "A", "Y", "X", "L", "R", "Start", "Select"];

/// Canonical `&'static str` for a button name, or `None` when the name is not
/// one of the twelve (a typo in a hand-edited preferences file).
pub fn button_name(name: &str) -> Option<&'static str> {
    BUTTONS.iter().copied().find(|&b| b == name)
}

/// Built-in key for `name`, or `None` for an unknown button name.
pub fn default_key(name: &str) -> Option<KeyCode> {
    DEFAULT_KEYMAP.iter().find(|&&(n, _)| n == name).map(|&(_, code)| code)
}

/// The key that actually drives `name` right now: the player's binding when
/// `keymap` carries one, else the built-in default.
///
/// This is the key the button *claims*; it is not necessarily the one that
/// reaches it, since another button's explicit binding wins over it (see
/// `resolve_key`). Anything shown to the player must go through `shown_key`.
pub fn effective_key(keymap: &BTreeMap<String, KeyCode>, name: &str) -> Option<KeyCode> {
    keymap.get(name).copied().or_else(|| default_key(name))
}

/// The key to display for `name`: its binding when that key really drives this
/// button, `None` when another button claims it.
///
/// A partial keymap (an entry dropped by `prefs::de_keymap`, a hand-edited
/// file) can leave a button whose binding is masked by another one's. Showing
/// that key anyway would tell the player a button responds when it does not —
/// the panel prints a dash instead, which is also what invites them to rebind
/// it.
pub fn shown_key(keymap: &BTreeMap<String, KeyCode>, name: &str) -> Option<KeyCode> {
    let key = effective_key(keymap, name)?;
    (resolve_key(keymap, key) == Some(name)).then_some(key)
}

/// Map a physical keyboard key to the SNES button it drives, or `None` if the
/// key drives none.
///
/// Explicit bindings are searched before defaults: a key the player moved onto
/// another button stops driving the one that held it by default, instead of
/// pressing both.
pub fn resolve_key(keymap: &BTreeMap<String, KeyCode>, key: KeyCode) -> Option<&'static str> {
    for name in BUTTONS {
        if keymap.get(name) == Some(&key) {
            return Some(name);
        }
    }
    for name in BUTTONS {
        if !keymap.contains_key(name) && default_key(name) == Some(key) {
            return Some(name);
        }
    }
    None
}

/// What `bind_key`/`pad::bind_button` did, so the panel can tell the player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindResult {
    /// Nothing changed: that key already drove that button (or the button name
    /// is not one of the twelve).
    Unchanged,
    /// The key was free; only the target button changed.
    Bound,
    /// The key was in use: the two buttons swapped bindings, so no button is
    /// ever left without one (see `bind_key`).
    Swapped(&'static str),
    /// The key was in use by a button that had nothing to receive in exchange
    /// (it was claiming the very key being assigned — a masked binding from a
    /// partial keymap): its explicit entry was dropped, so it goes back to its
    /// built-in binding instead of staying on a key it no longer drives.
    Reverted(&'static str),
}

/// Assign `key` to `name`, resolving a conflict by **swapping**: the button
/// that used `key` receives whatever `name` was bound to. An emulator with a
/// silently unbound button is worse than one whose two bindings traded places,
/// and the swap is undone by repeating the same assignment on the other button.
///
/// Both sides of a swap are written explicitly into `keymap` (never left on
/// their default), so `resolve_key`'s explicit-first rule cannot resurrect the
/// old binding.
///
/// Every decision here is taken on `resolve_key` — what the key *actually*
/// drives — never on `effective_key`, which only says what a button claims. A
/// partial keymap can have two buttons claiming one key (the file omits one of
/// them and its built-in key is the other's binding); deciding on the claim
/// would then write a second entry on the same key, leave the losing button
/// dead and report `Unchanged`, so re-assigning it could not repair it either.
pub fn bind_key(
    keymap: &mut BTreeMap<String, KeyCode>,
    name: &str,
    key: KeyCode,
) -> BindResult {
    let Some(name) = button_name(name) else { return BindResult::Unchanged };
    if resolve_key(keymap, key) == Some(name) {
        // Still write it down: the button may have been on its default, and an
        // explicit entry is what protects it from a later swap.
        keymap.insert(name.to_string(), key);
        return BindResult::Unchanged;
    }
    // What this button hands over in a swap. `None` when it was claiming `key`
    // itself: giving that back to the other button would put both on `key`
    // again, which is exactly the state being repaired.
    let previous = effective_key(keymap, name).filter(|&k| k != key);
    let conflict = resolve_key(keymap, key).filter(|&other| other != name);
    keymap.insert(name.to_string(), key);
    match (conflict, previous) {
        (Some(other), Some(previous)) => {
            keymap.insert(other.to_string(), previous);
            BindResult::Swapped(other)
        }
        (Some(other), None) => {
            // Nothing to hand over: drop the other button's explicit entry so
            // it falls back to its built-in key instead of keeping a binding
            // that no longer reaches it.
            keymap.remove(other);
            BindResult::Reverted(other)
        }
        (None, _) => BindResult::Bound,
    }
}

/// Keys the application acts on itself, with the function they trigger.
///
/// The game screen's hotkeys are dispatched before the emulated pad
/// (`video::App::handle_key`), so a button bound to one of these would never
/// reach the console: the capture refuses them instead of accepting a binding
/// that does nothing. Escape is not listed — it is what cancels a capture, so
/// it can never be assigned in the first place.
pub const RESERVED_KEYS: &[(KeyCode, &str)] = &[
    (KeyCode::Tab, "accéléré"),
    (KeyCode::KeyM, "muet"),
    (KeyCode::KeyP, "pause"),
    (KeyCode::KeyN, "image suivante"),
    (KeyCode::KeyO, "ouvrir une ROM"),
    (KeyCode::KeyC, "confirmation avant de quitter"),
    (KeyCode::KeyF, "compteur d'images"),
    (KeyCode::KeyV, "filtre"),
    (KeyCode::KeyR, "ratio"),
    (KeyCode::Comma, "réglages"),
    (KeyCode::Equal, "volume +"),
    (KeyCode::NumpadAdd, "volume +"),
    (KeyCode::Minus, "volume -"),
    (KeyCode::NumpadSubtract, "volume -"),
    (KeyCode::BracketLeft, "facteur d'accéléré"),
    (KeyCode::BracketRight, "facteur d'accéléré"),
    (KeyCode::F1, "taille de la fenêtre"),
    (KeyCode::F2, "taille de la fenêtre"),
    (KeyCode::F3, "taille de la fenêtre"),
    (KeyCode::F4, "taille de la fenêtre"),
    (KeyCode::F5, "sauvegarder l'état"),
    (KeyCode::F6, "réinitialiser la console"),
    (KeyCode::F7, "slot suivant"),
    (KeyCode::F8, "exporter la musique"),
    (KeyCode::F9, "charger l'état"),
    (KeyCode::F10, "reprise instantanée"),
    (KeyCode::F11, "plein écran"),
    (KeyCode::F12, "capture d'écran"),
    (KeyCode::Digit0, "slot de sauvegarde"),
    (KeyCode::Digit1, "slot de sauvegarde"),
    (KeyCode::Digit2, "slot de sauvegarde"),
    (KeyCode::Digit3, "slot de sauvegarde"),
    (KeyCode::Digit4, "slot de sauvegarde"),
    (KeyCode::Digit5, "slot de sauvegarde"),
    (KeyCode::Digit6, "slot de sauvegarde"),
    (KeyCode::Digit7, "slot de sauvegarde"),
    (KeyCode::Digit8, "slot de sauvegarde"),
    (KeyCode::Digit9, "slot de sauvegarde"),
];

/// The application function `key` already triggers, or `None` when the key is
/// free for a pad binding.
pub fn reserved_for(key: KeyCode) -> Option<&'static str> {
    RESERVED_KEYS.iter().find(|&&(code, _)| code == key).map(|&(_, what)| what)
}

/// Human-readable name of a physical key, for the bindings list. Physical keys
/// are scancode positions, so what is drawn is the *US* legend of that
/// position — the same convention the built-in mapping is documented with.
pub fn key_label(key: KeyCode) -> String {
    let named = match key {
        KeyCode::ArrowUp => "Flèche haut",
        KeyCode::ArrowDown => "Flèche bas",
        KeyCode::ArrowLeft => "Flèche gauche",
        KeyCode::ArrowRight => "Flèche droite",
        KeyCode::Enter => "Entrée",
        KeyCode::NumpadEnter => "Entrée (pavé)",
        KeyCode::Space => "Espace",
        KeyCode::Backspace => "Retour arrière",
        KeyCode::ShiftLeft => "Maj gauche",
        KeyCode::ShiftRight => "Maj droite",
        KeyCode::ControlLeft => "Ctrl gauche",
        KeyCode::ControlRight => "Ctrl droite",
        KeyCode::AltLeft => "Alt gauche",
        KeyCode::AltRight => "Alt droite",
        KeyCode::SuperLeft => "Cmd gauche",
        KeyCode::SuperRight => "Cmd droite",
        KeyCode::CapsLock => "Verr. maj",
        KeyCode::Tab => "Tab",
        KeyCode::Escape => "Échap",
        _ => "",
    };
    if !named.is_empty() {
        return named.to_string();
    }
    // `KeyCode`'s Debug is its winit variant name: `KeyZ`, `Digit4`,
    // `Numpad7`, `Semicolon`… Trim the family prefix so a letter reads as the
    // letter itself, and leave everything else as winit names it.
    let raw = format!("{key:?}");
    for prefix in ["Key", "Digit"] {
        if let Some(rest) = raw.strip_prefix(prefix) {
            return rest.to_string();
        }
    }
    if let Some(rest) = raw.strip_prefix("Numpad") {
        return format!("Pavé {rest}");
    }
    raw
}

/// Which device a capture is waiting on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Device {
    Keyboard,
    Gamepad,
}

/// Outcome of a key press while a capture is pending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Captured {
    /// Not capturing, or the press carries nothing for this capture (a key
    /// pressed while the capture waits for a *controller* button, which stays
    /// pending so the player can still reach for the pad).
    Ignored,
    /// Escape: the capture was abandoned, nothing changed.
    Cancelled,
    /// The key drives an application function (`RESERVED_KEYS`) and was
    /// refused; the capture stays pending so another key can be pressed.
    Reserved(&'static str),
    /// Assign `key` to `button`.
    Key { button: &'static str, key: KeyCode },
}

/// The settings panel's "press a key…" state machine.
///
/// Pure state: the panel starts a capture, the event loop feeds it the very
/// first key (or controller button) that arrives, and the resulting binding is
/// written to the preferences by the caller. Held here rather than in the panel
/// widget because the key that ends a capture is intercepted *before* the
/// application's own shortcuts, which happens in the event loop.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capture {
    pending: Option<(&'static str, Device)>,
    /// Last thing worth telling the player (conflict swapped, key refused);
    /// shown under the bindings list.
    pub notice: Option<String>,
}

impl Capture {
    /// Button and device currently awaited, if any.
    pub fn pending(&self) -> Option<(&'static str, Device)> {
        self.pending
    }

    /// True while a key/button press must be routed here instead of to the
    /// application's shortcuts and the emulated pad.
    pub fn is_active(&self) -> bool {
        self.pending.is_some()
    }

    /// Button awaited on `device`, for the panel to highlight its row.
    pub fn waiting_for(&self, device: Device) -> Option<&'static str> {
        self.pending.filter(|&(_, d)| d == device).map(|(button, _)| button)
    }

    /// Start (or move) the capture. An unknown button name is refused, so the
    /// panel cannot put the state machine in a state the resolution functions
    /// don't know.
    pub fn start(&mut self, button: &str, device: Device) {
        let Some(button) = button_name(button) else { return };
        self.pending = Some((button, device));
        self.notice = None;
    }

    /// Abandon the capture (Escape, closing the panel, a modal taking over).
    pub fn cancel(&mut self) {
        self.pending = None;
    }

    /// Feed one key press to the capture.
    pub fn on_key(&mut self, key: KeyCode) -> Captured {
        let Some((button, device)) = self.pending else { return Captured::Ignored };
        if key == KeyCode::Escape {
            self.pending = None;
            self.notice = None;
            return Captured::Cancelled;
        }
        // A capture aimed at the controller ignores the keyboard (except the
        // Escape above, which must always be a way out).
        if device != Device::Keyboard {
            return Captured::Ignored;
        }
        if let Some(what) = reserved_for(key) {
            self.notice = Some(format!(
                "{} est déjà un raccourci de l'application ({what}).",
                key_label(key)
            ));
            return Captured::Reserved(what);
        }
        self.pending = None;
        Captured::Key { button, key }
    }

    /// Consume a controller-button capture: returns the SNES button that was
    /// waiting for it, and ends the capture. `None` when the capture is not
    /// aimed at the controller.
    pub fn take_gamepad(&mut self) -> Option<&'static str> {
        let button = self.waiting_for(Device::Gamepad)?;
        self.pending = None;
        Some(button)
    }
}

/// Set a button on a JoypadState by its CLI/script name. Names are the ones
/// the --script contract uses: A B X Y L R Start Select Up Down Left Right.
pub fn set_button(state: &mut JoypadState, name: &str, pressed: bool) -> Result<(), String> {
    match name {
        "A" => state.a = pressed,
        "B" => state.b = pressed,
        "X" => state.x = pressed,
        "Y" => state.y = pressed,
        "L" => state.l = pressed,
        "R" => state.r = pressed,
        "Start" => state.start = pressed,
        "Select" => state.select = pressed,
        "Up" => state.up = pressed,
        "Down" => state.down = pressed,
        "Left" => state.left = pressed,
        "Right" => state.right = pressed,
        _ => return Err(format!("unknown button name: {name}")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A keymap holding exactly the built-in bindings, like a fresh
    /// preferences file (`prefs::default_keymap`).
    fn full_default() -> BTreeMap<String, KeyCode> {
        DEFAULT_KEYMAP.iter().map(|&(name, code)| (name.to_string(), code)).collect()
    }

    #[test]
    fn the_button_list_matches_the_built_in_table() {
        let from_table: Vec<&str> = DEFAULT_KEYMAP.iter().map(|&(name, _)| name).collect();
        assert_eq!(BUTTONS.to_vec(), from_table);
        for name in BUTTONS {
            assert_eq!(button_name(name), Some(name));
            assert!(default_key(name).is_some(), "{name} has no built-in key");
            // Every name must be one `set_button` accepts, or a binding would
            // resolve to a button the console cannot press.
            assert!(set_button(&mut JoypadState::default(), name, true).is_ok());
        }
        assert_eq!(button_name("Turbo"), None);
        assert_eq!(default_key("Turbo"), None);
    }

    #[test]
    fn an_empty_keymap_falls_back_to_every_built_in_key() {
        let empty = BTreeMap::new();
        for &(name, code) in DEFAULT_KEYMAP {
            assert_eq!(effective_key(&empty, name), Some(code), "{name}");
            assert_eq!(resolve_key(&empty, code), Some(name), "{name}");
        }
        assert_eq!(resolve_key(&empty, KeyCode::Space), None);
    }

    #[test]
    fn a_full_default_keymap_resolves_exactly_like_the_built_in_table() {
        let map = full_default();
        for &(name, code) in DEFAULT_KEYMAP {
            assert_eq!(resolve_key(&map, code), Some(name));
            assert_eq!(effective_key(&map, name), Some(code));
        }
    }

    #[test]
    fn a_rebound_button_answers_to_its_new_key_and_the_others_keep_theirs() {
        let mut map = BTreeMap::new();
        map.insert("A".to_string(), KeyCode::Space);
        assert_eq!(resolve_key(&map, KeyCode::Space), Some("A"));
        assert_eq!(effective_key(&map, "A"), Some(KeyCode::Space));
        // The key A held by default is now free…
        assert_eq!(resolve_key(&map, KeyCode::KeyX), None);
        // …and every button the file says nothing about still answers to its
        // built-in key.
        assert_eq!(resolve_key(&map, KeyCode::KeyZ), Some("B"));
        assert_eq!(resolve_key(&map, KeyCode::ArrowUp), Some("Up"));
    }

    /// The rule that keeps a hand-edited file unambiguous: a key explicitly
    /// bound to one button must not *also* press the button that holds it by
    /// default.
    #[test]
    fn an_explicit_binding_wins_over_a_default_one() {
        let mut map = BTreeMap::new();
        // X (default S) takes Z, which B holds by default.
        map.insert("X".to_string(), KeyCode::KeyZ);
        assert_eq!(resolve_key(&map, KeyCode::KeyZ), Some("X"));
        // B is then left without a key at all rather than sharing one.
        assert_eq!(effective_key(&map, "B"), Some(KeyCode::KeyZ));
        assert_ne!(resolve_key(&map, KeyCode::KeyZ), Some("B"));
    }

    /// What the panel prints must be what the console answers to: a claim
    /// another button won is shown as nothing at all.
    #[test]
    fn the_shown_key_is_the_one_that_really_drives_the_button() {
        let map = full_default();
        for &(name, code) in DEFAULT_KEYMAP {
            assert_eq!(shown_key(&map, name), Some(code), "{name}");
        }
        let empty = BTreeMap::new();
        for &(name, code) in DEFAULT_KEYMAP {
            assert_eq!(shown_key(&empty, name), Some(code), "{name}");
        }
        // X takes the key B holds by default: B has nothing to show.
        let mut map = BTreeMap::new();
        map.insert("X".to_string(), KeyCode::KeyZ);
        assert_eq!(shown_key(&map, "X"), Some(KeyCode::KeyZ));
        assert_eq!(shown_key(&map, "B"), None);
        assert_eq!(shown_key(&map, "Turbo"), None);
    }

    #[test]
    fn binding_a_free_key_touches_only_that_button() {
        let mut map = full_default();
        assert_eq!(bind_key(&mut map, "A", KeyCode::Space), BindResult::Bound);
        assert_eq!(map.get("A"), Some(&KeyCode::Space));
        assert_eq!(resolve_key(&map, KeyCode::Space), Some("A"));
        assert_eq!(resolve_key(&map, KeyCode::KeyX), None, "the old key is free again");
        // Every other button is untouched.
        for &(name, code) in DEFAULT_KEYMAP {
            if name != "A" {
                assert_eq!(effective_key(&map, name), Some(code), "{name}");
            }
        }
    }

    #[test]
    fn binding_a_key_already_in_use_swaps_the_two_buttons() {
        let mut map = full_default();
        // A takes B's key: the two trade places, so neither is left unbound.
        assert_eq!(bind_key(&mut map, "A", KeyCode::KeyZ), BindResult::Swapped("B"));
        assert_eq!(effective_key(&map, "A"), Some(KeyCode::KeyZ));
        assert_eq!(effective_key(&map, "B"), Some(KeyCode::KeyX));
        assert_eq!(resolve_key(&map, KeyCode::KeyZ), Some("A"));
        assert_eq!(resolve_key(&map, KeyCode::KeyX), Some("B"));
        // Repeating the same assignment on the other button undoes the swap.
        assert_eq!(bind_key(&mut map, "B", KeyCode::KeyZ), BindResult::Swapped("A"));
        assert_eq!(effective_key(&map, "B"), Some(KeyCode::KeyZ));
        assert_eq!(effective_key(&map, "A"), Some(KeyCode::KeyX));
    }

    /// A conflict with a button still sitting on its *default* key must swap
    /// too, and both sides must end up written down explicitly — otherwise the
    /// default would resurface and two buttons would answer to one key.
    #[test]
    fn a_conflict_with_a_default_binding_is_written_down_on_both_sides() {
        let mut map = BTreeMap::new();
        assert_eq!(bind_key(&mut map, "X", KeyCode::KeyZ), BindResult::Swapped("B"));
        assert_eq!(map.get("X"), Some(&KeyCode::KeyZ));
        assert_eq!(map.get("B"), Some(&KeyCode::KeyS), "B receives X's former key");
        assert_eq!(resolve_key(&map, KeyCode::KeyZ), Some("X"));
        assert_eq!(resolve_key(&map, KeyCode::KeyS), Some("B"));
    }

    /// A partial keymap — the case a dropped entry or a hand-edited file
    /// produces — must not end with two explicit entries on one key: the
    /// second button would be dead and the panel would still show it bound.
    #[test]
    fn a_masked_binding_is_repaired_instead_of_doubled() {
        // X explicitly holds Z, which B (absent from the file) holds by
        // default: B is currently dead.
        let mut map = BTreeMap::new();
        map.insert("X".to_string(), KeyCode::KeyZ);
        assert_eq!(resolve_key(&map, KeyCode::KeyZ), Some("X"));
        assert_eq!(shown_key(&map, "B"), None, "B's claim is masked by X");

        // Assigning Z to B must take it from X, not add a second entry on Z.
        assert_eq!(bind_key(&mut map, "B", KeyCode::KeyZ), BindResult::Reverted("X"));
        assert_eq!(map.get("B"), Some(&KeyCode::KeyZ));
        assert_eq!(map.get("X"), None, "X goes back to its built-in key");
        assert_eq!(resolve_key(&map, KeyCode::KeyZ), Some("B"));
        assert_eq!(resolve_key(&map, KeyCode::KeyS), Some("X"));
        assert_eq!(shown_key(&map, "B"), Some(KeyCode::KeyZ));
        assert_eq!(shown_key(&map, "X"), Some(KeyCode::KeyS));
        // No key drives two buttons.
        let mut driven: Vec<&str> = BUTTONS
            .iter()
            .filter_map(|&n| effective_key(&map, n).map(|k| (n, k)))
            .filter(|&(n, k)| resolve_key(&map, k) == Some(n))
            .map(|(n, _)| n)
            .collect();
        driven.sort_unstable();
        driven.dedup();
        assert_eq!(driven.len(), BUTTONS.len(), "every button must answer to something");
    }

    /// The other way round: rebinding the *masked* button repairs it too, and
    /// the button that claimed the key keeps it.
    #[test]
    fn a_masked_button_can_be_moved_to_a_free_key() {
        let mut map = BTreeMap::new();
        map.insert("X".to_string(), KeyCode::KeyZ);
        assert_eq!(bind_key(&mut map, "B", KeyCode::Space), BindResult::Bound);
        assert_eq!(resolve_key(&map, KeyCode::Space), Some("B"));
        assert_eq!(resolve_key(&map, KeyCode::KeyZ), Some("X"));
        assert_eq!(shown_key(&map, "B"), Some(KeyCode::Space));
    }

    #[test]
    fn rebinding_a_button_to_the_key_it_already_has_changes_nothing() {
        let mut map = full_default();
        assert_eq!(bind_key(&mut map, "A", KeyCode::KeyX), BindResult::Unchanged);
        assert_eq!(map, full_default());
        // Same on a button still on its default: the entry is materialised but
        // the resolution is identical.
        let mut map = BTreeMap::new();
        assert_eq!(bind_key(&mut map, "A", KeyCode::KeyX), BindResult::Unchanged);
        assert_eq!(map.get("A"), Some(&KeyCode::KeyX));
        // An unknown button name is refused instead of creating a phantom entry.
        assert_eq!(bind_key(&mut map, "Turbo", KeyCode::KeyG), BindResult::Unchanged);
        assert_eq!(map.get("Turbo"), None);
    }

    #[test]
    fn no_built_in_binding_collides_with_an_application_shortcut() {
        for &(name, code) in DEFAULT_KEYMAP {
            assert_eq!(reserved_for(code), None, "default key of {name} is a shortcut");
        }
        assert_eq!(reserved_for(KeyCode::F11), Some("plein écran"));
        assert_eq!(reserved_for(KeyCode::Tab), Some("accéléré"));
        assert_eq!(reserved_for(KeyCode::Digit7), Some("slot de sauvegarde"));
        assert_eq!(reserved_for(KeyCode::Space), None);
        // Escape is deliberately absent: it is what cancels a capture, so it
        // can never reach the reserved check.
        assert_eq!(reserved_for(KeyCode::Escape), None);
    }

    #[test]
    fn key_labels_read_as_the_key_and_never_as_a_winit_variant() {
        assert_eq!(key_label(KeyCode::KeyZ), "Z");
        assert_eq!(key_label(KeyCode::Digit4), "4");
        assert_eq!(key_label(KeyCode::Enter), "Entrée");
        assert_eq!(key_label(KeyCode::ShiftRight), "Maj droite");
        assert_eq!(key_label(KeyCode::ArrowUp), "Flèche haut");
        assert_eq!(key_label(KeyCode::Numpad7), "Pavé 7");
        // Anything without a French name keeps winit's own, which is still
        // readable, rather than showing nothing.
        assert_eq!(key_label(KeyCode::Semicolon), "Semicolon");
        for &(_, code) in DEFAULT_KEYMAP {
            assert!(!key_label(code).is_empty());
        }
    }

    #[test]
    fn a_fresh_capture_waits_for_nothing() {
        let capture = Capture::default();
        assert!(!capture.is_active());
        assert_eq!(capture.pending(), None);
        assert_eq!(capture.waiting_for(Device::Keyboard), None);
        assert_eq!(capture.notice, None);
        // A key arriving while nothing is captured is not swallowed.
        let mut capture = Capture::default();
        assert_eq!(capture.on_key(KeyCode::KeyZ), Captured::Ignored);
        assert_eq!(capture.take_gamepad(), None);
    }

    #[test]
    fn a_keyboard_capture_takes_the_first_key_and_ends() {
        let mut capture = Capture::default();
        capture.start("A", Device::Keyboard);
        assert!(capture.is_active());
        assert_eq!(capture.waiting_for(Device::Keyboard), Some("A"));
        assert_eq!(capture.waiting_for(Device::Gamepad), None);
        assert_eq!(
            capture.on_key(KeyCode::Space),
            Captured::Key { button: "A", key: KeyCode::Space }
        );
        assert!(!capture.is_active(), "the capture ends on the first key");
        // The next key is no longer swallowed.
        assert_eq!(capture.on_key(KeyCode::KeyZ), Captured::Ignored);
    }

    #[test]
    fn escape_cancels_a_capture_on_either_device() {
        for device in [Device::Keyboard, Device::Gamepad] {
            let mut capture = Capture::default();
            capture.start("Start", device);
            capture.notice = Some("…".to_string());
            assert_eq!(capture.on_key(KeyCode::Escape), Captured::Cancelled);
            assert!(!capture.is_active());
            assert_eq!(capture.notice, None);
        }
    }

    /// The whole point of routing keys through the capture *before* the
    /// application's shortcuts: F11 pressed in capture must not toggle
    /// fullscreen. It is refused (it would never reach the pad) and the capture
    /// stays open so another key can be pressed.
    #[test]
    fn a_shortcut_key_is_refused_and_the_capture_stays_open() {
        let mut capture = Capture::default();
        capture.start("A", Device::Keyboard);
        assert_eq!(capture.on_key(KeyCode::F11), Captured::Reserved("plein écran"));
        assert!(capture.is_active(), "the player must be able to press another key");
        let notice = capture.notice.clone().expect("a refusal must be explained");
        assert!(notice.contains("plein écran"), "{notice}");
        // A free key still lands afterwards, and clears nothing behind it.
        assert_eq!(
            capture.on_key(KeyCode::Space),
            Captured::Key { button: "A", key: KeyCode::Space }
        );
    }

    #[test]
    fn a_gamepad_capture_ignores_the_keyboard_and_yields_its_button_once() {
        let mut capture = Capture::default();
        capture.start("L", Device::Gamepad);
        assert_eq!(capture.waiting_for(Device::Gamepad), Some("L"));
        assert_eq!(capture.waiting_for(Device::Keyboard), None);
        assert_eq!(capture.on_key(KeyCode::Space), Captured::Ignored);
        assert!(capture.is_active(), "a stray key must not end a pad capture");
        assert_eq!(capture.take_gamepad(), Some("L"));
        assert!(!capture.is_active());
        assert_eq!(capture.take_gamepad(), None);
    }

    #[test]
    fn a_capture_only_ever_targets_a_real_button() {
        let mut capture = Capture::default();
        capture.start("Turbo", Device::Keyboard);
        assert!(!capture.is_active());
        // Starting a second capture moves the target instead of stacking.
        capture.start("A", Device::Keyboard);
        capture.start("B", Device::Gamepad);
        assert_eq!(capture.pending(), Some(("B", Device::Gamepad)));
        capture.cancel();
        assert!(!capture.is_active());
    }
}
