use unicode_width::UnicodeWidthChar;
// Private, disk-backed styled rows. Neither the row index nor long lines live in RAM.
use anyhow::Result;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use std::{
    fs::File,
    io::{BufWriter, Read, Seek, SeekFrom, Write},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Ink {
    pub rgb: [u8; 3],
    pub flags: u8,
}
impl Default for Ink {
    fn default() -> Self {
        Self {
            rgb: [210, 219, 230],
            flags: 0,
        }
    }
}
impl Ink {
    pub fn style(self) -> Style {
        let mut style = Style::default().fg(Color::Rgb(self.rgb[0], self.rgb[1], self.rgb[2]));
        for (bit, modifier) in [
            (1, Modifier::BOLD),
            (2, Modifier::ITALIC),
            (4, Modifier::UNDERLINED),
            (8, Modifier::CROSSED_OUT),
        ] {
            if self.flags & bit != 0 {
                style = style.add_modifier(modifier);
            }
        }
        style
    }
}

pub(super) struct Store {
    data: BufWriter<File>,
    index: BufWriter<File>,
    data_seek: bool,
    index_seek: bool,
    position: u64,
    row_start: u64,
    column: u64,
    pub rows: u64,
    pub max_width: u64,
}
impl Store {
    pub fn new() -> Result<Self> {
        Ok(Self {
            data: BufWriter::new(tempfile::tempfile()?),
            index: BufWriter::new(tempfile::tempfile()?),
            data_seek: false,
            index_seek: false,
            position: 0,
            row_start: 0,
            column: 0,
            rows: 0,
            max_width: 0,
        })
    }
    fn run(&mut self, value: &str, ink: Ink) -> Result<()> {
        if value.is_empty() {
            return Ok(());
        }
        let width = Span::raw(value).width() as u64;
        if self.data_seek {
            self.data.seek(SeekFrom::Start(self.position))?;
            self.data_seek = false;
        }
        self.data.write_all(&(value.len() as u32).to_le_bytes())?;
        self.data.write_all(&width.to_le_bytes())?;
        self.data.write_all(&ink.rgb)?;
        self.data.write_all(&[ink.flags])?;
        self.data.write_all(value.as_bytes())?;
        self.position += 16 + value.len() as u64;
        self.column += width;
        Ok(())
    }
    pub fn newline(&mut self) -> Result<()> {
        if self.index_seek {
            self.index.seek(SeekFrom::Start(self.rows * 24))?;
            self.index_seek = false;
        }
        for n in [self.row_start, self.position, self.column] {
            self.index.write_all(&n.to_le_bytes())?;
        }
        self.rows += 1;
        self.max_width = self.max_width.max(self.column);
        self.row_start = self.position;
        self.column = 0;
        Ok(())
    }
    pub fn column(&self) -> u64 {
        self.column
    }
    /// Strip terminal controls, expand tabs, and optionally reflow a Markdown row.
    pub fn write(&mut self, text: &str, ink: Ink, wrap: Option<u16>) -> Result<()> {
        let mut chunk = String::with_capacity(4096);
        let mut width = 0u64;
        for ch in text.chars() {
            if ch == '\r' {
                continue;
            }
            if ch == '\n' {
                self.run(&chunk, ink)?;
                chunk.clear();
                width = 0;
                self.newline()?;
                continue;
            }
            let ch = if ch.is_control() && ch != '\t' {
                '�'
            } else {
                ch
            };
            let count = if ch == '\t' {
                (4 - (self.column + width) % 4) as usize
            } else {
                1
            };
            for _ in 0..count {
                let ch = if ch == '\t' { ' ' } else { ch };
                let w = ch.width().unwrap_or(0) as u64;
                if wrap.is_some_and(|limit| self.column + width + w > u64::from(limit.max(1)))
                    && self.column + width > 0
                {
                    self.run(&chunk, ink)?;
                    chunk.clear();
                    width = 0;
                    self.newline()?;
                }
                chunk.push(ch);
                width += w;
                if chunk.len() >= 4096 {
                    self.run(&chunk, ink)?;
                    chunk.clear();
                    width = 0;
                }
            }
        }
        self.run(&chunk, ink)
    }
    pub fn finish(&mut self) -> Result<()> {
        if self.column > 0 || self.rows == 0 {
            self.newline()?;
        }
        self.data.flush()?;
        self.index.flush()?;
        Ok(())
    }
    pub fn line(&mut self, row: u64, left: u64, width: u16) -> Result<Line<'static>> {
        if row >= self.rows {
            return Ok(Line::default());
        }
        self.data.flush()?;
        self.index.flush()?;
        self.data_seek = true;
        self.index_seek = true;
        let mut entry = [0; 24];
        self.index.seek(SeekFrom::Start(row * 24))?;
        self.index.get_mut().read_exact(&mut entry)?;
        let mut pos = u64::from_le_bytes(entry[..8].try_into()?);
        let end = u64::from_le_bytes(entry[8..16].try_into()?);
        let mut column = 0;
        let right = left.saturating_add(u64::from(width));
        let mut spans = Vec::new();
        let mut combining = 0;
        while pos < end && column < right {
            let mut header = [0; 16];
            self.data.seek(SeekFrom::Start(pos))?;
            self.data.get_mut().read_exact(&mut header)?;
            let len = u32::from_le_bytes(header[..4].try_into()?) as usize;
            let run_width = u64::from_le_bytes(header[4..12].try_into()?);
            if column + run_width > left {
                let mut bytes = vec![0; len];
                self.data.get_mut().read_exact(&mut bytes)?;
                let mut visible = String::new();
                for ch in std::str::from_utf8(&bytes)?.chars() {
                    let w = ch.width().unwrap_or(0) as u64;
                    // A malicious run of zero-width combining characters must not turn
                    // a one-cell viewport into an unbounded in-memory string.
                    if w == 0 {
                        combining += 1;
                    } else {
                        combining = 0;
                    }
                    if combining > 16 {
                        continue;
                    }
                    if column >= left && column + w <= right {
                        visible.push(ch);
                    } else if column < right && column + w > left {
                        visible.push_str(&" ".repeat(
                            (column + w).min(right).saturating_sub(column.max(left)) as usize,
                        ));
                    }
                    column += w;
                    if column >= right {
                        break;
                    }
                }
                if !visible.is_empty() {
                    spans.push(Span::styled(
                        visible,
                        Ink {
                            rgb: header[12..15].try_into()?,
                            flags: header[15],
                        }
                        .style(),
                    ));
                }
            } else {
                column += run_width;
            }
            pos += 16 + len as u64;
        }
        Ok(Line::from(spans))
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[test]
    fn excessive_combining_marks_do_not_expand_the_visible_cache() {
        let mut store = Store::new().unwrap();
        store.write("a", Ink::default(), None).unwrap();
        store
            .write(&"\u{301}".repeat(100_000), Ink::default(), None)
            .unwrap();
        store.write("b\n", Ink::default(), None).unwrap();
        let line = store.line(0, 0, 10).unwrap();
        let text = line
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>();
        assert!(text.starts_with('a') && text.ends_with('b'));
        assert!(text.len() <= 34);
    }
    #[test]
    fn full_disk_is_reported_instead_of_silently_truncating_rows() {
        let mut store = Store::new().unwrap();
        store.data = BufWriter::new(
            std::fs::OpenOptions::new()
                .write(true)
                .open("/dev/full")
                .unwrap(),
        );
        store.write("content\n", Ink::default(), None).unwrap();
        assert!(store.finish().is_err());
    }
}
