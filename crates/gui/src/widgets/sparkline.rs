use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke, Fill};
use iced::{Rectangle, Point, Color, Theme, Renderer};
use std::collections::VecDeque;

/// GPU-accelerated Telemetry Sparkline canvas program.
pub struct Sparkline {
    pub points: VecDeque<f32>,
    pub line_color: Color,
}

impl<Message> canvas::Program<Message> for Sparkline {
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

        if self.points.len() < 2 {
            return vec![frame.into_geometry()];
        }

        // Find min/max values to scale
        let mut min_val = self.points[0];
        let mut max_val = self.points[0];

        for &val in &self.points {
            if val < min_val {
                min_val = val;
            }
            if val > max_val {
                max_val = val;
            }
        }

        let range = max_val - min_val;
        // Avoid flat lines division by zero
        let range = if range <= 0.0 { 1.0 } else { range };

        let num_points = self.points.len();
        let step_x = bounds.width / (num_points - 1) as f32;

        let scale_y = |val: f32| -> f32 {
            let ratio = (val - min_val) / range;
            // 15% top/bottom padding
            bounds.height - 5.0 - (ratio * (bounds.height - 10.0))
        };

        // Draw glowing line
        let path = Path::new(|builder| {
            builder.move_to(Point::new(0.0, scale_y(self.points[0])));

            for (idx, &val) in self.points.iter().enumerate().skip(1) {
                let px = (idx as f32) * step_x;
                let py = scale_y(val);
                builder.line_to(Point::new(px, py));
            }
        });

        // Main neon line
        frame.stroke(
            &path,
            Stroke::default()
                .with_color(self.line_color)
                .with_width(2.0),
        );

        // Optional filled area below line (translucent neon glow)
        let glow_path = Path::new(|builder| {
            builder.move_to(Point::new(0.0, bounds.height));
            builder.line_to(Point::new(0.0, scale_y(self.points[0])));

            for (idx, &val) in self.points.iter().enumerate().skip(1) {
                let px = (idx as f32) * step_x;
                let py = scale_y(val);
                builder.line_to(Point::new(px, py));
            }

            builder.line_to(Point::new(bounds.width, bounds.height));
            builder.close();
        });

        let mut glow_color = self.line_color;
        glow_color.a = 0.05; // 5% opacity

        frame.fill(
            &glow_path,
            Fill {
                style: canvas::Style::Solid(glow_color),
                ..Default::default()
            },
        );

        vec![frame.into_geometry()]
    }
}
