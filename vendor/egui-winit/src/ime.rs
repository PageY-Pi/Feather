use std::collections::HashSet;
use winit::{
    event::ElementState,
    keyboard::{Key, PhysicalKey},
};

#[derive(Default)]
pub(crate) struct ProcessedKeys {
    pressed: HashSet<PhysicalKey>,
}

impl ProcessedKeys {
    pub(crate) fn clear(&mut self) {
        self.pressed.clear();
    }

    /// Returns true when the event was already consumed by the platform IME.
    ///
    /// Windows reports IME-consumed key presses as `NamedKey::Process`
    /// (`VK_PROCESSKEY`). Releases retain their original logical key, so the
    /// physical key is remembered until its matching release arrives.
    pub(crate) fn consume(
        &mut self,
        ime_allowed: bool,
        physical_key: PhysicalKey,
        logical_key: &Key,
        state: ElementState,
    ) -> bool {
        if !ime_allowed {
            self.pressed.remove(&physical_key);
            return false;
        }

        if matches!(logical_key, Key::Named(winit::keyboard::NamedKey::Process)) {
            if state == ElementState::Pressed {
                self.pressed.insert(physical_key);
            }
            return true;
        }

        if state == ElementState::Released && self.pressed.remove(&physical_key) {
            return true;
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::{KeyCode, NamedKey};

    fn process_key() -> Key {
        Key::Named(NamedKey::Process)
    }

    #[test]
    fn popup_ime_backspace_press_and_release_are_filtered() {
        let mut keys = ProcessedKeys::default();
        let physical = PhysicalKey::Code(KeyCode::Backspace);
        assert!(keys.consume(true, physical, &process_key(), ElementState::Pressed));
        assert!(keys.consume(
            true,
            physical,
            &Key::Named(NamedKey::Backspace),
            ElementState::Released,
        ));
    }

    #[test]
    fn ordinary_backspace_is_not_filtered() {
        let mut keys = ProcessedKeys::default();
        let physical = PhysicalKey::Code(KeyCode::Backspace);
        let key = Key::Named(NamedKey::Backspace);
        assert!(!keys.consume(true, physical, &key, ElementState::Pressed));
        assert!(!keys.consume(true, physical, &key, ElementState::Released));
    }

    #[test]
    fn tracking_clears_when_ime_is_disabled() {
        let mut keys = ProcessedKeys::default();
        let physical = PhysicalKey::Code(KeyCode::Backspace);
        assert!(keys.consume(true, physical, &process_key(), ElementState::Pressed));
        assert!(!keys.consume(false, physical, &Key::Named(NamedKey::Backspace), ElementState::Released));
        assert!(!keys.consume(true, physical, &Key::Named(NamedKey::Backspace), ElementState::Released));
    }
}
