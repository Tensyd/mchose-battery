//! Passive enumeration report.
//!
//! Writing a working adapter for a new model needs facts that only the device owner can
//! collect: which interfaces it exposes, what the product string says, which page answers
//! the battery query. This module collects the part that is free — the interface list —
//! and deliberately stops there: nothing is opened, no report is written, no command is
//! sent. Serials and device paths are not read at all, so they cannot leak into a report
//! that ends up in a public issue.

use std::fmt::Write as _;
use std::path::PathBuf;

use hidapi::HidApi;
use windows_sys::Win32::Foundation::SYSTEMTIME;
use windows_sys::Win32::System::SystemInformation::{GetLocalTime, OSVERSIONINFOW};
use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MessageBoxW};

use crate::device::{DeviceSpec, InterfaceFacts, SPECS};

/// Vendor ids worth showing even when the interface is not a vendor collection: the ids
/// this project already supports, plus the ones seen in public research. Being listed here
/// is not a support claim, only a hint for the person reading the report.
const KNOWN_VENDOR_IDS: [u16; 5] = [
    0x3837, // MCHOSE devices this program supports today
    0x291D, // C-Media receiver used by the V9 PRO
    0x5253, // family documented by public research, unverified here
    0x2023, // first-generation A5 Pro Max receivers, unverified here
    0xA8A5, // G7 chip vendor id, unverified here
];

const VENDOR_PAGE_MIN: u16 = 0xFF00;

/// One HID interface, copied out of hidapi so the report can be formatted and tested
/// without a device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterfaceEntry {
    pub vendor_id: u16,
    pub product_id: u16,
    pub usage_page: u16,
    pub usage: u16,
    pub interface_number: i32,
    pub release_number: u16,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
}

impl InterfaceEntry {
    pub fn facts(&self) -> InterfaceFacts {
        InterfaceFacts::new(self.vendor_id, self.product_id, self.usage_page, self.usage)
    }

    fn is_vendor_collection(&self) -> bool {
        self.usage_page >= VENDOR_PAGE_MIN
    }

    fn is_known_vendor(&self) -> bool {
        KNOWN_VENDOR_IDS.contains(&self.vendor_id)
    }
}

/// Reads the interface list. No device is opened and no report is exchanged.
pub fn collect() -> Result<Vec<InterfaceEntry>, String> {
    let api = HidApi::new().map_err(|error| format!("HID 初始化失败：{error}"))?;

    let mut entries: Vec<InterfaceEntry> = api
        .device_list()
        .map(|info| InterfaceEntry {
            vendor_id: info.vendor_id(),
            product_id: info.product_id(),
            usage_page: info.usage_page(),
            usage: info.usage(),
            interface_number: info.interface_number(),
            release_number: info.release_number(),
            manufacturer: info.manufacturer_string().map(str::to_string),
            // `serial_number()` is deliberately not read: a report may be posted publicly.
            product: info.product_string().map(str::to_string),
        })
        .collect();

    entries.sort_by_key(|entry| {
        (
            entry.vendor_id,
            entry.product_id,
            entry.usage_page,
            entry.usage,
            entry.interface_number,
        )
    });
    Ok(entries)
}

fn hex16(value: u16) -> String {
    format!("{value:04X}")
}

fn describe(entry: &InterfaceEntry) -> String {
    let mut line = format!(
        "VID {}  PID {}  用法 {}:{}  接口 {}  版本 {}",
        hex16(entry.vendor_id),
        hex16(entry.product_id),
        hex16(entry.usage_page),
        hex16(entry.usage),
        entry.interface_number,
        hex16(entry.release_number),
    );

    if let Some(product) = entry.product.as_deref().filter(|text| !text.is_empty()) {
        let _ = write!(line, "  产品 \"{product}\"");
    }
    if let Some(manufacturer) = entry.manufacturer.as_deref().filter(|text| !text.is_empty()) {
        let _ = write!(line, "  厂商 \"{manufacturer}\"");
    }
    line
}

fn push_section(report: &mut String, title: &str, entries: &[&InterfaceEntry]) {
    let _ = writeln!(report, "[{}]", title);
    let _ = writeln!(report, "  共 {} 个", entries.len());
    if entries.is_empty() {
        let _ = writeln!(report, "  （无）");
    }
    for (index, entry) in entries.iter().enumerate() {
        let _ = writeln!(report, "  {}. {}", index + 1, describe(entry));
    }
    report.push('\n');
}

fn match_line(spec: &DeviceSpec, entries: &[&InterfaceEntry]) -> String {
    let found = entries.iter().any(|entry| spec.matches(&entry.facts()));
    format!(
        "  {:<14} {}  ({}:{} / {}:{})",
        spec.model,
        if found { "已找到" } else { "未找到" },
        hex16(spec.vendor_id),
        hex16(spec.product_id),
        hex16(spec.usage_page),
        hex16(spec.usage),
    )
}

/// One line of the read-out section. `--probe` is what turns "the device shows --" into
/// an actionable answer instead of a guess.
pub fn probe_line(result: &crate::device::ProbeResult) -> String {
    let outcome = match &result.outcome {
        Ok(percent) => format!("{percent}%"),
        // Most details already name the cause ("无响应，按任意键唤醒"); only add the
        // category when it says something the sentence does not.
        Err(error) if error.detail.contains(error.kind.label()) => error.detail.clone(),
        Err(error) => format!("{}（{}）", error.detail, error.kind.label()),
    };
    format!("  {:<14} {}", result.model, outcome)
}

fn push_probe_section(report: &mut String, results: Option<&[crate::device::ProbeResult]>) {
    report.push_str("[4 电量读取]\n");
    match results {
        Some(results) => {
            for result in results {
                report.push_str(&probe_line(result));
                report.push('\n');
            }
            report.push_str("  以上为只读查询；未向未知接口发送任何报文。\n");
        }
        None => {
            report.push_str("  （默认只枚举，不读取。加 --probe 执行只读电量查询。）\n");
        }
    }
    report.push('\n');
}

/// Formats the whole report. Split out from `collect()` so the shape of the output can be
/// tested without HID hardware.
pub fn format_report(
    entries: &[InterfaceEntry],
    version: &str,
    timestamp: &str,
    os_line: &str,
    results: Option<&[crate::device::ProbeResult]>,
) -> String {
    let mut report = String::new();

    report.push_str("MC Battery 诊断报告\n");
    report.push_str("========================\n");
    let _ = writeln!(report, "生成时间：{timestamp}");
    let _ = writeln!(report, "程序版本：{version}");
    let _ = writeln!(report, "系统版本：{os_line}");
    let _ = writeln!(report, "HID 接口总数：{}", entries.len());
    report.push('\n');
    report.push_str("说明：默认仅枚举系统 HID 接口；序列号与设备路径未读取，因此不会出现在报告里。\n");
    if results.is_some() {
        report.push_str("      本次额外执行了只读电量查询（--probe），仅针对已支持型号。\n");
    } else {
        report.push_str("      未打开任何设备、未发送任何查询指令。\n");
    }
    report.push('\n');

    let vendor: Vec<&InterfaceEntry> = entries
        .iter()
        .filter(|entry| entry.is_vendor_collection())
        .collect();
    push_section(&mut report, "1 厂商自定义接口（用法页 >= FF00）", &vendor);

    let known: Vec<&InterfaceEntry> = entries
        .iter()
        .filter(|entry| entry.is_known_vendor() && !entry.is_vendor_collection())
        .collect();
    push_section(&mut report, "2 已知厂商 ID 的其他接口", &known);

    report.push_str("[3 已知型号匹配]\n");
    for spec in SPECS.iter() {
        let line = match_line(spec, &vendor);
        report.push_str(&line);
        report.push('\n');
    }
    report.push('\n');

    push_probe_section(&mut report, results);

    report.push_str("提交新型号信息时，请附上本文件、该型号的完整名称与固件版本，\n");
    report.push_str("以及同一时刻官方软件显示的电量。缺少实测依据的型号不会被直接适配。\n");

    report
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn message_box(text: &str, warning: bool) {
    let caption = wide("MC Battery 诊断");
    let body = wide(text);
    let icon = if warning { MB_ICONWARNING } else { MB_ICONINFORMATION };
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            body.as_ptr(),
            caption.as_ptr(),
            MB_OK | icon,
        );
    }
}

fn timestamp() -> String {
    let mut time = SYSTEMTIME::default();
    unsafe { GetLocalTime(&mut time) };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute, time.wSecond
    )
}

fn os_line() -> String {
    let mut info = OSVERSIONINFOW {
        dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
        ..Default::default()
    };
    let status = unsafe { rtl_get_version(&mut info) };
    let arch = std::env::consts::ARCH;
    if status == 0 {
        format!(
            "Windows {}.{}.{} ({arch})",
            info.dwMajorVersion, info.dwMinorVersion, info.dwBuildNumber
        )
    } else {
        format!("Windows (版本未知, {arch})")
    }
}

#[link(name = "ntdll")]
unsafe extern "system" {
    #[link_name = "RtlGetVersion"]
    fn rtl_get_version(info: *mut OSVERSIONINFOW) -> i32;
}

fn report_path() -> (PathBuf, bool) {
    // Portable first: the report lands next to the executable, where it is easy to find.
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join("mchose-diagnose.txt");
        if std::fs::write(&candidate, b"").is_ok() {
            return (candidate, true);
        }
    }
    (std::env::temp_dir().join("mchose-diagnose.txt"), false)
}

/// Runs the diagnostic, writes the report and tells the user where it went.
pub fn run(probe: bool) {
    let entries = match collect() {
        Ok(entries) => entries,
        Err(error) => {
            message_box(&format!("{error}\n\n无法读取 HID 接口列表。"), true);
            return;
        }
    };

    // Only an explicit `--probe` reads devices, and even then only the supported models:
    // unknown interfaces are never opened or written to.
    let results = if probe {
        match crate::device::probe() {
            Ok(results) => Some(results),
            Err(error) => {
                message_box(&format!("{error}\n\n电量查询未完成。"), true);
                return;
            }
        }
    } else {
        None
    };

    let report = format_report(
        &entries,
        env!("CARGO_PKG_VERSION"),
        &timestamp(),
        &os_line(),
        results.as_deref(),
    );

    let (path, portable) = report_path();
    if let Err(error) = std::fs::write(&path, report.as_bytes()) {
        message_box(&format!("报告写入失败：{error}"), true);
        return;
    }

    // When the probe ran, presence is known for certain; otherwise fall back to matching
    // the enumerated interfaces.
    let found = match results.as_deref() {
        Some(results) => results.iter().filter(|result| result.present).count(),
        None => SPECS
            .iter()
            .filter(|spec| entries.iter().any(|entry| spec.matches(&entry.facts())))
            .count(),
    };

    let note = if portable {
        String::new()
    } else {
        "\n（程序目录不可写，已改存到临时目录）".to_string()
    };

    let read_note = if probe {
        format!(
            "\n电量查询：{} 款成功，{} 款未读到。",
            results
                .as_deref()
                .unwrap_or_default()
                .iter()
                .filter(|result| result.outcome.is_ok())
                .count(),
            results
                .as_deref()
                .unwrap_or_default()
                .iter()
                .filter(|result| result.outcome.is_err())
                .count()
        )
    } else {
        "\n未执行电量查询（如需，加 --probe）。".to_string()
    };

    message_box(
        &format!(
            "已枚举 {} 个 HID 接口，识别到 {} / {} 款已支持设备。{}\n\n报告位置：\n{}{}\n\n序列号与设备路径未读取。",
            entries.len(),
            found,
            SPECS.len(),
            read_note,
            path.display(),
            note
        ),
        false,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(vid: u16, pid: u16, page: u16, usage: u16, interface: i32, product: &str) -> InterfaceEntry {
        InterfaceEntry {
            vendor_id: vid,
            product_id: pid,
            usage_page: page,
            usage,
            interface_number: interface,
            release_number: 0x0100,
            manufacturer: Some("Test".to_string()),
            product: Some(product.to_string()),
        }
    }

    fn sample() -> Vec<InterfaceEntry> {
        vec![
            entry(0x3837, 0x100B, 0xFF01, 1, 2, "MCHOSE A7 V2 Ultra+"),
            entry(0x3837, 0x100B, 0xFF0B, 0x104, 2, "MCHOSE A7 V2 Ultra+"),
            entry(0x3837, 0x100B, 0x0001, 6, 0, "MCHOSE A7 V2 Ultra+"),
            entry(0x291D, 0x385D, 0xFF90, 1, 0, "MCHOSE V9 PRO"),
            entry(0x046D, 0xC52B, 0x0001, 2, 0, "USB Input Device"),
        ]
    }

    #[test]
    fn probe_section_explains_how_to_enable_it() {
        let report = format_report(
            &sample(),
            "0.6.0",
            "2026-10-27 19:00:00",
            "Windows 10.0.26100 (x86_64)",
            None,
        );
        assert!(report.contains("[4 电量读取]"));
        assert!(report.contains("加 --probe"));
        assert!(report.contains("未打开任何设备"));
    }

    #[test]
    fn probe_results_are_reported_per_device_with_their_reason() {
        let results = vec![
            crate::device::ProbeResult {
                model: "A7 V2 Ultra+",
                present: true,
                outcome: Ok(86),
            },
            crate::device::ProbeResult {
                model: "G87 V2",
                present: false,
                outcome: Err(crate::device::ReadError::not_present("未检测到接收器")),
            },
            crate::device::ProbeResult {
                model: "V9 PRO",
                present: true,
                outcome: Err(crate::device::ReadError::no_response("耳机未连接或已关闭")),
            },
        ];

        let report = format_report(
            &sample(),
            "0.6.0",
            "2026-10-27 19:00:00",
            "Windows 10.0.26100 (x86_64)",
            Some(&results),
        );

        assert!(report.contains("A7 V2 Ultra+"));
        assert!(report.contains("86%"));
        assert!(report.contains("未检测到接收器"));
        assert!(report.contains("耳机未连接或已关闭（无响应）"));
        assert!(report.contains("本次额外执行了只读电量查询"));
        // A probe run must not claim it was passive.
        assert!(!report.contains("未打开任何设备"));
    }

    #[test]
    fn probe_line_separates_a_missing_receiver_from_a_silent_device() {
        let missing = crate::device::ProbeResult {
            model: "G87 V2",
            present: false,
            outcome: Err(crate::device::ReadError::not_present("未检测到接收器")),
        };
        let silent = crate::device::ProbeResult {
            model: "G87 V2",
            present: true,
            outcome: Err(crate::device::ReadError::no_response("无响应，按任意键唤醒")),
        };

        assert!(probe_line(&missing).contains("未检测到接收器"));
        assert!(probe_line(&silent).contains("按任意键唤醒"));
        assert_ne!(probe_line(&missing), probe_line(&silent));
    }

    #[test]
    fn only_vendor_collections_land_in_the_first_section() {
        let report = format_report(&sample(), "0.3.0", "2026-10-27 19:00:00", "Windows 10.0.26100 (x86_64)", None);
        let section = report
            .split("[2 已知厂商 ID 的其他接口]")
            .next()
            .expect("first section");

        assert!(section.contains("用法 FF01:0001"));
        assert!(section.contains("用法 FF90:0001"));
        // The sibling collection on the same interface is listed too: seeing it is how a
        // dead collection gets identified.
        assert!(section.contains("用法 FF0B:0104"));
        assert!(!section.contains("用法 0001:0006"), "boot protocol stays out");
        assert!(!section.contains("046D"), "unrelated vendors stay out");
        assert!(section.contains("共 3 个"));
    }

    #[test]
    fn known_vendor_but_non_vendor_page_is_listed_separately() {
        let report = format_report(&sample(), "0.3.0", "2026-10-27 19:00:00", "Windows 10.0.26100 (x86_64)", None);
        let section = report
            .split("[2 已知厂商 ID 的其他接口]")
            .nth(1)
            .expect("second section");

        assert!(section.contains("用法 0001:0006"));
        assert!(section.contains("共 1 个"));
    }

    #[test]
    fn known_models_report_whether_their_interface_is_present() {
        let report = format_report(&sample(), "0.3.0", "2026-10-27 19:00:00", "Windows 10.0.26100 (x86_64)", None);

        assert!(report.contains("A7 V2 Ultra+"));
        assert!(report.contains("V9 PRO"));
        assert!(report.contains("3837:100B / FF01:0001"));
        // The keyboard receiver is absent from the sample, so it must read as missing.
        let keyboard_line = report
            .lines()
            .find(|line| line.contains("G87 V2"))
            .expect("keyboard line");
        assert!(keyboard_line.contains("未找到"));
    }

    #[test]
    fn report_states_that_nothing_was_sent_or_serialized() {
        let report = format_report(&sample(), "0.3.0", "2026-10-27 19:00:00", "Windows 10.0.26100 (x86_64)", None);

        assert!(report.contains("未发送任何查询指令"));
        assert!(report.contains("序列号与设备路径未读取"));
        assert!(report.contains("程序版本：0.3.0"));
    }

    #[test]
    fn an_empty_machine_still_produces_a_readable_report() {
        let report = format_report(&[], "0.3.0", "2026-10-27 19:00:00", "Windows 10.0.26100 (x86_64)", None);

        assert!(report.contains("HID 接口总数：0"));
        assert!(report.contains("（无）"));
        assert_eq!(report.matches("未找到").count(), SPECS.len());
    }

    #[test]
    fn serials_are_not_part_of_the_entry_shape() {
        // A regression guard: adding `serial` to InterfaceEntry would make it printable and
        // could leak into a publicly posted report.
        let entry = entry(0x3837, 0x100B, 0xFF01, 1, 2, "MCHOSE A7 V2 Ultra+");
        let described = describe(&entry);
        assert!(!described.to_lowercase().contains("serial"));
        assert!(!described.contains("序列号"));
    }
}
