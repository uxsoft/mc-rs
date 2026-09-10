//! Read-only full-screen viewer. Each session owns its workers and private disk caches.
mod graphics;
mod store;
mod terminal;
mod text;

use crate::vfs::{Context, Kind, VfsPath};
use anyhow::{Context as _, Result};
use crossterm::event::{KeyCode, KeyEvent, MouseEventKind};
use image::DynamicImage;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
};
use ratatui_image::{
    Resize,
    picker::{Picker, ProtocolType},
    protocol::Protocol,
};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use store::{Ink, Store};

const LINE_LIMIT: usize = 256 * 1024;
const PIXEL_LIMIT: u64 = 64 * 1024 * 1024;
const BG: Color = Color::Rgb(19, 23, 31);

#[derive(Clone, Debug, PartialEq, Eq)]
enum Format {
    Text,
    Code(String),
    Markdown,
    Image,
    Pdf,
    Binary,
}
impl Format {
    fn label(&self) -> &str {
        match self {
            Self::Text => "Text",
            Self::Code(name) => name,
            Self::Markdown => "Markdown",
            Self::Image => "Image",
            Self::Pdf => "PDF",
            Self::Binary => "Hex",
        }
    }
    fn graphics(&self) -> bool {
        matches!(self, Self::Image | Self::Pdf)
    }
}
fn detect(name: &std::path::Path, probe: &[u8]) -> Format {
    if probe.windows(5).take(1024).any(|s| s == b"%PDF-") {
        return Format::Pdf;
    }
    if image::guess_format(probe).is_ok() {
        return Format::Image;
    }
    if probe.contains(&0)
        || probe
            .iter()
            .filter(|b| **b < 32 && !matches!(**b, 9 | 10 | 13 | 12))
            .count()
            * 20
            > probe.len()
    {
        return Format::Binary;
    }
    let extension = name
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(extension.as_str(), "md" | "markdown" | "mdown" | "mkd") {
        return Format::Markdown;
    }
    if extension == "pdf" {
        return Format::Pdf;
    }
    if matches!(
        extension.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "ico"
    ) {
        return Format::Image;
    }
    let first = String::from_utf8_lossy(probe);
    let syntax = text::syntaxes()
        .find_syntax_by_extension(name.file_name().and_then(|s| s.to_str()).unwrap_or(""))
        .or_else(|| text::syntaxes().find_syntax_by_extension(&extension))
        .or_else(|| text::syntaxes().find_syntax_by_first_line(first.lines().next().unwrap_or("")));
    syntax.map_or(Format::Text, |s| Format::Code(s.name.clone()))
}

/// Map only a completed, privately owned snapshot, never a mutable source path.
struct Snapshot {
    map: Option<memmap2::Mmap>,
    _file: File,
}
impl Snapshot {
    fn new(file: File) -> Result<Self> {
        let map = if file.metadata()?.len() == 0 {
            None
        } else {
            // SAFETY: this anonymous temporary file has no remaining writers and is never
            // exposed by path. The owning file outlives the immutable mapping on all platforms.
            Some(unsafe { memmap2::MmapOptions::new().map(&file)? })
        };
        Ok(Self { map, _file: file })
    }
}
impl AsRef<[u8]> for Snapshot {
    fn as_ref(&self) -> &[u8] {
        self.map.as_deref().unwrap_or(&[])
    }
}
/// Present only the first ICO directory entry. The image crate otherwise chooses
/// the largest icon, unlike the viewer's first-frame policy. Payloads are not copied.
struct RasterInput<'a> {
    cursor: std::io::Cursor<&'a [u8]>,
    ico: bool,
}
impl<'a> RasterInput<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            cursor: std::io::Cursor::new(bytes),
            ico: bytes.starts_with(&[0, 0, 1, 0])
                && bytes.get(4..6).is_some_and(|count| count != [0, 0]),
        }
    }
}
impl Read for RasterInput<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let start = self.cursor.position();
        let n = self.cursor.read(buffer)?;
        if self.ico {
            for (offset, value) in [(4, 1), (5, 0)] {
                if offset >= start && offset < start + n as u64 {
                    buffer[(offset - start) as usize] = value;
                }
            }
        }
        Ok(n)
    }
}
impl std::io::Seek for RasterInput<'_> {
    fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
        std::io::Seek::seek(&mut self.cursor, position)
    }
}

#[derive(Clone)]
struct Source {
    format: Option<Format>,
    store: Option<Arc<Mutex<Store>>>,
    snapshot: Option<Arc<Snapshot>>,
    bytes: u64,
    total: u64,
    done: bool,
    error: Option<String>,
    note: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq)]
struct Request {
    id: u64,
    top: u64,
    left: u64,
    page: usize,
    zoom: i8,
    width: u16,
    height: u16,
}
struct Reply {
    id: u64,
    lines: Vec<Line<'static>>,
    image: Option<Protocol>,
    top: u64,
    left: u64,
    total: u64,
    pages: usize,
    page: usize,
    error: Option<String>,
}
impl Reply {
    fn empty(id: u64) -> Self {
        Self {
            id,
            lines: vec![],
            image: None,
            top: 0,
            left: 0,
            total: 0,
            pages: 0,
            page: 0,
            error: None,
        }
    }
}

pub struct Viewer {
    title: String,
    ctx: Context,
    source: Arc<Mutex<Source>>,
    desired: Arc<Mutex<Request>>,
    result: Arc<Mutex<Option<Reply>>>,
    current: Reply,
    pub redraw: bool,
    graphics: bool,
    modal: bool,
}
impl Viewer {
    pub fn open(path: VfsPath, ctx: Context, picker: Picker) -> Self {
        let title = path.display().to_string();
        let source = Arc::new(Mutex::new(Source {
            format: None,
            store: None,
            snapshot: None,
            bytes: 0,
            total: 0,
            done: false,
            error: None,
            note: None,
        }));
        let desired = Arc::new(Mutex::new(Request {
            id: 0,
            top: 0,
            left: 0,
            page: 0,
            zoom: 0,
            width: 1,
            height: 1,
        }));
        let result = Arc::new(Mutex::new(None));
        let revision = Arc::new(AtomicU64::new(0));
        {
            let source = source.clone();
            let ctx = ctx.clone();
            let revision = revision.clone();
            std::thread::spawn(move || {
                let outcome = load(path, &ctx, &source, &revision);
                let mut state = source.lock().unwrap();
                if !ctx.cancel.load(Ordering::Relaxed) {
                    state.done = true;
                    if let Err(error) = outcome {
                        state.error = Some(format!("{error:#}"));
                    }
                    revision.fetch_add(1, Ordering::Release);
                }
            });
        }
        {
            let source = source.clone();
            let ctx = ctx.clone();
            let desired = desired.clone();
            let result = result.clone();
            std::thread::spawn(move || {
                let mut cache = RenderCache::default();
                let mut last = (u64::MAX, u64::MAX);
                while ctx.check().is_ok() {
                    let request = *desired.lock().unwrap();
                    let rev = revision.load(Ordering::Acquire);
                    if last == (request.id, rev) {
                        std::thread::sleep(Duration::from_millis(30));
                        continue;
                    }
                    last = (request.id, rev);
                    let state = source.lock().unwrap().clone();
                    let rendered = render(request, &state, &mut cache, &picker, &ctx);
                    let reply = match rendered {
                        Ok(reply) => reply,
                        Err(error) => {
                            let mut reply = Reply::empty(request.id);
                            reply.error = Some(format!("{error:#}"));
                            reply
                        }
                    };
                    if ctx.check().is_ok() && desired.lock().unwrap().id == request.id {
                        *result.lock().unwrap() = Some(reply);
                    }
                }
            });
        }
        Self {
            title,
            ctx,
            source,
            desired,
            result,
            current: Reply::empty(0),
            redraw: false,
            graphics: false,
            modal: false,
        }
    }
    pub fn poll(&mut self) {
        if let Some(reply) = self.result.lock().unwrap().take()
            && reply.id == self.desired.lock().unwrap().id
        {
            self.redraw |= self.current.image.is_some() || reply.image.is_some();
            self.current = reply;
        }
        self.graphics = self
            .source
            .lock()
            .unwrap()
            .format
            .as_ref()
            .is_some_and(Format::graphics);
    }
    pub fn modal_changed(&mut self, modal: bool) -> bool {
        let changed = self.modal != modal;
        self.modal = modal;
        changed && self.current.image.is_some()
    }
    pub fn has_image(&self) -> bool {
        self.current.image.is_some()
    }
    pub fn resize(&mut self, width: u16, height: u16) {
        let mut request = self.desired.lock().unwrap();
        let size = (width.max(1), height.saturating_sub(2).max(1));
        if (request.width, request.height) != size {
            request.width = size.0;
            request.height = size.1;
            request.id += 1;
        }
    }
    /// Returns true when the viewer should close. All other input is consumed here.
    pub fn key(&mut self, key: KeyEvent) -> bool {
        if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
            return true;
        }
        let mut r = self.desired.lock().unwrap();
        let before = *r;
        let top = if r.id == self.current.id {
            self.current.top
        } else {
            r.top
        };
        let left = if r.id == self.current.id {
            self.current.left
        } else {
            r.left
        };
        match key.code {
            KeyCode::Up => r.top = top.saturating_sub(1),
            KeyCode::Down => r.top = top.saturating_add(1),
            KeyCode::PageUp => r.top = top.saturating_sub(u64::from(r.height)),
            KeyCode::PageDown => r.top = top.saturating_add(u64::from(r.height)),
            KeyCode::Home => {
                r.top = 0;
                r.left = 0;
            }
            KeyCode::End => r.top = u64::MAX,
            KeyCode::Left => r.left = left.saturating_sub(4),
            KeyCode::Right => r.left = left.saturating_add(4),
            KeyCode::Char('n') if self.graphics => {
                r.page = self
                    .current
                    .page
                    .saturating_add(1)
                    .min(self.current.pages.saturating_sub(1));
                r.top = 0;
                r.left = 0;
            }
            KeyCode::Char('p') if self.graphics => {
                r.page = self.current.page.saturating_sub(1);
                r.top = 0;
                r.left = 0;
            }
            KeyCode::Char('+') if self.graphics => r.zoom = (r.zoom + 1).min(8),
            KeyCode::Char('-') if self.graphics => r.zoom = (r.zoom - 1).max(-8),
            KeyCode::Char('0') if self.graphics => {
                r.zoom = 0;
                r.top = 0;
                r.left = 0;
            }
            _ => {}
        }
        if *r != before {
            r.id += 1;
        }
        false
    }
    pub fn mouse(&mut self, kind: MouseEventKind) {
        let code = match kind {
            MouseEventKind::ScrollUp => KeyCode::Up,
            MouseEventKind::ScrollDown => KeyCode::Down,
            MouseEventKind::ScrollLeft => KeyCode::Left,
            MouseEventKind::ScrollRight => KeyCode::Right,
            _ => return,
        };
        for _ in 0..3 {
            self.key(KeyEvent::from(code));
        }
    }
    pub fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        self.resize(area.width, area.height);
        frame.render_widget(
            Block::default().style(Style::default().bg(BG).fg(Color::Rgb(210, 219, 230))),
            area,
        );
        if area.height == 0 || area.width == 0 {
            return;
        }
        let source = self.source.lock().unwrap().clone();
        let format = source.format.as_ref().map_or("Loading", Format::label);
        let rendering = source.done
            && source.error.is_none()
            && self.current.error.is_none()
            && source.format.as_ref().is_some_and(Format::graphics)
            && (self.current.image.is_none() || self.current.id != self.desired.lock().unwrap().id);
        let progress = if rendering {
            " · Rendering…".into()
        } else if source.done {
            String::new()
        } else {
            format!(
                " · Loading {}/{}",
                crate::ui::size(source.bytes),
                crate::ui::size(source.total)
            )
        };
        let position = if self.current.pages > 0 {
            format!("page {}/{}", self.current.page + 1, self.current.pages)
        } else {
            format!(
                "row {}/{}",
                (self.current.top + 1).min(self.current.total),
                self.current.total
            )
        };
        let clean = |s: &str| {
            s.chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect::<String>()
        };
        frame.render_widget(
            Paragraph::new(format!(
                " {} · {} · Read-only{} · {}",
                format,
                position,
                progress,
                clean(&self.title)
            ))
            .style(
                Style::default()
                    .fg(Color::Rgb(96, 210, 190))
                    .bg(Color::Rgb(30, 38, 49)),
            ),
            Rect::new(area.x, area.y, area.width, 1),
        );
        let content = Rect::new(
            area.x,
            area.y.saturating_add(1),
            area.width,
            area.height.saturating_sub(2),
        );
        if let Some(error) = source.error.as_ref().or(self.current.error.as_ref()) {
            frame.render_widget(
                Paragraph::new(format!("Cannot view file: {}", clean(error)))
                    .wrap(ratatui::widgets::Wrap { trim: false })
                    .style(Style::default().fg(Color::LightRed)),
                content,
            );
        } else if let Some(image) = &self.current.image {
            frame.render_widget(graphics::Graphic(image), content);
        } else if rendering {
            frame.render_widget(Paragraph::new("Rendering image… Esc: cancel"), content);
        } else if self.current.lines.is_empty() && !source.done {
            frame.render_widget(Paragraph::new("Loading… Esc: cancel"), content);
        } else {
            frame.render_widget(Paragraph::new(self.current.lines.clone()), content);
        }
        if area.height > 1 {
            let controls = if self.graphics {
                "Esc/q Close · Arrows/Wheel Pan · PgUp/Dn Scroll · +/- Zoom · 0 Fit · n/p PDF page"
            } else {
                "Esc/q Close · Arrows/Wheel Scroll · PgUp/Dn Page · Home/End"
            };
            let footer = if let Some(note) = source.note {
                format!("{controls} · {note}")
            } else {
                controls.into()
            };
            frame.render_widget(
                Paragraph::new(footer).style(Style::default().bg(Color::Rgb(30, 38, 49))),
                Rect::new(area.x, area.bottom() - 1, area.width, 1),
            );
        }
    }
}
impl Drop for Viewer {
    fn drop(&mut self) {
        self.ctx.cancel.store(true, Ordering::Relaxed);
    }
}

fn load(path: VfsPath, ctx: &Context, shared: &Mutex<Source>, revision: &AtomicU64) -> Result<()> {
    let metadata = path.metadata(true, ctx)?;
    anyhow::ensure!(
        metadata.kind == Kind::File,
        "Only regular files can be viewed"
    );
    shared.lock().unwrap().total = metadata.size;
    let mut reader = BufReader::with_capacity(64 * 1024, path.fs.open_read(&path.path, ctx)?);
    let format = detect(&path.path, reader.fill_buf()?);
    shared.lock().unwrap().format = Some(format.clone());
    revision.fetch_add(1, Ordering::Release);
    match &format {
        Format::Text | Format::Code(_) | Format::Binary => {
            let store = Arc::new(Mutex::new(Store::new()?));
            shared.lock().unwrap().store = Some(store.clone());
            if format == Format::Binary {
                let mut offset = 0u64;
                loop {
                    ctx.check()?;
                    let mut bytes = [0; 16];
                    let mut n = 0;
                    while n < bytes.len() {
                        let got = reader.read(&mut bytes[n..])?;
                        if got == 0 {
                            break;
                        }
                        n += got;
                    }
                    if n == 0 {
                        break;
                    }
                    let mut value = format!("{offset:016x}  ");
                    for b in &bytes[..n] {
                        value.push_str(&format!("{b:02x} "));
                    }
                    value.push_str(&"   ".repeat(16 - n));
                    value.push_str(" │");
                    for b in &bytes[..n] {
                        value.push(if b.is_ascii_graphic() || *b == b' ' {
                            *b as char
                        } else {
                            '.'
                        });
                    }
                    value.push_str("│\n");
                    store.lock().unwrap().write(&value, Ink::default(), None)?;
                    offset += n as u64;
                    if offset.is_multiple_of(65536) || n < 16 {
                        shared.lock().unwrap().bytes = offset;
                        revision.fetch_add(1, Ordering::Release);
                    }
                }
                shared.lock().unwrap().bytes = offset;
            } else {
                let syntax = match &format {
                    Format::Code(name) => text::syntaxes().find_syntax_by_name(name),
                    _ => None,
                };
                let mut highlighter = syntax.map(text::highlighter);
                let mut long_line = false;
                loop {
                    ctx.check()?;
                    let bytes = fragment(&mut reader)?;
                    if bytes.is_empty() {
                        break;
                    }
                    let value = String::from_utf8_lossy(&bytes);
                    let ends = bytes.ends_with(b"\n");
                    let too_long = long_line || bytes.len() >= LINE_LIMIT;
                    let mut store = store.lock().unwrap();
                    if !too_long && let Some(h) = &mut highlighter {
                        text::highlight(&mut store, &value, h)?;
                    } else {
                        store.write(&value, Ink::default(), None)?;
                    }
                    drop(store);
                    if too_long {
                        shared.lock().unwrap().note =
                            Some("Very long lines use plain styling".into());
                        if ends {
                            highlighter = syntax.map(text::highlighter);
                        }
                    }
                    long_line = too_long && !ends;
                    shared.lock().unwrap().bytes += bytes.len() as u64;
                    revision.fetch_add(1, Ordering::Release);
                }
            }
            store.lock().unwrap().finish()?;
        }
        _ => {
            let mut file = tempfile::tempfile().context("Create private viewer snapshot")?;
            let mut buffer = [0; 64 * 1024];
            loop {
                ctx.check()?;
                let n = reader.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                file.write_all(&buffer[..n])
                    .context("Write viewer snapshot (check free temporary disk space)")?;
                shared.lock().unwrap().bytes += n as u64;
                revision.fetch_add(1, Ordering::Release);
            }
            file.flush()?;
            shared.lock().unwrap().snapshot = Some(Arc::new(Snapshot::new(file)?));
        }
    }
    Ok(())
}
fn fragment(reader: &mut impl BufRead) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    while bytes.len() < LINE_LIMIT {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        let n = available
            .iter()
            .position(|b| *b == b'\n')
            .map_or(available.len(), |n| n + 1)
            .min(LINE_LIMIT - bytes.len());
        bytes.extend_from_slice(&available[..n]);
        reader.consume(n);
        if bytes.ends_with(b"\n") {
            break;
        }
    }
    // Complete a split UTF-8 codepoint without allocating an unbounded line.
    for _ in 0..3 {
        if !std::str::from_utf8(&bytes).is_err_and(|e| e.error_len().is_none()) {
            break;
        }
        let available = reader.fill_buf()?;
        if available.first().is_none_or(|b| b & 0xc0 != 0x80) {
            break;
        }
        bytes.push(available[0]);
        reader.consume(1);
    }
    Ok(bytes)
}

#[derive(Default)]
struct RenderCache {
    markdown: Option<(u16, Store)>,
    image: Option<DynamicImage>,
    pdf: Option<hayro::hayro_syntax::Pdf>,
    // One rendered page is retained: revisiting other pages rerenders them.
    page: Option<(usize, u32, DynamicImage)>,
}
fn render(
    r: Request,
    source: &Source,
    cache: &mut RenderCache,
    picker: &Picker,
    ctx: &Context,
) -> Result<Reply> {
    let mut reply = Reply::empty(r.id);
    let Some(format) = &source.format else {
        return Ok(reply);
    };
    if let Some(store) = &source.store {
        let mut store = store.lock().unwrap();
        text_view(&mut reply, &mut store, r, !matches!(format, Format::Binary))?;
    } else if let Some(snapshot) = &source.snapshot {
        match format {
            Format::Markdown => {
                if cache
                    .markdown
                    .as_ref()
                    .is_none_or(|(width, _)| *width != r.width)
                {
                    let source = std::str::from_utf8(snapshot.as_ref().as_ref())
                        .context("Markdown must be UTF-8")?;
                    cache.markdown = Some((r.width, text::markdown(source, r.width, ctx)?));
                }
                text_view(
                    &mut reply,
                    &mut cache.markdown.as_mut().unwrap().1,
                    r,
                    false,
                )?;
            }
            Format::Image => {
                if cache.image.is_none() {
                    let mut reader = image::ImageReader::new(BufReader::new(RasterInput::new(
                        snapshot.as_ref().as_ref(),
                    )))
                    .with_guessed_format()?;
                    let mut limits = image::Limits::default();
                    limits.max_alloc = Some(PIXEL_LIMIT * 4);
                    reader.limits(limits);
                    let image = reader
                        .decode()
                        .context("Decode image (decoded image limit: 256 MiB)")?;
                    check_pixels(image.width(), image.height())?;
                    cache.image = Some(image);
                }
                graphics_view(&mut reply, cache.image.as_ref().unwrap(), r, picker, false)?;
            }
            Format::Pdf => {
                if cache.pdf.is_none() {
                    cache.pdf = Some(hayro::hayro_syntax::Pdf::new(snapshot.clone()).map_err(
                        |e| {
                            anyhow::anyhow!(
                                "Cannot open PDF (encrypted or unsupported document): {e:?}"
                            )
                        },
                    )?);
                }
                let pdf = cache.pdf.as_ref().unwrap();
                let pages = pdf.pages();
                reply.pages = pages.len();
                anyhow::ensure!(reply.pages > 0, "PDF has no pages");
                reply.page = r.page.min(reply.pages - 1);
                let page = &pages[reply.page];
                let (width, height) = page.render_dimensions();
                anyhow::ensure!(
                    width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0,
                    "Invalid PDF page dimensions"
                );
                let font = picker.font_size();
                let scale = (f32::from(r.width) * f32::from(font.0) / width) * zoom(r.zoom);
                let w = (width * scale).ceil().max(1.0) as u32;
                let h = (height * scale).ceil().max(1.0) as u32;
                check_pixels(w, h)?;
                anyhow::ensure!(
                    w <= u16::MAX as u32 && h <= u16::MAX as u32,
                    "PDF zoom exceeds renderer dimensions; zoom out"
                );
                if cache
                    .page
                    .as_ref()
                    .is_none_or(|(page, bits, _)| *page != reply.page || *bits != scale.to_bits())
                {
                    cache.page = None;
                    ctx.check()?;
                    let pixmap = hayro::render(
                        page,
                        &hayro::RenderCache::new(),
                        &Default::default(),
                        &hayro::RenderSettings {
                            x_scale: scale,
                            y_scale: scale,
                            width: Some(w as u16),
                            height: Some(h as u16),
                            bg_color: hayro::vello_cpu::color::palette::css::WHITE,
                        },
                    );
                    ctx.check()?;
                    let pixels =
                        image::RgbaImage::from_raw(w, h, pixmap.data_as_u8_slice().to_vec())
                            .ok_or_else(|| anyhow::anyhow!("Invalid PDF bitmap"))?;
                    cache.page = Some((
                        reply.page,
                        scale.to_bits(),
                        DynamicImage::ImageRgba8(pixels),
                    ));
                }
                graphics_view(&mut reply, &cache.page.as_ref().unwrap().2, r, picker, true)?;
            }
            _ => {}
        }
    }
    ctx.check()?;
    Ok(reply)
}
fn text_view(reply: &mut Reply, store: &mut Store, r: Request, numbers: bool) -> Result<()> {
    reply.total = store.rows;
    reply.top = r.top.min(store.rows.saturating_sub(u64::from(r.height)));
    let gutter = if numbers {
        (store.rows.max(1).ilog10() + 3).min(u32::from(r.width.saturating_sub(1))) as u16
    } else {
        0
    };
    let width = r.width.saturating_sub(gutter);
    reply.left = r.left.min(store.max_width.saturating_sub(u64::from(width)));
    for row in reply.top..store.rows.min(reply.top + u64::from(r.height)) {
        let mut line = store.line(row, reply.left, width)?;
        if gutter > 0 {
            line.spans.insert(
                0,
                Span::styled(
                    format!(
                        "{:>digits$}  ",
                        row + 1,
                        digits = gutter.saturating_sub(2) as usize
                    ),
                    Style::default().fg(Color::Rgb(123, 138, 156)),
                ),
            );
        }
        reply.lines.push(line);
    }
    Ok(())
}
fn zoom(step: i8) -> f32 {
    1.25_f32.powi(i32::from(step))
}
fn check_pixels(w: u32, h: u32) -> Result<()> {
    anyhow::ensure!(
        u64::from(w) * u64::from(h) <= PIXEL_LIMIT,
        "Decoded image/render exceeds 64 megapixels; reduce zoom or image dimensions"
    );
    Ok(())
}
fn graphics_view(
    reply: &mut Reply,
    image: &DynamicImage,
    r: Request,
    picker: &Picker,
    prescaled: bool,
) -> Result<()> {
    let (cell_w, cell_h) = picker.font_size();
    let (view_w, view_h) = (
        u32::from(r.width) * u32::from(cell_w),
        u32::from(r.height) * u32::from(cell_h),
    );
    let scale = if prescaled {
        1.0
    } else {
        (view_w as f32 / image.width() as f32).min(view_h as f32 / image.height() as f32)
            * zoom(r.zoom)
    };
    let w = (image.width() as f32 * scale).round().max(1.0) as u32;
    let h = (image.height() as f32 * scale).round().max(1.0) as u32;
    check_pixels(w, h)?;
    reply.total = u64::from(h.div_ceil(u32::from(cell_h)));
    reply.top = r.top.min(u64::from(
        h.saturating_sub(view_h).div_ceil(u32::from(cell_h)),
    ));
    reply.left = r.left.min(u64::from(
        w.saturating_sub(view_w).div_ceil(u32::from(cell_w)),
    ));
    let x = (reply.left * u64::from(cell_w)).min(u64::from(w.saturating_sub(view_w))) as u32;
    let y = (reply.top * u64::from(cell_h)).min(u64::from(h.saturating_sub(view_h))) as u32;
    if picker.protocol_type() == ProtocolType::Halfblocks {
        // Render only the visible source rectangle, directly to two pixels per cell.
        // A full pixel-size intermediate (and resizing it again) is wasted for half-blocks.
        let source_x = (u64::from(x) * u64::from(image.width()) / u64::from(w)) as u32;
        let source_y = (u64::from(y) * u64::from(image.height()) / u64::from(h)) as u32;
        let source_right = (u64::from((x + view_w).min(w)) * u64::from(image.width()))
            .div_ceil(u64::from(w)) as u32;
        let source_bottom = (u64::from((y + view_h).min(h)) * u64::from(image.height()))
            .div_ceil(u64::from(h)) as u32;
        let cropped = if source_x == 0
            && source_y == 0
            && source_right == image.width()
            && source_bottom == image.height()
        {
            std::borrow::Cow::Borrowed(image)
        } else {
            std::borrow::Cow::Owned(image.crop_imm(
                source_x,
                source_y,
                source_right - source_x,
                source_bottom - source_y,
            ))
        };
        let area = Rect::new(
            0,
            0,
            w.min(view_w).div_ceil(u32::from(cell_w)) as u16,
            h.min(view_h).div_ceil(u32::from(cell_h)) as u16,
        );
        let pixels = cropped.thumbnail_exact(u32::from(area.width), u32::from(area.height) * 2);
        reply.image = Some(Protocol::Halfblocks(
            ratatui_image::protocol::halfblocks::Halfblocks::new(pixels, area)?,
        ));
        return Ok(());
    }
    let crop = if w == image.width() && h == image.height() {
        image.crop_imm(x, y, w.min(view_w), h.min(view_h))
    } else {
        image
            .resize_exact(w, h, image::imageops::FilterType::Triangle)
            .crop_imm(x, y, w.min(view_w), h.min(view_h))
    };
    reply.image =
        Some(picker.new_protocol(crop, Rect::new(0, 0, r.width, r.height), Resize::Fit(None))?);
    Ok(())
}

pub use terminal::ImageColors;
pub use terminal::detect as detect_terminal;
/// All images in the alternate screen belong to this viewer. Sixel/iTerm are cleared
/// by Terminal::clear; Kitty also needs deletion of its retained image data.
pub fn clear_graphics(picker: &Picker) -> Result<()> {
    if picker.protocol_type() == ProtocolType::Kitty {
        let mut out = std::io::stdout().lock();
        let delete = "\x1b_Ga=d,d=A,q=2\x1b\\";
        if std::env::var_os("TMUX").is_some() {
            write!(
                out,
                "\x1bPtmux;{}\x1b\\",
                delete.replace('\x1b', "\x1b\x1b")
            )?;
        } else {
            out.write_all(delete.as_bytes())?;
        }
        out.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui_image::Image;
    use std::{path::Path, time::Instant};

    #[test]
    #[ignore = "manual image rendering benchmark; set MC_TEST_IMAGE to a local image"]
    fn image_render_timing() {
        let path = std::env::var_os("MC_TEST_IMAGE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/viewer.png")
            });
        let shared = source();
        let start = Instant::now();
        load(
            local(&path),
            &Context::default(),
            &shared,
            &AtomicU64::new(0),
        )
        .unwrap();
        eprintln!("snapshot: {:?}", start.elapsed());
        let source = shared.into_inner().unwrap();
        let mut cache = RenderCache::default();
        let picker = Picker::halfblocks();
        let mut r = request();
        r.width = 110;
        r.height = 28;
        let start = Instant::now();
        let first = render(r, &source, &mut cache, &picker, &Context::default()).unwrap();
        assert!(first.image.is_some());
        eprintln!("first frame: {:?}", start.elapsed());
        r.zoom = 2;
        let start = Instant::now();
        render(r, &source, &mut cache, &picker, &Context::default()).unwrap();
        eprintln!("zoom: {:?}", start.elapsed());
    }

    fn source() -> Mutex<Source> {
        Mutex::new(Source {
            format: None,
            store: None,
            snapshot: None,
            bytes: 0,
            total: 0,
            done: false,
            error: None,
            note: None,
        })
    }
    fn request() -> Request {
        Request {
            id: 1,
            top: 0,
            left: 0,
            page: 0,
            zoom: 0,
            width: 40,
            height: 8,
        }
    }
    fn local(path: &Path) -> VfsPath {
        VfsPath::new(Arc::new(crate::vfs::local::Local), path.to_owned())
    }
    fn row(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }
    fn wait(viewer: &mut Viewer, predicate: impl Fn(&Viewer) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            viewer.poll();
            if predicate(viewer) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "viewer timed out: {:?}",
                viewer.source.lock().unwrap().error
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    #[test]
    fn signatures_take_precedence_and_text_detects_names_and_shebangs() {
        assert_eq!(
            detect(
                Path::new("wrong.md"),
                include_bytes!("../../tests/fixtures/viewer.png")
            ),
            Format::Image
        );
        assert_eq!(
            detect(
                Path::new("wrong.rs"),
                include_bytes!("../../tests/fixtures/viewer.pdf")
            ),
            Format::Pdf
        );
        assert_eq!(detect(Path::new("README.MD"), b"# Hello"), Format::Markdown);
        assert!(matches!(
            detect(Path::new("main.rs"), b"fn main() {}"),
            Format::Code(_)
        ));
        assert!(matches!(
            detect(Path::new("Makefile"), b"all:\n\techo ok"),
            Format::Code(_)
        ));
        assert!(matches!(
            detect(Path::new("script"), b"#!/usr/bin/env python\nprint(1)"),
            Format::Code(_)
        ));
        assert_eq!(detect(Path::new("data"), b"\0\xff"), Format::Binary);
        assert_eq!(
            detect(Path::new("text"), "Žluťoučký 🦀".as_bytes()),
            Format::Text
        );
    }
    #[test]
    fn disk_rows_preserve_unicode_tabs_styles_and_interleaved_reads() {
        let mut store = Store::new().unwrap();
        store
            .write(
                "a\t界e\u{301}\x1b[31m\n",
                Ink {
                    rgb: [1, 2, 3],
                    flags: 1,
                },
                None,
            )
            .unwrap();
        assert_eq!(row(&store.line(0, 0, 30).unwrap()), "a   界e\u{301}�[31m");
        assert_eq!(row(&store.line(0, 5, 3).unwrap()), " e\u{301}�");
        store.write("second\n", Ink::default(), None).unwrap();
        store.finish().unwrap();
        assert_eq!(row(&store.line(1, 0, 20).unwrap()), "second");
        assert!(
            store.line(0, 0, 30).unwrap().spans[0]
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
    }
    #[test]
    fn markdown_styles_reflow_and_fenced_code_keeps_indentation() {
        let source = "# Heading\n\nA **bold** and *italic* paragraph with words to wrap.\n\n```rust\n/* hello\nworld */\n    let n = 123;\n```\n\n[link](https://example.com)\n\n![alt](missing.png)\n<script>inert</script>";
        let mut wide = text::markdown(source, 80, &Context::default()).unwrap();
        let mut narrow = text::markdown(source, 20, &Context::default()).unwrap();
        assert!(narrow.rows > wide.rows);
        assert!(wide.line(0, 0, 80).unwrap().spans.iter().any(|s| {
            s.style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
        }));
        let rows = (0..wide.rows)
            .map(|n| row(&wide.line(n, 0, 120).unwrap()))
            .collect::<Vec<_>>();
        assert!(rows.iter().any(|s| s.starts_with("    let n")));
        assert!(
            rows.iter()
                .any(|s| s.contains("[Image: alt] (missing.png)"))
        );
        assert!(rows.iter().any(|s| s.contains("<script>inert</script>")));
        assert!(rows.iter().any(|s| s.contains("https://example.com")));
        assert!(narrow.line(0, 0, 20).unwrap().width() <= 20);
    }
    #[test]
    fn highlighted_multiline_comments_survive_distant_disk_reads() {
        let mut store = Store::new().unwrap();
        let mut highlighter =
            text::highlighter(text::syntaxes().find_syntax_by_extension("rs").unwrap());
        for value in [
            "/* comment\n",
            "still comment\n",
            "*/\n",
            "let number = 42;\n",
        ] {
            text::highlight(&mut store, value, &mut highlighter).unwrap();
        }
        let comment = store.line(1, 0, 40).unwrap();
        assert_eq!(row(&comment), "still comment");
        let code = store.line(3, 0, 40).unwrap();
        assert!(
            code.spans
                .iter()
                .any(|s| s.style.fg != comment.spans[0].style.fg)
        );
    }
    #[test]
    fn long_line_is_complete_and_horizontal_navigation_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.txt");
        let mut file = File::create(&path).unwrap();
        let chunk = vec![b'x'; 1024 * 1024];
        for _ in 0..65 {
            file.write_all(&chunk).unwrap();
        }
        file.write_all(b"THE-END\nlast row\n").unwrap();
        drop(file);
        let shared = source();
        load(
            local(&path),
            &Context::default(),
            &shared,
            &AtomicU64::new(0),
        )
        .unwrap();
        let state = shared.into_inner().unwrap();
        assert_eq!(state.bytes, 65 * 1024 * 1024 + 17);
        let mut store = state
            .store
            .unwrap()
            .lock()
            .unwrap()
            .line(0, 65 * 1024 * 1024, 20)
            .unwrap();
        assert_eq!(row(&store), "THE-END");
        store.spans.clear();
    }
    #[test]
    fn empty_and_binary_files_render_without_panics() {
        let dir = tempfile::tempdir().unwrap();
        for (name, bytes) in [("empty", &b""[..]), ("binary", &b"\x00\xffhello"[..])] {
            let path = dir.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            let shared = source();
            load(
                local(&path),
                &Context::default(),
                &shared,
                &AtomicU64::new(0),
            )
            .unwrap();
            let state = shared.into_inner().unwrap();
            let reply = render(
                request(),
                &state,
                &mut RenderCache::default(),
                &Picker::halfblocks(),
                &Context::default(),
            )
            .unwrap();
            assert_eq!(reply.total, 1);
            if name == "binary" {
                assert!(row(&reply.lines[0]).contains("00 ff 68 65 6c 6c 6f"));
            }
        }
    }
    #[test]
    fn image_and_pdf_pages_render_through_halfblocks() {
        for name in ["viewer.png", "viewer.pdf"] {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(name);
            let shared = source();
            load(
                local(&path),
                &Context::default(),
                &shared,
                &AtomicU64::new(0),
            )
            .unwrap();
            let state = shared.into_inner().unwrap();
            let mut cache = RenderCache::default();
            let mut r = request();
            let first = render(
                r,
                &state,
                &mut cache,
                &Picker::halfblocks(),
                &Context::default(),
            )
            .unwrap();
            assert!(matches!(first.image, Some(Protocol::Halfblocks(_))));
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 8)).unwrap();
            terminal
                .draw(|frame| {
                    frame.render_widget(Image::new(first.image.as_ref().unwrap()), frame.area())
                })
                .unwrap();
            assert!(
                terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .any(|c| c.bg != Color::Reset)
            );
            if name.ends_with("pdf") {
                assert_eq!(first.pages, 2);
                r.page = usize::MAX;
                let second = render(
                    r,
                    &state,
                    &mut cache,
                    &Picker::halfblocks(),
                    &Context::default(),
                )
                .unwrap();
                assert_eq!((second.page, second.pages), (1, 2));
                assert_eq!(cache.page.as_ref().unwrap().0, 1);
            }
            r.top = u64::MAX;
            r.left = u64::MAX;
            let last = render(
                r,
                &state,
                &mut cache,
                &Picker::halfblocks(),
                &Context::default(),
            )
            .unwrap();
            assert!(last.top < 1000 && last.left < 1000);
        }
    }
    #[test]
    fn completed_download_shows_rendering_until_pixels_are_ready() {
        for format in [Format::Image, Format::Pdf] {
            let source = source();
            source.lock().unwrap().format = Some(format);
            source.lock().unwrap().done = true;
            let mut viewer = Viewer {
                title: "example".into(),
                ctx: Context::default(),
                source: Arc::new(source),
                desired: Arc::new(Mutex::new(request())),
                result: Arc::new(Mutex::new(None)),
                current: Reply::empty(1),
                redraw: false,
                graphics: true,
                modal: false,
            };
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 10)).unwrap();
            terminal.draw(|frame| viewer.draw(frame)).unwrap();
            let screen = |terminal: &ratatui::Terminal<ratatui::backend::TestBackend>| {
                terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|c| c.symbol())
                    .collect::<String>()
            };
            assert!(screen(&terminal).contains("Rendering image… Esc: cancel"));
            let pixels = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                32,
                32,
                image::Rgb([255, 0, 0]),
            ));
            let mut reply = Reply::empty(1);
            graphics_view(&mut reply, &pixels, request(), &Picker::halfblocks(), false).unwrap();
            viewer.current = reply;
            terminal.draw(|frame| viewer.draw(frame)).unwrap();
            assert!(!screen(&terminal).contains("Rendering"));
            // A solid image uses spaces with colored backgrounds, not block glyphs.
            assert!(
                terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .any(|c| { c.symbol() == " " && c.bg == Color::Rgb(255, 0, 0) })
            );
        }
    }

    #[test]
    fn photograph_sized_jpeg_reaches_a_visible_frame() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.jpg");
        image::RgbImage::from_fn(4032, 3024, |x, y| {
            image::Rgb([(x / 16) as u8, (y / 12) as u8, 128])
        })
        .save(&path)
        .unwrap();
        let mut viewer = Viewer::open(local(&path), Context::default(), Picker::halfblocks());
        viewer.resize(110, 30);
        wait(&mut viewer, Viewer::has_image);
        assert!(viewer.current.error.is_none());
        assert!(viewer.current.total > 0);
    }

    #[test]
    fn halfblock_zoom_and_pan_sample_the_visible_source_region() {
        let pixels = DynamicImage::ImageRgb8(image::RgbImage::from_fn(128, 128, |x, _| {
            image::Rgb(if x < 64 { [255, 0, 0] } else { [0, 0, 255] })
        }));
        for (left, expected) in [
            (0, Color::Rgb(255, 0, 0)),
            (u64::MAX, Color::Rgb(0, 0, 255)),
        ] {
            let r = Request {
                width: 4,
                height: 4,
                zoom: 8,
                left,
                ..request()
            };
            let mut reply = Reply::empty(r.id);
            graphics_view(&mut reply, &pixels, r, &Picker::halfblocks(), false).unwrap();
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(4, 4)).unwrap();
            terminal
                .draw(|frame| {
                    frame.render_widget(
                        graphics::Graphic(reply.image.as_ref().unwrap()),
                        frame.area(),
                    )
                })
                .unwrap();
            assert!(
                terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .all(|cell| cell.bg == expected)
            );
        }
    }
    #[test]
    fn navigation_resize_stale_responses_and_close() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rows.txt");
        std::fs::write(
            &path,
            (0..200).map(|n| format!("row {n}\n")).collect::<String>(),
        )
        .unwrap();
        let ctx = Context::default();
        let cancel = ctx.cancel.clone();
        let mut viewer = Viewer::open(local(&path), ctx, Picker::halfblocks());
        viewer.resize(40, 10);
        wait(&mut viewer, |v| v.current.total == 200);
        viewer.key(KeyEvent::from(KeyCode::End));
        wait(&mut viewer, |v| v.current.top == 192);
        viewer.resize(40, 20);
        wait(&mut viewer, |v| v.current.top == 182);
        viewer.key(KeyEvent::from(KeyCode::Home));
        let id = viewer.desired.lock().unwrap().id;
        *viewer.result.lock().unwrap() = Some(Reply::empty(id - 1));
        let previous = viewer.current.id;
        viewer.poll();
        assert_eq!(viewer.current.id, previous);
        wait(&mut viewer, |v| v.current.top == 0);
        let before = *viewer.desired.lock().unwrap();
        for code in [
            KeyCode::F(4),
            KeyCode::F(8),
            KeyCode::F(10),
            KeyCode::Delete,
            KeyCode::Char('x'),
        ] {
            assert!(!viewer.key(KeyEvent::from(code)));
        }
        assert_eq!(*viewer.desired.lock().unwrap(), before);
        assert!(viewer.key(KeyEvent::from(KeyCode::Esc)));
        drop(viewer);
        assert!(cancel.load(Ordering::Relaxed));
    }
    #[test]
    fn split_utf8_fragments_and_resource_errors_are_explicit() {
        let mut bytes = vec![b'a'; LINE_LIMIT - 1];
        bytes.extend_from_slice("🦀\n".as_bytes());
        let mut reader = BufReader::new(std::io::Cursor::new(bytes));
        let part = fragment(&mut reader).unwrap();
        assert!(std::str::from_utf8(&part).unwrap().ends_with('🦀'));
        assert!(check_pixels(100_000, 100_000).is_err());
        let ctx = Context::default();
        ctx.cancel.store(true, Ordering::Relaxed);
        assert!(text::markdown("hello", 20, &ctx).is_err());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.pdf");
        std::fs::write(&path, b"%PDF-broken").unwrap();
        let shared = source();
        load(
            local(&path),
            &Context::default(),
            &shared,
            &AtomicU64::new(0),
        )
        .unwrap();
        assert!(
            render(
                request(),
                &shared.into_inner().unwrap(),
                &mut RenderCache::default(),
                &Picker::halfblocks(),
                &Context::default()
            )
            .is_err()
        );
    }
    struct StreamingFs {
        release: Arc<std::sync::atomic::AtomicBool>,
        fail: bool,
    }
    struct StreamReader {
        data: std::io::Cursor<Vec<u8>>,
        release: Arc<std::sync::atomic::AtomicBool>,
        ctx: Context,
        fail: bool,
        ended: bool,
    }
    impl Read for StreamReader {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            let n = self.data.read(out)?;
            if n > 0 {
                return Ok(n);
            }
            while !self.release.load(Ordering::Relaxed) {
                self.ctx.check().map_err(std::io::Error::other)?;
                std::thread::sleep(Duration::from_millis(5));
            }
            if self.fail {
                return Err(std::io::Error::other("injected remote read failure"));
            }
            if self.ended {
                return Ok(0);
            }
            self.ended = true;
            self.data = std::io::Cursor::new(b"final remote row\n".to_vec());
            self.data.read(out)
        }
    }
    impl crate::vfs::FileSystem for StreamingFs {
        fn id(&self) -> String {
            "viewer-test".into()
        }
        fn label(&self, path: &Path) -> String {
            path.display().to_string()
        }
        fn metadata(&self, _: &Path, _: bool, _: &Context) -> Result<crate::vfs::Metadata> {
            Ok(crate::vfs::Metadata {
                kind: Kind::File,
                size: 11017,
                modified: std::time::SystemTime::UNIX_EPOCH,
                permissions: None,
            })
        }
        fn read_dir(&self, _: &Path, _: &Context) -> Result<Vec<crate::vfs::DirEntry>> {
            Ok(vec![])
        }
        fn open_read(&self, _: &Path, ctx: &Context) -> Result<Box<dyn Read + Send>> {
            Ok(Box::new(StreamReader {
                data: std::io::Cursor::new(b"remote row\n".repeat(1000)),
                release: self.release.clone(),
                ctx: ctx.clone(),
                fail: self.fail,
                ended: false,
            }))
        }
    }
    #[test]
    fn progressive_remote_reads_scroll_without_waiting_and_cancel_releases_caches() {
        let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let path = VfsPath::new(
            Arc::new(StreamingFs {
                release,
                fail: false,
            }),
            "rows.log".into(),
        );
        let mut viewer = Viewer::open(path, Context::default(), Picker::halfblocks());
        viewer.resize(40, 10);
        wait(&mut viewer, |v| v.current.total == 1000);
        assert!(!viewer.source.lock().unwrap().done);
        viewer.key(KeyEvent::from(KeyCode::PageDown));
        wait(&mut viewer, |v| v.current.top == 8);
        let weak = Arc::downgrade(&viewer.source);
        drop(viewer);
        let deadline = Instant::now() + Duration::from_secs(2);
        while weak.upgrade().is_some() {
            assert!(
                Instant::now() < deadline,
                "cancelled worker retained its disk caches"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    #[test]
    fn remote_read_failure_is_visible_and_end_follows_new_rows() {
        for fail in [false, true] {
            let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let path = VfsPath::new(
                Arc::new(StreamingFs {
                    release: release.clone(),
                    fail,
                }),
                "rows.log".into(),
            );
            let mut viewer = Viewer::open(path, Context::default(), Picker::halfblocks());
            viewer.resize(40, 10);
            wait(&mut viewer, |v| v.current.total == 1000);
            viewer.key(KeyEvent::from(KeyCode::End));
            release.store(true, Ordering::Relaxed);
            wait(&mut viewer, |v| v.source.lock().unwrap().done);
            if fail {
                assert!(
                    viewer
                        .source
                        .lock()
                        .unwrap()
                        .error
                        .as_ref()
                        .unwrap()
                        .contains("injected remote read failure")
                );
            } else {
                wait(&mut viewer, |v| {
                    v.current.total == 1001 && v.current.top == 993
                });
            }
        }
    }
    #[test]
    fn native_protocols_encode_and_snapshot_does_not_track_source_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.png");
        std::fs::write(&path, include_bytes!("../../tests/fixtures/viewer.png")).unwrap();
        let shared = source();
        load(
            local(&path),
            &Context::default(),
            &shared,
            &AtomicU64::new(0),
        )
        .unwrap();
        std::fs::write(&path, b"overwritten after snapshot").unwrap();
        let state = shared.into_inner().unwrap();
        for protocol in [
            ProtocolType::Kitty,
            ProtocolType::Sixel,
            ProtocolType::Iterm2,
        ] {
            let mut picker = Picker::halfblocks();
            picker.set_protocol_type(protocol);
            let reply = render(
                request(),
                &state,
                &mut RenderCache::default(),
                &picker,
                &Context::default(),
            )
            .unwrap();
            let image = reply.image.unwrap();
            assert!(image.area().width > 0);
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 8)).unwrap();
            terminal
                .draw(|f| f.render_widget(Image::new(&image), f.area()))
                .unwrap();
            assert!(
                terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .any(|cell| cell.symbol().contains('\x1b'))
            );
        }
    }
    #[test]
    fn ico_uses_first_entry_without_copying_or_modifying_source() {
        let image = image::load_from_memory(include_bytes!("../../tests/fixtures/viewer.png"))
            .unwrap()
            .into_rgba8();
        let mut png = std::io::Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let png = png.into_inner();
        let mut ico = vec![0, 0, 1, 0, 2, 0];
        for (dimension, size, offset) in [
            (2u8, png.len() as u32, 38u32),
            (4, 6, 38 + png.len() as u32),
        ] {
            ico.extend_from_slice(&[dimension, dimension, 0, 0, 1, 0, 32, 0]);
            ico.extend_from_slice(&size.to_le_bytes());
            ico.extend_from_slice(&offset.to_le_bytes());
        }
        ico.extend_from_slice(&png);
        ico.extend_from_slice(b"broken");
        let result = image::ImageReader::new(BufReader::new(RasterInput::new(&ico)))
            .with_guessed_format()
            .unwrap()
            .decode()
            .unwrap();
        assert_eq!((result.width(), result.height()), (2, 2));
        assert_eq!(&ico[4..6], &[2, 0]);
    }
}
