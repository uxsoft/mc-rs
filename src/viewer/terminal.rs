//! Capability probing must never leave a detached reader competing with Crossterm.
use crossterm::event::{KeyCode as K, KeyEvent, KeyModifiers as M};
use ratatui_image::picker::{Picker, ProtocolType};
use std::collections::VecDeque;

/// Color is pixel data in the half-block renderer, not optional text decoration.
/// Scope the override to image frames so NO_COLOR still applies to the panels.
pub struct ImageColors(bool);
impl ImageColors {
    pub fn new(has_image: bool) -> Self {
        let suppressed = has_image && std::env::var("NO_COLOR").is_ok_and(|s| !s.is_empty());
        if suppressed {
            crossterm::style::force_color_output(true);
        }
        Self(suppressed)
    }
}
impl Drop for ImageColors {
    fn drop(&mut self) {
        if self.0 {
            crossterm::style::force_color_output(false);
        }
    }
}

pub fn detect() -> (Picker, VecDeque<KeyEvent>) {
    #[cfg(unix)]
    let (protocol, font, pending) = query().unwrap_or_default();
    #[cfg(not(unix))]
    let (protocol, font, pending): Detection = (None, None, vec![]);
    let font = font.or_else(|| {
        let size = crossterm::terminal::window_size().ok()?;
        (size.columns > 0
            && size.rows > 0
            && size.width >= size.columns
            && size.height >= size.rows)
            .then(|| (size.width / size.columns, size.height / size.rows))
    });
    let mut picker = if let Some(font) = font {
        // This constructor does not read stdin and retains the library's tmux/iTerm hints.
        #[allow(deprecated)]
        Picker::from_fontsize(font)
    } else {
        Picker::halfblocks()
    };
    if font.is_some()
        && let Some(protocol) = protocol
    {
        let broken_kitty = std::env::var_os("WEZTERM_EXECUTABLE").is_some()
            || std::env::var_os("KONSOLE_VERSION").is_some();
        if !broken_kitty {
            picker.set_protocol_type(protocol);
        }
    }
    (picker, keys(&pending))
}

type Detection = (Option<ProtocolType>, Option<(u16, u16)>, Vec<u8>);
#[cfg(unix)]
fn query() -> anyhow::Result<Detection> {
    use ratatui_image::picker::cap_parser::{Parser, QueryStdioOptions, Response};
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    use std::{
        io::Write,
        time::{Duration, Instant},
    };
    let stdin = std::io::stdin();
    let query = Parser::query(
        std::env::var_os("TMUX").is_some(),
        QueryStdioOptions::default(),
    );
    std::io::stdout().write_all(query.as_bytes())?;
    std::io::stdout().flush()?;
    let deadline = Instant::now() + Duration::from_millis(250);
    let mut parser = Parser::new();
    let mut protocol = None;
    let mut font = None;
    let mut pending = vec![];
    let mut sequence = vec![];
    let mut consumed = 0;
    while Instant::now() < deadline && consumed < 4096 {
        let timeout = Timespec::try_from(deadline.saturating_duration_since(Instant::now()))?;
        let mut fds = [PollFd::new(&stdin, PollFlags::IN)];
        if poll(&mut fds, Some(&timeout))? == 0 {
            break;
        }
        let mut byte = [0];
        if rustix::io::read(&stdin, &mut byte)? == 0 {
            break;
        }
        consumed += 1;
        let ch = byte[0];
        if sequence.is_empty() && ch != 27 {
            pending.push(ch);
            continue;
        }
        sequence.push(ch);
        let responses = parser.push(char::from(ch));
        let mut done = false;
        for response in responses {
            match response {
                Response::Kitty => protocol = Some(ProtocolType::Kitty),
                Response::Sixel if protocol.is_none() => protocol = Some(ProtocolType::Sixel),
                Response::CellSize(Some(size)) => font = Some(size),
                Response::Status => done = true,
                _ => {}
            }
        }
        let csi_done =
            sequence.starts_with(b"\x1b[") && sequence.len() > 2 && (0x40..=0x7e).contains(&ch);
        let apc_done = sequence.starts_with(b"\x1b_") && sequence.ends_with(b"\x1b\\");
        let other_done = sequence.len() >= 2 && !matches!(sequence[1], b'[' | b'_' | b'O');
        let ss3_done = sequence.starts_with(b"\x1bO") && sequence.len() >= 3;
        if csi_done || apc_done || other_done || ss3_done {
            let capability = sequence.starts_with(b"\x1b_G")
                || sequence.starts_with(b"\x1b[?")
                || (sequence.starts_with(b"\x1b[") && matches!(ch, b't' | b'n' | b'R'));
            if !capability {
                pending.extend_from_slice(&sequence);
            }
            sequence.clear();
            parser = Parser::new();
        }
        if done {
            break;
        }
    }
    if !sequence.starts_with(b"\x1b_G") {
        pending.extend_from_slice(&sequence);
    }
    Ok((protocol, font, pending))
}

/// Replay ordinary input received during the short startup query.
fn keys(bytes: &[u8]) -> VecDeque<KeyEvent> {
    let mut keys = VecDeque::new();
    let mut i = 0;
    while i < bytes.len() {
        if (bytes[i..].starts_with(b"\x1b[") || bytes[i..].starts_with(b"\x1bO"))
            && let Some(end) = bytes[i + 2..]
                .iter()
                .position(|b| (0x40..=0x7e).contains(b))
        {
            let end = i + 2 + end;
            let args = std::str::from_utf8(&bytes[i + 2..end]).unwrap_or("");
            let code = match bytes[end] {
                b'A' => Some(K::Up),
                b'B' => Some(K::Down),
                b'C' => Some(K::Right),
                b'D' => Some(K::Left),
                b'H' => Some(K::Home),
                b'F' => Some(K::End),
                b'P'..=b'S' => Some(K::F(bytes[end] - b'P' + 1)),
                b'~' => match args.split(';').next().unwrap_or("") {
                    "1" | "7" => Some(K::Home),
                    "2" => Some(K::Insert),
                    "3" => Some(K::Delete),
                    "4" | "8" => Some(K::End),
                    "5" => Some(K::PageUp),
                    "6" => Some(K::PageDown),
                    "15" => Some(K::F(5)),
                    "17" => Some(K::F(6)),
                    "18" => Some(K::F(7)),
                    "19" => Some(K::F(8)),
                    "20" => Some(K::F(9)),
                    "21" => Some(K::F(10)),
                    _ => None,
                },
                _ => None,
            };
            if let Some(code) = code {
                keys.push_back(KeyEvent::from(code));
                i = end + 1;
                continue;
            }
        }
        let (code, modifiers, count) = match bytes[i] {
            27 => (K::Esc, M::NONE, 1),
            9 => (K::Tab, M::NONE, 1),
            10 | 13 => (K::Enter, M::NONE, 1),
            8 | 127 => (K::Backspace, M::NONE, 1),
            b @ 1..=26 => (K::Char((b'a' + b - 1) as char), M::CONTROL, 1),
            _ => {
                let value = String::from_utf8_lossy(&bytes[i..]);
                let ch = value.chars().next().unwrap_or('�');
                (K::Char(ch), M::NONE, ch.len_utf8().min(bytes.len() - i))
            }
        };
        keys.push_back(KeyEvent::new(code, modifiers));
        i += count;
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replay_keeps_startup_typing_and_navigation() {
        let keys = keys("aé\x1bOR\x1b[A\x1b\x0c".as_bytes());
        assert_eq!(
            keys.iter().map(|k| k.code).collect::<Vec<_>>(),
            vec![
                K::Char('a'),
                K::Char('é'),
                K::F(3),
                K::Up,
                K::Esc,
                K::Char('l')
            ]
        );
        assert_eq!(keys.back().unwrap().modifiers, M::CONTROL);
    }
}
