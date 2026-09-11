//! Vi-style key handling: counts, operator-pending prefixes and a `:` command line.

use winit::keyboard::{Key, NamedKey};

#[derive(Clone, Copy, Debug)]
pub struct KeyInput<'a> {
    pub key: &'a Key,
    pub ctrl: bool,
    pub cmd: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Move the cursor by `n` rows (negative = up).
    MoveCursor(i64),
    /// Scroll the view by `n` rows; the cursor follows only if it would leave the view.
    ScrollLines(i64),
    HalfPage(i64),
    Page(i64),
    GoTop,
    GoBottom,
    /// 1-based row.
    GoRow(u64),
    NextHunk(u64),
    PrevHunk(u64),
    FirstHunk,
    LastHunk,
    NextFile(u64),
    PrevFile(u64),
    OpenPicker,
    ScrollCols(i64),
    ColsHome,
    ColsEnd,
    CursorToTop,
    CursorToCenter,
    CursorToBottom,
    CycleWhitespace,
    SetWhitespace(&'static str),
    Search(String),
    SearchNext(u64),
    SearchPrev(u64),
    ZoomIn,
    ZoomOut,
    ZoomReset,
    ToggleTheme,
    ToggleHelp,
    Quit,
    /// A message for the status bar (e.g. an unknown `:` command).
    Message(String),
}

#[derive(Clone, Debug, PartialEq)]
enum Pending {
    None,
    G,
    Z,
    ZShift,
    OpenBracket,
    CloseBracket,
    Colon(String),
    Slash(String),
}

pub struct Vi {
    count: Option<u64>,
    pending: Pending,
}

impl Vi {
    pub fn new() -> Vi {
        Vi {
            count: None,
            pending: Pending::None,
        }
    }

    /// Text to show in the status bar for in-progress input.
    pub fn pending_display(&self) -> String {
        let mut s = String::new();
        if let Some(c) = self.count {
            s.push_str(&c.to_string());
        }
        match &self.pending {
            Pending::None => {}
            Pending::G => s.push('g'),
            Pending::Z => s.push('z'),
            Pending::ZShift => s.push('Z'),
            Pending::OpenBracket => s.push('['),
            Pending::CloseBracket => s.push(']'),
            Pending::Colon(c) => {
                s.push(':');
                s.push_str(c);
            }
            Pending::Slash(c) => {
                s.push('/');
                s.push_str(c);
            }
        }
        s
    }

    pub fn in_command_line(&self) -> bool {
        matches!(self.pending, Pending::Colon(_) | Pending::Slash(_))
    }

    fn reset(&mut self) {
        self.count = None;
        self.pending = Pending::None;
    }

    fn take_count(&mut self) -> u64 {
        self.count.take().unwrap_or(1)
    }

    pub fn key(&mut self, input: KeyInput) -> Option<Action> {
        if let Key::Named(NamedKey::Escape) = input.key {
            self.reset();
            return None;
        }
        // Command-line editing takes every key.
        if let Pending::Colon(_) | Pending::Slash(_) = &self.pending {
            return self.command_line_key(input);
        }

        let ch = match input.key {
            Key::Character(s) if !input.cmd => s.chars().next(),
            _ => None,
        };
        if input.cmd {
            if let Key::Character(s) = input.key {
                return match s.as_str() {
                    "=" | "+" => Some(Action::ZoomIn),
                    "-" => Some(Action::ZoomOut),
                    "0" => Some(Action::ZoomReset),
                    "p" => Some(Action::OpenPicker),
                    _ => None,
                };
            }
            return None;
        }
        if input.ctrl {
            let a = match ch {
                Some('d') => Action::HalfPage(self.count_signed(1)),
                Some('u') => Action::HalfPage(-self.count_signed(1)),
                Some('f') => Action::Page(self.count_signed(1)),
                Some('b') => Action::Page(-self.count_signed(1)),
                Some('e') => Action::ScrollLines(self.count_signed(1)),
                Some('y') => Action::ScrollLines(-self.count_signed(1)),
                Some('n') => Action::NextHunk(self.take_count()),
                Some('p') => Action::PrevHunk(self.take_count()),
                _ => {
                    self.reset();
                    return None;
                }
            };
            self.reset();
            return Some(a);
        }

        match std::mem::replace(&mut self.pending, Pending::None) {
            Pending::G => {
                return match ch {
                    Some('g') => Some(match self.count.take() {
                        Some(n) => Action::GoRow(n),
                        None => Action::GoTop,
                    }),
                    _ => {
                        self.reset();
                        None
                    }
                };
            }
            Pending::Z => {
                let a = match ch {
                    Some('t') => Some(Action::CursorToTop),
                    Some('z') => Some(Action::CursorToCenter),
                    Some('b') => Some(Action::CursorToBottom),
                    _ => None,
                };
                self.reset();
                return a;
            }
            Pending::ZShift => {
                self.reset();
                return match ch {
                    Some('Z') | Some('Q') => Some(Action::Quit),
                    _ => None,
                };
            }
            Pending::OpenBracket => {
                let n = self.take_count();
                self.reset();
                return match ch {
                    Some('c') => Some(Action::PrevHunk(n)),
                    Some('C') => Some(Action::FirstHunk),
                    Some('f') => Some(Action::PrevFile(n)),
                    _ => None,
                };
            }
            Pending::CloseBracket => {
                let n = self.take_count();
                self.reset();
                return match ch {
                    Some('c') => Some(Action::NextHunk(n)),
                    Some('C') => Some(Action::LastHunk),
                    Some('f') => Some(Action::NextFile(n)),
                    _ => None,
                };
            }
            Pending::None => {}
            Pending::Colon(_) | Pending::Slash(_) => unreachable!(),
        }

        if let Key::Named(named) = input.key {
            let a = match named {
                NamedKey::ArrowDown => Action::MoveCursor(self.count_signed(1)),
                NamedKey::ArrowUp => Action::MoveCursor(-self.count_signed(1)),
                NamedKey::ArrowLeft => Action::ScrollCols(-self.count_signed(4)),
                NamedKey::ArrowRight => Action::ScrollCols(self.count_signed(4)),
                NamedKey::PageDown | NamedKey::Space => Action::Page(self.count_signed(1)),
                NamedKey::PageUp => Action::Page(-self.count_signed(1)),
                NamedKey::Home => Action::GoTop,
                NamedKey::End => Action::GoBottom,
                NamedKey::Enter => Action::MoveCursor(self.count_signed(1)),
                NamedKey::Backspace => Action::MoveCursor(-self.count_signed(1)),
                _ => return None,
            };
            self.reset();
            return Some(a);
        }

        let c = ch?;
        if c.is_ascii_digit() && (c != '0' || self.count.is_some()) {
            let d = c.to_digit(10).unwrap() as u64;
            self.count = Some(
                self.count
                    .unwrap_or(0)
                    .saturating_mul(10)
                    .saturating_add(d)
                    .min(1 << 40),
            );
            return None;
        }
        let a = match c {
            'j' => Action::NextHunk(self.take_count()),
            'k' => Action::PrevHunk(self.take_count()),
            'h' => Action::ScrollCols(-self.count_signed(4)),
            'l' => Action::ScrollCols(self.count_signed(4)),
            '0' | '^' => Action::ColsHome,
            '$' => Action::ColsEnd,
            'G' => match self.count.take() {
                Some(n) => Action::GoRow(n),
                None => Action::GoBottom,
            },
            'n' => Action::SearchNext(self.take_count()),
            'N' => Action::SearchPrev(self.take_count()),
            'w' => Action::CycleWhitespace,
            't' => Action::ToggleTheme,
            'q' => Action::Quit,
            '?' => Action::ToggleHelp,
            '+' | '=' => Action::ZoomIn,
            '-' => Action::ZoomOut,
            'g' => {
                self.pending = Pending::G;
                return None;
            }
            'z' => {
                self.pending = Pending::Z;
                return None;
            }
            'Z' => {
                self.pending = Pending::ZShift;
                return None;
            }
            '[' => {
                self.pending = Pending::OpenBracket;
                return None;
            }
            ']' => {
                self.pending = Pending::CloseBracket;
                return None;
            }
            ':' => {
                self.count = None;
                self.pending = Pending::Colon(String::new());
                return None;
            }
            '/' => {
                self.count = None;
                self.pending = Pending::Slash(String::new());
                return None;
            }
            _ => {
                self.reset();
                return None;
            }
        };
        self.reset();
        Some(a)
    }

    fn count_signed(&mut self, default: i64) -> i64 {
        match self.count.take() {
            Some(n) => n.min(i64::MAX as u64) as i64,
            None => default,
        }
    }

    fn command_line_key(&mut self, input: KeyInput) -> Option<Action> {
        let (is_search, buf) = match &mut self.pending {
            Pending::Colon(b) => (false, b),
            Pending::Slash(b) => (true, b),
            _ => unreachable!(),
        };
        match input.key {
            Key::Named(NamedKey::Enter) => {
                let text = std::mem::take(buf);
                self.reset();
                if is_search {
                    return if text.is_empty() {
                        Some(Action::SearchNext(1))
                    } else {
                        Some(Action::Search(text))
                    };
                }
                Some(run_command(text.trim()))
            }
            Key::Named(NamedKey::Backspace) => {
                if buf.pop().is_none() {
                    self.reset();
                }
                None
            }
            Key::Named(NamedKey::Space) => {
                buf.push(' ');
                None
            }
            Key::Character(s) if !input.ctrl && !input.cmd => {
                buf.push_str(s);
                None
            }
            Key::Character(s) if input.ctrl && s.as_str() == "u" => {
                buf.clear();
                None
            }
            _ => None,
        }
    }
}

fn run_command(cmd: &str) -> Action {
    if cmd.is_empty() {
        return Action::Message(String::new());
    }
    if let Ok(n) = cmd.parse::<u64>() {
        return Action::GoRow(n);
    }
    let (name, arg) = match cmd.split_once(char::is_whitespace) {
        Some((n, a)) => (n, a.trim()),
        None => (cmd, ""),
    };
    match name {
        "q" | "q!" | "qa" | "quit" | "wq" | "x" => Action::Quit,
        "ws" | "whitespace" => match arg {
            "" => Action::CycleWhitespace,
            "exact" | "none" => Action::SetWhitespace("exact"),
            "eol" | "ignore-eol" => Action::SetWhitespace("eol"),
            "change" | "b" | "ignore-space-change" => Action::SetWhitespace("change"),
            "all" | "w" | "ignore-all-space" => Action::SetWhitespace("all"),
            other => Action::Message(format!("unknown whitespace mode: {other}")),
        },
        "set" => match arg {
            "ws" | "iw" => Action::CycleWhitespace,
            "light" | "dark" | "theme" => Action::ToggleTheme,
            other => Action::Message(format!("unknown option: {other}")),
        },
        "h" | "help" => Action::ToggleHelp,
        "n" | "next" => Action::NextFile(1),
        "N" | "prev" | "previous" => Action::PrevFile(1),
        "e" | "edit" | "files" | "f" => Action::OpenPicker,
        _ => Action::Message(format!("not a command: {cmd}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::SmolStr;

    fn press(vi: &mut Vi, c: char) -> Option<Action> {
        let key = Key::Character(SmolStr::new(c.to_string()));
        vi.key(KeyInput {
            key: &key,
            ctrl: false,
            cmd: false,
        })
    }

    fn named(vi: &mut Vi, n: NamedKey) -> Option<Action> {
        let key = Key::Named(n);
        vi.key(KeyInput {
            key: &key,
            ctrl: false,
            cmd: false,
        })
    }

    #[test]
    fn counts_and_motions() {
        let mut vi = Vi::new();
        assert_eq!(press(&mut vi, 'j'), Some(Action::NextHunk(1)));
        assert_eq!(press(&mut vi, '1'), None);
        assert_eq!(press(&mut vi, '0'), None);
        assert_eq!(press(&mut vi, 'k'), Some(Action::PrevHunk(10)));
        assert_eq!(
            named(&mut vi, NamedKey::ArrowDown),
            Some(Action::MoveCursor(1))
        );
        assert_eq!(press(&mut vi, ']'), None);
        assert_eq!(press(&mut vi, 'f'), Some(Action::NextFile(1)));
        assert_eq!(press(&mut vi, '0'), Some(Action::ColsHome));
        assert_eq!(press(&mut vi, 'g'), None);
        assert_eq!(press(&mut vi, 'g'), Some(Action::GoTop));
        assert_eq!(press(&mut vi, '5'), None);
        assert_eq!(press(&mut vi, 'G'), Some(Action::GoRow(5)));
        assert_eq!(press(&mut vi, ']'), None);
        assert_eq!(press(&mut vi, 'c'), Some(Action::NextHunk(1)));
        assert_eq!(press(&mut vi, '3'), None);
        assert_eq!(press(&mut vi, '['), None);
        assert_eq!(press(&mut vi, 'c'), Some(Action::PrevHunk(3)));
        assert_eq!(press(&mut vi, 'Z'), None);
        assert_eq!(press(&mut vi, 'Z'), Some(Action::Quit));
    }

    #[test]
    fn command_line() {
        let mut vi = Vi::new();
        assert_eq!(press(&mut vi, ':'), None);
        assert!(vi.in_command_line());
        press(&mut vi, 'q');
        assert_eq!(vi.pending_display(), ":q");
        assert_eq!(named(&mut vi, NamedKey::Enter), Some(Action::Quit));
        assert!(!vi.in_command_line());

        press(&mut vi, ':');
        press(&mut vi, '4');
        press(&mut vi, '2');
        assert_eq!(named(&mut vi, NamedKey::Enter), Some(Action::GoRow(42)));

        press(&mut vi, ':');
        assert_eq!(named(&mut vi, NamedKey::Backspace), None);
        assert!(!vi.in_command_line());

        press(&mut vi, '/');
        press(&mut vi, 'f');
        press(&mut vi, 'o');
        assert_eq!(
            named(&mut vi, NamedKey::Enter),
            Some(Action::Search("fo".into()))
        );
    }
}
