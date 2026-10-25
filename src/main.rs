#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod device;
mod tray;
mod ui;

fn main() {
    ui::run();
}
