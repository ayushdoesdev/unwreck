//! Interactive timeline with a file list and unified diff pane.
//!
//! Restore/undo/redo are returned as [`Outcome`]; the CLI applies them with the same
//! conflict checks as `unwreck restore`.
#![warn(missing_docs)]

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Wrap};

/// One timeline row in the picker.
pub struct Row {
    /// Snapshot id.
    pub id: u64,
    /// Relative time label (`3s ago`).
    pub when: String,
    /// Trigger label.
    pub label: String,
    /// Paths different from the previous snapshot.
    pub changed: usize,
}

/// What the user asked the CLI to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Restore this snapshot (same path as `unwreck restore ID`).
    Restore(u64),
    /// `unwreck undo`.
    Undo,
    /// `unwreck redo`.
    Redo,
    /// Quit with no restore.
    Quit,
}

/// Supplies the file list and unified diff for the selected snapshot.
pub trait DiffSource {
    /// Status char + path for files that differ from the previous snapshot (or disk).
    fn files(&self, snapshot_id: u64) -> Vec<(char, String)>;
    /// Unified diff for one path (`A`/`M`/`D`).
    fn unified(&self, snapshot_id: u64, path: &str) -> String;
}

/// Run the TUI. Returns [`Outcome::Quit`] on `q` / Esc.
pub fn run(rows: &[Row], diffs: &dyn DiffSource) -> std::io::Result<Outcome> {
    if rows.is_empty() {
        return Ok(Outcome::Quit);
    }
    let mut terminal = ratatui::init();
    let mut snap = ListState::default().with_selected(rows.len().checked_sub(1));
    let mut file = ListState::default().with_selected(Some(0));
    let mut pane = 0u8; // 0 timeline, 1 files
    let mut help = false;
    let result = loop {
        let sid = snap.selected().and_then(|i| rows.get(i)).map(|r| r.id);
        let files = sid.map(|id| diffs.files(id)).unwrap_or_default();
        if file.selected().is_some_and(|i| i >= files.len()) {
            file.select(files.len().checked_sub(1));
        }
        if file.selected().is_none() && !files.is_empty() {
            file.select(Some(0));
        }
        let patch = match (sid, file.selected().and_then(|i| files.get(i))) {
            (Some(id), Some((_, p))) => diffs.unified(id, p),
            _ => String::new(),
        };
        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(5), Constraint::Length(1)])
                .split(f.area());
            let body = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
                .split(chunks[0]);
            let right = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
                .split(body[1]);
            let items: Vec<ListItem> = rows
                .iter()
                .map(|r| {
                    ListItem::new(format!(
                        "#{:<5} {:>9}  {:>4} files  {}",
                        r.id, r.when, r.changed, r.label
                    ))
                })
                .collect();
            let list = List::new(items)
                .block(Block::bordered().title(if pane == 0 {
                    " snapshots (focused) "
                } else {
                    " snapshots "
                }))
                .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
            f.render_stateful_widget(list, body[0], &mut snap);
            let fitems: Vec<ListItem> = files
                .iter()
                .map(|(st, p)| ListItem::new(format!("{st} {p}")))
                .collect();
            let flist = List::new(fitems)
                .block(Block::bordered().title(if pane == 1 {
                    " files (focused) "
                } else {
                    " files "
                }))
                .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
            f.render_stateful_widget(flist, right[0], &mut file);
            let lines: Vec<Line> = patch.lines().map(color_diff_line).collect();
            let para = Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(Block::bordered().title(" diff "));
            f.render_widget(para, right[1]);
            let footer = if help {
                " enter restore · u undo · r redo · q quit · tab switch pane · j/k move · ? hide "
            } else {
                " enter restore · u undo · r redo · q quit · tab files · ? help "
            };
            f.render_widget(Paragraph::new(footer), chunks[1]);
        })?;
        if let Event::Key(k) = event::read()? {
            if k.kind != KeyEventKind::Press {
                continue;
            }
            match k.code {
                KeyCode::Char('q') | KeyCode::Esc => break Outcome::Quit,
                KeyCode::Char('?') => help = !help,
                KeyCode::Tab => pane = 1 - pane,
                KeyCode::Char('u') => break Outcome::Undo,
                KeyCode::Char('r') => break Outcome::Redo,
                KeyCode::Enter => {
                    break sid.map(Outcome::Restore).unwrap_or(Outcome::Quit);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if pane == 0 {
                        snap.select_previous();
                        file.select(Some(0));
                    } else {
                        file.select_previous();
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if pane == 0 {
                        snap.select_next();
                        file.select(Some(0));
                    } else {
                        file.select_next();
                    }
                }
                _ => {}
            }
        }
    };
    ratatui::restore();
    Ok(result)
}

fn color_diff_line(l: &str) -> Line<'static> {
    let style = if l.starts_with('+') && !l.starts_with("+++") {
        Style::new().fg(ratatui::style::Color::Green)
    } else if l.starts_with('-') && !l.starts_with("---") {
        Style::new().fg(ratatui::style::Color::Red)
    } else {
        Style::default()
    };
    Line::from(Span::styled(l.to_owned(), style))
}
