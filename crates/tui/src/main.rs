//! Mercury TUI - Terminal user interface for monitoring.

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use mercury_core::{Exchange, OrderBook, Symbol};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Row, Table},
};
use rust_decimal::Decimal;
use std::io;
use std::time::Duration;

/// Application state.
struct App {
    book: OrderBook,
    latency_p50: u64,
    latency_p99: u64,
    pnl: Decimal,
    position: Decimal,
}

impl App {
    fn new() -> Self {
        Self {
            book: OrderBook::new(Exchange::Binance, Symbol::new("BTCUSDT")),
            latency_p50: 0,
            latency_p99: 0,
            pnl: Decimal::ZERO,
            position: Decimal::ZERO,
        }
    }
}

fn main() -> Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();
    let result = run_app(&mut terminal, &mut app);

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

    result
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|f| ui(f, app))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press && key.code == KeyCode::Char('q') {
                    return Ok(());
                }
            }
        }
    }
}

fn ui(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(10),   // Order book
            Constraint::Length(5), // Stats
        ])
        .split(f.area());

    // Header
    let header = Paragraph::new(vec![Line::from(vec![
        Span::styled("Mercury ", Style::default().fg(Color::Cyan)),
        Span::raw("| "),
        Span::styled(app.book.symbol.as_str(), Style::default().fg(Color::Yellow)),
        Span::raw(" | "),
        Span::styled(
            app.book.exchange.to_string(),
            Style::default().fg(Color::Green),
        ),
    ])])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Mercury Trading Engine"),
    );
    f.render_widget(header, chunks[0]);

    // Order book
    let book_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(chunks[1]);

    // Bids
    let bid_rows: Vec<Row> = app
        .book
        .top_bids(10)
        .iter()
        .map(|l| {
            Row::new(vec![
                format!("{:.2}", l.price),
                format!("{:.4}", l.quantity),
            ])
            .style(Style::default().fg(Color::Green))
        })
        .collect();

    let bids_table = Table::new(
        bid_rows,
        [Constraint::Percentage(50), Constraint::Percentage(50)],
    )
    .header(Row::new(vec!["Price", "Quantity"]).style(Style::default().fg(Color::White)))
    .block(Block::default().borders(Borders::ALL).title("Bids"));
    f.render_widget(bids_table, book_chunks[0]);

    // Asks
    let ask_rows: Vec<Row> = app
        .book
        .top_asks(10)
        .iter()
        .map(|l| {
            Row::new(vec![
                format!("{:.2}", l.price),
                format!("{:.4}", l.quantity),
            ])
            .style(Style::default().fg(Color::Red))
        })
        .collect();

    let asks_table = Table::new(
        ask_rows,
        [Constraint::Percentage(50), Constraint::Percentage(50)],
    )
    .header(Row::new(vec!["Price", "Quantity"]).style(Style::default().fg(Color::White)))
    .block(Block::default().borders(Borders::ALL).title("Asks"));
    f.render_widget(asks_table, book_chunks[1]);

    // Stats
    let pnl_color = if app.pnl >= Decimal::ZERO {
        Color::Green
    } else {
        Color::Red
    };

    let stats = Paragraph::new(vec![
        Line::from(vec![
            Span::raw("Position: "),
            Span::styled(
                format!("{:.4}", app.position),
                Style::default().fg(Color::Yellow),
            ),
            Span::raw(" | PnL: "),
            Span::styled(format!("{:.2}", app.pnl), Style::default().fg(pnl_color)),
            Span::raw(" | Latency p50/p99: "),
            Span::styled(
                format!("{}/{}μs", app.latency_p50 / 1000, app.latency_p99 / 1000),
                Style::default().fg(Color::Cyan),
            ),
        ]),
        Line::from("Press 'q' to quit"),
    ])
    .block(Block::default().borders(Borders::ALL).title("Stats"));
    f.render_widget(stats, chunks[2]);
}
