use super::store::{Ink, Store};
use anyhow::Result;
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use std::sync::OnceLock;
use syntect::{
    easy::HighlightLines,
    highlighting::{FontStyle, ThemeSet},
    parsing::{SyntaxReference, SyntaxSet},
};

pub(super) fn syntaxes() -> &'static SyntaxSet {
    static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAXES.get_or_init(SyntaxSet::load_defaults_newlines)
}
pub(super) fn highlighter(syntax: &'static SyntaxReference) -> HighlightLines<'static> {
    static THEMES: OnceLock<ThemeSet> = OnceLock::new();
    let themes = THEMES.get_or_init(ThemeSet::load_defaults);
    HighlightLines::new(syntax, &themes.themes["base16-ocean.dark"])
}
pub(super) fn highlight(
    store: &mut Store,
    text: &str,
    highlighter: &mut HighlightLines<'_>,
) -> Result<()> {
    match highlighter.highlight_line(text, syntaxes()) {
        Ok(runs) => {
            for (style, value) in runs {
                let mut flags = 0;
                if style.font_style.contains(FontStyle::BOLD) {
                    flags |= 1;
                }
                if style.font_style.contains(FontStyle::ITALIC) {
                    flags |= 2;
                }
                if style.font_style.contains(FontStyle::UNDERLINE) {
                    flags |= 4;
                }
                store.write(
                    value,
                    Ink {
                        rgb: [style.foreground.r, style.foreground.g, style.foreground.b],
                        flags,
                    },
                    None,
                )?;
            }
        }
        Err(_) => store.write(text, Ink::default(), None)?,
    }
    Ok(())
}

/// Parse the mapped document once per width. Styled rows and their index stay on disk.
pub(super) fn markdown(source: &str, width: u16, cancel: &crate::vfs::Context) -> Result<Store> {
    let mut store = Store::new()?;
    let accent = Ink {
        rgb: [96, 210, 190],
        flags: 1,
    };
    let mut styles = vec![Ink::default()];
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut code: Option<HighlightLines<'static>> = None;
    let mut code_buffer = String::new();
    let mut code_long_line = false;
    let mut link_targets = Vec::new();
    let mut table_cell = 0;
    for event in Parser::new_ext(
        source,
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS,
    ) {
        cancel.check()?;
        let ink = *styles.last().unwrap();
        match event {
            Event::Start(tag) => {
                let mut next = ink;
                match tag {
                    Tag::Heading { .. } => {
                        paragraph(&mut store)?;
                        next = accent;
                    }
                    Tag::Paragraph => {
                        if code.is_none() && lists.is_empty() {
                            paragraph(&mut store)?;
                        }
                    }
                    Tag::Emphasis => next.flags |= 2,
                    Tag::Strong => next.flags |= 1,
                    Tag::Strikethrough => next.flags |= 8,
                    Tag::BlockQuote(_) => {
                        paragraph(&mut store)?;
                        store.write("│ ", accent, None)?;
                        next.rgb = [123, 138, 156];
                    }
                    Tag::List(start) => {
                        paragraph(&mut store)?;
                        lists.push(start);
                    }
                    Tag::Item => {
                        paragraph(&mut store)?;
                        store.write(&"  ".repeat(lists.len().saturating_sub(1)), ink, None)?;
                        let label = match lists.last_mut() {
                            Some(Some(n)) => {
                                let label = format!("{n}. ");
                                *n += 1;
                                label
                            }
                            _ => "• ".into(),
                        };
                        store.write(&label, accent, None)?;
                    }
                    Tag::CodeBlock(kind) => {
                        paragraph(&mut store)?;
                        let syntax = match kind {
                            CodeBlockKind::Fenced(lang) => syntaxes()
                                .find_syntax_by_token(lang.split_whitespace().next().unwrap_or("")),
                            _ => None,
                        }
                        .unwrap_or_else(|| syntaxes().find_syntax_plain_text());
                        code = Some(highlighter(syntax));
                    }
                    Tag::Link { dest_url, .. } => {
                        next.flags |= 4;
                        next.rgb = accent.rgb;
                        link_targets.push(dest_url);
                    }
                    Tag::Image { dest_url, .. } => {
                        store.write("[Image: ", ink, Some(width))?;
                        link_targets.push(dest_url);
                    }
                    Tag::Table(_) => paragraph(&mut store)?,
                    Tag::TableHead | Tag::TableRow => {
                        paragraph(&mut store)?;
                        table_cell = 0;
                        if matches!(tag, Tag::TableHead) {
                            next.flags |= 1;
                        }
                    }
                    Tag::TableCell => {
                        if table_cell > 0 {
                            store.write(" │ ", accent, Some(width))?;
                        }
                        table_cell += 1;
                    }
                    _ => {}
                }
                styles.push(next);
            }
            Event::End(tag) => {
                match tag {
                    TagEnd::CodeBlock => {
                        if let Some(mut h) = code.take()
                            && !code_buffer.is_empty()
                        {
                            highlight(&mut store, &code_buffer, &mut h)?;
                            code_buffer.clear();
                        }
                        code_long_line = false;
                        paragraph(&mut store)?;
                    }
                    TagEnd::Link | TagEnd::Image => {
                        if let Some(target) = link_targets.pop() {
                            if tag == TagEnd::Image {
                                store.write("]", ink, Some(width))?;
                            }
                            store.write(" (", ink, Some(width))?;
                            store.write(&target, ink, Some(width))?;
                            store.write(")", ink, Some(width))?;
                        }
                    }
                    TagEnd::List(_) => {
                        lists.pop();
                        paragraph(&mut store)?;
                    }
                    TagEnd::Paragraph
                    | TagEnd::Heading(_)
                    | TagEnd::BlockQuote(_)
                    | TagEnd::Item
                    | TagEnd::Table
                    | TagEnd::TableHead
                    | TagEnd::TableRow => paragraph(&mut store)?,
                    _ => {}
                }
                if styles.len() > 1 {
                    styles.pop();
                }
            }
            Event::Text(value) => {
                if let Some(h) = &mut code {
                    // Bound individual syntax operations even for a megabyte-long code line.
                    for part in value.split_inclusive('\n') {
                        if code_long_line || code_buffer.len() + part.len() > super::LINE_LIMIT {
                            if !code_buffer.is_empty() {
                                store.write(&code_buffer, Ink::default(), None)?;
                                code_buffer.clear();
                            }
                            store.write(part, Ink::default(), None)?;
                            code_long_line = !part.ends_with('\n');
                        } else {
                            code_buffer.push_str(part);
                            if part.ends_with('\n') {
                                highlight(&mut store, &code_buffer, h)?;
                                code_buffer.clear();
                            }
                        }
                    }
                } else {
                    prose(&mut store, &value, ink, width)?;
                }
            }
            Event::Code(value) => store.write(
                &value,
                Ink {
                    rgb: [235, 203, 139],
                    flags: 0,
                },
                Some(width),
            )?,
            Event::SoftBreak => store.write(" ", ink, Some(width))?,
            Event::HardBreak => store.newline()?,
            Event::Rule => {
                paragraph(&mut store)?;
                store.write(&"─".repeat(width as usize), accent, None)?;
                store.newline()?;
            }
            Event::TaskListMarker(done) => {
                store.write(if done { "[x] " } else { "[ ] " }, accent, Some(width))?
            }
            // Raw HTML is displayed as inert text, never interpreted or fetched.
            Event::Html(value) | Event::InlineHtml(value) => store.write(
                &value,
                Ink {
                    rgb: [123, 138, 156],
                    flags: 0,
                },
                Some(width),
            )?,
            _ => {}
        }
    }
    store.finish()?;
    Ok(store)
}
fn paragraph(store: &mut Store) -> Result<()> {
    if store.column() > 0 {
        store.newline()?;
    }
    Ok(())
}
fn prose(store: &mut Store, value: &str, ink: Ink, width: u16) -> Result<()> {
    for word in value.split_inclusive(char::is_whitespace) {
        let size = ratatui::text::Span::raw(word.trim_end()).width() as u64;
        if store.column() > 0 && store.column() + size > u64::from(width.max(1)) {
            store.newline()?;
        }
        store.write(word, ink, Some(width))?;
    }
    Ok(())
}
