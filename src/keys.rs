//! Teclas de crossterm → lo que acepta herdr para reenviarlas a un panel.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// Cómo se reenvía una tecla al panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    /// Nombre de tecla para `pane.send_keys` (`enter`, `ctrl+c`, `shift+tab`…).
    Key(String),
    /// Texto literal para `pane.send_text` (caracteres imprimibles o una secuencia de escape).
    Text(String),
}

/// Traduce una tecla. `None` si no tiene traducción (se ignora).
pub fn translate(ev: &KeyEvent) -> Option<Input> {
    if ev.kind == KeyEventKind::Release {
        return None;
    }
    let (ctrl, alt, shift) = (
        ev.modifiers.contains(KeyModifiers::CONTROL),
        ev.modifiers.contains(KeyModifiers::ALT),
        ev.modifiers.contains(KeyModifiers::SHIFT),
    );
    let key = |s: String| Some(Input::Key(s));
    let text = |s: &str| Some(Input::Text(s.to_string()));
    match ev.code {
        KeyCode::Char(' ') if ctrl && !alt => key("ctrl+space".into()),
        KeyCode::Char(c) => {
            if ctrl && !alt {
                key(format!("ctrl+{}", c.to_lowercase()))
            } else if alt && !ctrl {
                key(format!("alt+{}", c.to_lowercase()))
            } else if !ctrl && !alt {
                text(&c.to_string())
            } else {
                None
            }
        }
        KeyCode::Enter => key(if alt { "alt+enter".into() } else { "enter".into() }),
        KeyCode::Esc => key("escape".into()),
        KeyCode::Tab => key(if shift { "shift+tab".into() } else { "tab".into() }),
        KeyCode::BackTab => key("shift+tab".into()),
        KeyCode::Backspace => key("backspace".into()),
        KeyCode::Up => key(named("up", ctrl, alt, shift)),
        KeyCode::Down => key(named("down", ctrl, alt, shift)),
        KeyCode::Left => key(named("left", ctrl, alt, shift)),
        KeyCode::Right => key(named("right", ctrl, alt, shift)),
        KeyCode::F(n) if (1..=12).contains(&n) => key(format!("f{n}")),
        KeyCode::Delete => text("\x1b[3~"),
        KeyCode::Insert => text("\x1b[2~"),
        KeyCode::Home => text("\x1b[H"),
        KeyCode::End => text("\x1b[F"),
        KeyCode::PageUp => text("\x1b[5~"),
        KeyCode::PageDown => text("\x1b[6~"),
        _ => None,
    }
}

/// Nombre de tecla con los modificadores delante, en el orden que acepta herdr.
fn named(base: &str, ctrl: bool, alt: bool, shift: bool) -> String {
    let mut s = String::new();
    for (on, prefix) in [(ctrl, "ctrl+"), (alt, "alt+"), (shift, "shift+")] {
        if on {
            s.push_str(prefix);
        }
    }
    s.push_str(base);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn km(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn key(s: &str) -> Option<Input> {
        Some(Input::Key(s.into()))
    }

    fn text(s: &str) -> Option<Input> {
        Some(Input::Text(s.into()))
    }

    #[test]
    fn tabla() {
        assert_eq!(translate(&k(KeyCode::Char('a'))), text("a"));
        assert_eq!(translate(&k(KeyCode::Enter)), key("enter"));
        assert_eq!(translate(&km(KeyCode::Enter, KeyModifiers::ALT)), key("alt+enter"));
        assert_eq!(translate(&k(KeyCode::Esc)), key("escape"));
        assert_eq!(translate(&k(KeyCode::Tab)), key("tab"));
        assert_eq!(translate(&k(KeyCode::BackTab)), key("shift+tab"));
        assert_eq!(translate(&km(KeyCode::Tab, KeyModifiers::SHIFT)), key("shift+tab"));
        assert_eq!(translate(&k(KeyCode::Backspace)), key("backspace"));
        assert_eq!(translate(&k(KeyCode::Up)), key("up"));
        assert_eq!(translate(&k(KeyCode::Down)), key("down"));
        assert_eq!(translate(&k(KeyCode::Left)), key("left"));
        assert_eq!(translate(&k(KeyCode::Right)), key("right"));
        assert_eq!(
            translate(&km(KeyCode::Left, KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT)),
            key("ctrl+alt+shift+left")
        );
        for n in 1..=12u8 {
            assert_eq!(translate(&k(KeyCode::F(n))), key(&format!("f{n}")));
        }
        assert_eq!(translate(&km(KeyCode::Char('c'), KeyModifiers::CONTROL)), key("ctrl+c"));
        assert_eq!(translate(&km(KeyCode::Char('x'), KeyModifiers::ALT)), key("alt+x"));
        assert_eq!(translate(&km(KeyCode::Char(' '), KeyModifiers::CONTROL)), key("ctrl+space"));
        assert_eq!(translate(&k(KeyCode::Delete)), text("\x1b[3~"));
        assert_eq!(translate(&k(KeyCode::Insert)), text("\x1b[2~"));
        assert_eq!(translate(&k(KeyCode::Home)), text("\x1b[H"));
        assert_eq!(translate(&k(KeyCode::End)), text("\x1b[F"));
        assert_eq!(translate(&k(KeyCode::PageUp)), text("\x1b[5~"));
        assert_eq!(translate(&k(KeyCode::PageDown)), text("\x1b[6~"));
    }

    #[test]
    fn caracteres() {
        assert_eq!(translate(&km(KeyCode::Char('A'), KeyModifiers::SHIFT)), text("A"));
        assert_eq!(translate(&k(KeyCode::Char('Ñ'))), text("Ñ"));
        assert_eq!(translate(&k(KeyCode::Char('€'))), text("€"));
        assert_eq!(translate(&km(KeyCode::Char('Ñ'), KeyModifiers::CONTROL)), key("ctrl+ñ"));
        assert_eq!(translate(&km(KeyCode::Char('€'), KeyModifiers::ALT)), key("alt+€"));
    }

    #[test]
    fn sin_traduccion() {
        let release = KeyEvent::new_with_kind(KeyCode::Char('a'), KeyModifiers::NONE, KeyEventKind::Release);
        assert_eq!(translate(&release), None);
        assert_eq!(translate(&k(KeyCode::Null)), None);
        assert_eq!(translate(&k(KeyCode::F(13))), None);
        assert_eq!(translate(&km(KeyCode::Char('a'), KeyModifiers::CONTROL | KeyModifiers::ALT)), None);
    }
}
