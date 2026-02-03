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

mod ipc;
mod widgets;
use ipc::IpcClient;
use widgets::{ChartWidget, DepthWidget, TradeLogWidget};

/// Application state.
struct App {
    book: OrderBook,
    latency_p50: u64,
    latency_p99: u64,
    pnl: Decimal,
    position: Decimal,
    price_history: Vec<(f64, f64)>,
    trades: Vec<String>,
    start_time: std::time::Instant,
}

impl App {
    fn new() -> Self {
        Self {
            book: OrderBook::new(Exchange::Binance, Symbol::new("BTCUSDT")),
            latency_p50: 0,
            latency_p99: 0,
            pnl: Decimal::ZERO,
            position: Decimal::ZERO,
            price_history: Vec::with_capacity(100),
            trades: Vec::new(),
            start_time: std::time::Instant::now(),
        }
    }

    fn update_price(&mut self, price: f64) {
        let elapsed = self.start_time.elapsed().as_secs_f64();
        if self.price_history.len() >= 100 {
            self.price_history.remove(0);
        }
        self.price_history.push((elapsed, price));

        // Mock trade log update
        if self.price_history.len() % 10 == 0 {
            self.trades
                .push(format!("Trade: {:.2} @ {:.2}", 0.1, price));
            if self.trades.len() > 20 {
                self.trades.remove(0);
            }
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

    // Connect to IPC
    // In a real app we'd handle connection errors gracefully or retry
    let ipc_client = IpcClient::new(std::path::PathBuf::from("/tmp/mercury.sock"));

    // Spawn IPC connection loop
    let (tx, rx) = tokio::sync::mpsc::channel::<mercury_core::Event>(100);

    let rt = tokio::runtime::Runtime::new()?;
    rt.spawn(async move {
        loop {
            match ipc_client.connect().await {
                Ok(mut stream) => {
                    // Connected
                    while let Some(event) = stream.recv().await {
                        if tx.send(event).await.is_err() {
                            break;
                        }
                    }
                    // Stream ended (disconnected), retry
                }
                Err(_) => {
                    // Failed to connect, wait and retry
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    });

    let result = run_app(&mut terminal, &mut app, rx);

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

    result
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    mut rx: tokio::sync::mpsc::Receiver<mercury_core::Event>,
) -> Result<()> {
    loop {
        // Poll for IPC events (non-blocking)
        while let Ok(event) = rx.try_recv() {
            match event.payload {
                mercury_core::EventPayload::BookUpdate(update) => {
                    app.book.apply_update(&update);
                    // Update price history from best bid/ask
                    if let Some(best_bid) = app.book.best_bid() {
                        use rust_decimal::prelude::ToPrimitive;
                        if let Some(p) = best_bid.price.to_f64() {
                            app.update_price(p);
                        }
                    }
                }
                mercury_core::EventPayload::Trade(trade) => {
                    use rust_decimal::prelude::ToPrimitive;
                    if let Some(p) = trade.price.to_f64() {
                        app.update_price(p);
                        app.trades
                            .push(format!("Trade: {:.2} @ {:.2}", trade.quantity, trade.price));
                        if app.trades.len() > 20 {
                            app.trades.remove(0);
                        }
                    }
                }
                mercury_core::EventPayload::Fill(fill) => {
                    app.position += if fill.side == mercury_core::Side::Buy {
                        fill.quantity
                    } else {
                        -fill.quantity
                    };
                    // PnL calc is complex, simplified for now
                }
                _ => {}
            }
        }

        terminal.draw(|f| ui(f, app))?;

        if event::poll(Duration::from_millis(10))? {
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
            Constraint::Length(3),      // Header
            Constraint::Percentage(40), // Top: Chart + Trade Log
            Constraint::Percentage(40), // Bottom: Order Book + Depth
            Constraint::Length(5),      // Footer: Stats
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

    // Top Section: Chart (70%) + Trade Log (30%)
    let top_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(chunks[1]);

    // Chart
    let chart = ChartWidget::new(&app.price_history, "Real-time Price", Color::Cyan);
    f.render_widget(chart.render(), top_chunks[0]);

    // Trade Log
    let log = TradeLogWidget::new(&app.trades);
    f.render_widget(log.render(), top_chunks[1]);

    // Bottom Section: Order Book (60%) + Depth Viz (40%)
    let bottom_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(chunks[2]);

    // Order Book (Split into Bids/Asks)
    let book_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(bottom_chunks[0]);

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

    // Depth Visualization
    let depth_bids = app.book.top_bids(50);
    let depth_asks = app.book.top_asks(50);
    let depth = DepthWidget::new(&depth_bids, &depth_asks);
    f.render_widget(depth.render(), bottom_chunks[1]);

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
            Span::raw(" | Status: "),
            Span::styled(
                if app.price_history.is_empty() {
                    "Waiting..."
                } else {
                    "Active"
                },
                Style::default().fg(if app.price_history.is_empty() {
                    Color::Yellow
                } else {
                    Color::Green
                }),
            ),
        ]),
        Line::from("Press 'q' to quit"),
    ])
    .block(Block::default().borders(Borders::ALL).title("Stats"));
    f.render_widget(stats, chunks[3]);
}
