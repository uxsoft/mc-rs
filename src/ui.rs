use crate::{
    app::{App, Dialog},
    menu::{Action, MENUS},
};
use ratatui::{prelude::*, widgets::*};
use std::sync::atomic::Ordering;
use unicode_segmentation::UnicodeSegmentation;
mod icons;
const BG: Color = Color::Rgb(19, 23, 31);
const FG: Color = Color::Rgb(210, 219, 230);
const DIM: Color = Color::Rgb(123, 138, 156);
const ACCENT: Color = Color::Rgb(96, 210, 190);
const SELECTED: Color = Color::Rgb(38, 63, 75);
pub fn size(n: u64) -> String {
    if n < 1024 {
        n.to_string()
    } else if n < 1024 * 1024 {
        format!("{:.1}K", n as f64 / 1024.0)
    } else if n < 1024 * 1024 * 1024 {
        format!("{:.1}M", n as f64 / 1048576.0)
    } else {
        format!("{:.1}G", n as f64 / 1073741824.0)
    }
}
fn clip_line(value: &str, width: usize) -> String {
    let clean = value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>();
    if clean
        .graphemes(true)
        .map(|g| Span::raw(g).width())
        .sum::<usize>()
        <= width
    {
        return clean;
    }
    let mut out = String::new();
    let mut used = 0;
    for c in clean.graphemes(true) {
        let n = Span::raw(c).width();
        if used + n > width.saturating_sub(1) {
            break;
        }
        out.push_str(c);
        used += n;
    }
    if width > 0 {
        out.push('…');
    }
    out
}
pub(crate) struct SearchViewport {
    pub popup: Rect,
    pub results: Rect,
    pub start: usize,
}
pub(crate) fn search_viewport(area: Rect, cursor: usize) -> SearchViewport {
    let width = area.width.saturating_sub(4).min(86);
    let height = 19.min(area.height.saturating_sub(2));
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    let results = Rect::new(
        popup.x + 1,
        popup.y + 2,
        width.saturating_sub(2),
        height.saturating_sub(4),
    );
    SearchViewport {
        popup,
        results,
        start: cursor.saturating_sub(usize::from(results.height).saturating_sub(1)),
    }
}
fn popup(frame: &mut Frame, app: &mut App, title: &str, text: Text<'_>, height: u16) {
    let area = frame.area();
    let width = area.width.saturating_sub(4).min(86);
    let height = height.min(area.height.saturating_sub(2));
    let rect = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    app.dialog_area = rect;
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(FG).bg(BG))
            .block(
                Block::bordered()
                    .border_style(Style::default().fg(ACCENT))
                    .title(format!(" {title} ")),
            ),
        rect,
    );
}
pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    app.height = area.height;
    app.width = area.width;
    if let Some(viewer) = &mut app.viewer {
        viewer.draw(frame);
        draw_priority_overlays(frame, app);
        return;
    }
    frame.render_widget(Block::default().style(Style::default().bg(BG).fg(FG)), area);
    if area.width < 36 || area.height < 10 {
        frame.render_widget(
            Paragraph::new("Terminal too small — resize to at least 36 × 10. F10 quits.")
                .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(5),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);
    let bar_style = Style::default().fg(FG).bg(Color::Rgb(30, 38, 49));
    frame.render_widget(Block::default().style(bar_style), rows[0]);
    frame.render_widget(
        Paragraph::new(" mc ").style(
            Style::default()
                .fg(ACCENT)
                .bg(Color::Rgb(30, 38, 49))
                .bold(),
        ),
        Rect::new(area.x, area.y, 4, 1),
    );
    let mut x = area.x + 4;
    for (i, menu) in MENUS.iter().enumerate() {
        let rect = Rect::new(x, area.y, menu.label.len() as u16 + 2, 1);
        app.menu_tabs[i] = rect;
        let style = if app.menu.as_ref().is_some_and(|m| m.category == i) {
            Style::default().fg(BG).bg(ACCENT).bold()
        } else {
            bar_style
        };
        frame.render_widget(
            Paragraph::new(format!(" {} ", menu.label)).style(style),
            rect,
        );
        x = rect.right();
    }
    let panels =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(rows[1]);
    for (i, &rect) in panels.iter().enumerate() {
        app.areas[i] = rect;
        let p = &mut app.panels[i];
        let visible = rect.height.saturating_sub(3).max(1) as usize;
        if p.cursor < p.offset {
            p.offset = p.cursor;
        }
        if p.cursor >= p.offset + visible {
            p.offset = p.cursor + 1 - visible;
        }
        let border = if i == app.active {
            ACCENT
        } else {
            Color::Rgb(57, 70, 87)
        };
        let title = format!(
            " {}{} ",
            if !p.path.fs.capabilities().write {
                "▣ "
            } else {
                ""
            },
            p.label()
        );
        let bottom = if i == app.active && !app.quick.is_empty() {
            let query = app.quick.trim_start_matches('\0');
            let text = if query.is_empty() {
                " Type a filename · Esc cancels ".to_string()
            } else {
                format!(
                    " Find: {query}{} ",
                    if app.quick_found { "" } else { " · No match" }
                )
            };
            clip_line(&text, rect.width.saturating_sub(2) as usize)
        } else if p.selected.is_empty() {
            format!(
                " {} items · {:?}{} ",
                p.entries.len(),
                p.sort,
                if p.loading { " · Loading…" } else { "" }
            )
        } else {
            let (bytes, pending, errors) = p.selection_size();
            format!(
                " {} selected · {} bytes{}{} ",
                p.selected.len(),
                bytes,
                if pending > 0 {
                    " · Calculating…"
                } else {
                    ""
                },
                if errors > 0 { " · Partial" } else { "" }
            )
        };
        let block = Block::bordered()
            .border_style(Style::default().fg(border))
            .title(title)
            .title_bottom(bottom);
        frame.render_widget(block, rect);
        let inner = Rect::new(
            rect.x + 1,
            rect.y + 1,
            rect.width.saturating_sub(2),
            rect.height.saturating_sub(2),
        );
        let header = Row::new(["Name", "Size", "Modified"]).style(Style::default().fg(DIM));
        let mut table_rows = vec![];
        for index in p.offset..(p.offset + visible).min(p.entries.len() + 1) {
            let (name, bytes, date, selected, directory) = if index == 0 {
                (
                    format!("{} ..", icons::PARENT),
                    "UP".into(),
                    String::new(),
                    false,
                    true,
                )
            } else {
                let e = &p.entries[index - 1];
                let date: chrono::DateTime<chrono::Local> = e.modified.into();
                (
                    format!(
                        "{} {}",
                        icons::for_entry(&e.name, e.directory, e.link),
                        e.name
                    ),
                    if e.directory && !e.link {
                        if let Some(total) = p.directory_sizes.get(&e.path) {
                            format!(
                                "{}{}",
                                size(total.bytes),
                                if total.errors > 0 { "+" } else { "" }
                            )
                        } else if p.selected.contains(&e.path) {
                            "…".into()
                        } else {
                            "DIR".into()
                        }
                    } else {
                        size(e.size)
                    },
                    date.format("%m-%d %H:%M").to_string(),
                    p.selected.contains(&e.path),
                    e.directory,
                )
            };
            let style = Style::default().fg(if selected {
                Color::Rgb(243, 196, 113)
            } else if directory {
                ACCENT
            } else {
                FG
            });
            let style = if selected { style.bold() } else { style };
            let style = if index == p.cursor {
                style
                    .bg(if i == app.active {
                        SELECTED
                    } else {
                        Color::Rgb(31, 39, 50)
                    })
                    .bold()
            } else {
                style
            };
            table_rows.push(Row::new([name, bytes, date]).style(style));
        }
        let widths = if inner.width >= 48 {
            vec![
                Constraint::Min(10),
                Constraint::Length(7),
                Constraint::Length(11),
            ]
        } else {
            vec![
                Constraint::Min(10),
                Constraint::Length(6),
                Constraint::Length(0),
            ]
        };
        frame.render_widget(
            Table::new(table_rows, widths)
                .header(header)
                .column_spacing(1),
            inner,
        );
        if let Some(error) = &p.error {
            frame.render_widget(
                Paragraph::new(error.as_str())
                    .style(Style::default().fg(Color::LightRed))
                    .wrap(Wrap { trim: false }),
                Rect::new(
                    inner.x,
                    inner.y + 1,
                    inner.width,
                    inner.height.saturating_sub(1),
                ),
            );
        }
    }
    let (label, message, color) = match &app.status {
        crate::app::Status::Info(text) => ("Info", text, FG),
        crate::app::Status::Cancelled(text) => ("Cancelled", text, Color::Yellow),
        crate::app::Status::Failure(text) => ("Failed", text, Color::LightRed),
    };
    frame.render_widget(
        Paragraph::new(format!("{label}: {message}")).style(Style::default().fg(color).bg(BG)),
        rows[2],
    );
    let buttons = Layout::horizontal([Constraint::Ratio(1, 10); 10]).split(rows[3]);
    for (i, label) in [
        "Help", "Rename", "View", "Edit", "Copy", "Move", "Mkdir", "Delete", "Menu", "Quit",
    ]
    .iter()
    .enumerate()
    {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!("{}", i + 1), Style::default().fg(ACCENT)),
                Span::styled(format!(" {label}"), Style::default().fg(FG)),
            ]))
            .style(Style::default().bg(Color::Rgb(30, 38, 49))),
            buttons[i],
        );
    }
    if let Some(search) = &app.search {
        let results = search.results.lock().unwrap();
        let view = search_viewport(area, search.cursor);
        app.dialog_area = view.popup;
        frame.render_widget(Clear, view.popup);
        frame.render_widget(
            Block::bordered()
                .title("Find files")
                .style(Style::default().fg(FG).bg(BG)),
            view.popup,
        );
        frame.render_widget(
            Paragraph::new(clip_line(
                &format!(
                    "{} matches · {} · {} unreadable · cap 100,000",
                    results.len(),
                    if search.done.load(Ordering::Relaxed) {
                        "finished"
                    } else {
                        "searching"
                    },
                    *search.errors.lock().unwrap()
                ),
                usize::from(view.results.width),
            )),
            Rect::new(view.results.x, view.popup.y + 1, view.results.width, 1),
        );
        for (row, (i, path)) in results
            .iter()
            .enumerate()
            .skip(view.start)
            .take(usize::from(view.results.height))
            .enumerate()
        {
            frame.render_widget(
                Paragraph::new(clip_line(
                    &format!(
                        "{} {}",
                        if i == search.cursor { "›" } else { " " },
                        path.display()
                    ),
                    usize::from(view.results.width),
                ))
                .style(Style::default().fg(if i == search.cursor {
                    ACCENT
                } else {
                    FG
                })),
                Rect::new(
                    view.results.x,
                    view.results.y + row as u16,
                    view.results.width,
                    1,
                ),
            );
        }
        frame.render_widget(
            Paragraph::new("Enter: go   Ctrl+C: stop   Esc: close"),
            Rect::new(
                view.results.x,
                view.popup.bottom().saturating_sub(2),
                view.results.width,
                1,
            ),
        );
    }
    draw_menu(frame, app);
    if let Some(dialog) = &app.dialog {
        let (title, text, height) = match dialog {
            Dialog::Input {
                title,
                value,
                cursor,
                purpose,
            } => (
                title.clone(),
                Text::from(format!(
                    "{}\n {}▏{}\n\n Enter: confirm   Esc: cancel   Ctrl+U: clear",
                    match purpose {
                        crate::app::Purpose::Copy(sources) | crate::app::Purpose::Move(sources) =>
                            clip_line(
                                &sources
                                    .iter()
                                    .map(|p| p.display())
                                    .collect::<Vec<_>>()
                                    .join(", "),
                                80
                            ),
                        _ => String::new(),
                    },
                    &value[..*cursor],
                    &value[*cursor..]
                )),
                7,
            ),
            Dialog::Delete { sources, permanent } => (
                "Delete selected items".into(),
                Text::from(vec![
                    Line::from(format!(" {} item(s). Choose how to delete:", sources.len())),
                    Line::from(""),
                    Line::styled(
                        if *permanent {
                            "    Trash              [ Permanently delete ]"
                        } else {
                            "  [ Trash ]              Permanently delete"
                        },
                        Style::default().fg(if *permanent { Color::LightRed } else { ACCENT }),
                    ),
                    Line::from(clip_line(
                        &sources
                            .iter()
                            .map(|p| p.display())
                            .collect::<Vec<_>>()
                            .join(", "),
                        80,
                    )),
                    Line::from(" Tab/←/→ or click: choose   Enter: confirm   Esc: cancel"),
                ]),
                9,
            ),
            Dialog::Message { title, text } => (
                title.clone(),
                Text::from(format!("{text}\n\nEnter / Esc: close")),
                (text.lines().count() + 5).min(25) as u16,
            ),
            Dialog::Jobs => {
                let mut lines = vec![];
                let row_width = frame
                    .area()
                    .width
                    .saturating_sub(4)
                    .min(86)
                    .saturating_sub(2) as usize;
                for (i, job) in app
                    .jobs
                    .iter()
                    .enumerate()
                    .skip(app.job_cursor / 4 * 4)
                    .take(4)
                {
                    let p = job.progress.lock().unwrap();
                    lines.push(Line::styled(
                        clip_line(
                            &format!(
                                "{} {} · {} · {} files · {}",
                                if i == app.job_cursor { ">" } else { " " },
                                job.title,
                                if p.done {
                                    if p.error.is_some() {
                                        "failed"
                                    } else {
                                        "finished"
                                    }
                                } else if p.conflict.is_some() {
                                    "waiting"
                                } else {
                                    "running"
                                },
                                p.files,
                                size(p.bytes)
                            ),
                            row_width,
                        ),
                        Style::default().fg(if i == app.job_cursor { ACCENT } else { FG }),
                    ));
                    lines.push(Line::from(clip_line(&p.current, row_width)));
                }
                while lines.len() < 8 {
                    lines.push(Line::from(""));
                }
                if let Some(job) = app.jobs.get(app.job_cursor) {
                    let p = job.progress.lock().unwrap();
                    if !p.done {
                        let rate = p.bytes as f64 / job.started.elapsed().as_secs_f64().max(0.1);
                        if let Some(total) = p.current_total {
                            let ratio = if total == 0 {
                                1.0
                            } else {
                                (p.current_bytes as f64 / total as f64).min(1.0)
                            };
                            let bars = (ratio * 20.0) as usize;
                            let eta = if rate > 0.0 {
                                format!(
                                    "{:.0}s",
                                    total.saturating_sub(p.current_bytes) as f64 / rate
                                )
                            } else {
                                "—".into()
                            };
                            lines.push(Line::from(format!(
                                "File [{}{}] {:.0}% · {}/s · ETA {}",
                                "━".repeat(bars),
                                "─".repeat(20 - bars),
                                ratio * 100.0,
                                size(rate as u64),
                                eta
                            )));
                        }
                    }
                    if let Some(error) = &p.error {
                        lines.push(Line::styled(
                            error.clone(),
                            Style::default().fg(Color::LightRed),
                        ));
                    }
                } else {
                    lines.push(Line::from("No jobs yet."));
                }
                lines.push(Line::from(
                    "↑/↓: select · c: cancel selected · r: retry failed",
                ));
                lines.push(Line::from(
                    "Retry restarts remaining sources; conflicts ask again.",
                ));
                ("Background jobs".into(), Text::from(lines), 23)
            }
            Dialog::Quit => (
                "Jobs still running".into(),
                Text::from(
                    "Enter: cancel jobs and stay until cancellation completes\nEsc: keep working",
                ),
                6,
            ),
        };
        popup(frame, app, &title, text, height);
        let footer = match &app.dialog {
            Some(Dialog::Input { .. } | Dialog::Delete { .. }) => " [ Confirm ]     [ Cancel ]",
            Some(Dialog::Jobs) => " [ Cancel ] [ Retry ] [ Close ]",
            Some(Dialog::Quit) => " [ Cancel jobs ]  [ Keep working ]",
            _ => " [ Close ]",
        };
        let a = app.dialog_area;
        frame.render_widget(
            Paragraph::new(footer).style(Style::default().fg(ACCENT).bg(BG)),
            Rect::new(
                a.x + 1,
                a.bottom().saturating_sub(2),
                a.width.saturating_sub(2),
                1,
            ),
        );
    }
    if let Some(task) = &app.archive {
        let text = format!(
            "Reading archive index…\n{} entries\nEsc: cancel",
            *task.bytes.lock().unwrap()
        );
        popup(frame, app, "Open archive", text.into(), 7);
    }
    draw_priority_overlays(frame, app);
}

fn draw_priority_overlays(frame: &mut Frame, app: &mut App) {
    let conflict = app.jobs.iter().find_map(|j| {
        j.progress
            .lock()
            .unwrap()
            .conflict
            .as_ref()
            .map(|c| c.path.display().to_string())
    });
    if let Some(path) = conflict {
        popup(frame, app, "Destination exists", format!("{path}\n\no: overwrite   a: overwrite all\ns / Enter: skip   n: skip all   Esc: cancel job").into(), 9);
    }
    if app.viewing.is_some() {
        popup(
            frame,
            app,
            "View file",
            "Preparing stream…\nEsc: cancel".into(),
            6,
        );
    }
    if app.connecting.is_some() {
        popup(
            frame,
            app,
            "Connecting / opening directory",
            "Working…\nEsc: cancel".into(),
            6,
        );
    }
    if let Some(password) = &app.password {
        let remote = crate::vfs::remote::is_url(&password.request.resource);
        let text = format!(
            "{}\n{}\n{}\nEnter: unlock   Esc: cancel",
            password.request.resource,
            if password.request.confirmation {
                "Type trust to accept for this connection:"
            } else if password.request.retry {
                if remote {
                    "Authentication rejected. Try again:"
                } else {
                    "Password rejected or encrypted data damaged. Try again:"
                }
            } else {
                "Password:"
            },
            if password.request.confirmation {
                password.value.to_string()
            } else {
                "•".repeat(password.value.chars().count())
            }
        );
        popup(
            frame,
            app,
            if remote {
                "Remote authentication"
            } else {
                "Unlock archive"
            },
            text.into(),
            if password.request.confirmation {
                12
            } else {
                10
            },
        );
    }
}

fn draw_menu(frame: &mut Frame, app: &mut App) {
    let Some(state) = &mut app.menu else {
        return;
    };
    let menu = &MENUS[state.category];
    let area = frame.area();
    let width = 34.min(area.width);
    let height = (menu.items.len() as u16 + 2).min(area.height.saturating_sub(2));
    let rect = Rect::new(
        app.menu_tabs[state.category].x.min(area.right() - width),
        area.y + 1,
        width,
        height,
    );
    app.menu_area = rect;
    let visible = height.saturating_sub(2).max(1) as usize;
    if state.cursor < state.offset {
        state.offset = state.cursor;
    }
    if state.cursor >= state.offset + visible {
        state.offset = state.cursor + 1 - visible;
    }
    frame.render_widget(Clear, rect);
    let surface = Color::Rgb(30, 38, 49);
    frame.render_widget(
        Block::bordered()
            .border_style(Style::default().fg(Color::Rgb(70, 87, 104)))
            .style(Style::default().bg(surface)),
        rect,
    );
    for (row, (index, item)) in menu
        .items
        .iter()
        .enumerate()
        .skip(state.offset)
        .take(visible)
        .enumerate()
    {
        let checked = match item.action {
            Action::Sort(sort) => app.panels[app.active].sort == sort,
            Action::Hidden => app.panels[app.active].hidden,
            _ => false,
        };
        let style = if index == state.cursor {
            Style::default().fg(FG).bg(SELECTED).bold()
        } else {
            Style::default().fg(FG).bg(surface)
        };
        let label = format!("{} {}", if checked { "✓" } else { " " }, item.label);
        let row_area = Rect::new(rect.x + 1, rect.y + 1 + row as u16, rect.width - 2, 1);
        frame.render_widget(Paragraph::new(label).style(style), row_area);
        let shortcut_width = item.shortcut.len() as u16;
        if shortcut_width > 0 {
            frame.render_widget(
                Paragraph::new(item.shortcut).style(style.fg(if index == state.cursor {
                    ACCENT
                } else {
                    DIM
                })),
                Rect::new(
                    rect.right() - shortcut_width - 2,
                    row_area.y,
                    shortcut_width,
                    1,
                ),
            );
        }
    }
}

#[cfg(test)]
mod review_tests {
    use super::*;
    #[test]
    fn search_geometry_keeps_selection_visible_at_all_supported_sizes() {
        for (width, height) in [(36, 10), (80, 24), (120, 40)] {
            for cursor in [0, 8, 14, 999] {
                let view = search_viewport(Rect::new(0, 0, width, height), cursor);
                assert!(
                    cursor >= view.start && cursor < view.start + usize::from(view.results.height)
                );
                assert!(view.results.bottom() < view.popup.bottom());
                assert_eq!(clip_line("e\u{301}👩‍💻long", 4), "e\u{301}👩‍💻…");
                assert!(!clip_line("path\nnext", 80).contains('\n'));
            }
        }
    }
}
