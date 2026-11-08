use accesskit_xplat::{Adapter, KeyEvent};
use std::{
    collections::{HashMap, HashSet},
    ffi::{CStr, CString},
    time::Instant,
};
use winit::{
    event::{Ime, WindowEvent},
    keyboard::{Key, KeyCode, KeyLocation, ModifiersState, NamedKey, PhysicalKey},
    platform::scancode::PhysicalKeyExtScancode,
};

#[derive(Default)]
pub(super) struct Keyboard {
    modifiers: ModifiersState,
    consumed: HashSet<PhysicalKey>,
    decisions: HashMap<PhysicalKey, Decision>,
    forwarded: HashSet<PhysicalKey>,
    held_modifiers: HashMap<PhysicalKey, Modifier>,
    suppress_ime: bool,
    clock: Option<Instant>,
    timestamp: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Modifier {
    Command,
    Orca,
    AltGraph,
}

struct Decision {
    protected: bool,
    forward: bool,
}

impl Keyboard {
    /// Protected printable presses never reach AT, including their repeats/releases.
    /// Ctrl/Super (nonprinting text) and held Orca modifiers are explicit command exceptions.
    /// Suppression is sticky; repeats can revoke permission as focus/modifiers change.
    /// Forwarded navigation/command IDs and hardware codes remain visible on the AT bus.
    fn should_forward(
        &mut self,
        physical: PhysicalKey,
        logical: &Key,
        text: Option<&str>,
        pressed: bool,
        repeat: bool,
        protected: bool,
    ) -> bool {
        let modifier = if matches!(logical, Key::Named(NamedKey::AltGraph)) {
            Some(Modifier::AltGraph)
        } else if matches!(
            physical,
            PhysicalKey::Code(KeyCode::Insert | KeyCode::CapsLock)
        ) || matches!(logical, Key::Named(NamedKey::Insert | NamedKey::CapsLock))
        {
            Some(Modifier::Orca)
        } else if matches!(
            physical,
            PhysicalKey::Code(
                KeyCode::ControlLeft
                    | KeyCode::ControlRight
                    | KeyCode::MetaLeft
                    | KeyCode::MetaRight
            )
        ) || matches!(logical, Key::Named(NamedKey::Control | NamedKey::Meta))
        {
            Some(Modifier::Command)
        } else {
            None
        };
        if !pressed {
            self.held_modifiers.remove(&physical);
        } else if let Some(modifier) = modifier {
            self.held_modifiers.insert(physical, modifier);
        }
        if !pressed {
            self.decisions
                .remove(&physical)
                .is_some_and(|decision| decision.forward)
        } else {
            let printable_text = text.is_some_and(|text| text.chars().any(|ch| !ch.is_control()));
            let printable = matches!(logical, Key::Character(_) | Key::Dead(_))
                || printable_text
                || matches!(logical, Key::Named(named) if format!("{named:?}") == "Space");
            let held = |modifier| self.held_modifiers.values().any(|held| *held == modifier);
            let command = !held(Modifier::AltGraph)
                && (held(Modifier::Orca) || (held(Modifier::Command) && !printable_text));
            if repeat {
                self.decisions.get_mut(&physical).is_some_and(|decision| {
                    decision.forward &= !(protected || decision.protected) || !printable || command;
                    decision.forward
                })
            } else {
                let forward = !protected || !printable || command;
                self.decisions
                    .insert(physical, Decision { protected, forward });
                forward
            }
        }
    }
    pub(super) fn process(
        &mut self,
        adapter: &Adapter,
        event: &WindowEvent,
        protected: bool,
    ) -> bool {
        self.process_with(
            event,
            protected,
            || adapter.has_key_listeners(),
            |event| adapter.forward_key_event(event),
        )
    }

    fn process_with(
        &mut self,
        event: &WindowEvent,
        protected: bool,
        mut has_listeners: impl FnMut() -> bool,
        mut forward: impl FnMut(KeyEvent) -> bool,
    ) -> bool {
        match event {
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::Focused(false) => {
                self.consumed.clear();
                self.suppress_ime = false;
                self.modifiers = ModifiersState::empty();
                self.held_modifiers.clear();
                self.decisions.clear();
                self.forwarded.clear();
            }
            WindowEvent::Ime(Ime::Commit(_) | Ime::Preedit(_, _)) => return self.suppress_ime,
            WindowEvent::KeyboardInput {
                event,
                is_synthetic,
                ..
            } => {
                if *is_synthetic {
                    if !event.state.is_pressed() {
                        self.held_modifiers.remove(&event.physical_key);
                        self.decisions.remove(&event.physical_key);
                        self.forwarded.remove(&event.physical_key);
                        self.consumed.remove(&event.physical_key);
                    }
                    return false;
                }
                let pressed = event.state.is_pressed();
                let has_press = if pressed && !event.repeat {
                    self.forwarded.remove(&event.physical_key);
                    self.consumed.remove(&event.physical_key);
                    true
                } else if pressed {
                    self.forwarded.contains(&event.physical_key)
                } else {
                    self.forwarded.remove(&event.physical_key)
                };
                let allowed = self.should_forward(
                    event.physical_key,
                    &event.logical_key,
                    event
                        .text_with_all_modifiers
                        .as_deref()
                        .or(event.text.as_deref()),
                    pressed,
                    event.repeat,
                    protected,
                );
                if !allowed || !has_press {
                    return self.filter_key(event.physical_key, pressed, false);
                }
                let consumed = if has_listeners() {
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
                    )
                    .is_some_and(|payload| {
                        if pressed {
                            self.forwarded.insert(event.physical_key);
                        }
                        forward(payload)
                    })
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
}

fn key_event(
    key: &Key,
    location: KeyLocation,
    scancode: Option<u32>,
    pressed: bool,
    modifiers: ModifiersState,
    timestamp: u32,
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
    let event = KeyEvent {
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
    Some(event)
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::KeyCode;
    #[test]
    fn protected_printables_never_forward_any_phase() {
        for text in ["q", " ", "7"] {
            let mut keyboard = Keyboard::default();
            let physical = PhysicalKey::Code(KeyCode::KeyQ);
            let logical = Key::Character(text.into());
            let mut forwards = 0;
            for (pressed, repeat, protected) in [
                (true, false, true),
                (true, true, false),
                (false, false, false),
            ] {
                if keyboard.should_forward(
                    physical,
                    &logical,
                    Some(text),
                    pressed,
                    repeat,
                    protected,
                ) {
                    forwards += 1;
                }
            }
            assert_eq!(forwards, 0);
        }
    }
    #[test]
    fn protected_commands_and_navigation_forward() {
        let q = PhysicalKey::Code(KeyCode::KeyQ);
        let logical = Key::Character("q".into());
        for (code, named) in [
            (KeyCode::ControlLeft, NamedKey::Control),
            (KeyCode::MetaLeft, NamedKey::Meta),
            (KeyCode::Insert, NamedKey::Insert),
            (KeyCode::Numpad0, NamedKey::Insert),
            (KeyCode::CapsLock, NamedKey::CapsLock),
        ] {
            let mut keyboard = Keyboard::default();
            let modifier = PhysicalKey::Code(code);
            let text = if matches!(code, KeyCode::ControlLeft | KeyCode::MetaLeft) {
                "\x11"
            } else {
                "q"
            };
            assert!(keyboard.should_forward(modifier, &Key::Named(named), None, true, false, true));
            assert!(keyboard.should_forward(q, &logical, Some(text), true, false, true));
            let released = if code == KeyCode::Numpad0 {
                Key::Character("0".into())
            } else {
                Key::Named(named)
            };
            assert!(keyboard.should_forward(modifier, &released, None, false, false, true));
            assert!(!keyboard.should_forward(q, &logical, Some("q"), true, true, true));
            assert!(!keyboard.should_forward(q, &logical, Some("q"), false, false, true));
            assert!(!keyboard.should_forward(q, &logical, Some("q"), true, false, true));
        }
        for named in [
            NamedKey::Tab,
            NamedKey::ArrowLeft,
            NamedKey::Backspace,
            NamedKey::Enter,
            NamedKey::Escape,
            NamedKey::Delete,
            NamedKey::F1,
        ] {
            let mut keyboard = Keyboard::default();
            assert!(keyboard.should_forward(q, &Key::Named(named), Some("\t"), true, false, true));
            assert!(keyboard.should_forward(q, &Key::Named(named), Some("\t"), false, false, true));
        }
        let mut keyboard = Keyboard::default();
        let mut seen = Vec::new();
        deliver(
            &mut keyboard,
            &input(
                KeyCode::ControlLeft,
                Key::Named(NamedKey::Control),
                None,
                true,
            ),
            true,
            &mut seen,
        );
        let mut press = input(KeyCode::KeyA, Key::Character("a".into()), Some("a"), true);
        if let WindowEvent::KeyboardInput { event, .. } = &mut press {
            event.text_with_all_modifiers = Some("\x01".into());
        }
        deliver(&mut keyboard, &press, true, &mut seen);
        deliver(
            &mut keyboard,
            &input(
                KeyCode::ControlLeft,
                Key::Named(NamedKey::Control),
                None,
                false,
            ),
            true,
            &mut seen,
        );
        deliver(
            &mut keyboard,
            &input(KeyCode::KeyA, Key::Character("a".into()), Some("a"), false),
            true,
            &mut seen,
        );
        assert_eq!(
            seen.iter()
                .filter(|event| event.id == 97)
                .map(|event| event.event_type)
                .collect::<Vec<_>>(),
            [0, 1]
        );
    }
    #[test]
    fn unprotected_press_decision_and_named_text() {
        let mut keyboard = Keyboard::default();
        let key = PhysicalKey::Code(KeyCode::KeyQ);
        let logical = Key::Character("q".into());
        assert!(keyboard.should_forward(key, &logical, Some("q"), true, false, false));
        assert!(!keyboard.should_forward(key, &logical, Some("q"), true, true, true));
        assert!(!keyboard.should_forward(key, &logical, Some("q"), false, false, true));
        assert!(!keyboard.should_forward(
            key,
            &Key::Unidentified(winit::keyboard::NativeKey::Unidentified),
            Some(" "),
            true,
            false,
            true
        ));
    }
    #[test]
    fn ordinary_special_payloads() {
        assert!(
            key_event(
                &Key::Character("".into()),
                KeyLocation::Standard,
                None,
                true,
                ModifiersState::empty(),
                100
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
                100
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
                    101
                )
                .unwrap()
                .id,
                id
            );
        }
        let ordinary = key_event(
            &Key::Character("q".into()),
            KeyLocation::Standard,
            Some(16),
            true,
            ModifiersState::empty(),
            100,
        )
        .unwrap();
        assert_eq!(ordinary.event_string, "q");
        assert_eq!(ordinary.id, 113);
        let invalid_code = key_event(
            &Key::Character("a".into()),
            KeyLocation::Standard,
            Some(u32::MAX),
            true,
            ModifiersState::empty(),
            100,
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
    fn protected_press_stays_suppressed_after_focus_changes() {
        let mut keyboard = Keyboard::default();
        let key = PhysicalKey::Code(KeyCode::KeyA);
        let logical = Key::Character("a".into());
        assert!(!keyboard.should_forward(key, &logical, None, true, false, true));
        assert!(!keyboard.should_forward(key, &logical, None, true, true, false));
        assert!(!keyboard.should_forward(key, &logical, None, false, false, false));
        assert!(keyboard.should_forward(key, &logical, None, true, false, false));
    }

    fn input(code: KeyCode, logical: Key, text: Option<&str>, pressed: bool) -> WindowEvent {
        WindowEvent::KeyboardInput {
            device_id: None,
            is_synthetic: false,
            event: winit::event::KeyEvent {
                physical_key: PhysicalKey::Code(code),
                logical_key: logical.clone(),
                text: text.map(Into::into),
                location: KeyLocation::Standard,
                state: if pressed {
                    winit::event::ElementState::Pressed
                } else {
                    winit::event::ElementState::Released
                },
                repeat: false,
                text_with_all_modifiers: text.map(Into::into),
                key_without_modifiers: logical,
            },
        }
    }

    fn deliver(
        keyboard: &mut Keyboard,
        event: &WindowEvent,
        protected: bool,
        seen: &mut Vec<KeyEvent>,
    ) -> bool {
        keyboard.process_with(
            event,
            protected,
            || true,
            |payload| {
                seen.push(payload);
                false
            },
        )
    }

    #[test]
    fn protected_payloads_are_absent_and_modifier_changes_do_not_reveal_them() {
        for text in ["q", " ", "7"] {
            let mut keyboard = Keyboard::default();
            let mut seen = Vec::new();
            let mut event = input(KeyCode::KeyQ, Key::Character(text.into()), Some(text), true);
            assert!(!deliver(&mut keyboard, &event, true, &mut seen));
            if let WindowEvent::KeyboardInput { event, .. } = &mut event {
                event.repeat = true;
            }
            assert!(!deliver(&mut keyboard, &event, false, &mut seen));
            assert!(!deliver(
                &mut keyboard,
                &input(
                    KeyCode::KeyQ,
                    Key::Character(text.into()),
                    Some(text),
                    false
                ),
                false,
                &mut seen
            ));
            assert!(seen.is_empty());
        }
        for (code, named) in [
            (KeyCode::ControlLeft, NamedKey::Control),
            (KeyCode::Insert, NamedKey::Insert),
        ] {
            let mut keyboard = Keyboard::default();
            let mut seen = Vec::new();
            let mut event = input(KeyCode::KeyQ, Key::Character("q".into()), Some("q"), true);
            deliver(&mut keyboard, &event, true, &mut seen);
            deliver(
                &mut keyboard,
                &input(code, Key::Named(named), None, true),
                true,
                &mut seen,
            );
            seen.clear();
            if let WindowEvent::KeyboardInput { event, .. } = &mut event {
                event.repeat = true;
            }
            deliver(&mut keyboard, &event, true, &mut seen);
            deliver(
                &mut keyboard,
                &input(KeyCode::KeyQ, Key::Character("q".into()), Some("q"), false),
                true,
                &mut seen,
            );
            assert!(seen.is_empty());
        }
    }

    #[test]
    fn altgraph_and_printable_control_alt_chords_are_not_commands() {
        for text in ["@", "€"] {
            for altgraph in [true, false] {
                let mut keyboard = Keyboard::default();
                let mut seen = Vec::new();
                deliver(
                    &mut keyboard,
                    &input(
                        KeyCode::ControlLeft,
                        Key::Named(NamedKey::Control),
                        None,
                        true,
                    ),
                    true,
                    &mut seen,
                );
                deliver(
                    &mut keyboard,
                    &input(
                        KeyCode::AltRight,
                        Key::Named(if altgraph {
                            NamedKey::AltGraph
                        } else {
                            NamedKey::Alt
                        }),
                        None,
                        true,
                    ),
                    true,
                    &mut seen,
                );
                seen.clear();
                let mut event = input(KeyCode::KeyQ, Key::Character(text.into()), Some(text), true);
                deliver(&mut keyboard, &event, true, &mut seen);
                if let WindowEvent::KeyboardInput { event, .. } = &mut event {
                    event.repeat = true;
                }
                deliver(&mut keyboard, &event, true, &mut seen);
                deliver(
                    &mut keyboard,
                    &input(
                        KeyCode::KeyQ,
                        Key::Character(text.into()),
                        Some(text),
                        false,
                    ),
                    true,
                    &mut seen,
                );
                assert!(seen.is_empty());
            }
        }
    }

    #[test]
    fn alt_only_printables_are_suppressed_but_navigation_is_forwarded() {
        for text in [Some("q"), None] {
            let mut keyboard = Keyboard::default();
            let mut seen = Vec::new();
            deliver(
                &mut keyboard,
                &input(KeyCode::AltLeft, Key::Named(NamedKey::Alt), None, true),
                true,
                &mut seen,
            );
            seen.clear();
            let mut event = input(KeyCode::KeyQ, Key::Character("q".into()), text, true);
            deliver(&mut keyboard, &event, true, &mut seen);
            if let WindowEvent::KeyboardInput { event, .. } = &mut event {
                event.repeat = true;
            }
            deliver(&mut keyboard, &event, true, &mut seen);
            deliver(
                &mut keyboard,
                &input(KeyCode::KeyQ, Key::Character("q".into()), text, false),
                true,
                &mut seen,
            );
            assert!(seen.is_empty());
            deliver(
                &mut keyboard,
                &input(
                    KeyCode::ArrowLeft,
                    Key::Named(NamedKey::ArrowLeft),
                    None,
                    true,
                ),
                true,
                &mut seen,
            );
            deliver(
                &mut keyboard,
                &input(
                    KeyCode::ArrowLeft,
                    Key::Named(NamedKey::ArrowLeft),
                    None,
                    false,
                ),
                true,
                &mut seen,
            );
            assert_eq!(seen.len(), 2);
        }
    }

    #[test]
    fn blur_synthetic_orphans_and_fresh_presses() {
        let mut keyboard = Keyboard::default();
        let mut seen = Vec::new();
        let press = input(KeyCode::KeyQ, Key::Character("q".into()), Some("q"), true);
        let release = input(KeyCode::KeyQ, Key::Character("q".into()), Some("q"), false);
        deliver(&mut keyboard, &press, true, &mut seen);
        deliver(
            &mut keyboard,
            &WindowEvent::Focused(false),
            false,
            &mut seen,
        );
        deliver(&mut keyboard, &release, false, &mut seen);
        assert!(seen.is_empty());
        deliver(&mut keyboard, &press, false, &mut seen);
        assert_eq!(seen.len(), 1);
        let mut synthetic = release.clone();
        if let WindowEvent::KeyboardInput { is_synthetic, .. } = &mut synthetic {
            *is_synthetic = true;
        }
        deliver(&mut keyboard, &synthetic, false, &mut seen);
        deliver(&mut keyboard, &release, false, &mut seen);
        assert_eq!(seen.len(), 1);
        deliver(&mut keyboard, &press, false, &mut seen);
        deliver(&mut keyboard, &release, false, &mut seen);
        assert_eq!(seen.len(), 3);
        deliver(&mut keyboard, &press, true, &mut seen);
        deliver(&mut keyboard, &press, false, &mut seen);
        assert_eq!(seen.len(), 4);
        assert!(!keyboard.process_with(&press, false, || false, |_| panic!("no listeners")));
        deliver(&mut keyboard, &release, false, &mut seen);
        assert_eq!(seen.len(), 4);
    }

    #[test]
    fn ime_never_reaches_the_bus() {
        let mut keyboard = Keyboard::default();
        let mut seen = Vec::new();
        assert!(keyboard.filter_key(PhysicalKey::Code(KeyCode::KeyA), true, true));
        deliver(
            &mut keyboard,
            &input(KeyCode::KeyQ, Key::Character("q".into()), Some("q"), false),
            true,
            &mut seen,
        );
        assert!(keyboard.suppress_ime);
        for suppress in [false, true] {
            keyboard.suppress_ime = suppress;
            for event in [
                WindowEvent::Ime(Ime::Commit("compose".into())),
                WindowEvent::Ime(Ime::Preedit("compose".into(), Some((0, 1)))),
            ] {
                assert_eq!(deliver(&mut keyboard, &event, true, &mut seen), suppress);
            }
        }
        assert!(seen.is_empty());
    }

    #[test]
    fn repeat_can_revoke_permission_but_release_alone_preserves_commands() {
        let mut keyboard = Keyboard::default();
        let mut seen = Vec::new();
        let mut event = input(KeyCode::KeyQ, Key::Character("q".into()), Some("q"), true);
        deliver(&mut keyboard, &event, false, &mut seen);
        assert_eq!(seen.len(), 1);
        seen.clear();
        if let WindowEvent::KeyboardInput { event, .. } = &mut event {
            event.repeat = true;
        }
        deliver(&mut keyboard, &event, true, &mut seen);
        deliver(&mut keyboard, &event, false, &mut seen);
        deliver(
            &mut keyboard,
            &input(KeyCode::KeyQ, Key::Character("q".into()), Some("q"), false),
            false,
            &mut seen,
        );
        assert!(seen.is_empty());
        for repeat in [false, true] {
            let mut keyboard = Keyboard::default();
            let mut seen = Vec::new();
            deliver(
                &mut keyboard,
                &input(
                    KeyCode::ControlLeft,
                    Key::Named(NamedKey::Control),
                    None,
                    true,
                ),
                true,
                &mut seen,
            );
            let mut event = input(
                KeyCode::KeyQ,
                Key::Character("q".into()),
                Some("\x11"),
                true,
            );
            deliver(&mut keyboard, &event, true, &mut seen);
            assert_eq!(seen.iter().filter(|event| event.id == 113).count(), 1);
            deliver(
                &mut keyboard,
                &input(
                    KeyCode::ControlLeft,
                    Key::Named(NamedKey::Control),
                    None,
                    false,
                ),
                true,
                &mut seen,
            );
            seen.clear();
            if repeat {
                if let WindowEvent::KeyboardInput { event, .. } = &mut event {
                    event.repeat = true;
                    event.text_with_all_modifiers = Some("q".into());
                }
                deliver(&mut keyboard, &event, true, &mut seen);
            }
            deliver(
                &mut keyboard,
                &input(KeyCode::KeyQ, Key::Character("q".into()), Some("q"), false),
                true,
                &mut seen,
            );
            assert_eq!(seen.len(), usize::from(!repeat));
        }
    }

    #[test]
    fn privacy_revocation_preserves_consumed_command_lifecycle() {
        let mut keyboard = Keyboard::default();
        let mut seen = Vec::new();
        deliver(
            &mut keyboard,
            &input(KeyCode::Insert, Key::Named(NamedKey::Insert), None, true),
            true,
            &mut seen,
        );
        let mut event = input(KeyCode::KeyT, Key::Character("t".into()), Some("t"), true);
        assert!(keyboard.process_with(
            &event,
            true,
            || true,
            |payload| {
                seen.push(payload);
                true
            }
        ));
        deliver(
            &mut keyboard,
            &input(KeyCode::Insert, Key::Named(NamedKey::Insert), None, false),
            true,
            &mut seen,
        );
        if let WindowEvent::KeyboardInput { event, .. } = &mut event {
            event.repeat = true;
        }
        assert!(keyboard.process_with(
            &event,
            true,
            || true,
            |_| panic!("private repeat forwarded")
        ));
        assert!(keyboard.process_with(
            &input(KeyCode::KeyT, Key::Character("t".into()), Some("t"), false),
            true,
            || true,
            |_| panic!("private release forwarded")
        ));
        for event in [
            WindowEvent::Ime(Ime::Commit("t".into())),
            WindowEvent::Ime(Ime::Preedit("t".into(), Some((0, 1)))),
        ] {
            assert!(keyboard.process_with(&event, true, || true, |_| panic!("IME forwarded")));
        }
        assert!(keyboard.consumed.is_empty());
        assert!(!deliver(
            &mut keyboard,
            &input(KeyCode::KeyA, Key::Character("a".into()), Some("a"), true),
            true,
            &mut seen
        ));
        assert!(!deliver(
            &mut keyboard,
            &WindowEvent::Ime(Ime::Commit("a".into())),
            true,
            &mut seen
        ));
    }
}
