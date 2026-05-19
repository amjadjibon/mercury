//! Mercury TUI - Terminal user interface for monitoring.

use anyhow::Result;
use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use mercury_core::{Exchange, OrderBook, Side, Symbol};
use mercury_metrics::PnLTracker;
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Row, Table, Tabs},
};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use std::io;
use std::time::Duration;

mod ipc;
mod widgets;
use ipc::IpcClient;
use widgets::{ChartWidget, DepthWidget};

#[derive(Parser)]
#[command(name = "mercury-tui", about = "Mercury TUI monitor")]
struct Args {
    /// Symbol(s) to monitor — repeat for multiple: -s BTCUSDT -s ETHUSDT
    #[arg(short, long, required = true)]
    symbol: Vec<String>,

    /// Exchange (binance, coinbase)
    #[arg(short, long, default_value = "binance")]
    exchange: String,
}

struct FillEntry {
    elapsed_secs: u64,
    side: Side,
    price: Decimal,
    quantity: Decimal,
    position_after: Decimal,
}

impl FillEntry {
    fn to_display(&self) -> String {
        let side_str = match self.side {
            Side::Buy => "BUY ",
            Side::Sell => "SELL",
        };
        format!(
            "[{:>4}s] {} {:.4} @ {:.2}  pos:{:+.4}",
            self.elapsed_secs, side_str, self.quantity, self.price, self.position_after
        )
    }
}

struct SymbolTab {
    book: OrderBook,
    symbol: Symbol,
    pnl_tracker: PnLTracker,
    position: Decimal,
    cost_basis: Decimal,
    unrealized_pnl: Decimal,
    realized_pnl: Decimal,
    price_history: Vec<(f64, f64)>,
    pnl_history: Vec<(f64, f64)>,
    fills: Vec<FillEntry>,
    start_time: std::time::Instant,
    latency_p50: u64,
    latency_p99: u64,
}

impl SymbolTab {
    fn new(symbol: Symbol, exchange: Exchange) -> Self {
        Self {
            book: OrderBook::new(exchange, symbol),
            symbol,
            pnl_tracker: PnLTracker::new(),
            position: Decimal::ZERO,
            cost_basis: Decimal::ZERO,
            unrealized_pnl: Decimal::ZERO,
            realized_pnl: Decimal::ZERO,
            price_history: Vec::with_capacity(200),
            pnl_history: Vec::with_capacity(200),
            fills: Vec::new(),
            start_time: std::time::Instant::now(),
            latency_p50: 0,
            latency_p99: 0,
        }
    }

    fn on_price(&mut self, price: f64) {
        let elapsed = self.start_time.elapsed().as_secs_f64();
        if self.price_history.len() >= 200 {
            self.price_history.remove(0);
        }
        self.price_history.push((elapsed, price));
        self.push_pnl();
    }

    fn push_pnl(&mut self) {
        let elapsed = self.start_time.elapsed().as_secs_f64();
        if let Some(pnl) = self.total_pnl().to_f64() {
            if self.pnl_history.len() >= 200 {
                self.pnl_history.remove(0);
            }
            self.pnl_history.push((elapsed, pnl));
        }
    }

    fn on_fill(&mut self, fill: &mercury_core::Fill) {
        self.pnl_tracker.on_fill(fill);

        let value = fill.price * fill.quantity;
        match fill.side {
            Side::Buy => {
                self.position += fill.quantity;
                self.cost_basis += value;
            }
            Side::Sell => {
                if self.position > Decimal::ZERO {
                    let avg = self.cost_basis / self.position;
                    self.cost_basis -= avg * fill.quantity;
                }
                self.position -= fill.quantity;
            }
        }

        self.realized_pnl = self.pnl_tracker.total_realized_pnl();

        self.fills.push(FillEntry {
            elapsed_secs: self.start_time.elapsed().as_secs(),
            side: fill.side,
            price: fill.price,
            quantity: fill.quantity,
            position_after: self.position,
        });
        if self.fills.len() > 50 {
            self.fills.remove(0);
        }
        self.push_pnl();
    }

    fn avg_entry(&self) -> Option<Decimal> {
        if self.position == Decimal::ZERO {
            None
        } else {
            Some(self.cost_basis / self.position)
        }
    }

    fn total_pnl(&self) -> Decimal {
        self.realized_pnl + self.unrealized_pnl
    }
}

struct App {
    tabs: Vec<SymbolTab>,
    selected: usize,
    kill_switch: bool,
}

impl App {
    fn new(symbols: Vec<Symbol>, exchange: Exchange) -> Self {
        let tabs = symbols
            .into_iter()
            .map(|s| SymbolTab::new(s, exchange))
            .collect();
        Self { tabs, selected: 0, kill_switch: false }
    }

    fn active(&self) -> &SymbolTab {
        &self.tabs[self.selected]
    }

    fn next_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.selected = (self.selected + 1) % self.tabs.len();
        }
    }

    fn prev_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.selected = self.selected.checked_sub(1).unwrap_or(self.tabs.len() - 1);
        }
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    let exchange = match args.exchange.to_lowercase().as_str() {
        "coinbase" => Exchange::Coinbase,
        _ => Exchange::Binance,
    };
    let symbols: Vec<Symbol> = args.symbol.iter().map(|s| Symbol::new(s)).collect();

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(symbols, exchange);

    let ipc_client = IpcClient::new(std::path::PathBuf::from("/tmp/mercury.sock"));
    let (tx, rx) = tokio::sync::mpsc::channel::<mercury_core::Event>(256);

    let rt = tokio::runtime::Runtime::new()?;
    rt.spawn(async move {
        loop {
            match ipc_client.connect().await {
                Ok(mut stream) => {
                    while let Some(event) = stream.recv().await {
                        if tx.send(event).await.is_err() {
                            break;
                        }
                    }
                }
                Err(_) => {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    });

    let result = run_app(&mut terminal, &mut app, rx);

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
        while let Ok(event) = rx.try_recv() {
            match event.payload {
                mercury_core::EventPayload::BookUpdate(update) => {
                    // Route to the matching symbol tab.
                    for tab in app.tabs.iter_mut() {
                        if tab.symbol == update.symbol {
                            tab.book.apply_update(&update);
                            if let Some(mid) = tab.book.mid_price() {
                                tab.on_price(mid.to_f64());
                                tab.unrealized_pnl = tab.pnl_tracker.unrealized_pnl(tab.symbol, mid.to_decimal());
                            }
                        }
                    }
                }
                mercury_core::EventPayload::Fill(fill) => {
                    for tab in app.tabs.iter_mut() {
                        if tab.symbol == fill.symbol {
                            tab.on_fill(&fill);
                        }
                    }
                }
                mercury_core::EventPayload::Trade(trade) => {
                    for tab in app.tabs.iter_mut() {
                        if tab.symbol == trade.symbol {
                            if let Some(p) = trade.price.to_f64() {
                                tab.on_price(p);
                            }
                        }
                    }
                }
                mercury_core::EventPayload::LatencyReport(report) => {
                    for tab in app.tabs.iter_mut() {
                        tab.latency_p50 = report.p50_ns;
                        tab.latency_p99 = report.p99_ns;
                    }
                }
                _ => {}
            }
        }

        terminal.draw(|f| ui(f, app))?;

        if event::poll(Duration::from_millis(16))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') => return Ok(()),
                        KeyCode::Char('k') => app.kill_switch = !app.kill_switch,
                        KeyCode::Right | KeyCode::Char('l') => app.next_tab(),
                        KeyCode::Left | KeyCode::Char('h') => app.prev_tab(),
                        _ => {}
                    }
                }
            }
        }
    }
}

fn pnl_color(v: Decimal) -> Color {
    if v > Decimal::ZERO {
        Color::Green
    } else if v < Decimal::ZERO {
        Color::Red
    } else {
        Color::Gray
    }
}

fn ui(f: &mut Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(3),      // Tab bar
            Constraint::Length(3),      // Header / price line
            Constraint::Percentage(43), // Chart + Fill Log
            Constraint::Percentage(36), // Order Book + Depth
            Constraint::Length(6),      // Stats
        ])
        .split(area);

    // ── Tab bar ──────────────────────────────────────────────────────────────
    let tab_titles: Vec<Line> = app
        .tabs
        .iter()
        .map(|t| Line::from(t.symbol.as_str().to_string()))
        .collect();
    let tabs_widget = Tabs::new(tab_titles)
        .select(app.selected)
        .block(Block::default().borders(Borders::ALL).title("Symbols  ← h/l →"))
        .style(Style::default().fg(Color::DarkGray))
        .highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
    f.render_widget(tabs_widget, chunks[0]);

    let tab = app.active();

    // ── Header ──────────────────────────────────────────────────────────────
    let spread_str = tab
        .book
        .spread_bps()
        .map(|s| format!("{:.2} bps", s.to_f64()))
        .unwrap_or_else(|| "–".to_string());

    let mid_str = tab
        .book
        .mid_price()
        .map(|m| format!("{:.2}", m.to_f64()))
        .unwrap_or_else(|| "–".to_string());

    let kill_span = if app.kill_switch {
        Span::styled(" [KILL SWITCH ON] ", Style::default().fg(Color::Black).bg(Color::Red).add_modifier(Modifier::BOLD))
    } else {
        Span::styled(" [k: kill switch] ", Style::default().fg(Color::DarkGray))
    };

    let header = Paragraph::new(Line::from(vec![
        Span::styled("Mercury ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::raw("| "),
        Span::styled(tab.book.symbol.as_str(), Style::default().fg(Color::Yellow)),
        Span::raw("  mid "),
        Span::styled(mid_str, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::raw("  spread "),
        Span::styled(spread_str, Style::default().fg(Color::Cyan)),
        Span::raw("  fills "),
        Span::styled(tab.fills.len().to_string(), Style::default().fg(Color::White)),
        Span::raw("  "),
        kill_span,
        Span::styled("  q: quit", Style::default().fg(Color::DarkGray)),
    ]))
    .block(Block::default().borders(Borders::ALL).title("Mercury Paper Trading"));
    f.render_widget(header, chunks[1]);

    // ── Chart + Fill Log ────────────────────────────────────────────────────
    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(chunks[2]);

    let chart_area = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(top[0]);

    let price_chart = ChartWidget::new(&tab.price_history, "Mid Price", Color::Cyan);
    f.render_widget(price_chart.render(), chart_area[0]);

    let pnl_line_color = if tab.total_pnl() >= Decimal::ZERO { Color::Green } else { Color::Red };
    let pnl_chart = ChartWidget::new(&tab.pnl_history, "PnL (USDT)", pnl_line_color);
    f.render_widget(pnl_chart.render(), chart_area[1]);

    let fill_items: Vec<ListItem> = tab
        .fills
        .iter()
        .rev()
        .map(|f| {
            let color = match f.side {
                Side::Buy => Color::Green,
                Side::Sell => Color::Red,
            };
            ListItem::new(Span::styled(f.to_display(), Style::default().fg(color)))
        })
        .collect();

    let fill_list = List::new(fill_items)
        .block(Block::default().borders(Borders::ALL).title("Fills"));
    f.render_widget(fill_list, top[1]);

    // ── Order Book + Depth ──────────────────────────────────────────────────
    let bottom = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(chunks[3]);

    let book_halves = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(bottom[0]);

    let bid_rows: Vec<Row> = tab
        .book
        .top_bids(12)
        .iter()
        .map(|l| {
            Row::new(vec![format!("{:.2}", l.price), format!("{:.4}", l.quantity)])
                .style(Style::default().fg(Color::Green))
        })
        .collect();

    f.render_widget(
        Table::new(bid_rows, [Constraint::Percentage(50), Constraint::Percentage(50)])
            .header(Row::new(vec!["Price", "Qty"]).style(Style::default().fg(Color::White).add_modifier(Modifier::BOLD)))
            .block(Block::default().borders(Borders::ALL).title("Bids")),
        book_halves[0],
    );

    let ask_rows: Vec<Row> = tab
        .book
        .top_asks(12)
        .iter()
        .map(|l| {
            Row::new(vec![format!("{:.2}", l.price), format!("{:.4}", l.quantity)])
                .style(Style::default().fg(Color::Red))
        })
        .collect();

    f.render_widget(
        Table::new(ask_rows, [Constraint::Percentage(50), Constraint::Percentage(50)])
            .header(Row::new(vec!["Price", "Qty"]).style(Style::default().fg(Color::White).add_modifier(Modifier::BOLD)))
            .block(Block::default().borders(Borders::ALL).title("Asks")),
        book_halves[1],
    );

    let depth_bids = tab.book.top_bids(50);
    let depth_asks = tab.book.top_asks(50);
    f.render_widget(DepthWidget::new(&depth_bids, &depth_asks).render(), bottom[1]);

    // ── Stats ────────────────────────────────────────────────────────────────
    let pos_color = if tab.position > Decimal::ZERO {
        Color::Green
    } else if tab.position < Decimal::ZERO {
        Color::Red
    } else {
        Color::Gray
    };

    let avg_entry_str = tab
        .avg_entry()
        .map(|p| format!("{:.2}", p))
        .unwrap_or_else(|| "–".to_string());

    let uptime = tab.start_time.elapsed().as_secs();
    let uptime_str = format!("{:02}:{:02}:{:02}", uptime / 3600, (uptime % 3600) / 60, uptime % 60);

    let stats = Paragraph::new(vec![
        Line::from(vec![
            Span::raw("Position : "),
            Span::styled(format!("{:+.4}", tab.position), Style::default().fg(pos_color).add_modifier(Modifier::BOLD)),
            Span::raw("   Avg Entry : "),
            Span::styled(avg_entry_str, Style::default().fg(Color::Yellow)),
        ]),
        Line::from(vec![
            Span::raw("Realized : "),
            Span::styled(format!("{:+.4} USDT", tab.realized_pnl), Style::default().fg(pnl_color(tab.realized_pnl)).add_modifier(Modifier::BOLD)),
            Span::raw("   Unrealized : "),
            Span::styled(format!("{:+.4} USDT", tab.unrealized_pnl), Style::default().fg(pnl_color(tab.unrealized_pnl)).add_modifier(Modifier::BOLD)),
            Span::raw("   Total : "),
            Span::styled(format!("{:+.4} USDT", tab.total_pnl()), Style::default().fg(pnl_color(tab.total_pnl())).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![
            Span::raw("Uptime   : "),
            Span::styled(uptime_str, Style::default().fg(Color::Cyan)),
            Span::raw("   Status : "),
            Span::styled(
                if app.kill_switch { "HALTED" } else if tab.price_history.is_empty() { "Connecting..." } else { "Active" },
                Style::default().fg(if app.kill_switch { Color::Red } else if tab.price_history.is_empty() { Color::Yellow } else { Color::Green }),
            ),
            Span::raw("   Latency p50/p99 : "),
            Span::styled(
                format!("{}/{}μs", tab.latency_p50 / 1000, tab.latency_p99 / 1000),
                Style::default().fg(Color::Cyan),
            ),
        ]),
    ])
    .block(Block::default().borders(Borders::ALL).title("P&L"));
    f.render_widget(stats, chunks[4]);
}
