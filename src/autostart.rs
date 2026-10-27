use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, REG_SZ, RegCloseKey, RegCreateKeyW, RegDeleteValueW,
    RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "MC Battery";
/// The program registered itself under this name before the rename. If it is not removed,
/// the machine ends up with two entries and one of them points at a deleted file.
const LEGACY_VALUE_NAME: &str = "MCHOSE Battery";
const MAX_VALUE_BYTES: u32 = 4096;
/// Boot is not a user request to see the window: start in the tray instead.
const SILENT_FLAG: &str = "--silent";

fn read_value(key: HKEY, name: &str) -> Option<String> {
    let name = wide(name);
    let mut kind = 0_u32;
    let mut size = 0_u32;

    let status = unsafe {
        RegQueryValueExW(
            key,
            name.as_ptr(),
            std::ptr::null(),
            &mut kind,
            std::ptr::null_mut(),
            &mut size,
        )
    };

    if status != ERROR_SUCCESS || kind != REG_SZ || size == 0 || size > MAX_VALUE_BYTES {
        return None;
    }

    let mut buffer = vec![0_u8; size as usize];
    let status = unsafe {
        RegQueryValueExW(
            key,
            name.as_ptr(),
            std::ptr::null(),
            &mut kind,
            buffer.as_mut_ptr(),
            &mut size,
        )
    };

    if status == ERROR_SUCCESS {
        Some(decode(&buffer[..size as usize]))
    } else {
        None
    }
}

fn with_run_key<T>(action: impl FnOnce(HKEY) -> T) -> Option<T> {
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
        let result = action(key);
        RegCloseKey(key);
        Some(result)
    }
}

/// The Run entry and the name it was stored under, whether or not it is the current name.
fn stored_entry() -> Option<(&'static str, String)> {
    with_run_key(|key| {
        [VALUE_NAME, LEGACY_VALUE_NAME]
            .into_iter()
            .find_map(|name| read_value(key, name).map(|value| (name, value)))
    })
    .flatten()
}

/// True when the Run entry exists and belongs to this program.
///
/// An entry under the legacy name counts: this build is the one that wrote it, and after
/// the rename its path points at a file that is about to disappear.
pub fn is_enabled() -> bool {
    match stored_entry() {
        Some((name, _)) if name == LEGACY_VALUE_NAME => true,
        Some((_, stored)) => {
            executable_of(&stored).eq_ignore_ascii_case(&current_executable())
        }
        None => false,
    }
}

/// Brings an existing Run entry in line with this build: current value name, current
/// executable path, and the silent flag. Called once at startup.
pub fn sync() {
    let Some(expected) = command_line() else {
        return;
    };
    let entry = stored_entry();
    let borrowed = entry
        .as_ref()
        .map(|(name, command)| (*name, command.as_str()));
    if should_rewrite(borrowed, &expected, &current_executable()) {
        let _ = set_enabled(true);
    }
}

/// Decides whether a stored entry is stale.
///
/// Any entry under the legacy name is rewritten: the rename changed both the value name
/// and the executable path, so the old value can only ever launch a file that no longer
/// exists. With the current name, only a stale command line is corrected — a different
/// path probably means someone keeps a second copy around, and hijacking it would be rude.
fn should_rewrite(entry: Option<(&str, &str)>, expected: &str, current_executable: &str) -> bool {
    let Some((name, stored)) = entry else {
        return false;
    };
    if name == LEGACY_VALUE_NAME {
        return true;
    }
    stored != expected && executable_of(stored).eq_ignore_ascii_case(current_executable)
}

fn current_executable() -> String {
    std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_default()
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

        // Drop the pre-rename value either way, so enabling or disabling both converge on
        // exactly one entry (or none).
        let legacy = wide(LEGACY_VALUE_NAME);
        RegDeleteValueW(key, legacy.as_ptr());

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
        assert!(
            command.ends_with(" --silent"),
            "boot must not pop the window up"
        );
    }

    #[test]
    fn executable_of_strips_arguments_and_quotes() {
        assert_eq!(
            executable_of(r#""C:\a b\app.exe" --silent"#),
            r"C:\a b\app.exe"
        );
        assert_eq!(executable_of(r#""C:\a\app.exe""#), r"C:\a\app.exe");
        assert_eq!(executable_of(r"C:\a\app.exe"), r"C:\a\app.exe");
        assert_eq!(executable_of(r"C:\a\app.exe --silent"), r"C:\a\app.exe");
        assert_eq!(executable_of("  "), "");
    }

    #[test]
    fn the_current_value_name_is_the_product_name() {
        // Guards the rename: an entry written under the old name must not be created again.
        assert_eq!(VALUE_NAME, "MC Battery");
        assert_ne!(VALUE_NAME, LEGACY_VALUE_NAME);
    }

    #[test]
    fn a_legacy_entry_is_always_rewritten() {
        let old = r#""C:\Users\me\Desktop\MCHOSE-Battery.exe" --silent"#;
        let expected = r#""C:\Users\me\Desktop\MC-Battery.exe" --silent"#;
        assert!(should_rewrite(
            Some((LEGACY_VALUE_NAME, old)),
            expected,
            r"C:\Users\me\Desktop\MC-Battery.exe"
        ));
    }

    #[test]
    fn an_up_to_date_entry_is_left_alone() {
        let command = r#""C:\app\mc-battery.exe" --silent"#;
        assert!(!should_rewrite(
            Some((VALUE_NAME, command)),
            command,
            r"C:\app\mc-battery.exe"
        ));
    }

    #[test]
    fn a_current_entry_without_the_silent_flag_is_corrected() {
        assert!(should_rewrite(
            Some((VALUE_NAME, r#""C:\app\mc-battery.exe""#)),
            r#""C:\app\mc-battery.exe" --silent"#,
            r"C:\app\mc-battery.exe"
        ));
    }

    #[test]
    fn another_installation_is_not_hijacked() {
        // Same value name, different path: a second copy someone keeps on purpose.
        assert!(!should_rewrite(
            Some((VALUE_NAME, r#""D:\other\mc-battery.exe""#)),
            r#""C:\app\mc-battery.exe" --silent"#,
            r"C:\app\mc-battery.exe"
        ));
    }

    #[test]
    fn no_entry_means_nothing_to_do() {
        assert!(!should_rewrite(None, r#""C:\app\mc-battery.exe" --silent"#, r"C:\app\mc-battery.exe"));
    }
}
