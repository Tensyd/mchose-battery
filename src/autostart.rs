use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, REG_SZ, RegCloseKey, RegCreateKeyW, RegDeleteValueW,
    RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "MCHOSE Battery";
const MAX_VALUE_BYTES: u32 = 4096;
/// Boot is not a user request to see the window: start in the tray instead.
const SILENT_FLAG: &str = "--silent";

/// Reads the Run entry, if any.
fn stored_command() -> Option<String> {
    unsafe {
        let mut key: HKEY = std::ptr::null_mut();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            wide(RUN_KEY).as_ptr(),
            0,
            KEY_QUERY_VALUE,
            &mut key,
        ) != ERROR_SUCCESS
        {
            return None;
        }

        let name = wide(VALUE_NAME);
        let mut kind = 0_u32;
        let mut size = 0_u32;
        let status = RegQueryValueExW(
            key,
            name.as_ptr(),
            std::ptr::null(),
            &mut kind,
            std::ptr::null_mut(),
            &mut size,
        );

        let stored = if status == ERROR_SUCCESS
            && kind == REG_SZ
            && size > 0
            && size <= MAX_VALUE_BYTES
        {
            let mut buffer = vec![0_u8; size as usize];
            let status = RegQueryValueExW(
                key,
                name.as_ptr(),
                std::ptr::null(),
                &mut kind,
                buffer.as_mut_ptr(),
                &mut size,
            );
            if status == ERROR_SUCCESS {
                Some(decode(&buffer[..size as usize]))
            } else {
                None
            }
        } else {
            None
        };

        RegCloseKey(key);
        stored
    }
}

/// True when the Run entry exists and still points at this executable.
///
/// Only the executable path is compared: a value written by an older build has no
/// `--silent` flag, and that must not read as "autostart is off".
pub fn is_enabled() -> bool {
    let Ok(current) = std::env::current_exe() else {
        return false;
    };
    let Some(stored) = stored_command() else {
        return false;
    };

    executable_of(&stored).eq_ignore_ascii_case(&current.display().to_string())
}

/// Rewrites an entry left by an older build so the next boot starts silently.
pub fn migrate_to_silent_start() {
    if !is_enabled() {
        return;
    }
    let Some(expected) = command_line() else {
        return;
    };
    if stored_command().as_deref() == Some(expected.as_str()) {
        return;
    }
    let _ = set_enabled(true);
}

/// Writes or removes the per-user Run entry. No elevation required.
pub fn set_enabled(enabled: bool) -> bool {
    let Some(command) = command_line() else {
        return false;
    };

    unsafe {
        let mut key: HKEY = std::ptr::null_mut();
        if RegCreateKeyW(HKEY_CURRENT_USER, wide(RUN_KEY).as_ptr(), &mut key) != ERROR_SUCCESS {
            return false;
        }

        let name = wide(VALUE_NAME);
        let result = if enabled {
            let value = wide(&command);
            RegSetValueExW(
                key,
                name.as_ptr(),
                0,
                REG_SZ,
                value.as_ptr() as *const u8,
                (value.len() * 2) as u32,
            ) == ERROR_SUCCESS
        } else {
            let status = RegDeleteValueW(key, name.as_ptr());
            status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND
        };

        RegCloseKey(key);
        result
    }
}

fn command_line() -> Option<String> {
    let path = std::env::current_exe().ok()?;
    Some(format!("\"{}\" {}", path.display(), SILENT_FLAG))
}

/// Pulls the executable out of a Run value. Handles both `"path" --args` and a bare path.
fn executable_of(command: &str) -> String {
    let trimmed = command.trim();
    if let Some(rest) = trimmed.strip_prefix('"') {
        return match rest.find('"') {
            Some(end) => rest[..end].to_string(),
            None => rest.to_string(),
        };
    }
    trimmed.split_whitespace().next().unwrap_or("").to_string()
}

fn decode(bytes: &[u8]) -> String {
    let (pairs, _) = bytes.as_chunks::<2>();
    let units: Vec<u16> = pairs
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .take_while(|unit| *unit != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

fn wide(text: &str) -> Vec<u16> {
    OsStr::new(text).encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_stops_at_terminator() {
        let mut bytes = Vec::new();
        for unit in "C:\\test\\app.exe".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes.extend_from_slice(&[0, 0]);
        assert_eq!(decode(&bytes), "C:\\test\\app.exe");
    }

    #[test]
    fn command_line_is_quoted() {
        let command = command_line().expect("current exe");
        assert!(command.starts_with('"') && command.contains('"'));
    }

    #[test]
    fn command_line_asks_for_a_silent_start() {
        let command = command_line().expect("current exe");
        assert!(command.ends_with(" --silent"), "boot must not pop the window up");
    }

    #[test]
    fn executable_of_strips_arguments_and_quotes() {
        assert_eq!(executable_of(r#""C:\a b\app.exe" --silent"#), r"C:\a b\app.exe");
        assert_eq!(executable_of(r#""C:\a\app.exe""#), r"C:\a\app.exe");
        assert_eq!(executable_of(r"C:\a\app.exe"), r"C:\a\app.exe");
        assert_eq!(executable_of(r"C:\a\app.exe --silent"), r"C:\a\app.exe");
        assert_eq!(executable_of("  "), "");
    }
}
