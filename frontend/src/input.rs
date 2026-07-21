//! Keyboard -> JoypadState mapping (Z=B, X=A, A=Y, S=X, Q=L, W=R, arrows,
//! Enter=Start, RShift=Select).

use snes_core::JoypadState;
use winit::keyboard::KeyCode;

/// Map a physical keyboard key (layout-independent scancode position, so the
/// mapping stays put on non-QWERTY layouts) to the CLI/script button name it
/// drives, or `None` if the key has no mapping.
pub fn keycode_to_button(key: KeyCode) -> Option<&'static str> {
    Some(match key {
        KeyCode::ArrowUp => "Up",
        KeyCode::ArrowDown => "Down",
        KeyCode::ArrowLeft => "Left",
        KeyCode::ArrowRight => "Right",
        KeyCode::KeyZ => "B",
        KeyCode::KeyX => "A",
        KeyCode::KeyA => "Y",
        KeyCode::KeyS => "X",
        KeyCode::KeyQ => "L",
        KeyCode::KeyW => "R",
        KeyCode::Enter => "Start",
        KeyCode::ShiftRight => "Select",
        _ => return None,
    })
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
