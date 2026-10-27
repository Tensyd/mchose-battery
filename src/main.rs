#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod device;
mod diagnose;
mod tray;
mod ui;

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();

    // `--diagnose` writes a passive interface report instead of starting the tray.
    if arguments.iter().any(|argument| argument == "--diagnose") {
        diagnose::run(arguments.iter().any(|argument| argument == "--probe"));
        return;
    }

    // `--silent` is what the autostart entry passes: start in the tray, no window.
    let start_hidden = arguments.iter().any(|argument| argument == "--silent");
    ui::run(start_hidden);
}
