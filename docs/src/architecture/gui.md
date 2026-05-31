# Desktop GUI

Mercury features a premium, GPU-accelerated native desktop trading console built on the modern, functional APIs of **Iced v0.14.0**.

The GUI is themed around a highly polished **Obsidian Cyberpunk** palette, designed to deliver a gorgeous visual flow matching professional institutional trading terminals (like Bloomberg and TradingView):

*   **Deep Obsidian Black** (`[10, 10, 12]` / `[20, 20, 25]`): Solid, high-depth backgrounds.
*   **Neon Teal** (`[0, 240, 200]`): Highlight color for buying, live status, inventory increases, and active input highlights.
*   **Neon Pink/Coral** (`[255, 60, 120]`): Highlight color for selling, offline state, and risk alerts.
*   **Slate Gray** (`[150, 150, 155]`): Muted color for secondary indicators, grid lines, and subtitles.

---

## Architecture Overview

The GUI operates on a **non-blocking asynchronous design** conforming to the Elm architecture (State, Subscription, Update, View) without introducing heap-allocation overheads to the trading hot-path. 

```text
       Desktop GUI (Iced)
        ├── Spawner Task ──►tokio::process::Child (Engine process)
        │                         │ (pipes stdout/stderr)
        │                         ▼
        │                  System Logs Terminal Panel
        │
        └── Telemetry Stream ◄──Unix Socket (/tmp/mercury.sock)
                                  │ (receives L2 snapshots & latencies)
                                  ▼
                           - Depth Map Canvas
                           - Sparkline Canvas
                           - Header Metrics Card Deck
```

The GUI consists of three main architectural subsystems:

### 1. Detached Subprocess Spawner & Log Piping
To provide a comprehensive control console, the GUI allows starting and stopping the CLI trading engine directly:
*   **State Safety**: Storing child process streams directly in Iced state structs breaks Rust's compilation because the handles are not `Sync`. To circumvent this, Mercury registers a thread-safe static communication channel (`LOG_TX: Mutex<Option<mpsc::UnboundedSender<Message>>>`) mapped to an unfolding asynchronous subscription stream.
*   **stdout/stderr Piping**: Spawns `mercury` executable (resolved dynamically via `std::env::current_exe()`) in the background. Standard output and errors are captured, routed through a non-blocking `tokio::select!` block, and piped into the scrolling **System Logs** terminal panel.
*   **Graceful Shutdown**: Sends a termination trigger via a oneshot channel to safely kill (`child.kill()`) and wait on (`child.wait()`) the background task, returning the dashboard to offline state.

### 2. Unix Socket Telemetry Stream
Upon launching, the GUI automatically begins subscribing to the Unix domain socket `/tmp/mercury.sock` published by the engine's `IpcServer`. An auto-reconnecting stream reads serialized JSON events, unpacking:
*   **`BookUpdate`**: High-frequency order book depth layers.
*   **`Trade`**: Active fills matching on the matching engine.
*   **`LatencyReport`**: Latency calculations showing fast packet delivery statistics.

### 3. GPU-Accelerated Graphic Canvases
The L2 Liquidity Wall and Telemetry Sparklines are drawn inside Iced's `canvas::Program` widgets, which leverage GPU acceleration for instantaneous redraws on every tick:
*   **L2 Depth Map (`depth_map.rs`)**: Renders cumulative bid (Teal) and ask (Pink) volume walls. It maps price ranges (`min_price`, `mid_price`, `max_price`) and volume bars mathematically to dynamic grid ticks along the margins.
*   **Telemetry Sparkline (`sparkline.rs`)**: Tracks real-time p50 latency historical movements with a glowing line chart, calculating and presenting latency markers (e.g. `max: 18.5 μs`, `mid`, `min`) on the vertical margin.

---

## Visual Control Features

The dashboard layout partitions into three specialized control zones:

### 1. Top Card Ticker Deck
An obsidian-style deck composed of individual sub-cards with thin borders and subtle padding:
*   **Branding Badge**: Displaying `"MERCURY"` in Neon Teal and subtitle `"HFT ENGINE v1.0.0"`.
*   **Active Symbol Card**: Keeps active currency focus (e.g., `BTCUSDT`).
*   **System Status Card**: Real-time engine health using a dynamic green-teal or pink-red dot indicator and active label.
*   **Net Position Exposure**: Instantly sums the net position size for the current symbol and color-codes the badge (Neon Teal for positive inventory, Neon Pink for short positions, Gray for neutral).
*   **Unrealized & Active PnL Card**: Displays the live dollar-value strategy PnL, highlighting positive or negative returns with dynamic HFT color schemes.
*   **Bid/Ask Spread Ticker**: Compares `best_bid` and `best_ask` in real-time, displaying the absolute and percentage-based bid-ask spread dynamically.

### 2. Quick Execution & Launch Console
Located in the right panel:
*   **Engine Launcher**: Type the target symbol, select strategy tabs ("RSI", "Maker", "Mom", "Arb", "ML"), select Paper or Live, and click **START ENGINE**. The action button glows vibrant Neon Teal (Start) or Neon Pink (Stop) on hover.
*   **Manual Overrides Console**: Allows setting manual execution quantities and custom limit prices (with focused teal highlight borders) to place urgent **BUY / LONG** or **SELL / SHORT** orders.
*   **Emergency Kill Switch**: A prominent red button that calls the risk engine to cancel all outstanding orders and immediately flatten active positions.

### 3. Middle Pane Tabs
Traders can swap content between:
*   **Price Chart**: A high-end OHLCV Candlestick chart featuring real-time trade aggregation, bottom volume bars, dynamic right price tick scales, and transaction execution marks mapped as diamonds.
*   **Matching Prints**: Live feed table showing historical ticks, fill times, buy/sell flags, and filled quantities.
*   **System Logs**: A color-coded terminal viewport displaying real-time stdout/stderr lines from the running subprocess, highlighted for warnings and system updates.

---

## Compiling & Running

Ensure you have the required graphics libraries on your OS (Vulkan, Metal, or DX12 compatible GPU drivers).

```bash
# Build and execute the GUI terminal
cargo run -p mercury-gui
```
