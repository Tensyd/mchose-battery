#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod device;
mod diagnose;
mod tray;
mod ui;

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowW, SW_SHOW, SetForegroundWindow, ShowWindow,
};

const WINDOW_CLASS: &str = "MchoseBatteryWindow";

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();

    // `--diagnose` writes a passive interface report instead of starting the tray.
    if arguments.iter().any(|argument| argument == "--diagnose") {
        diagnose::run(arguments.iter().any(|argument| argument == "--probe"));
        return;
    }

    // The tray icon and the autostart switch both assume exactly one process: two copies
    // mean two icons, doubled device traffic, and switches that disagree with each other.
    if !claim_single_instance() {
        return;
    }

    // `--silent` is what the autostart entry passes: start in the tray, no window.
    let start_hidden = arguments.iter().any(|argument| argument == "--silent");
    ui::run(start_hidden);
}

/// Claims the per-session single-instance mutex. Returns false when another copy already
/// holds it, after bringing that copy's window up so the click does something visible.
fn claim_single_instance() -> bool {
    let name = wide(r"Local\MCBattery.Tray");

    unsafe {
        let mutex = CreateMutexW(std::ptr::null(), 1, name.as_ptr());
        if mutex.is_null() {
            // Failing open beats refusing to start if the mutex cannot be created.
            return true;
        }

        if GetLastError() == ERROR_ALREADY_EXISTS {
            focus_existing_window();
            return false;
        }

        // The handle is deliberately never closed: the mutex has to stay held for the whole
        // process lifetime, and the OS releases it on exit.
        true
    }
}

fn focus_existing_window() {
    let class = wide(WINDOW_CLASS);
    unsafe {
        let hwnd = FindWindowW(class.as_ptr(), std::ptr::null());
        if hwnd.is_null() {
            // The other copy is still starting up; nothing to raise yet.
            return;
        }
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
    }
}

fn wide(text: &str) -> Vec<u16> {
    OsStr::new(text)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}
