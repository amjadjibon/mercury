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

pub struct DepthWidget<'a> {
    pub bids: &'a [mercury_core::Level],
    pub asks: &'a [mercury_core::Level],
    pub max_depth: f64,
}

impl<'a> DepthWidget<'a> {
    pub fn new(bids: &'a [mercury_core::Level], asks: &'a [mercury_core::Level]) -> Self {
        // Find max density for scaling
        let max_bid = bids
            .iter()
            .map(|l| l.quantity.to_string().parse::<f64>().unwrap_or(0.0))
            .fold(0.0, f64::max);
        let max_ask = asks
            .iter()
            .map(|l| l.quantity.to_string().parse::<f64>().unwrap_or(0.0))
            .fold(0.0, f64::max);

        Self {
            bids,
            asks,
            max_depth: max_bid.max(max_ask).max(1.0),
        }
    }

    pub fn render(&self) -> ratatui::widgets::BarChart<'_> {
        // Create a simple histogram representation
        // For TUI limitation, we use BarChart or Sparkline. BarChart is better for categorized data.
        // But BarChart takes discrete bars. Let's simplify and just return a usage description since
        // full depth visualization in terminal needs more complex canvas implementation.
        // Actually, let's implement a sparkline-like view using block characters if we had more time.
        // For now, let's use a BarChart to show spread distribution.

        ratatui::widgets::BarChart::default()
            .block(
                Block::default()
                    .title("Depth Distribution")
                    .borders(Borders::ALL),
            )
            .data(&[
                (
                    "Best Bid",
                    (self
                        .bids
                        .first()
                        .map(|l| l.quantity.to_string().parse::<f64>().unwrap_or(0.0))
                        .unwrap_or(0.0)
                        * 100.0) as u64,
                ),
                (
                    "Best Ask",
                    (self
                        .asks
                        .first()
                        .map(|l| l.quantity.to_string().parse::<f64>().unwrap_or(0.0))
                        .unwrap_or(0.0)
                        * 100.0) as u64,
                ),
            ])
            .bar_width(10)
            .bar_gap(2)
            .max((self.max_depth * 100.0) as u64)
    }
}

pub struct TradeLogWidget<'a> {
    pub trades: &'a [String],
}

impl<'a> TradeLogWidget<'a> {
    pub fn new(trades: &'a [String]) -> Self {
        Self { trades }
    }

    pub fn render(&self) -> ratatui::widgets::List<'_> {
        let items: Vec<ratatui::widgets::ListItem> = self
            .trades
            .iter()
            .rev() // Show newest first
            .take(20)
            .map(|t| ratatui::widgets::ListItem::new(Span::raw(t)))
            .collect();

        ratatui::widgets::List::new(items)
            .block(Block::default().title("Trade Log").borders(Borders::ALL))
    }
}
