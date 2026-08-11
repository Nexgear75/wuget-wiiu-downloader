//! Full-screen title picker.
//!
//! Typing filters the 3600-odd catalog entries with a fuzzy match, Tab cycles
//! the region filter, Space queues several titles at once.

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use nucleo_matcher::{
    Config, Matcher,
    pattern::{CaseMatching, Normalization, Pattern},
};
use ratatui::{
    DefaultTerminal,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};

use crate::catalog::{self, Kind, Title};

/// Searchable text for one catalog entry, kept alongside its index.
#[derive(Clone)]
struct Entry {
    index: usize,
    haystack: String,
}

impl AsRef<str> for Entry {
    fn as_ref(&self) -> &str {
        &self.haystack
    }
}

struct App {
    /// Every browsable title, in catalog order.
    titles: Vec<&'static Title>,
    entries: Vec<Entry>,
    matcher: Matcher,

    query: String,
    /// `None` means "every region".
    region: Option<String>,
    regions: Vec<String>,
    kind_filter: Option<Kind>,

    /// Indices into `titles`, after filtering.
    visible: Vec<usize>,
    list_state: ListState,
    selected: Vec<usize>,
}

impl App {
    fn new() -> Self {
        let titles = catalog::browsable();
        let entries = titles
            .iter()
            .enumerate()
            .map(|(index, t)| Entry {
                index,
                // Matching on the id too lets people paste a Title ID.
                haystack: format!("{} {} {}", t.name, t.region, t.title_id),
            })
            .collect();

        let regions = catalog::regions()
            .into_iter()
            .map(str::to_string)
            .collect();

        let mut app = App {
            titles,
            entries,
            matcher: Matcher::new(Config::DEFAULT),
            query: String::new(),
            region: None,
            regions,
            kind_filter: Some(Kind::Game),
            visible: Vec::new(),
            list_state: ListState::default(),
            selected: Vec::new(),
        };
        app.refilter();
        app
    }

    fn refilter(&mut self) {
        let region = self.region.clone();
        let kind = self.kind_filter;

        let candidates: Vec<Entry> = self
            .entries
            .iter()
            .filter(|e| {
                let t = self.titles[e.index];
                region.as_deref().is_none_or(|r| t.region == r)
                    && kind.is_none_or(|k| t.kind == k)
            })
            .cloned()
            .collect();

        self.visible = if self.query.is_empty() {
            candidates.into_iter().map(|e| e.index).collect()
        } else {
            let pattern = Pattern::parse(&self.query, CaseMatching::Ignore, Normalization::Smart);
            pattern
                .match_list(candidates, &mut self.matcher)
                .into_iter()
                .map(|(e, _score)| e.index)
                .collect()
        };

        if self.visible.is_empty() {
            self.list_state.select(None);
        } else {
            let at = self
                .list_state
                .selected()
                .unwrap_or(0)
                .min(self.visible.len() - 1);
            self.list_state.select(Some(at));
        }
    }

    fn move_by(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let last = self.visible.len() - 1;
        let current = self.list_state.selected().unwrap_or(0) as isize;
        self.list_state
            .select(Some((current + delta).clamp(0, last as isize) as usize));
    }

    fn current(&self) -> Option<&'static Title> {
        let at = self.list_state.selected()?;
        Some(self.titles[*self.visible.get(at)?])
    }

    fn toggle_current(&mut self) {
        let Some(at) = self.list_state.selected() else {
            return;
        };
        let index = self.visible[at];
        if !self.titles[index].is_obtainable() {
            return;
        }
        match self.selected.iter().position(|&i| i == index) {
            Some(pos) => {
                self.selected.remove(pos);
            }
            None => self.selected.push(index),
        }
    }

    fn cycle_region(&mut self) {
        let next = match &self.region {
            None => self.regions.first().cloned(),
            Some(current) => {
                let pos = self.regions.iter().position(|r| r == current);
                match pos {
                    Some(p) if p + 1 < self.regions.len() => Some(self.regions[p + 1].clone()),
                    _ => None,
                }
            }
        };
        self.region = next;
        self.refilter();
    }

    fn cycle_kind(&mut self) {
        const ORDER: [Option<Kind>; 5] = [
            Some(Kind::Game),
            Some(Kind::Update),
            Some(Kind::Dlc),
            Some(Kind::Demo),
            None,
        ];
        let pos = ORDER.iter().position(|k| *k == self.kind_filter).unwrap_or(0);
        self.kind_filter = ORDER[(pos + 1) % ORDER.len()];
        self.refilter();
    }

    /// What the user finally chose: the checked titles, or the highlighted one.
    fn take_selection(&self) -> Vec<&'static Title> {
        if !self.selected.is_empty() {
            return self.selected.iter().map(|&i| self.titles[i]).collect();
        }
        self.current()
            .filter(|t| t.is_obtainable())
            .into_iter()
            .collect()
    }
}

/// Run the picker. Returns the chosen titles, empty if the user quit.
pub fn run() -> Result<Vec<&'static Title>> {
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut DefaultTerminal) -> Result<Vec<&'static Title>> {
    let mut app = App::new();

    loop {
        terminal.draw(|frame| draw(frame, &mut app))?;

        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => return Ok(Vec::new()),
            (KeyCode::Enter, _) => return Ok(app.take_selection()),
            (KeyCode::Up, _) => app.move_by(-1),
            (KeyCode::Down, _) => app.move_by(1),
            (KeyCode::PageUp, _) => app.move_by(-10),
            (KeyCode::PageDown, _) => app.move_by(10),
            (KeyCode::Tab, _) => app.cycle_region(),
            (KeyCode::BackTab, _) => app.cycle_kind(),
            (KeyCode::Backspace, _) => {
                app.query.pop();
                app.refilter();
            }
            // Space both types and toggles: only toggle when the query is idle.
            (KeyCode::Char(' '), _) if app.query.is_empty() => app.toggle_current(),
            (KeyCode::Char(c), m) if !m.contains(KeyModifiers::CONTROL) => {
                app.query.push(c);
                app.refilter();
            }
            _ => {}
        }
    }
}

fn draw(frame: &mut ratatui::Frame, app: &mut App) {
    let [search_area, list_area, help_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(3),
        Constraint::Length(3),
    ])
    .areas(frame.area());

    let region = app.region.as_deref().unwrap_or("toutes");
    let kind = app.kind_filter.map_or("tous", Kind::label);
    let header = format!(
        " Recherche  ─  région : {region}  ─  type : {kind}  ─  {} résultats ",
        app.visible.len()
    );

    frame.render_widget(
        Paragraph::new(app.query.as_str())
            .block(Block::default().borders(Borders::ALL).title(header)),
        search_area,
    );

    let items: Vec<ListItem> = app
        .visible
        .iter()
        .map(|&index| {
            let title = app.titles[index];
            let checked = app.selected.contains(&index);
            let obtainable = title.is_obtainable();

            let mark = if checked { "◉ " } else { "  " };
            let legit = if title.has_ticket { "légit" } else { "—" };
            let line = Line::from(vec![
                Span::raw(mark),
                Span::raw(format!("{:<58}", truncate(&title.name, 58))),
                Span::raw(format!("{:<5}", title.region)),
                Span::raw(format!("{:<8}", title.kind.label())),
                Span::raw(format!("{legit:<6}")),
                Span::raw(title.title_id.clone()).dim(),
            ]);

            let style = if !obtainable {
                Style::default().fg(Color::DarkGray)
            } else if checked {
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(line).style(style)
        })
        .collect();

    let selected_note = if app.selected.is_empty() {
        String::new()
    } else {
        format!(" — {} sélectionné(s)", app.selected.len())
    };

    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" Titres{selected_note} ")),
            )
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
        list_area,
        &mut app.list_state,
    );

    frame.render_widget(
        Paragraph::new(
            "↑↓ naviguer   Espace cocher (recherche vide)   Tab région   ⇧Tab type   \
             Entrée télécharger   Échap quitter",
        )
        .dim()
        .block(Block::default().borders(Borders::ALL)),
        help_area,
    );
}

fn truncate(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    s.chars().take(width.saturating_sub(1)).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_on_games_and_filters_by_query() {
        let mut app = App::new();
        assert!(!app.visible.is_empty());
        assert!(
            app.visible
                .iter()
                .all(|&i| app.titles[i].kind == Kind::Game),
            "le filtre par défaut doit ne montrer que des jeux"
        );

        app.query = "wind waker".into();
        app.refilter();
        assert!(!app.visible.is_empty(), "aucun résultat pour 'wind waker'");
        assert!(
            app.titles[app.visible[0]].name.to_lowercase().contains("wind waker"),
            "premier résultat inattendu : {}",
            app.titles[app.visible[0]].name
        );
    }

    #[test]
    fn a_title_id_is_searchable() {
        let mut app = App::new();
        app.query = "0005000010143500".into();
        app.refilter();
        // Fuzzy matching also lets near-miss ids through; the exact one ranks first.
        assert_eq!(app.titles[app.visible[0]].title_id, "0005000010143500");
    }

    #[test]
    fn region_and_kind_filters_cycle_back_to_all() {
        let mut app = App::new();

        app.cycle_region();
        assert!(app.region.is_some());
        for _ in 0..app.regions.len() {
            app.cycle_region();
        }
        assert!(app.region.is_none(), "le cycle des régions doit revenir à « toutes »");

        for _ in 0..5 {
            app.cycle_kind();
        }
        assert_eq!(app.kind_filter, Some(Kind::Game));
    }

    #[test]
    fn selection_falls_back_to_the_highlighted_row() {
        let mut app = App::new();
        app.query = "wind waker".into();
        app.refilter();

        assert_eq!(app.take_selection().len(), 1, "sans coche, la ligne courante");

        app.toggle_current();
        app.move_by(1);
        app.toggle_current();
        assert_eq!(app.take_selection().len(), 2, "deux titres cochés");
    }

    #[test]
    fn unobtainable_titles_cannot_be_checked() {
        let mut app = App::new();
        app.kind_filter = None;
        app.query.clear();
        app.refilter();

        let Some(at) = app.visible.iter().position(|&i| !app.titles[i].is_obtainable()) else {
            return; // tout le catalogue est obtenable, rien à vérifier
        };
        app.list_state.select(Some(at));
        app.toggle_current();
        assert!(app.selected.is_empty());
    }
}
