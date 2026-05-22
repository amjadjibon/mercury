use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke, Fill, Text};
use iced::{Rectangle, Point, Color, Theme, Renderer};


/// L2 Order Book Depth Map GPU Canvas widget.
pub struct DepthMap {
    pub bids: Vec<(f64, f64)>, // (price, quantity)
    pub asks: Vec<(f64, f64)>, // (price, quantity)
}

impl<Message> canvas::Program<Message> for DepthMap {
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

        // Background
        let bg_fill = Fill {
            style: canvas::Style::Solid(Color::from_rgb8(15, 15, 20)),
            ..Default::default()
        };
        frame.fill(&Path::rectangle(Point::ORIGIN, bounds.size()), bg_fill);

        // Draw horizontal grid lines (e.g. at 25%, 50%, 75% height)
        let grid_stroke = Stroke::default()
            .with_color(Color::from_rgba8(255, 255, 255, 0.05))
            .with_width(1.0);
        for i in 1..4 {
            let y = bounds.height * (i as f32) / 4.0;
            let line = Path::new(|builder| {
                builder.move_to(Point::new(0.0, y));
                builder.line_to(Point::new(bounds.width, y));
            });
            frame.stroke(&line, grid_stroke);
        }

        // Draw vertical grid lines (e.g. at 25%, 75% width)
        for i in &[0.25, 0.75] {
            let x = bounds.width * i;
            let line = Path::new(|builder| {
                builder.move_to(Point::new(x, 0.0));
                builder.line_to(Point::new(x, bounds.height));
            });
            frame.stroke(&line, grid_stroke);
        }

        if self.bids.is_empty() && self.asks.is_empty() {
            // Draw loading state message if empty
            return vec![frame.into_geometry()];
        }

        // 1. Calculate cumulative volumes
        let mut cum_bids: Vec<(f64, f64)> = Vec::new();
        let mut sum_bid = 0.0;
        for &(p, q) in &self.bids {
            sum_bid += q;
            cum_bids.push((p, sum_bid));
        }

        let mut cum_asks: Vec<(f64, f64)> = Vec::new();
        let mut sum_ask = 0.0;
        for &(p, q) in &self.asks {
            sum_ask += q;
            cum_asks.push((p, sum_ask));
        }

        // Find min/max prices and max cumulative volume for scaling
        let min_price = self.bids.last().map(|&(p, _)| p).unwrap_or(0.0);
        let max_price = self.asks.last().map(|&(p, _)| p).unwrap_or(1.0);
        let max_volume = sum_bid.max(sum_ask).max(1.0);

        let price_range = max_price - min_price;
        if price_range <= 0.0 {
            return vec![frame.into_geometry()];
        }

        let scale_x = |price: f64| -> f32 {
            let ratio = (price - min_price) / price_range;
            (ratio as f32) * bounds.width
        };

        let scale_y = |volume: f64| -> f32 {
            let ratio = volume / max_volume;
            // Leave 10% padding at top
            bounds.height - (ratio as f32) * (bounds.height * 0.9)
        };

        // Midpoint separation line
        let mid_price = if !self.bids.is_empty() && !self.asks.is_empty() {
            (self.bids[0].0 + self.asks[0].0) / 2.0
        } else {
            min_price + price_range / 2.0
        };
        let mid_x = scale_x(mid_price);

        // Draw Price labels along the bottom edge
        let price_color = Color::from_rgba8(255, 255, 255, 0.4);
        let price_size = 10.0;

        // Min price (left)
        frame.fill_text(Text {
            content: format!("{:.2}", min_price),
            position: Point::new(10.0, bounds.height - 18.0),
            color: price_color,
            size: price_size.into(),
            align_x: iced::alignment::Horizontal::Left.into(),
            align_y: iced::alignment::Vertical::Center.into(),
            ..Default::default()
        });

        // Mid price (center)
        frame.fill_text(Text {
            content: format!("Mid: {:.2}", mid_price),
            position: Point::new(mid_x, bounds.height - 18.0),
            color: price_color,
            size: price_size.into(),
            align_x: iced::alignment::Horizontal::Center.into(),
            align_y: iced::alignment::Vertical::Center.into(),
            ..Default::default()
        });

        // Max price (right)
        frame.fill_text(Text {
            content: format!("{:.2}", max_price),
            position: Point::new(bounds.width - 10.0, bounds.height - 18.0),
            color: price_color,
            size: price_size.into(),
            align_x: iced::alignment::Horizontal::Right.into(),
            align_y: iced::alignment::Vertical::Center.into(),
            ..Default::default()
        });

        // Draw Volume levels for horizontal grid lines
        for i in 1..4 {
            let y = bounds.height * (i as f32) / 4.0;
            // Calculate corresponding volume value at this height y
            let volume_at_y = max_volume * ((bounds.height - y) as f64) / (bounds.height as f64 * 0.9);
            if volume_at_y >= 0.0 {
                frame.fill_text(Text {
                    content: format!("{:.1}", volume_at_y),
                    position: Point::new(10.0, y - 8.0),
                    color: Color::from_rgba8(255, 255, 255, 0.25),
                    size: 9.0.into(),
                    align_x: iced::alignment::Horizontal::Left.into(),
                    align_y: iced::alignment::Vertical::Center.into(),
                    ..Default::default()
                });
            }
        }

        // Draw Spread label in upper middle
        if !self.bids.is_empty() && !self.asks.is_empty() {
            let spread = self.asks[0].0 - self.bids[0].0;
            frame.fill_text(Text {
                content: format!("Spread: {:.2}", spread),
                position: Point::new(mid_x, 15.0),
                color: Color::from_rgb8(255, 60, 120), // neon pink
                size: 11.0.into(),
                align_x: iced::alignment::Horizontal::Center.into(),
                align_y: iced::alignment::Vertical::Center.into(),
                ..Default::default()
            });
        }

        // 2. Draw Bid Depth (Left side - Neon Green/Teal)
        if !cum_bids.is_empty() {
            let bid_path = Path::new(|builder| {
                // Start at bottom of mid-price
                builder.move_to(Point::new(mid_x, bounds.height));
                
                // Plot cumulative volumes from highest bid (nearest mid) to lowest bid
                for &(p, v) in &cum_bids {
                    let px = scale_x(p);
                    let py = scale_y(v);
                    builder.line_to(Point::new(px, py));
                }
                
                // Complete polygon: down to bottom-left corner
                if let Some(&(p_last, _)) = cum_bids.last() {
                    builder.line_to(Point::new(scale_x(p_last), bounds.height));
                }
                builder.close();
            });

            // Filled green area (translucent)
            frame.fill(
                &bid_path,
                Fill {
                    style: canvas::Style::Solid(Color::from_rgba8(0, 240, 200, 0.15)),
                    ..Default::default()
                },
            );

            // Neon green stroke line on top
            let bid_line = Path::new(|builder| {
                if let Some(&(p_first, v_first)) = cum_bids.first() {
                    builder.move_to(Point::new(scale_x(p_first), scale_y(v_first)));
                }
                for &(p, v) in cum_bids.iter().skip(1) {
                    builder.line_to(Point::new(scale_x(p), scale_y(v)));
                }
            });
            frame.stroke(
                &bid_line,
                Stroke::default()
                    .with_color(Color::from_rgb8(0, 240, 200))
                    .with_width(2.0),
            );
        }

        // 3. Draw Ask Depth (Right side - Hot Coral/Pink)
        if !cum_asks.is_empty() {
            let ask_path = Path::new(|builder| {
                // Start at bottom of mid-price
                builder.move_to(Point::new(mid_x, bounds.height));
                
                // Plot cumulative volumes from lowest ask (nearest mid) to highest ask
                for &(p, v) in &cum_asks {
                    let px = scale_x(p);
                    let py = scale_y(v);
                    builder.line_to(Point::new(px, py));
                }
                
                // Complete polygon: down to bottom-right corner
                if let Some(&(p_last, _)) = cum_asks.last() {
                    builder.line_to(Point::new(scale_x(p_last), bounds.height));
                }
                builder.close();
            });

            // Filled pink/red area (translucent)
            frame.fill(
                &ask_path,
                Fill {
                    style: canvas::Style::Solid(Color::from_rgba8(255, 60, 120, 0.15)),
                    ..Default::default()
                },
            );

            // Neon pink/red stroke line on top
            let ask_line = Path::new(|builder| {
                if let Some(&(p_first, v_first)) = cum_asks.first() {
                    builder.move_to(Point::new(scale_x(p_first), scale_y(v_first)));
                }
                for &(p, v) in cum_asks.iter().skip(1) {
                    builder.line_to(Point::new(scale_x(p), scale_y(v)));
                }
            });
            frame.stroke(
                &ask_line,
                Stroke::default()
                    .with_color(Color::from_rgb8(255, 60, 120))
                    .with_width(2.0),
            );
        }

        // Midpoint dotted divider
        let mid_divider = Path::new(|builder| {
            builder.move_to(Point::new(mid_x, 0.0));
            builder.line_to(Point::new(mid_x, bounds.height));
        });
        frame.stroke(
            &mid_divider,
            Stroke::default()
                .with_color(Color::from_rgba8(255, 255, 255, 0.2))
                .with_width(1.0),
        );

        vec![frame.into_geometry()]
    }
}
