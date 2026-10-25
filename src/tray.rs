use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::Foundation::{HWND, POINT};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
    DeleteObject, GetDC, ReleaseDC,
};
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION,
    NOTIFYICONDATAW, NOTIFYICON_VERSION_4, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, DestroyIcon, DestroyMenu, GetCursorPos,
    HICON, HMENU, ICONINFO, MF_GRAYED, MF_SEPARATOR, MF_STRING, PostMessageW, SetForegroundWindow,
    TrackPopupMenu, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RIGHTBUTTON, WM_NULL,
};

use crate::device::{Reading, Snapshot};

pub const WM_APP_TRAY: u32 = 0x8000;
pub const MENU_SHOW_WINDOW: usize = 1001;
pub const MENU_EXIT: usize = 1002;

const TRAY_ID: u32 = 1;
const APP_ICON: &[u8] = include_bytes!("app-icon.bgra");
const APP_ICON_SIZE: i32 = 64;

pub struct Tray {
    hwnd: HWND,
    icon: HICON,
    added: bool,
}

impl Tray {
    pub fn new(hwnd: HWND, snapshot: &Snapshot) -> Option<Self> {
        let icon = create_app_icon();
        if icon.is_null() {
            return None;
        }

        let mut data = base_data(hwnd, icon);
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        copy_tip(&mut data, &tooltip_text(snapshot));
        let added = unsafe { Shell_NotifyIconW(NIM_ADD, &data) != 0 };

        if !added {
            unsafe { DestroyIcon(icon) };
            return None;
        }

        let mut version = base_data(hwnd, icon);
        version.uFlags = 0;
        unsafe {
            version.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            Shell_NotifyIconW(NIM_SETVERSION, &version);
        }

        Some(Self {
            hwnd,
            icon,
            added: true,
        })
    }

    pub fn update_tooltip(&self, snapshot: &Snapshot) {
        if !self.added {
            return;
        }
        let mut data = base_data(self.hwnd, self.icon);
        data.uFlags = NIF_TIP | NIF_SHOWTIP;
        copy_tip(&mut data, &tooltip_text(snapshot));
        unsafe {
            Shell_NotifyIconW(NIM_MODIFY, &data);
        }
    }

    pub fn show_menu(&self, snapshot: &Snapshot) {
        unsafe {
            let menu = CreatePopupMenu();
            if menu.is_null() {
                return;
            }

            for device in [&snapshot.mouse, &snapshot.keyboard, &snapshot.headset] {
                append_menu(
                    menu,
                    MF_STRING | MF_GRAYED,
                    0,
                    &format!("{}   {}", device.model, percent_label(&device.reading)),
                );
            }
            append_menu(menu, MF_SEPARATOR, 0, "");
            append_menu(menu, MF_STRING, MENU_SHOW_WINDOW, "显示主界面");
            append_menu(menu, MF_STRING, MENU_EXIT, "退出");

            let mut point = POINT { x: 0, y: 0 };
            GetCursorPos(&mut point);
            SetForegroundWindow(self.hwnd);
            TrackPopupMenu(
                menu,
                TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                0,
                self.hwnd,
                std::ptr::null(),
            );
            PostMessageW(self.hwnd, WM_NULL, 0, 0);
            DestroyMenu(menu);
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        if self.added {
            let mut data = base_data(self.hwnd, self.icon);
            data.uFlags = 0;
            unsafe {
                Shell_NotifyIconW(NIM_DELETE, &data);
                DestroyIcon(self.icon);
            }
        }
    }
}

fn base_data(hwnd: HWND, icon: HICON) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW::default();
    data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = hwnd;
    data.uID = TRAY_ID;
    data.uCallbackMessage = WM_APP_TRAY;
    data.hIcon = icon;
    data
}

fn copy_tip(data: &mut NOTIFYICONDATAW, text: &str) {
    let limit = data.szTip.len() - 1;
    for (target, character) in data
        .szTip
        .iter_mut()
        .zip(text.encode_utf16().take(limit))
    {
        *target = character;
    }
}

pub fn tooltip_text(snapshot: &Snapshot) -> String {
    if snapshot.initial {
        return "MCHOSE 电量  |  正在读取设备…".to_string();
    }

    format!(
        "MCHOSE 电量  |  鼠标 {}  ·  键盘 {}  ·  耳机 {}",
        percent_label(&snapshot.mouse.reading),
        percent_label(&snapshot.keyboard.reading),
        percent_label(&snapshot.headset.reading),
    )
}

fn percent_label(reading: &Reading) -> String {
    match reading {
        Reading::Available(value) | Reading::Stale(value) => format!("{}%", value.percent),
        Reading::Unavailable(message) if message.contains("正在读取") => "读取中".to_string(),
        Reading::Unavailable(_) => "--".to_string(),
    }
}

unsafe fn append_menu(menu: HMENU, flags: u32, id: usize, text: &str) {
    let wide: Vec<u16> = OsStr::new(text).encode_wide().chain(Some(0)).collect();
    unsafe {
        AppendMenuW(menu, flags, id, wide.as_ptr());
    }
}

/// Window/taskbar icon: the MCHOSE mark taken from the site favicon.
pub fn create_app_icon() -> HICON {
    unsafe {
        let screen = GetDC(std::ptr::null_mut());
        if screen.is_null() {
            return std::ptr::null_mut();
        }

        let mut info = BITMAPINFO::default();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = APP_ICON_SIZE;
        info.bmiHeader.biHeight = -APP_ICON_SIZE;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;

        let mut pixels: *mut std::ffi::c_void = std::ptr::null_mut();
        let color = CreateDIBSection(
            screen,
            &info,
            DIB_RGB_COLORS,
            &mut pixels,
            std::ptr::null_mut(),
            0,
        );
        if color.is_null() || pixels.is_null() {
            ReleaseDC(std::ptr::null_mut(), screen);
            return std::ptr::null_mut();
        }
        std::ptr::copy_nonoverlapping(
            APP_ICON.as_ptr(),
            pixels as *mut u8,
            APP_ICON.len(),
        );

        let stride = ((APP_ICON_SIZE as usize + 15) / 16) * 2;
        let mut mask_bits = vec![0_u8; stride * APP_ICON_SIZE as usize];
        for y in 0..APP_ICON_SIZE as usize {
            for x in 0..APP_ICON_SIZE as usize {
                let alpha = APP_ICON[(y * APP_ICON_SIZE as usize + x) * 4 + 3];
                if alpha == 0 {
                    mask_bits[y * stride + x / 8] |= 0x80 >> (x % 8);
                }
            }
        }
        let mask = CreateBitmap(
            APP_ICON_SIZE,
            APP_ICON_SIZE,
            1,
            1,
            mask_bits.as_ptr() as *const std::ffi::c_void,
        );

        let icon_info = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&icon_info);

        DeleteObject(mask);
        DeleteObject(color);
        ReleaseDC(std::ptr::null_mut(), screen);
        icon
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{BatteryReading, DeviceKind, DeviceReading};

    fn device(kind: DeviceKind, reading: Reading) -> DeviceReading {
        let model = match kind {
            DeviceKind::Mouse => "A7 V2 Ultra+",
            DeviceKind::Keyboard => "G87 V2",
            DeviceKind::Headset => "V9 PRO",
        };
        DeviceReading {
            kind,
            model,
            reading,
        }
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            mouse: device(
                DeviceKind::Mouse,
                Reading::Available(BatteryReading { percent: 85 }),
            ),
            keyboard: device(
                DeviceKind::Keyboard,
                Reading::Available(BatteryReading { percent: 68 }),
            ),
            headset: device(
                DeviceKind::Headset,
                Reading::Unavailable("耳机未连接或已关闭".into()),
            ),
            initial: false,
        }
    }

    #[test]
    fn tooltip_lists_all_devices() {
        let text = tooltip_text(&snapshot());
        assert_eq!(
            text,
            "MCHOSE 电量  |  鼠标 85%  ·  键盘 68%  ·  耳机 --"
        );
        assert!(text.encode_utf16().count() < 128);
    }

    #[test]
    fn tooltip_has_loading_state() {
        assert_eq!(
            tooltip_text(&Snapshot::loading()),
            "MCHOSE 电量  |  正在读取设备…"
        );
    }
}
