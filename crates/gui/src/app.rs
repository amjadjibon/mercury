use iced::widget::{column, row, container, text, button, text_input, canvas, scrollable};
use iced::{Element, Length, Subscription, Theme, Color, Task, Font, font};
use std::collections::VecDeque;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixStream;
use std::sync::Mutex;
use tokio::sync::mpsc;

use mercury_core::events::{Event, EventPayload};
use mercury_core::Side;

use crate::widgets::depth_map::DepthMap;
use crate::widgets::sparkline::Sparkline;

static LOG_TX: Mutex<Option<mpsc::UnboundedSender<Message>>> = Mutex::new(None);


/// Mercury Trading Desktop App state.
pub struct MercuryApp {
    is_connected: bool,
    symbol: String,
    best_bid: f64,
    best_ask: f64,
    bids: Vec<(f64, f64)>, // (price, quantity)
    asks: Vec<(f64, f64)>, // (price, quantity)
    recent_trades: VecDeque<TradeState>,
    latency_history: VecDeque<f32>,
    p50_us: f32,
    p99_us: f32,
    manual_amount: String,
    manual_price: String,
    status_message: String,
    active_positions: Vec<PositionState>,

    // Engine process launcher states
    engine_running: bool,
    engine_symbol: String,
    engine_strategy: String,
    engine_paper: bool,
    engine_logs: VecDeque<String>,
    show_logs_tab: bool,
    engine_shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

#[allow(dead_code)]
#[derive(Clone)]
struct TradeState {
    time: String,
    symbol: String,
    price: f64,
    quantity: f64,
    side: Side,
}

#[derive(Clone)]
struct PositionState {
    symbol: String,
    size: f64,
    entry_price: f64,
    pnl: f64,
}

#[derive(Debug, Clone)]
pub enum Message {
    EventReceived(Event),
    ConnectionLost,
    BuyClicked,
    SellClicked,
    KillSwitchClicked,
    AmountChanged(String),
    PriceChanged(String),
    Tick,

    // Launcher messages
    StartEngine,
    StopEngine,
    EngineLogReceived(String),
    EngineStopped,
    EngineSymbolChanged(String),
    EngineStrategyChanged(String),
    EnginePaperToggled,
    ToggleMiddleTab(bool),
}

fn engine_log_stream() -> impl futures_util::stream::Stream<Item = Message> {
    let (tx, rx) = mpsc::unbounded_channel::<Message>();
    if let Ok(mut guard) = LOG_TX.lock() {
        *guard = Some(tx);
    }
    
    futures_util::stream::unfold(rx, |mut rx| async move {
        match rx.recv().await {
            Some(msg) => Some((msg, rx)),
            None => None,
        }
    })
}

fn spawn_engine_process(
    symbol: String,
    strategy: String,
    is_paper: bool,
    mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
) {
    tokio::spawn(async move {
        let Ok(mut exe_path) = std::env::current_exe() else {
            send_log("[SYSTEM ERROR] Failed to resolve current executable path.".to_string());
            send_stopped();
            return;
        };
        exe_path.pop(); // Remove "mercury-gui"
        exe_path.push("mercury"); // Add "mercury"

        if !exe_path.exists() {
            send_log(format!("[SYSTEM ERROR] Mercury binary not found at {:?}", exe_path));
            send_stopped();
            return;
        }

        send_log(format!("[SYSTEM] Starting engine symbol={} strategy={} paper={}", symbol, strategy, is_paper));

        let mut args = vec!["run".to_string(), "--symbol".to_string(), symbol];
        args.push("--strategy".to_string());
        args.push(strategy);
        if is_paper {
            args.push("--paper".to_string());
        }

        let mut cmd = tokio::process::Command::new(&exe_path);
        cmd.args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                send_log(format!("[SYSTEM ERROR] Failed to spawn process: {}", e));
                send_stopped();
                return;
            }
        };

        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();

        let mut stdout_reader = BufReader::new(stdout).lines();
        let mut stderr_reader = BufReader::new(stderr).lines();

        loop {
            tokio::select! {
                res = stdout_reader.next_line() => {
                    match res {
                        Ok(Some(line)) => {
                            send_log(line);
                        }
                        _ => break, // EOF
                    }
                }
                res = stderr_reader.next_line() => {
                    match res {
                        Ok(Some(line)) => {
                            send_log(format!("[STDERR] {}", line));
                        }
                        _ => {}
                    }
                }
                _ = &mut shutdown_rx => {
                    send_log("[SYSTEM] Shutting down engine...".to_string());
                    let _ = child.kill().await;
                    break;
                }
            }
        }

        let _ = child.wait().await;
        send_log("[SYSTEM] Engine stopped.".to_string());
        send_stopped();
    });
}

fn send_log(msg: String) {
    if let Ok(guard) = LOG_TX.lock() {
        if let Some(tx) = &*guard {
            let _ = tx.send(Message::EngineLogReceived(msg));
        }
    }
}

fn send_stopped() {
    if let Ok(guard) = LOG_TX.lock() {
        if let Some(tx) = &*guard {
            let _ = tx.send(Message::EngineStopped);
        }
    }
}

fn connect_stream() -> impl futures_util::stream::Stream<Item = Message> {
    futures_util::stream::unfold(None, |mut state| async move {
        let reader = match &mut state {
            Some(r) => r,
            None => {
                match UnixStream::connect("/tmp/mercury.sock").await {
                    Ok(stream) => {
                        state = Some(BufReader::new(stream));
                        state.as_mut().unwrap()
                    }
                    Err(_) => {
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        return Some((Message::ConnectionLost, None));
                    }
                }
            }
        };
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(bytes) if bytes > 0 => {
                    if let Ok(event) = serde_json::from_str::<Event>(&line) {
                        return Some((Message::EventReceived(event), state));
                    }
                }
                _ => {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    return Some((Message::ConnectionLost, None));
                }
            }
        }
    })
}

impl MercuryApp {
    pub fn new() -> (Self, Task<Message>) {
        (
            Self {
                is_connected: false,
                symbol: "BTCUSDT".to_string(),
                best_bid: 0.0,
                best_ask: 0.0,
                bids: Vec::new(),
                asks: Vec::new(),
                recent_trades: VecDeque::with_capacity(30),
                latency_history: {
                    let mut q = VecDeque::new();
                    // Initialize with zero telemetry points to make sparkline look nice
                    for _ in 0..60 {
                        q.push_back(0.0);
                    }
                    q
                },
                p50_us: 0.0,
                p99_us: 0.0,
                manual_amount: "".to_string(),
                manual_price: "".to_string(),
                status_message: "System initialized. Waiting for engine stream...".to_string(),
                active_positions: vec![
                    PositionState {
                        symbol: "BTCUSDT".to_string(),
                        size: 0.0,
                        entry_price: 50000.0,
                        pnl: 0.0,
                    }
                ],
                engine_running: false,
                engine_symbol: "BTCUSDT".to_string(),
                engine_strategy: "rsi".to_string(),
                engine_paper: true,
                engine_logs: {
                    let mut q = VecDeque::new();
                    q.push_back("[SYSTEM] Log terminal console initialized.".to_string());
                    q
                },
                show_logs_tab: false,
                engine_shutdown_tx: None,
            },
            Task::none(),
        )
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::ConnectionLost => {
                self.is_connected = false;
                self.status_message = "Disconnected from engine. Retrying connect...".to_string();
            }
            Message::EventReceived(event) => {
                self.is_connected = true;
                match event.payload {
                    EventPayload::BookUpdate(update) => {
                        self.symbol = update.symbol.as_str().to_string();
                        
                        // Parse top 15 bids
                        let mut new_bids = Vec::new();
                        for level in update.bid_levels().iter().take(15) {
                            new_bids.push((level.price.to_f64(), level.quantity.to_f64()));
                        }
                        self.bids = new_bids;

                        // Parse top 15 asks
                        let mut new_asks = Vec::new();
                        for level in update.ask_levels().iter().take(15) {
                            new_asks.push((level.price.to_f64(), level.quantity.to_f64()));
                        }
                        self.asks = new_asks;

                        if !self.bids.is_empty() {
                            self.best_bid = self.bids[0].0;
                        }
                        if !self.asks.is_empty() {
                            self.best_ask = self.asks[0].0;
                        }
                    }
                    EventPayload::Trade(trade) => {
                        let price_f64 = trade.price.to_string().parse::<f64>().unwrap_or(0.0);
                        let qty_f64 = trade.quantity.to_string().parse::<f64>().unwrap_or(0.0);
                        
                        let trade_state = TradeState {
                            time: chrono::Local::now().format("%H:%M:%S").to_string(),
                            symbol: trade.symbol.as_str().to_string(),
                            price: price_f64,
                            quantity: qty_f64,
                            side: trade.side,
                        };

                        self.recent_trades.push_front(trade_state);
                        if self.recent_trades.len() > 25 {
                            self.recent_trades.pop_back();
                        }
                    }
                    EventPayload::Fill(fill) => {
                        let price_f64 = fill.price.to_string().parse::<f64>().unwrap_or(0.0);
                        let qty_f64 = fill.quantity.to_string().parse::<f64>().unwrap_or(0.0);
                        self.status_message = format!(
                            "Order filled: {:?} {} {} @ {}",
                            fill.side, qty_f64, fill.symbol.as_str(), price_f64
                        );

                        // Update local positions tracking for display
                        let fill_symbol = fill.symbol.as_str().to_string();
                        if let Some(pos) = self.active_positions.iter_mut().find(|p| p.symbol == fill_symbol) {
                            let side_multiplier = if fill.side == Side::Buy { 1.0 } else { -1.0 };
                            pos.size += qty_f64 * side_multiplier;
                            pos.entry_price = price_f64;
                            pos.pnl = if pos.size.abs() > 0.0 {
                                (self.best_bid - pos.entry_price) * pos.size
                            } else {
                                0.0
                            };
                        }
                    }
                    EventPayload::LatencyReport(report) => {
                        // Latency is in nanoseconds, convert to microseconds (1us = 1000ns)
                        let p50_us = (report.p50_ns as f32) / 1000.0;
                        let p99_us = (report.p99_ns as f32) / 1000.0;
                        self.p50_us = p50_us;
                        self.p99_us = p99_us;

                        self.latency_history.push_back(p50_us);
                        if self.latency_history.len() > 100 {
                            self.latency_history.pop_front();
                        }
                    }
                    EventPayload::RiskAlert(alert) => {
                        self.status_message = format!("ALERT [Risk]: {}", alert.message);
                    }
                    _ => {}
                }
            }
            Message::BuyClicked => {
                let amount = self.manual_amount.parse::<f64>().unwrap_or(0.0);
                self.status_message = format!("Manual override: BUY Order sent for {} BTC", amount);
            }
            Message::SellClicked => {
                let amount = self.manual_amount.parse::<f64>().unwrap_or(0.0);
                self.status_message = format!("Manual override: SELL Order sent for {} BTC", amount);
            }
            Message::KillSwitchClicked => {
                self.status_message = "CRITICAL: GLOBAL KILL SWITCH TRIGGERED. Cancelling all orders!".to_string();
                for pos in &mut self.active_positions {
                    pos.size = 0.0;
                    pos.pnl = 0.0;
                }
            }
            Message::AmountChanged(amt) => {
                self.manual_amount = amt;
            }
            Message::PriceChanged(price) => {
                self.manual_price = price;
            }
            Message::Tick => {
                // Update live position PnLs based on latest bid
                for pos in &mut self.active_positions {
                    if pos.size.abs() > 0.0 {
                        pos.pnl = (self.best_bid - pos.entry_price) * pos.size;
                    }
                }
            }
            Message::StartEngine => {
                if !self.engine_running {
                    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
                    self.engine_shutdown_tx = Some(shutdown_tx);
                    self.engine_running = true;
                    self.status_message = "Launching engine...".to_string();
                    
                    self.engine_logs.clear();
                    self.engine_logs.push_back("[SYSTEM] Spawning engine process...".to_string());

                    spawn_engine_process(
                        self.engine_symbol.clone(),
                        self.engine_strategy.clone(),
                        self.engine_paper,
                        shutdown_rx,
                    );
                }
            }
            Message::StopEngine => {
                if self.engine_running {
                    if let Some(tx) = self.engine_shutdown_tx.take() {
                        let _ = tx.send(());
                    }
                    self.status_message = "Stopping engine...".to_string();
                }
            }
            Message::EngineLogReceived(log_line) => {
                self.engine_logs.push_back(log_line);
                if self.engine_logs.len() > 100 {
                    self.engine_logs.pop_front();
                }
            }
            Message::EngineStopped => {
                self.engine_running = false;
                self.engine_shutdown_tx = None;
                self.status_message = "Engine process terminated.".to_string();
            }
            Message::EngineSymbolChanged(sym) => {
                self.engine_symbol = sym;
            }
            Message::EngineStrategyChanged(strat) => {
                self.engine_strategy = strat;
            }
            Message::EnginePaperToggled => {
                self.engine_paper = !self.engine_paper;
            }
            Message::ToggleMiddleTab(show_logs) => {
                self.show_logs_tab = show_logs;
            }
        }
        Task::none()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::batch(vec![
            iced::time::every(std::time::Duration::from_millis(500)).map(|_| Message::Tick),
            Subscription::run(connect_stream),
            Subscription::run(engine_log_stream),
        ])
    }

    pub fn theme(&self) -> Theme {
        Theme::Dark
    }

    pub fn view(&self) -> Element<'_, Message> {
        // CURATED HSL/RGB SLEEK COLORS
        let theme_text_teal = Color::from_rgb8(0, 240, 200);
        let theme_text_pink = Color::from_rgb8(255, 60, 120);
        let theme_gray = Color::from_rgb8(150, 150, 155);

        // 1. TOP HEADER PANEL
        let header = container(
            row![
                text("MERCURY")
                    .size(24)
                    .font(Font { weight: font::Weight::Bold, ..Font::default() })
                    .color(theme_text_teal)
                    .width(Length::FillPortion(1)),
                row![
                    text("SYMBOL: ")
                        .size(14)
                        .color(theme_gray),
                    text(&self.symbol)
                        .size(16)
                        .font(Font { weight: font::Weight::Bold, ..Font::default() }),
                ]
                .spacing(5)
                .width(Length::FillPortion(1)),
                row![
                    text("STATE: ")
                        .size(14)
                        .color(theme_gray),
                    text(if self.is_connected { "LIVE" } else { "OFFLINE" })
                        .size(16)
                        .font(Font { weight: font::Weight::Bold, ..Font::default() })
                        .color(if self.is_connected { theme_text_teal } else { theme_text_pink }),
                ]
                .spacing(5)
                .width(Length::FillPortion(1)),
                row![
                    text("BEST BID: ")
                        .size(14)
                        .color(theme_gray),
                    text(format!("{:.2}", self.best_bid))
                        .size(16)
                        .font(Font { weight: font::Weight::Bold, ..Font::default() })
                        .color(theme_text_teal),
                    text(" | BEST ASK: ")
                        .size(14)
                        .color(theme_gray),
                    text(format!("{:.2}", self.best_ask))
                        .size(16)
                        .font(Font { weight: font::Weight::Bold, ..Font::default() })
                        .color(theme_text_pink),
                ]
                .spacing(5)
                .width(Length::FillPortion(2)),
            ]
            .spacing(20)
            .align_y(iced::Alignment::Center)
        )
        .style(move |_theme| container::Style {
            background: Some(iced::Background::Color(Color::from_rgb8(20, 20, 25))),
            border: iced::Border {
                color: Color::from_rgb8(35, 35, 40),
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        })
        .padding(15)
        .width(Length::Fill);

        // 2. MAIN OBSIDIAN BODY (3 PANELS GRID)
        
        // A. LEFT PANEL (L2 Book & Depth Map)
        let mut bids_col = column![text("BIDS").size(14).font(Font { weight: font::Weight::Bold, ..Font::default() }).color(theme_text_teal)].spacing(8);
        for &(p, q) in self.bids.iter().take(10) {
            bids_col = bids_col.push(
                row![
                    text(format!("{:.2}", p)).size(13).color(theme_text_teal),
                    text(format!("{:.4}", q)).size(13).width(Length::Fill).align_x(iced::alignment::Horizontal::Right)
                ]
            );
        }

        let mut asks_col = column![text("ASKS").size(14).font(Font { weight: font::Weight::Bold, ..Font::default() }).color(theme_text_pink)].spacing(8);
        for &(p, q) in self.asks.iter().take(10) {
            asks_col = asks_col.push(
                row![
                    text(format!("{:.2}", p)).size(13).color(theme_text_pink),
                    text(format!("{:.4}", q)).size(13).width(Length::Fill).align_x(iced::alignment::Horizontal::Right)
                ]
            );
        }

        let book_view = row![
            bids_col.width(Length::FillPortion(1)),
            asks_col.width(Length::FillPortion(1))
        ]
        .spacing(15)
        .padding(10);

        let depth_canvas = canvas(DepthMap {
            bids: self.bids.clone(),
            asks: self.asks.clone(),
        })
        .width(Length::Fill)
        .height(Length::FillPortion(1));

        let left_panel = container(
            column![
                text("L2 ORDER BOOK DEPTH").size(15).font(Font { weight: font::Weight::Bold, ..Font::default() }),
                book_view.height(Length::FillPortion(1)),
                text("CUMULATIVE LIQUIDITY WALLS").size(12).font(Font { weight: font::Weight::Bold, ..Font::default() }).color(theme_gray),
                depth_canvas.height(Length::FillPortion(1)),
            ]
            .spacing(10)
        )
        .style(move |_theme| container::Style {
            background: Some(iced::Background::Color(Color::from_rgb8(20, 20, 25))),
            border: iced::Border {
                color: Color::from_rgb8(35, 35, 40),
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        })
        .padding(15)
        .width(Length::FillPortion(1))
        .height(Length::Fill);

        // B. MIDDLE PANEL (Telemetry, Sparklines, Trade Execution Logs)
        let sparkline_canvas = canvas(Sparkline {
            points: self.latency_history.clone(),
            line_color: theme_text_teal,
        })
        .width(Length::Fill)
        .height(Length::FillPortion(1));

        let telemetry_info = row![
            column![
                text("p50 LATENCY").size(12).color(theme_gray),
                text(format!("{:.2} μs", self.p50_us)).size(18).font(Font { weight: font::Weight::Bold, ..Font::default() }).color(theme_text_teal),
            ]
            .spacing(4),
            column![
                text("p99 LATENCY").size(12).color(theme_gray),
                text(format!("{:.2} μs", self.p99_us)).size(18).font(Font { weight: font::Weight::Bold, ..Font::default() }).color(theme_text_pink),
            ]
            .spacing(4)
        ]
        .spacing(20)
        .padding(5);

        let mut trades_col = column![
            row![
                text("TIME").size(12).font(Font { weight: font::Weight::Bold, ..Font::default() }).color(theme_gray).width(Length::FillPortion(1)),
                text("SIDE").size(12).font(Font { weight: font::Weight::Bold, ..Font::default() }).color(theme_gray).width(Length::FillPortion(1)),
                text("PRICE").size(12).font(Font { weight: font::Weight::Bold, ..Font::default() }).color(theme_gray).width(Length::FillPortion(2)),
                text("SIZE").size(12).font(Font { weight: font::Weight::Bold, ..Font::default() }).color(theme_gray).width(Length::FillPortion(2)),
            ]
        ]
        .spacing(6);

        for t in self.recent_trades.iter().take(10) {
            let is_buy = t.side == Side::Buy;
            trades_col = trades_col.push(
                row![
                    text(&t.time).size(13).width(Length::FillPortion(1)),
                    text(if is_buy { "BUY" } else { "SELL" })
                        .size(13)
                        .font(Font { weight: font::Weight::Bold, ..Font::default() })
                        .color(if is_buy { theme_text_teal } else { theme_text_pink })
                        .width(Length::FillPortion(1)),
                    text(format!("{:.2}", t.price)).size(13).width(Length::FillPortion(2)),
                    text(format!("{:.4}", t.quantity)).size(13).width(Length::FillPortion(2)),
                ]
            );
        }

        // Tab buttons for matching prints vs system logs
        let trades_tab_btn = button(text("MATCHING PRINTS").size(12).font(Font { weight: font::Weight::Bold, ..Font::default() }))
            .padding(8);
        let logs_tab_btn = button(text("SYSTEM LOGS").size(12).font(Font { weight: font::Weight::Bold, ..Font::default() }))
            .padding(8);

        let (trades_tab_btn, logs_tab_btn) = if !self.show_logs_tab {
            (
                trades_tab_btn.style(move |_, _| button::Style {
                    background: Some(iced::Background::Color(Color::from_rgb8(30, 30, 35))),
                    text_color: theme_text_teal,
                    border: iced::Border { color: theme_text_teal, width: 1.0, radius: 4.0.into() },
                    ..Default::default()
                }),
                logs_tab_btn.style(move |_, _| button::Style {
                    background: Some(iced::Background::Color(Color::from_rgb8(15, 15, 20))),
                    text_color: theme_gray,
                    border: iced::Border { color: Color::from_rgb8(40, 40, 45), width: 1.0, radius: 4.0.into() },
                    ..Default::default()
                })
                .on_press(Message::ToggleMiddleTab(true))
            )
        } else {
            (
                trades_tab_btn.style(move |_, _| button::Style {
                    background: Some(iced::Background::Color(Color::from_rgb8(15, 15, 20))),
                    text_color: theme_gray,
                    border: iced::Border { color: Color::from_rgb8(40, 40, 45), width: 1.0, radius: 4.0.into() },
                    ..Default::default()
                })
                .on_press(Message::ToggleMiddleTab(false)),
                logs_tab_btn.style(move |_, _| button::Style {
                    background: Some(iced::Background::Color(Color::from_rgb8(30, 30, 35))),
                    text_color: theme_text_teal,
                    border: iced::Border { color: theme_text_teal, width: 1.0, radius: 4.0.into() },
                    ..Default::default()
                })
            )
        };

        let tabs_row = row![trades_tab_btn, logs_tab_btn].spacing(10);

        let mut logs_col = column![].spacing(4);
        for line in &self.engine_logs {
            let text_color = if line.contains("[ERROR]") || line.contains("[STDERR]") {
                theme_text_pink
            } else if line.contains("[SYSTEM]") {
                theme_text_teal
            } else {
                Color::WHITE
            };
            logs_col = logs_col.push(
                text(line)
                    .size(11)
                    .font(Font { family: font::Family::Monospace, ..Font::default() })
                    .color(text_color)
            );
        }

        let middle_lower_title = if !self.show_logs_tab {
            text("REAL-TIME MATCHING PRINTS").size(15).font(Font { weight: font::Weight::Bold, ..Font::default() })
        } else {
            text("ENGINE SYSTEM LOGS (STDOUT)").size(15).font(Font { weight: font::Weight::Bold, ..Font::default() })
        };

        let middle_lower_content = if !self.show_logs_tab {
            scrollable(trades_col).height(Length::FillPortion(1))
        } else {
            scrollable(logs_col).height(Length::FillPortion(1))
        };

        let middle_panel = container(
            column![
                text("ENGINE TELEMETRY (MICROSECONDS)").size(15).font(Font { weight: font::Weight::Bold, ..Font::default() }),
                telemetry_info,
                sparkline_canvas.height(Length::FillPortion(1)),
                row![middle_lower_title, tabs_row].spacing(20).align_y(iced::Alignment::Center),
                middle_lower_content.height(Length::FillPortion(1)),
            ]
            .spacing(15)
        )
        .style(move |_theme| container::Style {
            background: Some(iced::Background::Color(Color::from_rgb8(20, 20, 25))),
            border: iced::Border {
                color: Color::from_rgb8(35, 35, 40),
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        })
        .padding(15)
        .width(Length::FillPortion(1))
        .height(Length::Fill);

        // C. RIGHT PANEL (Active positions, overrides console, launcher console, global kill switch)
        let mut pos_view = column![
            row![
                text("SYMBOL").size(12).font(Font { weight: font::Weight::Bold, ..Font::default() }).color(theme_gray).width(Length::FillPortion(2)),
                text("SIZE").size(12).font(Font { weight: font::Weight::Bold, ..Font::default() }).color(theme_gray).width(Length::FillPortion(2)),
                text("PnL (USD)").size(12).font(Font { weight: font::Weight::Bold, ..Font::default() }).color(theme_gray).width(Length::FillPortion(2)),
            ]
        ]
        .spacing(6);

        for p in &self.active_positions {
            let pnl_color = if p.pnl >= 0.0 { theme_text_teal } else { theme_text_pink };
            pos_view = pos_view.push(
                row![
                    text(&p.symbol).size(13).font(Font { weight: font::Weight::Bold, ..Font::default() }).width(Length::FillPortion(2)),
                    text(format!("{:.3}", p.size)).size(13).width(Length::FillPortion(2)),
                    text(format!("${:.2}", p.pnl)).size(13).font(Font { weight: font::Weight::Bold, ..Font::default() }).color(pnl_color).width(Length::FillPortion(2)),
                ]
            );
        }

        let amount_input = text_input("0.1", &self.manual_amount)
            .on_input(Message::AmountChanged)
            .padding(8)
            .style(move |_theme, status| text_input::Style {
                background: iced::Background::Color(Color::from_rgb8(15, 15, 20)),
                border: iced::Border {
                    color: match status {
                        iced::widget::text_input::Status::Focused { .. } => theme_text_teal,
                        iced::widget::text_input::Status::Hovered => Color::from_rgb8(70, 70, 75),
                        _ => Color::from_rgb8(45, 45, 50),
                    },
                    width: 1.0,
                    radius: 4.0.into(),
                },
                placeholder: Color::from_rgb8(100, 100, 105),
                value: Color::WHITE,
                selection: Color::from_rgba8(0, 240, 200, 0.2),
                icon: Color::from_rgb8(150, 150, 150),
            });

        let price_input = text_input("Limit Price (Optional)", &self.manual_price)
            .on_input(Message::PriceChanged)
            .padding(8);

        let buy_btn = button(
            text("BUY / LONG")
                .size(14)
                .font(Font { weight: font::Weight::Bold, ..Font::default() })
                .color(Color::BLACK)
        )
        .style(move |_, status| button::Style {
            background: match status {
                iced::widget::button::Status::Hovered => Some(iced::Background::Color(Color::from_rgb8(0, 200, 160))),
                _ => Some(iced::Background::Color(theme_text_teal)),
            },
            text_color: Color::BLACK,
            border: iced::Border {
                color: theme_text_teal,
                width: 1.0,
                radius: 4.0.into(),
            },
            ..Default::default()
        })
        .on_press(Message::BuyClicked)
        .padding(10);

        let sell_btn = button(
            text("SELL / SHORT")
                .size(14)
                .font(Font { weight: font::Weight::Bold, ..Font::default() })
                .color(Color::WHITE)
        )
        .style(move |_, status| button::Style {
            background: match status {
                iced::widget::button::Status::Hovered => Some(iced::Background::Color(Color::from_rgb8(220, 40, 100))),
                _ => Some(iced::Background::Color(theme_text_pink)),
            },
            text_color: Color::WHITE,
            border: iced::Border {
                color: theme_text_pink,
                width: 1.0,
                radius: 4.0.into(),
            },
            ..Default::default()
        })
        .on_press(Message::SellClicked)
        .padding(10);

        let quick_execute = column![
            text("MANUAL EXECUTION CONSOLE").size(14).font(Font { weight: font::Weight::Bold, ..Font::default() }),
            row![
                text("SIZE (BTC): ").size(13).color(theme_gray).width(Length::Fixed(80.0)),
                amount_input.width(Length::Fill)
            ].align_y(iced::Alignment::Center),
            row![
                text("PRICE (USD): ").size(13).color(theme_gray).width(Length::Fixed(80.0)),
                price_input.width(Length::Fill)
            ].align_y(iced::Alignment::Center),
            row![
                buy_btn.width(Length::Fill),
                sell_btn.width(Length::Fill)
            ].spacing(10)
        ]
        .spacing(12);

        // Engine settings & buttons
        let symbol_input = text_input("Symbol (e.g. BTCUSDT)", &self.engine_symbol)
            .on_input(Message::EngineSymbolChanged)
            .padding(6);

        // Cyberpunk strategy chips
        let strategies = vec!["rsi", "market_maker", "momentum", "arbitrage", "inference"];
        let mut strategy_row = row![].spacing(6);
        for strat in strategies {
            let is_selected = self.engine_strategy == strat;
            let strat_label = match strat {
                "rsi" => "RSI",
                "market_maker" => "Maker",
                "momentum" => "Mom",
                "arbitrage" => "Arb",
                "inference" => "ML",
                _ => strat,
            };
            
            let mut btn = button(text(strat_label).size(12).font(Font { weight: font::Weight::Bold, ..Font::default() }));
            
            if is_selected {
                btn = btn.style(move |_theme, _status| button::Style {
                    background: Some(iced::Background::Color(theme_text_teal)),
                    text_color: Color::BLACK,
                    border: iced::Border {
                        color: theme_text_teal,
                        width: 1.0,
                        radius: 4.0.into(),
                    },
                    shadow: iced::Shadow::default(),
                    ..Default::default()
                });
            } else {
                btn = btn.style(move |_theme, _status| button::Style {
                    background: Some(iced::Background::Color(Color::from_rgb8(30, 30, 35))),
                    text_color: theme_gray,
                    border: iced::Border {
                        color: Color::from_rgb8(50, 50, 55),
                        width: 1.0,
                        radius: 4.0.into(),
                    },
                    shadow: iced::Shadow::default(),
                    ..Default::default()
                })
                .on_press(Message::EngineStrategyChanged(strat.to_string()));
            }
            strategy_row = strategy_row.push(btn.padding(6));
        }

        // Cyberpunk Paper / Live Toggles
        let paper_btn = button(text("PAPER").size(12).font(Font { weight: font::Weight::Bold, ..Font::default() }))
            .padding(8);
        let live_btn = button(text("LIVE").size(12).font(Font { weight: font::Weight::Bold, ..Font::default() }))
            .padding(8);
            
        let (paper_btn, live_btn) = if self.engine_paper {
            (
                paper_btn.style(move |_, _| button::Style {
                    background: Some(iced::Background::Color(theme_text_teal)),
                    text_color: Color::BLACK,
                    border: iced::Border { color: theme_text_teal, width: 1.0, radius: 4.0.into() },
                    ..Default::default()
                }),
                live_btn.style(move |_, _| button::Style {
                    background: Some(iced::Background::Color(Color::from_rgb8(30, 30, 35))),
                    text_color: theme_gray,
                    border: iced::Border { color: Color::from_rgb8(50, 50, 55), width: 1.0, radius: 4.0.into() },
                    ..Default::default()
                })
                .on_press(Message::EnginePaperToggled)
            )
        } else {
            (
                paper_btn.style(move |_, _| button::Style {
                    background: Some(iced::Background::Color(Color::from_rgb8(30, 30, 35))),
                    text_color: theme_gray,
                    border: iced::Border { color: Color::from_rgb8(50, 50, 55), width: 1.0, radius: 4.0.into() },
                    ..Default::default()
                })
                .on_press(Message::EnginePaperToggled),
                live_btn.style(move |_, _| button::Style {
                    background: Some(iced::Background::Color(theme_text_pink)),
                    text_color: Color::WHITE,
                    border: iced::Border { color: theme_text_pink, width: 1.0, radius: 4.0.into() },
                    ..Default::default()
                })
                .on_press(Message::EnginePaperToggled)
            )
        };

        // Launch Action Button (START / STOP)
        let launch_btn = if self.engine_running {
            button(text("STOP ENGINE").size(14).font(Font { weight: font::Weight::Bold, ..Font::default() }).color(Color::WHITE))
                .style(move |_, _| button::Style {
                    background: Some(iced::Background::Color(theme_text_pink)),
                    text_color: Color::WHITE,
                    border: iced::Border { color: theme_text_pink, width: 1.0, radius: 6.0.into() },
                    ..Default::default()
                })
                .on_press(Message::StopEngine)
        } else {
            button(text("START ENGINE").size(14).font(Font { weight: font::Weight::Bold, ..Font::default() }).color(Color::BLACK))
                .style(move |_, _| button::Style {
                    background: Some(iced::Background::Color(theme_text_teal)),
                    text_color: Color::BLACK,
                    border: iced::Border { color: theme_text_teal, width: 1.0, radius: 6.0.into() },
                    ..Default::default()
                })
                .on_press(Message::StartEngine)
        };

        let engine_console = column![
            text("ENGINE SYSTEM LAUNCHER").size(14).font(Font { weight: font::Weight::Bold, ..Font::default() }),
            row![
                text("SYMBOL: ").size(13).color(theme_gray).width(Length::Fixed(60.0)),
                symbol_input.width(Length::Fill)
            ].align_y(iced::Alignment::Center),
            column![
                text("STRATEGY: ").size(13).color(theme_gray),
                strategy_row.width(Length::Fill)
            ].spacing(5),
            row![
                text("MODE: ").size(13).color(theme_gray).width(Length::Fixed(60.0)),
                row![paper_btn.width(Length::FillPortion(1)), live_btn.width(Length::FillPortion(1))].spacing(10).width(Length::Fill)
            ].align_y(iced::Alignment::Center),
            launch_btn.width(Length::Fill).padding(10)
        ]
        .spacing(12);

        let kill_switch_btn = button(
            text("EMERGENCY KILL SWITCH")
                .size(16)
                .font(Font { weight: font::Weight::Bold, ..Font::default() })
                .color(Color::WHITE)
        )
        .style(move |_, status| button::Style {
            background: match status {
                iced::widget::button::Status::Hovered => Some(iced::Background::Color(Color::from_rgb8(255, 20, 20))),
                _ => Some(iced::Background::Color(Color::from_rgb8(180, 0, 0))),
            },
            text_color: Color::WHITE,
            border: iced::Border {
                color: Color::from_rgb8(255, 60, 60),
                width: 2.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        })
        .on_press(Message::KillSwitchClicked)
        .padding(15);

        let right_panel = container(
            column![
                text("INVENTORY POSITIONS").size(15).font(Font { weight: font::Weight::Bold, ..Font::default() }),
                pos_view.height(Length::FillPortion(1)),
                engine_console.height(Length::FillPortion(3)),
                quick_execute.height(Length::FillPortion(3)),
                kill_switch_btn.width(Length::Fill),
            ]
            .spacing(15)
        )
        .style(move |_theme| container::Style {
            background: Some(iced::Background::Color(Color::from_rgb8(20, 20, 25))),
            border: iced::Border {
                color: Color::from_rgb8(35, 35, 40),
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        })
        .padding(15)
        .width(Length::FillPortion(1))
        .height(Length::Fill);

        // 3. BOTTOM FEEDBACK LOG LINE
        let footer = container(
            row![
                text("LOG:")
                    .size(12)
                    .font(Font { weight: font::Weight::Bold, ..Font::default() })
                    .color(theme_gray),
                text(&self.status_message)
                    .size(13)
                    .font(Font { weight: font::Weight::Bold, ..Font::default() })
                    .color(Color::WHITE),
            ]
            .spacing(10)
        )
        .style(move |_theme| container::Style {
            background: Some(iced::Background::Color(Color::from_rgb8(10, 10, 12))),
            border: iced::Border {
                color: Color::from_rgb8(25, 25, 30),
                width: 1.0,
                radius: 4.0.into(),
            },
            ..Default::default()
        })
        .padding(10)
        .width(Length::Fill);

        // Assemble columns and return
        let view_content = column![
            header,
            row![left_panel, middle_panel, right_panel]
                .spacing(15)
                .padding(10)
                .height(Length::Fill),
            footer
        ]
        .spacing(5);

        container(view_content)
            .style(move |_theme| container::Style {
                background: Some(iced::Background::Color(Color::from_rgb8(10, 10, 12))),
                ..Default::default()
            })
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}
