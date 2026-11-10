//! The `ferrofire top` screen: a table of tags (value, age, Good/Stale, trend; a changed value is highlighted) over a
//! log pane. `App` holds the state and takes `Msg`s and keys, `render` draws it, `run` is the loop; the terminal itself
//! is only touched in `tui`, so everything else is tested on a `TestBackend`.

use crate::log::{Level, LEVELS};
use crate::probe::glob_match;
use crate::top::{Msg, Row, Table};
use ratatui::backend::Backend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Cell, Paragraph, Row as TRow, Table as TTable};
use ratatui::{Frame, Terminal};
use std::collections::VecDeque;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

const LOG_KEEP: usize = 200;
const LOG_ROWS: u16 = 8;
const TICK: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Sort {
    Name,
    /// Oldest first.
    Age,
    /// Stale first.
    Stale,
}

impl Sort {
    fn next(self) -> Sort {
        match self {
            Sort::Name => Sort::Age,
            Sort::Age => Sort::Stale,
            Sort::Stale => Sort::Name,
        }
    }
}

/// `text` is a substring unless it holds `*` or `?`, then a whole-name glob.
fn matches(text: &str, name: &str) -> bool {
    match text.contains(['*', '?']) {
        true => glob_match(text, name),
        false => name.contains(text),
    }
}

/// The rows matching `filter`, in `sort` order (rows come in by name).
pub fn visible(mut rows: Vec<Row>, filter: &str, sort: Sort) -> Vec<Row> {
    rows.retain(|r| matches(filter, &r.name));
    match sort {
        Sort::Name => {}
        Sort::Age => rows.sort_by(|a, b| b.age.total_cmp(&a.age)),
        Sort::Stale => rows.sort_by_key(|r| !r.stale),
    }
    rows
}

pub struct App {
    table: Table,
    label: String,
    filter: String,
    editing: bool,
    sort: Sort,
    /// Rows as of the moment of pausing; the table keeps updating underneath.
    frozen: Option<Vec<Row>>,
    min: Level,
    log: VecDeque<String>,
    closed: Option<String>,
}

impl App {
    pub fn new(table: Table, label: String, filter: String) -> App {
        App {
            table,
            label,
            filter,
            editing: false,
            sort: Sort::Name,
            frozen: None,
            min: Level::Info,
            log: VecDeque::new(),
            closed: None,
        }
    }

    pub fn msg(&mut self, m: Msg) {
        match m {
            Msg::Tag { name, value, at } => self.table.apply(&name, value, at),
            Msg::Frame => {}
            Msg::Log(l) => {
                if self.log.len() == LOG_KEEP {
                    self.log.pop_front();
                }
                self.log.push_back(l);
            }
            Msg::Closed(why) => self.closed = Some(why),
        }
    }

    /// True when the key quits.
    pub fn key(&mut self, k: KeyEvent, now: f64) -> bool {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            return true;
        }
        match self.editing {
            true => self.edit(k.code),
            false => return self.command(k.code, now),
        }
        false
    }

    /// Typing into the filter.
    fn edit(&mut self, code: KeyCode) {
        match code {
            KeyCode::Enter | KeyCode::Esc => self.editing = false,
            KeyCode::Backspace => drop(self.filter.pop()),
            KeyCode::Char(c) => self.filter.push(c),
            _ => {}
        }
    }

    /// A command key; true to quit.
    fn command(&mut self, code: KeyCode, now: f64) -> bool {
        match code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('/') => self.editing = true,
            KeyCode::Char('s') => self.sort = self.sort.next(),
            KeyCode::Char('p') => {
                self.frozen = match self.frozen {
                    Some(_) => None,
                    None => Some(self.table.rows(now)),
                }
            }
            KeyCode::Char('l') => self.min = next_level(self.min),
            _ => {}
        }
        false
    }

    fn rows(&self, now: f64) -> Vec<Row> {
        let rows = self.frozen.clone().unwrap_or_else(|| self.table.rows(now));
        visible(rows, &self.filter, self.sort)
    }
}

fn next_level(l: Level) -> Level {
    let i = LEVELS.iter().position(|(_, v)| *v == l).unwrap_or(0);
    LEVELS[(i + 1) % LEVELS.len()].1
}

/// Eight-step bar chart of the samples, scaled to their own min and max (flat is the lowest bar).
fn spark(hist: &[f64]) -> String {
    let (lo, hi) = hist
        .iter()
        .fold((f64::MAX, f64::MIN), |(l, h), v| (l.min(*v), h.max(*v)));
    let bars = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let step = |v: &f64| match hi > lo {
        true => ((v - lo) / (hi - lo) * 7.0).round() as usize,
        false => 0,
    };
    hist.iter().map(|v| bars[step(v)]).collect()
}

fn row_cells(r: &Row) -> TRow<'static> {
    let state = if r.stale { "STALE" } else { "good" };
    let colour = if r.stale { Color::Red } else { Color::Green };
    let value = match r.hot {
        true => Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        false => Style::new(),
    };
    TRow::new([
        Cell::from(r.name.clone()),
        Cell::from(format!("{}", r.value)).style(value),
        Cell::from(format!("{:.1}s", r.age)),
        Cell::from(state).style(Style::new().fg(colour)),
        Cell::from(spark(&r.hist)),
    ])
}

pub fn render(f: &mut Frame, app: &App, now: f64) {
    let [head, body, log, help] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(LOG_ROWS),
        Constraint::Length(1),
    ])
    .areas(f.area());
    let rows = app.rows(now);
    let stale = rows.iter().filter(|r| r.stale).count();
    let state = match (&app.closed, &app.frozen) {
        (Some(why), _) => format!("[{why}] "),
        (_, Some(_)) => "[paused] ".into(),
        _ => String::new(),
    };
    f.render_widget(
        Paragraph::new(format!(
            "{state}{}: {} tags, {stale} stale, sort {:?}, filter `{}`",
            app.label,
            rows.len(),
            app.sort,
            app.filter
        )),
        head,
    );
    let widths = [
        Constraint::Min(20),
        Constraint::Length(14),
        Constraint::Length(8),
        Constraint::Length(6),
        Constraint::Length(TREND_W),
    ];
    let header = TRow::new(["tag", "value", "age", "state", "trend"])
        .style(Style::new().add_modifier(Modifier::BOLD));
    f.render_widget(
        TTable::new(rows.iter().map(row_cells), widths).header(header),
        body,
    );
    let shown: Vec<&str> = app
        .log
        .iter()
        .filter(|l| Level::of_line(l) <= app.min)
        .map(String::as_str)
        .collect();
    let keep = shown.len().saturating_sub(usize::from(LOG_ROWS) - 1);
    let title = format!("log (>= {})", app.min.name());
    f.render_widget(
        Paragraph::new(shown[keep..].join("\n")).block(Block::bordered().title(title)),
        log,
    );
    let help_text = match app.editing {
        true => "filter: type, Enter or Esc to finish".to_string(),
        false => "q quit  / filter  s sort  p pause  l log level".to_string(),
    };
    f.render_widget(Paragraph::new(help_text), help);
}

const TREND_W: u16 = 16;

/// Draw, apply what arrived, handle a key, until quit. `key` waits up to the given time for a key press.
pub fn run<B: Backend>(
    term: &mut Terminal<B>,
    app: &mut App,
    rx: &Receiver<Msg>,
    mut key: impl FnMut(Duration) -> Option<KeyEvent>,
    mut now: impl FnMut() -> f64,
) -> Result<(), B::Error> {
    loop {
        rx.try_iter().for_each(|m| app.msg(m));
        let t = now();
        term.draw(|f| render(f, app, t))?;
        if key(TICK).is_some_and(|k| app.key(k, t)) {
            return Ok(());
        }
    }
}

/// Take over the terminal and run the screen; the clock is `t0` scaled by `speed` (a replay may run fast).
pub fn tui(
    table: Table,
    label: String,
    filter: String,
    rx: Receiver<Msg>,
    (t0, speed): (Instant, f64),
) -> std::io::Result<()> {
    let mut app = App::new(table, label, filter);
    let mut term = ratatui::init();
    let key = |wait| {
        let ready = event::poll(wait).unwrap_or(false);
        match ready.then(event::read) {
            Some(Ok(Event::Key(k))) if k.kind == KeyEventKind::Press => Some(k),
            _ => None,
        }
    };
    let r = run(&mut term, &mut app, &rx, key, || {
        t0.elapsed().as_secs_f64() * speed
    });
    ratatui::restore();
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use std::sync::mpsc::channel;

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }
    fn app() -> App {
        let mut a = App::new(Table::new(2.0), "unit".into(), String::new());
        for (n, v, at) in [
            ("lower.height", 1.5, 0.0),
            ("pump.on", 1.0, 0.0),
            ("upper.height", 7.0, 0.0),
        ] {
            a.msg(Msg::Tag {
                name: n.into(),
                value: v,
                at,
            });
        }
        a.msg(Msg::Tag {
            name: "pump.on".into(),
            value: 0.0,
            at: 9.5,
        }); // changed just now
        a.msg(Msg::Tag {
            name: "upper.height".into(),
            value: 7.0,
            at: 9.0,
        });
        a
    }
    fn draw(a: &App, now: f64) -> ratatui::buffer::Buffer {
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        term.draw(|f| render(f, a, now)).unwrap();
        term.backend().buffer().clone()
    }
    fn text(b: &ratatui::buffer::Buffer) -> Vec<String> {
        let w = usize::from(b.area.width);
        b.content()
            .chunks(w)
            .map(|r| r.iter().map(|c| c.symbol()).collect())
            .collect()
    }

    #[test]
    fn the_table_shows_value_age_state_and_trend() {
        let screen = text(&draw(&app(), 10.0));
        assert!(
            screen[0].starts_with("unit: 3 tags, 1 stale, sort Name, filter ``"),
            "{}",
            screen[0]
        );
        let row = |tag: &str| {
            screen
                .iter()
                .find(|l| l.starts_with(tag))
                .unwrap_or_else(|| panic!("{screen:#?}"))
        };
        assert!(
            row("lower.height").contains("1.5")
                && row("lower.height").contains("10.0s")
                && row("lower.height").contains("STALE")
        );
        assert!(row("upper.height").contains("1.0s") && row("upper.height").contains("good"));
        assert!(
            row("pump.on").contains("▁█") || row("pump.on").contains("█▁"),
            "{}",
            row("pump.on")
        );
        assert!(screen[19].contains("q quit"));
    }

    #[test]
    fn a_changed_value_is_highlighted_for_a_second() {
        let find = |b: &ratatui::buffer::Buffer, tag: &str| {
            let lines = text(b);
            let y = lines.iter().position(|l| l.starts_with(tag)).unwrap();
            let x = tag.len() + lines[y][tag.len()..].find(|c| c != ' ').unwrap(); // first char of the value column
            b.cell((x as u16, y as u16)).unwrap().fg
        };
        let hot = draw(&app(), 10.0);
        assert_eq!(find(&hot, "pump.on"), Color::Yellow);
        assert_ne!(find(&hot, "upper.height"), Color::Yellow);
        assert_ne!(find(&draw(&app(), 10.6), "pump.on"), Color::Yellow);
    }

    #[test]
    fn filter_sort_and_pause_from_the_keyboard() {
        let mut a = app();
        let names = |a: &App| {
            a.rows(10.0)
                .iter()
                .map(|r| r.name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&a), ["lower.height", "pump.on", "upper.height"]);
        a.key(key(KeyCode::Char('/')), 10.0);
        for c in "pump".chars() {
            a.key(key(KeyCode::Char(c)), 10.0); // typed into the filter: 'p' did not pause, 'q' would not quit
        }
        assert_eq!(names(&a), ["pump.on"]);
        a.key(key(KeyCode::Backspace), 10.0);
        a.key(key(KeyCode::Enter), 10.0);
        assert_eq!(names(&a), ["pump.on"]); // `pum`
        a.key(key(KeyCode::Char('/')), 10.0);
        a.key(key(KeyCode::Esc), 10.0);
        assert!(text(&draw(&a, 10.0))[0].contains("filter `pum`"));
        a.filter = "*.height".into();
        assert_eq!(names(&a), ["lower.height", "upper.height"]);
        a.filter.clear();
        a.key(key(KeyCode::Char('s')), 10.0); // oldest first
        assert_eq!(names(&a)[0], "lower.height");
        a.key(key(KeyCode::Char('s')), 10.0); // stale first
        assert_eq!(names(&a)[0], "lower.height");
        a.key(key(KeyCode::Char('s')), 10.0);
        assert_eq!(a.sort, Sort::Name);
        a.key(key(KeyCode::Char('p')), 10.0);
        a.msg(Msg::Tag {
            name: "upper.height".into(),
            value: 99.0,
            at: 10.0,
        });
        let frozen = a
            .rows(11.0)
            .into_iter()
            .find(|r| r.name == "upper.height")
            .unwrap();
        assert_eq!(frozen.value, 7.0);
        assert!(text(&draw(&a, 10.0))[0].starts_with("[paused] unit: "));
        a.key(key(KeyCode::Char('p')), 12.0);
        assert_eq!(a.rows(12.0)[2].value, 99.0);
    }

    #[test]
    fn rows_sort_by_age_or_staleness() {
        let mut t = Table::new(2.0);
        for (n, at) in [("a", 5.0), ("b", 1.0), ("c", 4.0), ("d", 0.0)] {
            t.apply(n, 1.0, at);
        }
        let by = |s| {
            visible(t.rows(5.0), "", s)
                .iter()
                .map(|r| r.name.clone())
                .collect::<String>()
        };
        assert_eq!(
            (by(Sort::Name).as_str(), by(Sort::Age).as_str()),
            ("abcd", "dbca")
        );
        assert_eq!(by(Sort::Stale), "bdac"); // stale (b, d) first, each group by name
    }

    #[test]
    fn the_log_pane_filters_by_level_and_quit_keys_quit() {
        let mut a = app();
        for l in [
            "Debug: noise",
            "Info: hello",
            "Warn: careful",
            "Error: boom",
        ] {
            a.msg(Msg::Log(l.into()));
        }
        let pane = |a: &App| text(&draw(a, 10.0)).join("\n");
        assert!(
            pane(&a).contains("log (>= info)")
                && pane(&a).contains("Warn: careful")
                && !pane(&a).contains("noise")
        );
        a.key(key(KeyCode::Char('l')), 0.0); // debug
        assert!(pane(&a).contains("Debug: noise"));
        a.key(key(KeyCode::Char('l')), 0.0); // wraps to error
        assert!(pane(&a).contains("Error: boom") && !pane(&a).contains("careful"));
        for _ in 0..300 {
            a.msg(Msg::Log("Info: x".into()));
        }
        assert_eq!(a.log.len(), LOG_KEEP);
        a.msg(Msg::Closed("process exited".into()));
        assert!(text(&draw(&a, 1.0))[0].starts_with("[process exited] unit: "));
        assert!(!a.key(key(KeyCode::Char('x')), 0.0));
        assert!(a.key(key(KeyCode::Char('q')), 0.0));
        assert!(a.key(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            0.0
        ));
        assert!(!a.key(key(KeyCode::Char('c')), 0.0));
    }

    #[test]
    fn spark_scales_to_its_own_range() {
        assert_eq!(spark(&[0.0, 7.0]), "▁█");
        assert_eq!(spark(&[3.0, 3.0]), "▁▁");
        assert_eq!(spark(&[0.0, 3.5, 7.0]), "▁▅█");
        assert_eq!(spark(&[]), "");
    }

    #[test]
    fn the_loop_applies_messages_draws_and_stops_on_quit() {
        let (tx, rx) = channel();
        tx.send(Msg::Tag {
            name: "x".into(),
            value: 4.0,
            at: 0.0,
        })
        .unwrap();
        let mut a = App::new(Table::new(2.0), "loop".into(), String::new());
        let mut term = Terminal::new(TestBackend::new(60, 14)).unwrap();
        let mut keys = vec![Some(key(KeyCode::Char('q'))), None].into_iter();
        let mut tick = 0.0;
        run(
            &mut term,
            &mut a,
            &rx,
            |_| keys.next().unwrap(),
            || {
                tick += 1.0;
                tick
            },
        )
        .unwrap();
        assert!(text(term.backend().buffer())
            .iter()
            .any(|l| l.starts_with("x ") && l.contains("4")));
        assert_eq!(keys.next(), Some(None), "stopped at the first quit");
    }
}
