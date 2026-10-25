use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, BitBlt, CLEARTYPE_QUALITY,
    CLIP_DEFAULT_PRECIS, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, CreatePen,
    CreateSolidBrush, DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS, DeleteDC, DeleteObject,
    DrawTextW, EndPaint, FF_DONTCARE, FW_BOLD, FW_NORMAL, FillRect, HALFTONE, HDC, HFONT,
    InvalidateRect, LineTo, MoveToEx, OUT_DEFAULT_PRECIS, PAINTSTRUCT, PS_SOLID, RoundRect, SRCCOPY,
    ScreenToClient, SelectObject, SetBkMode, SetStretchBltMode, SetTextColor, StretchDIBits,
    TRANSPARENT, Arc as GdiArc,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow,
    SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DestroyWindow,
    DefWindowProcW, DispatchMessageW, GWLP_USERDATA, GetClientRect, GetCursorPos, GetMessageW,
    ICON_BIG, ICON_SMALL, IDC_ARROW, IDC_HAND, IsWindow, LoadCursorW, MSG, PostMessageW,
    PostQuitMessage, RegisterClassW, SW_HIDE, SW_SHOW, SendMessageW, SetCursor, SetForegroundWindow,
    SetWindowLongPtrW, ShowWindow, TranslateMessage, WM_ACTIVATE, WM_CLOSE, WM_COMMAND,
    WM_CONTEXTMENU, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_LBUTTONUP, WM_NCCREATE, WM_PAINT,
    WM_RBUTTONUP, WM_SETCURSOR, WM_SETICON, WNDCLASSW, WS_CAPTION, WS_MINIMIZEBOX, WS_OVERLAPPED,
    WS_SYSMENU, WS_VISIBLE,
};

use crate::autostart;
use crate::device::{DeviceKind, Reading, Snapshot, carry_last_known, read_snapshot};
use crate::tray::{MENU_EXIT, MENU_SHOW_WINDOW, Tray, WM_APP_TRAY, create_app_icon};

const APP_TITLE: &str = "MCHOSE 电量";
const CLIENT_WIDTH: i32 = 430;
const CLIENT_HEIGHT: i32 = 332;
const WM_APP_REFRESH: u32 = 0x8001;
const WA_INACTIVE: usize = 0;

/// Header wordmark from mchose.com.cn, pre-composited on the window background.
const LOGO: &[u8] = include_bytes!("logo.bgra");
const LOGO_WIDTH: i32 = 437;
const LOGO_HEIGHT: i32 = 66;

/// Set while a one-shot read is in flight, so focus + click cannot stack reads.
static REFRESHING: AtomicBool = AtomicBool::new(false);

const BG: COLORREF = rgb(247, 248, 250);
const CARD: COLORREF = rgb(255, 255, 255);
const CARD_SHADOW: COLORREF = rgb(238, 241, 245);
const BORDER: COLORREF = rgb(226, 232, 240);
const TEXT: COLORREF = rgb(15, 23, 42);
const MUTED: COLORREF = rgb(100, 116, 139);
const FAINT: COLORREF = rgb(148, 163, 184);
const ACCENT: COLORREF = rgb(37, 99, 235);
const ACCENT_SOFT: COLORREF = rgb(239, 246, 255);
const ACCENT_BORDER: COLORREF = rgb(219, 234, 254);
const RED: COLORREF = rgb(239, 68, 68);

const SWITCH_WIDTH: f32 = 38.0;
const SWITCH_HEIGHT: f32 = 20.0;
const SWITCH_TOP: f32 = 14.0;
const SWITCH_RIGHT: f32 = 410.0;

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

struct WindowContext {
    snapshot: Arc<Mutex<Snapshot>>,
    hwnd: HWND,
    tray: Option<Tray>,
    autostart: bool,
}

pub fn run() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    let snapshot = Arc::new(Mutex::new(Snapshot::loading()));
    let class_name = wide("MchoseBatteryWindow");
    let title = wide(APP_TITLE);

    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let cursor = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
        let app_icon = create_app_icon();
        let window_class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(window_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: app_icon,
            hCursor: cursor,
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class_name.as_ptr(),
        };
        if RegisterClassW(&window_class) == 0 {
            return;
        }

        let dpi = screen_dpi();
        let scale = dpi as f32 / 96.0;
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let mut window_rect = RECT {
            left: 0,
            top: 0,
            right: scale_units(CLIENT_WIDTH, scale),
            bottom: scale_units(CLIENT_HEIGHT, scale),
        };
        AdjustWindowRectExForDpi(&mut window_rect, style, 0, 0, dpi);
        let window_width = window_rect.right - window_rect.left;
        let window_height = window_rect.bottom - window_rect.top;
        let screen_width = windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics(0);
        let screen_height = windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics(1);
        let x = (screen_width - window_width) / 2;
        let y = (screen_height - window_height) / 2;

        let context = Box::new(WindowContext {
            snapshot: snapshot.clone(),
            hwnd: std::ptr::null_mut(),
            tray: None,
            autostart: autostart::is_enabled(),
        });
        let context_ptr = Box::into_raw(context);

        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            style | WS_VISIBLE,
            x,
            y,
            window_width,
            window_height,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            context_ptr as *mut _,
        );
        if hwnd.is_null() {
            drop(Box::from_raw(context_ptr));
            return;
        }

        (*context_ptr).hwnd = hwnd;
        SendMessageW(hwnd, WM_SETICON, ICON_BIG as usize, app_icon as isize);
        SendMessageW(hwnd, WM_SETICON, ICON_SMALL as usize, app_icon as isize);
        resize_for_dpi(hwnd);
        ShowWindow(hwnd, SW_SHOW);
        let loading = snapshot.lock().map(|state| state.clone()).unwrap_or_else(|_| Snapshot::loading());
        (*context_ptr).tray = Tray::new(hwnd, &loading);
        InvalidateRect(hwnd, std::ptr::null(), 1);

        request_refresh(hwnd);
        pump_messages();
    }
}

unsafe fn pump_messages() {
    let mut message: MSG = unsafe { std::mem::zeroed() };
    while unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) } > 0 {
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

/// Reads the devices exactly once, then goes quiet again. Nothing polls on a timer.
fn request_refresh(hwnd: HWND) {
    if REFRESHING.swap(true, Ordering::AcqRel) {
        return;
    }
    let snapshot = match unsafe { context(hwnd) } {
        Some(context) => context.snapshot.clone(),
        None => {
            REFRESHING.store(false, Ordering::Release);
            return;
        }
    };

    let hwnd_value = hwnd as usize;
    std::thread::spawn(move || {
        let mut next = read_snapshot();
        if let Ok(mut state) = snapshot.lock() {
            carry_last_known(&mut next.mouse.reading, &state.mouse.reading);
            carry_last_known(&mut next.keyboard.reading, &state.keyboard.reading);
            *state = next;
        }
        REFRESHING.store(false, Ordering::Release);

        let hwnd = hwnd_value as HWND;
        unsafe {
            if IsWindow(hwnd) != 0 {
                PostMessageW(hwnd, WM_APP_REFRESH, 0, 0);
            }
        }
    });
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        match message {
            WM_NCCREATE => {
                let create = &*(lparam as *const CREATESTRUCTW);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
                DefWindowProcW(hwnd, message, wparam, lparam)
            }
            WM_PAINT => {
                paint(hwnd);
                0
            }
            WM_ERASEBKGND => 1,
            WM_ACTIVATE => {
                if wparam & 0xFFFF != WA_INACTIVE {
                    request_refresh(hwnd);
                }
                0
            }
            WM_SETCURSOR => {
                let mut pt = POINT { x: 0, y: 0 };
                GetCursorPos(&mut pt);
                ScreenToClient(hwnd, &mut pt);
                if hit_autostart_toggle(pt.x, pt.y, window_scale(hwnd)) {
                    let hand = LoadCursorW(std::ptr::null_mut(), IDC_HAND);
                    SetCursor(hand);
                    1
                } else {
                    DefWindowProcW(hwnd, message, wparam, lparam)
                }
            }
            WM_LBUTTONUP => {
                let x = (lparam as u32 & 0xFFFF) as u16 as i16 as i32;
                let y = ((lparam as u32 >> 16) & 0xFFFF) as u16 as i16 as i32;
                if hit_autostart_toggle(x, y, window_scale(hwnd)) {
                    toggle_autostart(hwnd);
                } else {
                    request_refresh(hwnd);
                }
                0
            }
            WM_DPICHANGED => {
                let suggested = &*(lparam as *const RECT);
                windows_sys::Win32::UI::WindowsAndMessaging::SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    suggested.left,
                    suggested.top,
                    suggested.right - suggested.left,
                    suggested.bottom - suggested.top,
                    0x0010 | 0x0004,
                );
                InvalidateRect(hwnd, std::ptr::null(), 1);
                0
            }
            WM_APP_REFRESH => {
                let snapshot = context(hwnd)
                    .and_then(|context| context.snapshot.lock().ok())
                    .map(|state| state.clone());
                if let (Some(snapshot), Some(context)) = (snapshot, context_mut(hwnd))
                    && let Some(tray) = &context.tray
                {
                    tray.update_tooltip(&snapshot);
                }
                InvalidateRect(hwnd, std::ptr::null(), 0);
                0
            }
            WM_APP_TRAY => {
                handle_tray_event(hwnd, (lparam as u32) & 0xFFFF);
                0
            }
            WM_COMMAND => {
                match wparam & 0xFFFF {
                    MENU_SHOW_WINDOW => show_main_window(hwnd),
                    MENU_EXIT => {
                        DestroyWindow(hwnd);
                    }
                    _ => {}
                }
                0
            }
            WM_CLOSE => {
                let has_tray = context(hwnd).is_some_and(|context| context.tray.is_some());
                if has_tray {
                    ShowWindow(hwnd, SW_HIDE);
                } else {
                    DestroyWindow(hwnd);
                }
                0
            }
            WM_DESTROY => {
                let pointer = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                if pointer != 0 {
                    drop(Box::from_raw(pointer as *mut WindowContext));
                }
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }
}

unsafe fn context(hwnd: HWND) -> Option<&'static WindowContext> {
    let pointer =
        unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
    if pointer == 0 {
        None
    } else {
        Some(unsafe { &*(pointer as *mut WindowContext) })
    }
}

unsafe fn context_mut(hwnd: HWND) -> Option<&'static mut WindowContext> {
    let pointer =
        unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
    if pointer == 0 {
        None
    } else {
        Some(unsafe { &mut *(pointer as *mut WindowContext) })
    }
}

unsafe fn show_main_window(hwnd: HWND) {
    unsafe {
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
    }
}

unsafe fn handle_tray_event(hwnd: HWND, event: u32) {
    match event {
        WM_LBUTTONUP => unsafe { show_main_window(hwnd) },
        WM_RBUTTONUP | WM_CONTEXTMENU => {
            let snapshot = unsafe { context(hwnd) }
                .and_then(|context| context.snapshot.lock().ok())
                .map(|state| state.clone());
            if let (Some(snapshot), Some(context)) = (snapshot, unsafe { context(hwnd) })
                && let Some(tray) = &context.tray
            {
                tray.show_menu(&snapshot);
            }
        }
        _ => {}
    }
}

unsafe fn paint(hwnd: HWND) {
    let mut paint_struct: PAINTSTRUCT = unsafe { std::mem::zeroed() };
    let window_dc = unsafe { BeginPaint(hwnd, &mut paint_struct) };
    let mut client = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    unsafe { GetClientRect(hwnd, &mut client) };
    let width = client.right.max(1);
    let height = client.bottom.max(1);

    let memory_dc = unsafe { CreateCompatibleDC(window_dc) };
    let bitmap = unsafe { CreateCompatibleBitmap(window_dc, width, height) };
    let old_bitmap = unsafe { SelectObject(memory_dc, bitmap) };

    let scale = window_scale(hwnd);
    let (snapshot, autostart_enabled) = match unsafe { context(hwnd) } {
        Some(context) => (
            context
                .snapshot
                .lock()
                .map(|state| state.clone())
                .unwrap_or_else(|_| Snapshot::loading()),
            context.autostart,
        ),
        None => (Snapshot::loading(), false),
    };

    unsafe {
        render(memory_dc, width, height, scale, &snapshot, autostart_enabled);
        BitBlt(window_dc, 0, 0, width, height, memory_dc, 0, 0, SRCCOPY);
        SelectObject(memory_dc, old_bitmap);
        DeleteObject(bitmap);
        DeleteDC(memory_dc);
        EndPaint(hwnd, &paint_struct);
    }
}

unsafe fn render(
    hdc: HDC,
    width: i32,
    height: i32,
    scale: f32,
    snapshot: &Snapshot,
    autostart_enabled: bool,
) {
    unsafe {
        fill_rect(hdc, 0, 0, width, height, BG);

        let card_title_font = make_font(s(16.0, scale), FW_BOLD as i32);
        let percent_font = make_font(s(29.0, scale), FW_BOLD as i32);
        let switch_font = make_font(s(12.0, scale), FW_NORMAL as i32);

        draw_logo(hdc, s(20.0, scale), s(15.0, scale), s(18.0, scale));
        draw_autostart(hdc, scale, autostart_enabled, switch_font);

        for (index, device) in [&snapshot.mouse, &snapshot.keyboard, &snapshot.headset]
            .into_iter()
            .enumerate()
        {
            draw_device_card(
                hdc,
                scale,
                s(20.0, scale),
                s(46.0 + index as f32 * 92.0, scale),
                s(390.0, scale),
                s(82.0, scale),
                device,
                card_title_font,
                percent_font,
            );
        }

        DeleteObject(card_title_font);
        DeleteObject(percent_font);
        DeleteObject(switch_font);
    }
    let _ = height;
}

fn switch_rect(scale: f32) -> (i32, i32, i32, i32) {
    let width = s(SWITCH_WIDTH, scale);
    let height = s(SWITCH_HEIGHT, scale);
    let right = s(SWITCH_RIGHT, scale);
    let top = s(SWITCH_TOP, scale);
    (right - width, top, right, top + height)
}

fn hit_autostart_toggle(x: i32, y: i32, scale: f32) -> bool {
    let (left, top, right, bottom) = switch_rect(scale);
    let hit_left = left - s(66.0, scale);
    let hit_right = right + s(2.0, scale);
    let hit_top = top - s(2.0, scale);
    let hit_bottom = bottom + s(2.0, scale);
    (hit_left..=hit_right).contains(&x) && (hit_top..=hit_bottom).contains(&y)
}

fn window_scale(hwnd: HWND) -> f32 {
    unsafe {
        let mut client = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        GetClientRect(hwnd, &mut client);
        let width = client.right.max(1);
        let height = client.bottom.max(1);
        (GetDpiForWindow(hwnd) as f32 / 96.0)
            .min(width as f32 / CLIENT_WIDTH as f32)
            .min(height as f32 / CLIENT_HEIGHT as f32)
    }
}

unsafe fn toggle_autostart(hwnd: HWND) {
    let Some(context) = (unsafe { context_mut(hwnd) }) else {
        return;
    };
    let next = !context.autostart;
    if autostart::set_enabled(next) {
        context.autostart = next;
        unsafe { InvalidateRect(hwnd, std::ptr::null(), 0) };
    }
}

unsafe fn draw_autostart(hdc: HDC, scale: f32, enabled: bool, label_font: HFONT) {
    unsafe {
        let (left, top, right, bottom) = switch_rect(scale);
        draw_text(
            hdc,
            "开机自启",
            &mut rect(left - s(62.0, scale), top + s(1.0, scale), s(54.0, scale), bottom - top),
            label_font,
            MUTED,
            0x00000002 | 0x00000004,
        );

        draw_smooth_switch(hdc, left, top, right - left, bottom - top, enabled, scale);
    }
}

#[inline]
fn blend(fg: (u8, u8, u8), bg: (u8, u8, u8), alpha: f32) -> (u8, u8, u8) {
    (
        (fg.0 as f32 * alpha + bg.0 as f32 * (1.0 - alpha)).round() as u8,
        (fg.1 as f32 * alpha + bg.1 as f32 * (1.0 - alpha)).round() as u8,
        (fg.2 as f32 * alpha + bg.2 as f32 * (1.0 - alpha)).round() as u8,
    )
}

#[inline]
fn colorref_to_rgb(c: COLORREF) -> (u8, u8, u8) {
    ((c & 0xFF) as u8, ((c >> 8) & 0xFF) as u8, ((c >> 16) & 0xFF) as u8)
}

unsafe fn draw_smooth_switch(
    hdc: HDC,
    left: i32,
    top: i32,
    width: i32,
    height: i32,
    enabled: bool,
    scale: f32,
) {
    let pw = width;
    let ph = height;
    if pw <= 0 || ph <= 0 {
        return;
    }

    let bg_color = (247, 248, 250);
    let accent_color = (37, 99, 235);
    let off_color = (226, 232, 240);
    let off_border_color = (203, 213, 225);
    let knob_color = (255, 255, 255);
    let knob_border_color = if enabled { (205, 212, 222) } else { (200, 205, 215) };
    let track_color = if enabled { accent_color } else { off_color };

    let r = ph as f32 / 2.0;
    let seg_half = ((pw as f32 - ph as f32) / 2.0).max(0.0);
    let cx = pw as f32 / 2.0;
    let cy = ph as f32 / 2.0;

    let padding = 2.0 * scale;
    let knob_r = (ph as f32 - padding * 2.0) / 2.0;
    let knob_cx = if enabled {
        pw as f32 - padding - knob_r
    } else {
        padding + knob_r
    };
    let knob_cy = cy;

    let mut buf = vec![0u8; (pw * ph * 4) as usize];

    for y in 0..ph {
        let py = y as f32 + 0.5;
        let row_offset = (y * pw * 4) as usize;
        for x in 0..pw {
            let px = x as f32 + 0.5;

            // 1. Capsule Signed Distance Field
            let dx = ((px - cx).abs() - seg_half).max(0.0);
            let dy = py - cy;
            let dist_capsule = (dx * dx + dy * dy).sqrt() - r;
            let alpha_capsule = (0.5 - dist_capsule).clamp(0.0, 1.0);

            let mut col = bg_color;
            if alpha_capsule > 0.0 {
                if !enabled {
                    let border_w = 1.0 * scale;
                    let dist_inner = dist_capsule + border_w;
                    let alpha_inner = (0.5 - dist_inner).clamp(0.0, 1.0);
                    let track_fill = (241, 245, 249);
                    let track_c = blend(track_fill, off_border_color, alpha_inner);
                    col = blend(track_c, col, alpha_capsule);
                } else {
                    col = blend(track_color, col, alpha_capsule);
                }
            }

            // 2. Knob subtle drop shadow
            let s_dy = py - (knob_cy + 0.8 * scale);
            let s_dx = px - knob_cx;
            let shadow_dist = (s_dx * s_dx + s_dy * s_dy).sqrt() - knob_r;
            if shadow_dist < 1.5 * scale {
                let s_alpha = ((0.5 - shadow_dist / (1.5 * scale)).clamp(0.0, 1.0)) * 0.15;
                col = blend((0, 0, 0), col, s_alpha);
            }

            // 3. Knob body & border
            let k_dx = px - knob_cx;
            let k_dy = py - knob_cy;
            let k_dist = (k_dx * k_dx + k_dy * k_dy).sqrt() - knob_r;
            let alpha_knob = (0.5 - k_dist).clamp(0.0, 1.0);
            if alpha_knob > 0.0 {
                let k_border_dist = k_dist + 0.9 * scale;
                let alpha_k_border = (0.5 - k_border_dist).clamp(0.0, 1.0);
                let k_c = blend(knob_color, knob_border_color, alpha_k_border);
                col = blend(k_c, col, alpha_knob);
            }

            let idx = row_offset + (x * 4) as usize;
            buf[idx] = col.2;     // B
            buf[idx + 1] = col.1; // G
            buf[idx + 2] = col.0; // R
            buf[idx + 3] = 255;   // A
        }
    }

    unsafe {
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = pw;
        info.bmiHeader.biHeight = -ph;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;

        StretchDIBits(
            hdc,
            left,
            top,
            pw,
            ph,
            0,
            0,
            pw,
            ph,
            buf.as_ptr() as *const _,
            &info,
            DIB_RGB_COLORS,
            SRCCOPY,
        );
    }
}

unsafe fn draw_smooth_bar(
    hdc: HDC,
    left: i32,
    top: i32,
    width: i32,
    height: i32,
    fraction: f32,
    color_rgb: (u8, u8, u8),
) {
    let pw = width;
    let ph = height;
    if pw <= 0 || ph <= 0 {
        return;
    }

    let bg_color = (255, 255, 255);
    let track_color = (241, 245, 249);

    let r = ph as f32 / 2.0;
    let seg_half = ((pw as f32 - ph as f32) / 2.0).max(0.0);
    let cx = pw as f32 / 2.0;
    let cy = ph as f32 / 2.0;

    let fill_w = if fraction > 0.0 {
        ((pw as f32 * fraction).round() as i32).max(ph)
    } else {
        0
    };
    let fill_seg_half = ((fill_w as f32 - ph as f32) / 2.0).max(0.0);
    let fill_cx = r + fill_seg_half;

    let mut buf = vec![0u8; (pw * ph * 4) as usize];

    for y in 0..ph {
        let py = y as f32 + 0.5;
        let row_offset = (y * pw * 4) as usize;
        for x in 0..pw {
            let px = x as f32 + 0.5;

            // Track
            let dx = ((px - cx).abs() - seg_half).max(0.0);
            let dy = py - cy;
            let dist_track = (dx * dx + dy * dy).sqrt() - r;
            let alpha_track = (0.5 - dist_track).clamp(0.0, 1.0);

            let mut col = blend(track_color, bg_color, alpha_track);

            // Fill
            if fill_w > 0 {
                let fdx = ((px - fill_cx).abs() - fill_seg_half).max(0.0);
                let f_dist = (fdx * fdx + dy * dy).sqrt() - r;
                let alpha_fill = (0.5 - f_dist).clamp(0.0, 1.0);
                if alpha_fill > 0.0 {
                    col = blend(color_rgb, col, alpha_fill);
                }
            }

            let idx = row_offset + (x * 4) as usize;
            buf[idx] = col.2;
            buf[idx + 1] = col.1;
            buf[idx + 2] = col.0;
            buf[idx + 3] = 255;
        }
    }

    unsafe {
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = pw;
        info.bmiHeader.biHeight = -ph;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;

        StretchDIBits(
            hdc,
            left,
            top,
            pw,
            ph,
            0,
            0,
            pw,
            ph,
            buf.as_ptr() as *const _,
            &info,
            DIB_RGB_COLORS,
            SRCCOPY,
        );
    }
}

unsafe fn draw_logo(hdc: HDC, left: i32, top: i32, height: i32) {
    unsafe {
        let width = (height as f32 * LOGO_WIDTH as f32 / LOGO_HEIGHT as f32).round() as i32;
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = LOGO_WIDTH;
        info.bmiHeader.biHeight = -LOGO_HEIGHT;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;

        SetStretchBltMode(hdc, HALFTONE);
        StretchDIBits(
            hdc,
            left,
            top,
            width,
            height,
            0,
            0,
            LOGO_WIDTH,
            LOGO_HEIGHT,
            LOGO.as_ptr() as *const std::ffi::c_void,
            &info,
            DIB_RGB_COLORS,
            SRCCOPY,
        );
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn draw_device_card(
    hdc: HDC,
    scale: f32,
    left: i32,
    top: i32,
    width: i32,
    height: i32,
    device: &crate::device::DeviceReading,
    title_font: HFONT,
    percent_font: HFONT,
) {
    unsafe {
        // Subtle depth shadow
        draw_rounded_rect(
            hdc,
            left,
            top + s(1.0, scale),
            left + width,
            top + height + s(1.0, scale),
            s(12.0, scale),
            CARD_SHADOW,
            CARD_SHADOW,
            scale,
        );

        // Card body
        draw_rounded_rect(
            hdc,
            left,
            top,
            left + width,
            top + height,
            s(12.0, scale),
            CARD,
            BORDER,
            scale,
        );

        let (label, color, fraction) = match &device.reading {
            Reading::Available(reading) => (
                format!("{}%", reading.percent),
                battery_color(reading.percent),
                reading.percent as f32 / 100.0,
            ),
            Reading::Stale(reading) => (
                format!("{}%", reading.percent),
                FAINT,
                reading.percent as f32 / 100.0,
            ),
            Reading::Unavailable(_) => ("--".to_string(), FAINT, 0.0),
        };

        let icon_size = s(46.0, scale);
        draw_device_icon(
            hdc,
            scale,
            left + s(16.0, scale),
            top + (height - icon_size) / 2,
            device.kind,
            color,
        );

        draw_text(
            hdc,
            device.model,
            &mut rect(
                left + s(74.0, scale),
                top + s(18.0, scale),
                s(190.0, scale),
                s(24.0, scale),
            ),
            title_font,
            TEXT,
            0x00000000,
        );

        let pct_area_width = s(92.0, scale);
        let pct_right = left + width - s(20.0, scale);
        draw_text(
            hdc,
            &label,
            &mut rect(
                pct_right - pct_area_width,
                top + s(10.0, scale),
                pct_area_width,
                height - s(20.0, scale),
            ),
            percent_font,
            color,
            0x00000002 | 0x00000004,
        );

        let bar_left = left + s(74.0, scale);
        let bar_right = left + width - s(122.0, scale);
        let bar_top = top + s(50.0, scale);
        let bar_height = s(6.5, scale);
        draw_smooth_bar(
            hdc,
            bar_left,
            bar_top,
            bar_right - bar_left,
            bar_height,
            fraction,
            colorref_to_rgb(color),
        );
    }
}

fn battery_color(percent: u8) -> COLORREF {
    match percent {
        0..=20 => RED,
        _ => ACCENT,
    }
}

unsafe fn draw_device_icon(
    hdc: HDC,
    scale: f32,
    left: i32,
    top: i32,
    kind: DeviceKind,
    color: COLORREF,
) {
    unsafe {
        let size = s(46.0, scale);
        let badge_bg = if color == FAINT {
            rgb(245, 246, 248)
        } else {
            ACCENT_SOFT
        };
        let badge_border = if color == FAINT {
            rgb(230, 233, 238)
        } else {
            ACCENT_BORDER
        };
        draw_rounded_rect(
            hdc,
            left,
            top,
            left + size,
            top + size,
            s(12.0, scale),
            badge_bg,
            badge_border,
            scale,
        );

        let icon_color = if color == FAINT { FAINT } else { ACCENT };
        let pen = CreatePen(PS_SOLID, s(1.6, scale).max(1), icon_color);
        let old_pen = SelectObject(hdc, pen);
        let brush = CreateSolidBrush(rgb(255, 255, 255));
        let old_brush = SelectObject(hdc, brush);

        let cx = left + size / 2;
        let cy = top + size / 2;

        match kind {
            DeviceKind::Mouse => {
                let mw = s(18.0, scale);
                let mh = s(28.0, scale);
                let mx1 = cx - mw / 2;
                let my1 = cy - mh / 2;
                let mx2 = mx1 + mw;
                let my2 = my1 + mh;
                RoundRect(hdc, mx1, my1, mx2, my2, s(18.0, scale), s(18.0, scale));
                MoveToEx(hdc, cx, my1 + s(1.0, scale), std::ptr::null_mut());
                LineTo(hdc, cx, my1 + s(11.0, scale));
                let ww = s(4.0, scale);
                let wh = s(6.0, scale);
                draw_rounded_rect(
                    hdc,
                    cx - ww / 2,
                    my1 + s(4.0, scale),
                    cx - ww / 2 + ww,
                    my1 + s(4.0, scale) + wh,
                    s(2.0, scale),
                    icon_color,
                    icon_color,
                    scale,
                );
            }
            DeviceKind::Keyboard => {
                let kw = s(30.0, scale);
                let kh = s(19.0, scale);
                let kx1 = cx - kw / 2;
                let ky1 = cy - kh / 2;
                let kx2 = kx1 + kw;
                let ky2 = ky1 + kh;
                RoundRect(hdc, kx1, ky1, kx2, ky2, s(4.0, scale), s(4.0, scale));
                for step in 0..4 {
                    let bx = kx1 + s(3.0 + step as f32 * 6.0, scale);
                    fill_rect(hdc, bx, ky1 + s(3.0, scale), bx + s(4.0, scale), ky1 + s(5.5, scale), icon_color);
                }
                for step in 0..4 {
                    let bx = kx1 + s(3.0 + step as f32 * 6.0, scale);
                    fill_rect(hdc, bx, ky1 + s(7.5, scale), bx + s(4.0, scale), ky1 + s(10.0, scale), icon_color);
                }
                fill_rect(hdc, kx1 + s(3.0, scale), ky1 + s(12.0, scale), kx1 + s(6.0, scale), ky1 + s(14.5, scale), icon_color);
                draw_rounded_rect(
                    hdc,
                    kx1 + s(8.0, scale),
                    ky1 + s(12.0, scale),
                    kx2 - s(8.0, scale),
                    ky1 + s(14.5, scale),
                    s(1.0, scale),
                    icon_color,
                    icon_color,
                    scale,
                );
                fill_rect(hdc, kx2 - s(6.0, scale), ky1 + s(12.0, scale), kx2 - s(3.0, scale), ky1 + s(14.5, scale), icon_color);
            }
            DeviceKind::Headset => {
                let hw = s(30.0, scale);
                let hh = s(28.0, scale);
                let hx1 = cx - hw / 2;
                let hy1 = cy - hh / 2;
                let hx2 = hx1 + hw;

                GdiArc(
                    hdc,
                    hx1 + s(3.0, scale),
                    hy1 + s(1.0, scale),
                    hx2 - s(3.0, scale),
                    hy1 + s(25.0, scale),
                    hx2,
                    hy1 + s(13.0, scale),
                    hx1,
                    hy1 + s(13.0, scale),
                );

                let cushion_pen = CreatePen(PS_SOLID, 1, badge_border);
                let prev_pen = SelectObject(hdc, cushion_pen);
                GdiArc(
                    hdc,
                    hx1 + s(5.5, scale),
                    hy1 + s(3.5, scale),
                    hx2 - s(5.5, scale),
                    hy1 + s(24.0, scale),
                    hx2,
                    hy1 + s(14.0, scale),
                    hx1,
                    hy1 + s(14.0, scale),
                );
                SelectObject(hdc, prev_pen);
                DeleteObject(cushion_pen);

                let cw = s(7.0, scale);
                let ch = s(14.0, scale);
                let cup_y = hy1 + s(11.0, scale);
                let left_cup_x = hx1 + s(1.0, scale);
                RoundRect(hdc, left_cup_x, cup_y, left_cup_x + cw, cup_y + ch, s(4.0, scale), s(4.0, scale));
                MoveToEx(hdc, left_cup_x + s(4.0, scale), cup_y + s(2.0, scale), std::ptr::null_mut());
                LineTo(hdc, left_cup_x + s(4.0, scale), cup_y + ch - s(2.0, scale));

                let right_cup_x = hx2 - s(1.0, scale) - cw;
                RoundRect(hdc, right_cup_x, cup_y, right_cup_x + cw, cup_y + ch, s(4.0, scale), s(4.0, scale));
                MoveToEx(hdc, right_cup_x + s(3.0, scale), cup_y + s(2.0, scale), std::ptr::null_mut());
                LineTo(hdc, right_cup_x + s(3.0, scale), cup_y + ch - s(2.0, scale));
            }
        }

        SelectObject(hdc, old_brush);
        SelectObject(hdc, old_pen);
        DeleteObject(brush);
        DeleteObject(pen);
    }
}

unsafe fn fill_rect(hdc: HDC, left: i32, top: i32, right: i32, bottom: i32, color: COLORREF) {
    unsafe {
        let brush = CreateSolidBrush(color);
        let rect = RECT {
            left,
            top,
            right,
            bottom,
        };
        FillRect(hdc, &rect, brush);
        DeleteObject(brush);
    }
}

unsafe fn draw_rounded_rect(
    hdc: HDC,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    radius: i32,
    fill: COLORREF,
    border: COLORREF,
    scale: f32,
) {
    unsafe {
        let brush = CreateSolidBrush(fill);
        let pen = CreatePen(PS_SOLID, s(1.0, scale), border);
        let old_brush = SelectObject(hdc, brush);
        let old_pen = SelectObject(hdc, pen);
        RoundRect(hdc, left, top, right, bottom, radius, radius);
        SelectObject(hdc, old_pen);
        SelectObject(hdc, old_brush);
        DeleteObject(pen);
        DeleteObject(brush);
    }
}

unsafe fn draw_text(
    hdc: HDC,
    text: &str,
    bounds: &mut RECT,
    font: HFONT,
    color: COLORREF,
    format: u32,
) {
    unsafe {
        let wide: Vec<u16> = OsStr::new(text).encode_wide().collect();
        let old_font = SelectObject(hdc, font);
        SetBkMode(hdc, TRANSPARENT as i32);
        SetTextColor(hdc, color);
        DrawTextW(
            hdc,
            wide.as_ptr(),
            wide.len() as i32,
            bounds,
            format | 0x00000020 | 0x00000100,
        );
        SelectObject(hdc, old_font);
    }
}

fn make_font(height: i32, weight: i32) -> HFONT {
    let face = wide("Microsoft YaHei UI");
    unsafe {
        CreateFontW(
            -height,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_DEFAULT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            CLEARTYPE_QUALITY as u32,
            DEFAULT_PITCH as u32 | FF_DONTCARE as u32,
            face.as_ptr(),
        )
    }
}

fn rect(left: i32, top: i32, width: i32, height: i32) -> RECT {
    RECT {
        left,
        top,
        right: left + width,
        bottom: top + height,
    }
}

fn s(value: f32, scale: f32) -> i32 {
    (value * scale).round() as i32
}

fn scale_units(value: i32, scale: f32) -> i32 {
    (value as f32 * scale).round() as i32
}

fn wide(text: &str) -> Vec<u16> {
    OsStr::new(text).encode_wide().chain(std::iter::once(0)).collect()
}

fn screen_dpi() -> u32 {
    use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
    unsafe { GetDpiForSystem().max(96) }
}

fn resize_for_dpi(hwnd: HWND) {
    unsafe {
        let dpi = GetDpiForWindow(hwnd).max(96);
        let scale = dpi as f32 / 96.0;
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: scale_units(CLIENT_WIDTH, scale),
            bottom: scale_units(CLIENT_HEIGHT, scale),
        };
        AdjustWindowRectExForDpi(&mut rect, style, 0, 0, dpi);
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        let screen_width = windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics(0);
        let screen_height = windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics(1);
        windows_sys::Win32::UI::WindowsAndMessaging::SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            (screen_width - width) / 2,
            (screen_height - height) / 2,
            width,
            height,
            0x0004 | 0x0010,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switch_sits_in_the_header_row() {
        let (left, top, right, bottom) = switch_rect(1.0);
        assert_eq!((left, top, right, bottom), (372, 14, 410, 34));
    }

    #[test]
    fn switch_scales_with_dpi() {
        let (left, top, right, bottom) = switch_rect(1.5);
        assert_eq!(right - left, 57);
        assert_eq!(bottom - top, 30);
        assert_eq!((left, top), (558, 21));
    }

    #[test]
    fn hit_test_covers_label_and_toggle() {
        // Toggle capsule on 1.0 scale is 372..=410, 14..=34
        assert!(hit_autostart_toggle(390, 24, 1.0));
        // Label text on 1.0 scale is roughly 306..=360
        assert!(hit_autostart_toggle(330, 24, 1.0));
        // Outside
        assert!(!hit_autostart_toggle(100, 24, 1.0));
    }

    #[test]
    fn battery_color_marks_red_at_and_below_20_percent() {
        assert_eq!(battery_color(0), RED);
        assert_eq!(battery_color(10), RED);
        assert_eq!(battery_color(20), RED);
        assert_eq!(battery_color(21), ACCENT);
        assert_eq!(battery_color(83), ACCENT);
        assert_eq!(battery_color(100), ACCENT);
    }
}
