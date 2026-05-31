use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke, Fill, Text};
use iced::{Rectangle, Point, Color, Theme, Renderer};

/// Standard CandleBar data structure for OHLCV bars.
#[derive(Clone, Debug)]
pub struct CandleBar {
    pub time: i64, // Unix timestamp in seconds
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

/// Execution fill markers representation.
#[derive(Clone, Debug)]
pub struct ChartMark {
    pub price: f64,
    pub is_buy: bool,
    pub time: i64,
}

/// GPU-accelerated OHLCV Candlestick and volume canvas program.
pub struct Candlestick {
    pub candles: Vec<CandleBar>,
    pub marks: Vec<ChartMark>,
}

impl<Message> canvas::Program<Message> for Candlestick {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());

        // Define dimensions with a right pricing sidebar (60px) and bottom timeline bar (20px)
        let right_sidebar_width = 70.0;
        let bottom_bar_height = 20.0;
        
        let chart_width = bounds.width - right_sidebar_width;
        let chart_height = bounds.height - bottom_bar_height;

        // Background
        let bg_fill = Fill {
            style: canvas::Style::Solid(Color::from_rgb8(10, 10, 12)),
            ..Default::default()
        };
        frame.fill(&Path::rectangle(Point::ORIGIN, bounds.size()), bg_fill);

        // Sidebar Background
        let sidebar_fill = Fill {
            style: canvas::Style::Solid(Color::from_rgb8(15, 15, 18)),
            ..Default::default()
        };
        frame.fill(
            &Path::rectangle(Point::new(chart_width, 0.0), iced::Size::new(right_sidebar_width, bounds.height)),
            sidebar_fill,
        );

        // Check if we have candles to render
        if self.candles.is_empty() {
            // Render loading text
            frame.fill_text(Text {
                content: String::from("AWAITING MARKET DATA FEED..."),
                position: Point::new(chart_width / 2.0, chart_height / 2.0),
                color: Color::from_rgb8(100, 100, 105),
                size: 14.0.into(),
                align_x: iced::alignment::Horizontal::Center.into(),
                align_y: iced::alignment::Vertical::Center.into(),
                ..Default::default()
            });
            return vec![frame.into_geometry()];
        }

        // Find price range (high/low bounds)
        let mut min_price = self.candles[0].low;
        let mut max_price = self.candles[0].high;
        let mut max_vol = self.candles[0].volume;

        for candle in &self.candles {
            if candle.low < min_price {
                min_price = candle.low;
            }
            if candle.high > max_price {
                max_price = candle.high;
            }
            if candle.volume > max_vol {
                max_vol = candle.volume;
            }
        }

        // Padding on price scale (5% top/bottom)
        let price_range = max_price - min_price;
        let price_range = if price_range <= 0.0 { 1.0 } else { price_range };
        let padding = price_range * 0.05;
        let max_price_padded = max_price + padding;
        let min_price_padded = min_price - padding;
        let price_range_padded = max_price_padded - min_price_padded;

        // Scaling function
        let scale_y = |price: f64| -> f32 {
            let ratio = (price - min_price_padded) / price_range_padded;
            (chart_height - (ratio as f32 * chart_height)) as f32
        };

        // Draw horizontal grid lines and price tags
        let grid_stroke = Stroke::default()
            .with_color(Color::from_rgba8(255, 255, 255, 0.03))
            .with_width(1.0);
        let text_size = 9.0;
        let tick_color = Color::from_rgb8(120, 120, 125);

        let levels = 4; // Dividers
        for i in 0..=levels {
            let ratio = i as f64 / levels as f64;
            let price = min_price_padded + ratio * price_range_padded;
            let y = scale_y(price);

            // Horizontal Grid line
            let line = Path::new(|builder| {
                builder.move_to(Point::new(0.0, y));
                builder.line_to(Point::new(chart_width, y));
            });
            frame.stroke(&line, grid_stroke);

            // Sidebar Tick Label
            frame.fill_text(Text {
                content: format!("{:.2}", price),
                position: Point::new(chart_width + 8.0, y),
                color: tick_color,
                size: text_size.into(),
                align_x: iced::alignment::Horizontal::Left.into(),
                align_y: iced::alignment::Vertical::Center.into(),
                ..Default::default()
            });
        }

        // Render Candles
        let num_candles = self.candles.len();
        let step_x = chart_width / num_candles as f32;
        let candle_width = (step_x * 0.7).max(1.0);

        let theme_text_teal = Color::from_rgb8(0, 240, 200);
        let theme_text_pink = Color::from_rgb8(255, 60, 120);

        for (idx, candle) in self.candles.iter().enumerate() {
            let cx = (idx as f32) * step_x + (step_x / 2.0);
            
            let is_bullish = candle.close >= candle.open;
            let candle_color = if is_bullish { theme_text_teal } else { theme_text_pink };

            let y_high = scale_y(candle.high);
            let y_low = scale_y(candle.low);
            let y_open = scale_y(candle.open);
            let y_close = scale_y(candle.close);

            let y_body_top = y_open.min(y_close);
            let y_body_bottom = y_open.max(y_close);
            let body_height = (y_body_bottom - y_body_top).max(1.0);

            // Draw wick path
            let wick_path = Path::new(|builder| {
                builder.move_to(Point::new(cx, y_high));
                builder.line_to(Point::new(cx, y_low));
            });
            frame.stroke(
                &wick_path,
                Stroke::default()
                    .with_color(candle_color)
                    .with_width(1.0),
            );

            // Draw candle body
            let body_path = Path::rectangle(
                Point::new(cx - candle_width / 2.0, y_body_top),
                iced::Size::new(candle_width, body_height),
            );
            frame.fill(
                &body_path,
                Fill {
                    style: canvas::Style::Solid(candle_color),
                    ..Default::default()
                },
            );

            // Draw Volume Bar
            if max_vol > 0.0 {
                let vol_ratio = candle.volume / max_vol;
                let vol_height = (vol_ratio as f32 * (chart_height * 0.15)) as f32; // max 15% height
                let y_vol = chart_height - vol_height;

                let mut vol_color = candle_color;
                vol_color.a = 0.12; // Translucent outline

                let vol_path = Path::rectangle(
                    Point::new(cx - candle_width / 2.0, y_vol),
                    iced::Size::new(candle_width, vol_height),
                );
                frame.fill(
                    &vol_path,
                    Fill {
                        style: canvas::Style::Solid(vol_color),
                        ..Default::default()
                    },
                );
            }

            // Draw bottom timeline time codes (at interval intervals, e.g. every 10th candle)
            if idx % 10 == 0 || idx == num_candles - 1 {
                let hours = (candle.time / 3600) % 24;
                let minutes = (candle.time / 60) % 60;
                
                frame.fill_text(Text {
                    content: format!("{:02}:{:02}", hours, minutes),
                    position: Point::new(cx, chart_height + 8.0),
                    color: Color::from_rgb8(100, 100, 105),
                    size: text_size.into(),
                    align_x: iced::alignment::Horizontal::Center.into(),
                    align_y: iced::alignment::Vertical::Center.into(),
                    ..Default::default()
                });
            }
        }

        // Render Execution Marks (Diamond shapes)
        if !self.marks.is_empty() && num_candles > 1 {
            let min_time = self.candles.first().unwrap().time;
            let max_time = self.candles.last().unwrap().time;
            let time_range = max_time - min_time;

            if time_range > 0 {
                for mark in &self.marks {
                    if mark.time < min_time || mark.time > max_time {
                        continue;
                    }

                    // Linear interpolation of X position based on mark timestamp
                    let time_ratio = (mark.time - min_time) as f64 / time_range as f64;
                    let mx = (time_ratio as f32 * chart_width) as f32;
                    let my = scale_y(mark.price);

                    let mark_color = if mark.is_buy { theme_text_teal } else { theme_text_pink };
                    let size = 5.0;

                    // Draw Diamond Shape
                    let diamond_path = Path::new(|builder| {
                        builder.move_to(Point::new(mx, my - size));
                        builder.line_to(Point::new(mx + size, my));
                        builder.line_to(Point::new(mx, my + size));
                        builder.line_to(Point::new(mx - size, my));
                        builder.close();
                    });

                    // Fill diamond
                    frame.fill(
                        &diamond_path,
                        Fill {
                            style: canvas::Style::Solid(mark_color),
                            ..Default::default()
                        },
                    );

                    // Outline with high-contrast white border
                    frame.stroke(
                        &diamond_path,
                        Stroke::default()
                            .with_color(Color::WHITE)
                            .with_width(1.0),
                    );
                }
            }
        }

        vec![frame.into_geometry()]
    }
}
