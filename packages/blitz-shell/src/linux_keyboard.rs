use accesskit_xplat::{Adapter, KeyEvent};
use std::{
    collections::HashSet,
    ffi::{CStr, CString},
    time::Instant,
};
use winit::{
    event::{Ime, WindowEvent},
    keyboard::{Key, KeyLocation, ModifiersState, NamedKey, PhysicalKey},
    platform::scancode::PhysicalKeyExtScancode,
};

#[derive(Default)]
pub(super) struct Keyboard {
    modifiers: ModifiersState,
    consumed: HashSet<PhysicalKey>,
    protected: HashSet<PhysicalKey>,
    suppress_ime: bool,
    clock: Option<Instant>,
    timestamp: u32,
}

impl Keyboard {
    pub(super) fn process(
        &mut self,
        adapter: &Adapter,
        event: &WindowEvent,
        protected: bool,
    ) -> bool {
        match event {
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::Focused(false) => {
                self.consumed.clear();
                self.suppress_ime = false;
                self.modifiers = ModifiersState::empty();
            }
            WindowEvent::Ime(Ime::Commit(_) | Ime::Preedit(_, _)) => return self.suppress_ime,
            WindowEvent::KeyboardInput {
                event,
                is_synthetic,
                ..
            } => {
                if *is_synthetic {
                    return false;
                }
                let pressed = event.state.is_pressed();
                let protected = self.protected_key(event.physical_key, pressed, protected);
                let consumed = if adapter.has_key_listeners() {
                    let elapsed = self
                        .clock
                        .get_or_insert_with(Instant::now)
                        .elapsed()
                        .as_millis() as u32;
                    self.timestamp = elapsed.max(self.timestamp.wrapping_add(1));
                    key_event(
                        &event.logical_key,
                        event.location,
                        event.physical_key.to_scancode(),
                        pressed,
                        self.modifiers,
                        self.timestamp,
                        protected,
                    )
                    .is_some_and(|event| adapter.forward_key_event(event))
                } else {
                    false
                };
                return self.filter_key(event.physical_key, pressed, consumed);
            }
            _ => {}
        }
        false
    }

    fn filter_key(&mut self, key: PhysicalKey, pressed: bool, consumed: bool) -> bool {
        if pressed {
            let consumed = consumed || self.consumed.contains(&key);
            self.suppress_ime = consumed;
            if consumed {
                self.consumed.insert(key);
            }
            consumed
        } else {
            self.consumed.remove(&key) || consumed
        }
    }

    fn protected_key(&mut self, key: PhysicalKey, pressed: bool, protected: bool) -> bool {
        if pressed {
            if protected {
                self.protected.insert(key);
            }
            protected || self.protected.contains(&key)
        } else {
            self.protected.remove(&key) || protected
        }
    }
}

fn key_event(
    key: &Key,
    location: KeyLocation,
    scancode: Option<u32>,
    pressed: bool,
    modifiers: ModifiersState,
    timestamp: u32,
    protected: bool,
) -> Option<KeyEvent> {
    let xkb = xkbcommon_dl::xkbcommon_option()?;
    let right = location == KeyLocation::Right;
    let name = match key {
        Key::Character(text) => {
            let mut chars = text.chars();
            let ch = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            if location == KeyLocation::Numpad {
                match ch {
                    '0'..='9' => format!("KP_{ch}"),
                    '+' => "KP_Add".into(),
                    '-' => "KP_Subtract".into(),
                    '*' => "KP_Multiply".into(),
                    '/' => "KP_Divide".into(),
                    '.' => "KP_Decimal".into(),
                    ',' => "KP_Separator".into(),
                    _ => format!("U{:04X}", ch as u32),
                }
            } else {
                format!("U{:04X}", ch as u32)
            }
        }
        Key::Named(named) => {
            let name = match named {
                NamedKey::ArrowUp => "Up",
                NamedKey::ArrowDown => "Down",
                NamedKey::ArrowLeft => "Left",
                NamedKey::ArrowRight => "Right",
                NamedKey::Backspace => "BackSpace",
                NamedKey::PageUp => "Page_Up",
                NamedKey::PageDown => "Page_Down",
                NamedKey::Enter => {
                    if location == KeyLocation::Numpad {
                        "Enter"
                    } else {
                        "Return"
                    }
                }
                NamedKey::CapsLock => "Caps_Lock",
                NamedKey::NumLock => "Num_Lock",
                NamedKey::ScrollLock => "Scroll_Lock",
                NamedKey::PrintScreen => "Print",
                NamedKey::ContextMenu => "Menu",
                NamedKey::Control => {
                    if right {
                        "Control_R"
                    } else {
                        "Control_L"
                    }
                }
                NamedKey::Shift => {
                    if right {
                        "Shift_R"
                    } else {
                        "Shift_L"
                    }
                }
                NamedKey::Alt => {
                    if right {
                        "Alt_R"
                    } else {
                        "Alt_L"
                    }
                }
                NamedKey::Meta => {
                    if right {
                        "Super_R"
                    } else {
                        "Super_L"
                    }
                }
                NamedKey::AltGraph => "ISO_Level3_Shift",
                _ => "",
            };
            let name = if name.is_empty() {
                format!("{named:?}")
            } else {
                name.to_string()
            };
            if location == KeyLocation::Numpad {
                format!("KP_{name}")
            } else {
                name
            }
        }
        _ => return None,
    };
    let name = CString::new(name).ok()?;
    // The name is NUL terminated and the returned symbol is an integer.
    let id = unsafe {
        (xkb.xkb_keysym_from_name)(
            name.as_ptr(),
            xkbcommon_dl::xkb_keysym_flags::XKB_KEYSYM_NO_FLAGS,
        )
    };
    if id == 0 {
        return None;
    }
    let mut buffer = [0i8; 128];
    // libxkbcommon writes at most buffer.len() bytes, including the terminator.
    let length = unsafe { (xkb.xkb_keysym_get_name)(id, buffer.as_mut_ptr(), buffer.len()) };
    if length < 0 || length as usize >= buffer.len() {
        return None;
    }
    // A successful xkb_keysym_get_name writes a NUL terminated string.
    let symbol_name = unsafe { CStr::from_ptr(buffer.as_ptr()) }.to_str().ok()?;
    let text = match key {
        Key::Character(text) => Some(text.as_str()),
        _ => None,
    };
    let mut event = KeyEvent {
        event_type: u32::from(!pressed),
        id: id as i32,
        hw_code: scancode
            .and_then(|code| code.checked_add(8))
            .and_then(|code| i16::try_from(code).ok())
            .unwrap_or(0),
        modifiers: i16::from(modifiers.shift_key())
            | (i16::from(modifiers.control_key()) << 2)
            | (i16::from(modifiers.alt_key()) << 3)
            | (i16::from(modifiers.meta_key()) << 6),
        timestamp: timestamp as i32,
        event_string: text.unwrap_or(symbol_name).to_string(),
        is_text: true,
    };
    if protected && text.is_some() {
        event.event_string.clear();
    }
    Some(event)
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::KeyCode;
    #[test]
    fn ordinary_special_and_protected_payloads() {
        assert!(
            key_event(
                &Key::Character("".into()),
                KeyLocation::Standard,
                None,
                true,
                ModifiersState::empty(),
                100,
                false
            )
            .is_none()
        );
        assert!(
            key_event(
                &Key::Character("ab".into()),
                KeyLocation::Standard,
                None,
                true,
                ModifiersState::empty(),
                100,
                false
            )
            .is_none()
        );
        let event = key_event(
            &Key::Character("a".into()),
            KeyLocation::Standard,
            Some(30),
            true,
            ModifiersState::CONTROL,
            100,
            false,
        )
        .unwrap();
        assert_eq!(
            (event.id, event.hw_code, event.modifiers, event.timestamp),
            (97, 38, 4, 100)
        );
        assert_eq!(event.event_string, "a");
        for (key, location, id) in [
            (NamedKey::Tab, KeyLocation::Standard, 0xff09),
            (NamedKey::ArrowUp, KeyLocation::Numpad, 0xff97),
            (NamedKey::Delete, KeyLocation::Standard, 0xffff),
            (NamedKey::Control, KeyLocation::Left, 0xffe3),
        ] {
            assert_eq!(
                key_event(
                    &Key::Named(key),
                    location,
                    Some(1),
                    false,
                    ModifiersState::empty(),
                    101,
                    false
                )
                .unwrap()
                .id,
                id
            );
        }
        let protected = key_event(
            &Key::Character("q".into()),
            KeyLocation::Standard,
            Some(16),
            true,
            ModifiersState::empty(),
            100,
            true,
        )
        .unwrap();
        assert!(protected.event_string.is_empty());
        assert_eq!(protected.id, 113);
        let invalid_code = key_event(
            &Key::Character("a".into()),
            KeyLocation::Standard,
            Some(u32::MAX),
            true,
            ModifiersState::empty(),
            100,
            false,
        )
        .unwrap();
        assert_eq!(invalid_code.hw_code, 0);
    }
    #[test]
    fn consumed_press_repeat_release_and_ime() {
        let mut keyboard = Keyboard::default();
        let a = PhysicalKey::Code(KeyCode::KeyA);
        assert!(!keyboard.filter_key(a, true, false));
        assert!(!keyboard.suppress_ime);
        assert!(keyboard.filter_key(a, true, true));
        assert!(keyboard.suppress_ime);
        assert!(keyboard.filter_key(a, true, false));
        assert!(keyboard.filter_key(a, false, false));
        assert!(keyboard.suppress_ime);
        assert!(!keyboard.filter_key(a, true, false));
        assert!(!keyboard.suppress_ime);
    }

    #[test]
    fn protected_press_stays_redacted_after_focus_changes() {
        let mut keyboard = Keyboard::default();
        let key = PhysicalKey::Code(KeyCode::KeyA);
        assert!(keyboard.protected_key(key, true, true));
        assert!(keyboard.protected_key(key, true, false));
        assert!(keyboard.protected_key(key, false, false));
        assert!(!keyboard.protected_key(key, true, false));
    }
}
