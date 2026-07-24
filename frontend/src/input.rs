//! Keyboard -> JoypadState mapping (Z=B, X=A, A=Y, S=X, Q=L, W=R, arrows,
//! Enter=Start, RShift=Select).

use snes_core::JoypadState;
use winit::keyboard::KeyCode;

/// Built-in button -> physical key mapping. Physical keys are
/// layout-independent scancode positions, so the mapping stays put on
/// non-QWERTY layouts. Button names are the ones the `--script` contract uses.
/// Single source of truth: `prefs::default_keymap` derives the persisted
/// defaults from this table.
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

/// Map a physical keyboard key to the CLI/script button name it drives, or
/// `None` if the key has no mapping.
pub fn keycode_to_button(key: KeyCode) -> Option<&'static str> {
    DEFAULT_KEYMAP.iter().find(|&&(_, code)| code == key).map(|&(name, _)| name)
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
