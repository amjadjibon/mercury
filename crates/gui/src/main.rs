mod app;
mod widgets;

use app::MercuryApp;

fn main() -> iced::Result {
    iced::application(MercuryApp::new, MercuryApp::update, MercuryApp::view)
        .title("MERCURY DESKTOP HFT")
        .window(iced::window::Settings {
            size: iced::Size::new(1280.0, 800.0),
            resizable: true,
            decorations: true,
            ..Default::default()
        })
        .theme(MercuryApp::theme)
        .subscription(MercuryApp::subscription)
        .run()
}
