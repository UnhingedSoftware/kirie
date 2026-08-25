//! `kirie workshop browse` — the Workshop as a terminal UI.
//!
//! The CLI answers a question you already know how to ask. Browsing is the
//! other half: type, look, refine, subscribe. Same engine underneath — every
//! query goes through [`super::search`], so the two surfaces can never
//! disagree about what the Workshop holds or what kirie can render.
//!
//! Two things shape the implementation:
//!
//! * **A query takes seconds** (a helper process, then Steam, then the
//!   network), so it runs on a worker thread and the UI keeps drawing. A
//!   terminal that stops repainting while it waits reads as a hang.
//! * **Steam is not asked per keystroke.** A search happens on Enter, never on
//!   input, because every query is a real request to Valve.

use std::sync::mpsc;

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

use super::{Item, Query};

/// Sorts the `s` key cycles through, in the order a browser wants them.
const SORTS: [&str; 4] = ["popular", "trend", "recent", "rated"];

/// How often the loop wakes to repaint while a query is in flight.
const TICK: std::time::Duration = std::time::Duration::from_millis(120);

/// What the worker thread reports back.
enum Message {
    /// A page of results for the query that asked for them.
    Results(Vec<Item>),
    /// The query failed; the message is the user's to read.
    Failed(String),
    /// A subscription finished (or did not).
    Subscribed(String),
}

/// Everything the UI draws.
struct App {
    /// The search box's contents.
    input: String,
    /// Which sort of [`SORTS`] is selected.
    sort: usize,
    /// 1-based page.
    page: u32,
    /// The last page of results.
    items: Vec<Item>,
    /// Which result is highlighted.
    selected: ListState,
    /// A query is in flight.
    busy: bool,
    /// The line under the results — the last thing that happened.
    status: String,
    /// Set when the user asks to leave.
    quit: bool,
}

impl App {
    /// A fresh browser, before its first query.
    fn new() -> Self {
        let mut selected = ListState::default();
        selected.select(Some(0));
        Self {
            input: String::new(),
            sort: 0,
            page: 1,
            items: Vec::new(),
            selected,
            busy: false,
            status: "enter a search and press Enter, or press Enter for what is popular".to_owned(),
            quit: false,
        }
    }

    /// The query the current UI state describes.
    fn query(&self) -> Query {
        Query {
            text: (!self.input.trim().is_empty()).then(|| self.input.trim().to_owned()),
            sort: SORTS[self.sort].to_owned(),
            page: self.page,
            ..Query::default()
        }
    }

    /// The highlighted result, if there is one.
    fn current(&self) -> Option<&Item> {
        self.selected.selected().and_then(|i| self.items.get(i))
    }

    /// Move the highlight, saturating at both ends (wrapping a list of search
    /// results reads as a jump, not a move).
    fn step(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }
        let last = self.items.len() - 1;
        let current = self.selected.selected().unwrap_or(0);
        let next = current.saturating_add_signed(delta).min(last);
        self.selected.select(Some(next));
    }
}

/// Run the browser until the user leaves it.
///
/// # Errors
/// When the terminal cannot be put into raw mode, or drawing fails. A failed
/// *query* is not an error here — it is shown in the status line and the user
/// carries on.
pub fn run() -> Result<()> {
    // Fail before touching the terminal: dropping a user into a raw-mode
    // screen that can only say "no Steam" is worse than saying it in the
    // shell they are already in.
    if super::helper_path().is_none() {
        anyhow::bail!(
            "kirie-steam-helper was not found (it ships beside kirie; set \
             KIRIE_STEAM_HELPER to point at it)"
        );
    }

    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal);
    ratatui::restore();
    result
}

/// Draw, wait, react, repeat.
fn event_loop(terminal: &mut DefaultTerminal) -> Result<()> {
    let (tx, rx) = mpsc::channel::<Message>();
    let mut app = App::new();
    // Open on something to look at rather than an empty screen.
    app.busy = true;
    start_query(&app, &tx);

    while !app.quit {
        terminal.draw(|frame| draw(frame, &mut app))?;

        while let Ok(message) = rx.try_recv() {
            match message {
                Message::Results(items) => {
                    app.busy = false;
                    app.status = format!("{} result(s), page {}", items.len(), app.page);
                    app.items = items;
                    app.selected.select(Some(0));
                }
                Message::Failed(why) => {
                    app.busy = false;
                    app.status = why;
                }
                Message::Subscribed(what) => app.status = what,
            }
        }

        if event::poll(TICK)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            handle_key(&mut app, key.code, key.modifiers, &tx);
        }
    }
    Ok(())
}

/// Apply one keypress.
fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers, tx: &mpsc::Sender<Message>) {
    let control = modifiers.contains(KeyModifiers::CONTROL);
    match code {
        KeyCode::Esc => app.quit = true,
        KeyCode::Char('c' | 'd') if control => app.quit = true,
        KeyCode::Enter => {
            app.page = 1;
            app.busy = true;
            app.status = "searching…".to_owned();
            start_query(app, tx);
        }
        // Typing edits the search box: the results are only ever a page behind
        // what was actually asked for, never a page behind what was typed.
        KeyCode::Char(c) => app.input.push(c),
        KeyCode::Backspace => {
            app.input.pop();
        }
        KeyCode::Down => app.step(1),
        KeyCode::Up => app.step(-1),
        KeyCode::PageDown => app.step(10),
        KeyCode::PageUp => app.step(-10),
        KeyCode::Tab => {
            app.sort = (app.sort + 1) % SORTS.len();
            app.page = 1;
            app.busy = true;
            app.status = format!("sorting by {}…", SORTS[app.sort]);
            start_query(app, tx);
        }
        KeyCode::Right => {
            app.page += 1;
            app.busy = true;
            app.status = format!("page {}…", app.page);
            start_query(app, tx);
        }
        KeyCode::Left if app.page > 1 => {
            app.page -= 1;
            app.busy = true;
            app.status = format!("page {}…", app.page);
            start_query(app, tx);
        }
        KeyCode::F(2) => {
            if let Some(item) = app.current() {
                let id = item.id.clone();
                let title = item.title.clone();
                let sender = tx.clone();
                // Subscribing is the one keypress that changes the user's Steam
                // account, so it is a distinct key rather than a letter that
                // could be typed into the search box by accident.
                let _ = std::thread::Builder::new()
                    .name("kirie-workshop-subscribe".to_owned())
                    .spawn(move || {
                        let message = match super::subscribe(&id) {
                            Ok(item) if item.installed => {
                                format!("{title} is already installed")
                            }
                            Ok(_) => format!("subscribed to {title}; Steam is downloading it"),
                            Err(err) => format!("could not subscribe: {err}"),
                        };
                        let _ = sender.send(Message::Subscribed(message));
                    });
            }
        }
        _ => {}
    }
}

/// Run the app's current query on a worker thread.
fn start_query(app: &App, tx: &mpsc::Sender<Message>) {
    let query = app.query();
    let sender = tx.clone();
    let spawned = std::thread::Builder::new()
        .name("kirie-workshop-query".to_owned())
        .spawn(move || {
            let message = match super::search(&query) {
                Ok(items) => Message::Results(items),
                Err(err) => Message::Failed(err.to_string()),
            };
            let _ = sender.send(message);
        });
    if let Err(err) = spawned {
        let _ = tx.send(Message::Failed(format!("could not start the query: {err}")));
    }
}

/// Paint one frame.
fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(7),
            Constraint::Length(1),
        ])
        .split(frame.area());

    draw_search(frame, areas[0], app);
    draw_results(frame, areas[1], app);
    draw_detail(frame, areas[2], app);
    draw_status(frame, areas[3], app);
}

/// The search box and the current sort.
fn draw_search(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let title = format!(" search — sort: {} — page {} ", SORTS[app.sort], app.page);
    let box_ = Paragraph::new(app.input.as_str()).block(Block::default().borders(Borders::ALL).title(title));
    frame.render_widget(box_, area);
}

/// The result list.
fn draw_results(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let rows: Vec<ListItem<'_>> = app
        .items
        .iter()
        .map(|item| {
            // Same three marks `kirie list` uses, so a user reads one list
            // having learnt the other.
            let (mark, colour) = match (item.renderable, item.kind) {
                (true, _) => (" ", Color::Reset),
                (false, "asset") => ("-", Color::DarkGray),
                (false, _) => ("!", Color::Yellow),
            };
            let here = if item.installed {
                " [installed]"
            } else if item.subscribed {
                " [subscribed]"
            } else {
                ""
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{mark} "), Style::default().fg(colour)),
                Span::styled(format!("{:<9} ", item.kind), Style::default().fg(Color::Cyan)),
                Span::raw(item.title.clone()),
                Span::styled(here, Style::default().fg(Color::Green)),
            ]))
        })
        .collect();

    let list = List::new(rows)
        .block(Block::default().borders(Borders::ALL).title(" results "))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("");
    frame.render_stateful_widget(list, area, &mut app.selected);
}

/// The selected item, in full.
fn draw_detail(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let body = app.current().map_or_else(
        || vec![Line::from("nothing selected")],
        |item| {
            let mut lines = vec![
                Line::from(vec![
                    Span::styled(item.title.clone(), Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(format!("  ({})", item.id)),
                ]),
                Line::from(format!(
                    "{}  ·  {}  ·  {}% of {} votes",
                    item.kind,
                    super::human_size(item.size),
                    (item.score * 100.0).round() as i32,
                    item.votes.0 + item.votes.1
                )),
                Line::from(item.tags.join(", ")),
            ];
            if let Some(reason) = &item.reason {
                lines.push(Line::from(Span::styled(
                    format!("cannot render: {reason}"),
                    Style::default().fg(Color::Yellow),
                )));
            }
            if let Some(dir) = &item.dir {
                lines.push(Line::from(format!("installed at {}", dir.display())));
            }
            lines
        },
    );

    let detail = Paragraph::new(body)
        .block(Block::default().borders(Borders::ALL).title(" item "))
        .wrap(Wrap { trim: true });
    frame.render_widget(detail, area);
}

/// The status line, which doubles as the key reference.
fn draw_status(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let keys = "enter search · tab sort · ←/→ page · ↑/↓ select · F2 subscribe · esc quit";
    let text = if app.busy {
        format!("… {}", app.status)
    } else {
        format!("{}  |  {keys}", app.status)
    };
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_query_follows_the_ui_state() {
        let mut app = App::new();
        app.input = "  miku  ".to_owned();
        app.sort = 2;
        app.page = 3;
        let query = app.query();
        assert_eq!(query.text.as_deref(), Some("miku"));
        assert_eq!(query.sort, "recent");
        assert_eq!(query.page, 3);

        // An empty box is "no text", not a search for nothing.
        app.input = "   ".to_owned();
        assert!(app.query().text.is_none());
    }

    #[test]
    fn selection_saturates_at_both_ends() {
        let mut app = App::new();
        assert_eq!(app.selected.selected(), Some(0));
        // Moving in an empty list must not select a row that is not there.
        app.step(5);
        assert_eq!(app.selected.selected(), Some(0));
    }
}
