use ratatui::{
    style::{Color, Style},
    symbols::Marker,
    text::Span,
    widgets::{Axis, Block, Borders, Chart, Dataset, GraphType},
};

pub struct ChartWidget<'a> {
    pub data: &'a [(f64, f64)],
    pub title: &'a str,
    pub x_label: &'a str,
    pub y_label: &'a str,
    pub color: Color,
}

impl<'a> ChartWidget<'a> {
    pub fn new(data: &'a [(f64, f64)], title: &'a str, color: Color) -> Self {
        Self {
            data,
            title,
            x_label: "Time",
            y_label: "Price",
            color,
        }
    }

    pub fn render(&self) -> Chart<'_> {
        let datasets = vec![
            Dataset::default()
                .name(self.title)
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(self.color))
                .data(self.data),
        ];

        // determine bounds
        let y_min = self
            .data
            .iter()
            .map(|(_, y)| *y)
            .fold(f64::INFINITY, f64::min);
        let y_max = self
            .data
            .iter()
            .map(|(_, y)| *y)
            .fold(f64::NEG_INFINITY, f64::max);

        let x_min = self.data.first().map(|(x, _)| *x).unwrap_or(0.0);
        let x_max = self.data.last().map(|(x, _)| *x).unwrap_or(100.0);

        Chart::new(datasets)
            .block(Block::default().title(self.title).borders(Borders::ALL))
            .x_axis(
                Axis::default()
                    .title(self.x_label)
                    .style(Style::default().fg(Color::Gray))
                    .bounds([x_min, x_max])
                    .labels(vec![
                        Span::styled(
                            format!("{:.1}", x_min),
                            Style::default().add_modifier(ratatui::style::Modifier::BOLD),
                        ),
                        Span::styled(
                            format!("{:.1}", x_max),
                            Style::default().add_modifier(ratatui::style::Modifier::BOLD),
                        ),
                    ]),
            )
            .y_axis(
                Axis::default()
                    .title(self.y_label)
                    .style(Style::default().fg(Color::Gray))
                    .bounds([y_min * 0.999, y_max * 1.001]) // dynamic scaling
                    .labels(vec![
                        Span::styled(
                            format!("{:.2}", y_min),
                            Style::default().add_modifier(ratatui::style::Modifier::BOLD),
                        ),
                        Span::styled(
                            format!("{:.2}", y_max),
                            Style::default().add_modifier(ratatui::style::Modifier::BOLD),
                        ),
                    ]),
            )
    }
}

pub struct DepthWidget {
    bid_points: Vec<(f64, f64)>,
    ask_points: Vec<(f64, f64)>,
    x_min: f64,
    x_max: f64,
    y_max: f64,
}

impl DepthWidget {
    pub fn new(bids: &[mercury_core::Level], asks: &[mercury_core::Level]) -> Self {
        use rust_decimal::prelude::ToPrimitive;

        let mut bid_points: Vec<(f64, f64)> = Vec::with_capacity(bids.len());
        let mut cum: f64 = 0.0;
        for level in bids.iter() {
            if let (Some(p), Some(q)) = (level.price.to_f64(), level.quantity.to_f64()) {
                cum += q;
                bid_points.push((p, cum));
            }
        }
        // Bids arrive high→low; reverse so the line plots left-to-right
        bid_points.reverse();

        let mut ask_points: Vec<(f64, f64)> = Vec::with_capacity(asks.len());
        cum = 0.0;
        for level in asks.iter() {
            if let (Some(p), Some(q)) = (level.price.to_f64(), level.quantity.to_f64()) {
                cum += q;
                ask_points.push((p, cum));
            }
        }

        let x_min = bid_points.first().map(|(p, _)| *p).unwrap_or(0.0);
        let x_max = ask_points.last().map(|(p, _)| *p).unwrap_or(1.0);
        let y_max = bid_points
            .iter()
            .chain(ask_points.iter())
            .map(|(_, q)| *q)
            .fold(0.0_f64, f64::max)
            .max(1.0);

        Self { bid_points, ask_points, x_min, x_max, y_max }
    }

    pub fn render(&self) -> Chart<'_> {
        let datasets = vec![
            Dataset::default()
                .name("Bids")
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(Color::Green))
                .data(&self.bid_points),
            Dataset::default()
                .name("Asks")
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(Color::Red))
                .data(&self.ask_points),
        ];

        let x_min = self.x_min;
        let x_max = self.x_max;
        let y_max = self.y_max;

        Chart::new(datasets)
            .block(Block::default().title("Depth").borders(Borders::ALL))
            .x_axis(
                Axis::default()
                    .style(Style::default().fg(Color::Gray))
                    .bounds([x_min * 0.999, x_max * 1.001])
                    .labels(vec![
                        Span::styled(
                            format!("{:.0}", x_min),
                            Style::default().add_modifier(ratatui::style::Modifier::BOLD),
                        ),
                        Span::styled(
                            format!("{:.0}", x_max),
                            Style::default().add_modifier(ratatui::style::Modifier::BOLD),
                        ),
                    ]),
            )
            .y_axis(
                Axis::default()
                    .title("Cum Qty")
                    .style(Style::default().fg(Color::Gray))
                    .bounds([0.0, y_max * 1.05])
                    .labels(vec![
                        Span::raw("0"),
                        Span::styled(
                            format!("{:.2}", y_max),
                            Style::default().add_modifier(ratatui::style::Modifier::BOLD),
                        ),
                    ]),
            )
    }
}

