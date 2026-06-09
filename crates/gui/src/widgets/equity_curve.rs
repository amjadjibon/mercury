//! GPU-accelerated Equity Curve vector canvas widget.

use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke, Fill, Text};
use iced::{Rectangle, Point, Color, Theme, Renderer};

/// GPU-accelerated Equity Curve canvas program.
pub struct EquityCurve {
    pub equity_series: Vec<f64>,
}

impl<Message> canvas::Program<Message> for EquityCurve {
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

        // Dark obsidian background
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

        // Draw vertical grid lines (e.g. at 25%, 50%, 75% width)
        for i in 1..4 {
            let x = bounds.width * (i as f32) / 4.0;
            let line = Path::new(|builder| {
                builder.move_to(Point::new(x, 0.0));
                builder.line_to(Point::new(x, bounds.height));
            });
            frame.stroke(&line, grid_stroke);
        }

        if self.equity_series.len() < 2 {
            // Display empty message
            frame.fill_text(Text {
                content: "No historical trade fills to display equity curve".to_string(),
                position: Point::new(bounds.width / 2.0, bounds.height / 2.0),
                color: Color::from_rgba8(255, 255, 255, 0.3),
                size: 14.0.into(),
                align_x: iced::alignment::Horizontal::Center.into(),
                align_y: iced::alignment::Vertical::Center,
                ..Default::default()
            });
            return vec![frame.into_geometry()];
        }

        let n = self.equity_series.len();

        // Find min/max values to scale
        let mut min_val = self.equity_series[0];
        let mut max_val = self.equity_series[0];
        for &val in &self.equity_series {
            if val < min_val {
                min_val = val;
            }
            if val > max_val {
                max_val = val;
            }
        }

        let raw_range = max_val - min_val;
        let range = if raw_range <= 0.0 { 1000.0 } else { raw_range };

        // 15% top/bottom padding
        let display_min = min_val - range * 0.15;
        let display_max = max_val + range * 0.15;
        let display_range = display_max - display_min;

        let step_x = bounds.width / (n - 1) as f32;

        let scale_y = |val: f64| -> f32 {
            let ratio = (val - display_min) / display_range;
            bounds.height - 15.0 - (ratio as f32 * (bounds.height - 30.0))
        };

        // Precompute peak running series for drawdown tracking
        let mut peak_series = Vec::with_capacity(n);
        let mut current_peak = self.equity_series[0];
        for &val in &self.equity_series {
            if val > current_peak {
                current_peak = val;
            }
            peak_series.push(current_peak);
        }

        // Draw Drawdown Shaded Region (Neon Pink with low opacity)
        let dd_path = Path::new(|builder| {
            // Traverse from left to right along the peak curve
            builder.move_to(Point::new(0.0, scale_y(peak_series[0])));
            for (idx, &peak) in peak_series.iter().enumerate().skip(1) {
                builder.line_to(Point::new(idx as f32 * step_x, scale_y(peak)));
            }
            // Traverse from right to left along the actual equity curve
            for (idx, &eq) in self.equity_series.iter().enumerate().rev() {
                builder.line_to(Point::new(idx as f32 * step_x, scale_y(eq)));
            }
            builder.close();
        });

        frame.fill(
            &dd_path,
            Fill {
                style: canvas::Style::Solid(Color::from_rgba8(255, 60, 120, 0.12)), // 12% opacity Neon Pink
                ..Default::default()
            },
        );

        // Draw Peak line (dim dotted/dashed effect or thin line)
        let peak_path = Path::new(|builder| {
            builder.move_to(Point::new(0.0, scale_y(peak_series[0])));
            for (idx, &peak) in peak_series.iter().enumerate().skip(1) {
                builder.line_to(Point::new(idx as f32 * step_x, scale_y(peak)));
            }
        });
        frame.stroke(
            &peak_path,
            Stroke::default()
                .with_color(Color::from_rgba8(255, 255, 255, 0.15))
                .with_width(1.0),
        );

        // Draw Main Equity Curve (Neon Teal)
        let curve_path = Path::new(|builder| {
            builder.move_to(Point::new(0.0, scale_y(self.equity_series[0])));
            for (idx, &eq) in self.equity_series.iter().enumerate().skip(1) {
                builder.line_to(Point::new(idx as f32 * step_x, scale_y(eq)));
            }
        });
        frame.stroke(
            &curve_path,
            Stroke::default()
                .with_color(Color::from_rgb8(0, 240, 200)) // Neon Teal
                .with_width(2.0),
        );

        // Draw scale tick labels on vertical margin
        let text_size = 9.0;

        // Max peak equity label
        frame.fill_text(Text {
            content: format!("max: ${:.2}", max_val),
            position: Point::new(10.0, 12.0),
            color: Color::from_rgb8(0, 240, 200), // Neon Teal
            size: text_size.into(),
            align_x: iced::alignment::Horizontal::Left.into(),
            align_y: iced::alignment::Vertical::Center,
            ..Default::default()
        });

        // Current/final equity label
        let final_val = self.equity_series[n - 1];
        frame.fill_text(Text {
            content: format!("final: ${:.2}", final_val),
            position: Point::new(bounds.width - 10.0, scale_y(final_val) - 10.0),
            color: Color::from_rgb8(255, 255, 255),
            size: text_size.into(),
            align_x: iced::alignment::Horizontal::Right.into(),
            align_y: iced::alignment::Vertical::Bottom,
            ..Default::default()
        });

        // Min equity label
        frame.fill_text(Text {
            content: format!("min: ${:.2}", min_val),
            position: Point::new(10.0, bounds.height - 12.0),
            color: Color::from_rgb8(255, 60, 120), // Neon Pink
            size: text_size.into(),
            align_x: iced::alignment::Horizontal::Left.into(),
            align_y: iced::alignment::Vertical::Center,
            ..Default::default()
        });

        vec![frame.into_geometry()]
    }
}
