#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod device;
mod diagnose;
mod tray;
mod ui;

fn main() {
    // `--diagnose` writes a passive interface report instead of starting the tray.
    if std::env::args().any(|argument| argument == "--diagnose") {
        diagnose::run();
        return;
    }

    ui::run();
}
