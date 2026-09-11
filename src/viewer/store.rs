use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
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
    pending: String,
    pending_ink: Ink,
    pending_wrap: Option<u16>,
    buffer: String,
    buffer_ink: Ink,
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
            pending: String::new(),
            pending_ink: Ink::default(),
            pending_wrap: None,
            buffer: String::new(),
            buffer_ink: Ink::default(),
            rows: 0,
            max_width: 0,
        })
    }
    fn run(&mut self, value: &str, ink: Ink) -> Result<()> {
        if value.is_empty() {
            return Ok(());
        }
        let width = value.graphemes(true).map(|g| g.width() as u64).sum::<u64>();
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
        Ok(())
    }
    fn flush_buffer(&mut self) -> Result<()> {
        let value = std::mem::take(&mut self.buffer);
        self.run(&value, self.buffer_ink)
    }
    fn flush_cluster(&mut self) -> Result<()> {
        let mut cluster = std::mem::take(&mut self.pending);
        if cluster.is_empty() {
            return Ok(());
        }
        let width = cluster.width() as u64;
        if self
            .pending_wrap
            .is_some_and(|limit| self.column > 0 && self.column + width > u64::from(limit.max(1)))
        {
            self.end_row()?;
        }
        if self.buffer_ink != self.pending_ink || self.buffer.len() + cluster.len() > 4096 {
            self.flush_buffer()?;
        }
        self.buffer_ink = self.pending_ink;
        self.buffer.push_str(&cluster);
        self.column += width;
        cluster.clear();
        self.pending = cluster;
        Ok(())
    }
    pub fn newline(&mut self) -> Result<()> {
        self.flush_cluster()?;
        self.end_row()
    }
    fn end_row(&mut self) -> Result<()> {
        self.flush_buffer()?;
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
        self.column + self.pending.width() as u64
    }
    /// Strip terminal controls, expand tabs, and optionally reflow a Markdown row.
    pub fn write(&mut self, text: &str, ink: Ink, wrap: Option<u16>) -> Result<()> {
        for ch in text.chars() {
            match ch {
                '\r' => continue,
                '\n' => {
                    self.newline()?;
                    continue;
                }
                '\t' => {
                    self.flush_cluster()?;
                    let count = 4 - self.column % 4;
                    for _ in 0..count {
                        self.write(" ", ink, wrap)?;
                    }
                    continue;
                }
                _ => {}
            }
            let ch = if ch.is_control() { '�' } else { ch };
            let old_len = self.pending.len();
            self.pending.push(ch);
            if (old_len == 1 && self.pending.is_ascii())
                || self.pending.graphemes(true).nth(1).is_some()
            {
                self.pending.truncate(old_len);
                self.flush_cluster()?;
                self.pending.push(ch);
                self.pending_ink = ink;
                self.pending_wrap = wrap;
            } else if old_len == 0 {
                self.pending_ink = ink;
                self.pending_wrap = wrap;
            } else if self.pending.chars().count() > 17 {
                // Bound pathological combining/ZWJ clusters even across styled spans.
                self.pending.truncate(old_len);
            }
        }
        Ok(())
    }
    pub fn finish(&mut self) -> Result<()> {
        self.flush_cluster()?;
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
                for cluster in std::str::from_utf8(&bytes)?.graphemes(true) {
                    let w = cluster.width() as u64;
                    if column >= left && column + w <= right {
                        visible.push_str(cluster);
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
    fn clusters_survive_styles_chunks_wrapping_and_clipping() {
        for padding in [0, 4095, 4096] {
            let mut store = Store::new().unwrap();
            store
                .write(&"a".repeat(padding), Ink::default(), None)
                .unwrap();
            for part in ["e", "\u{301}", "👩", "\u{200d}", "💻", "x\n"] {
                let ink = Ink {
                    flags: 1,
                    ..Ink::default()
                };
                store.write(part, ink, None).unwrap();
            }
            assert_eq!(store.max_width, padding as u64 + 4);
            let line = |s: &mut Store, left, width| {
                s.line(0, left, width)
                    .unwrap()
                    .spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            };
            assert_eq!(line(&mut store, padding as u64, 4), "e\u{301}👩‍💻x");
            assert_eq!(line(&mut store, padding as u64, 1), "e\u{301}");
            assert_eq!(line(&mut store, padding as u64 + 2, 2), " x");
            assert_eq!(line(&mut store, padding as u64 + 1, 1), " ");
        }
        let mut store = Store::new().unwrap();
        store.write("e\u{301}👩‍💻x", Ink::default(), Some(3)).unwrap();
        store.finish().unwrap();
        assert_eq!(store.rows, 2);
        assert_eq!(store.max_width, 3);
    }
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
