use std::thread;
use std::time::Duration;

use hidapi::{DeviceInfo, HidApi};

const MCHOSE_VID: u16 = 0x3837;
const MOUSE_PID: u16 = 0x100B;
const KEYBOARD_RECEIVER_PID: u16 = 0x201B;
const HEADSET_VID: u16 = 0x291D;
const HEADSET_PID: u16 = 0x385D;

const A7_CONFIG_USAGE_PAGE: u16 = 0xFF01;
const A7_CONFIG_USAGE: u16 = 1;
const G87_CONFIG_USAGE_PAGE: u16 = 0xFF02;
const G87_CONFIG_USAGE: u16 = 2;
const V9_CONFIG_USAGE_PAGE: u16 = 0xFF90;
const V9_CONFIG_USAGE: u16 = 1;

const A7_REPORT_ID: u8 = 0x11;
// Full-length feature reports wake a mouse that has gone to standby; short
// 21-byte requests are ignored while it sleeps.
const A7_FEATURE_LENGTH: usize = 81;
const A7_MEANINGFUL_BYTES: usize = 11;
const G87_REPORT_ID: u8 = 0x13;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    Mouse,
    Keyboard,
    Headset,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatteryReading {
    pub percent: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reading {
    Available(BatteryReading),
    Stale(BatteryReading),
    Unavailable(String),
}

pub fn carry_last_known(new: &mut Reading, previous: &Reading) {
    let Reading::Unavailable(message) = new else {
        return;
    };
    if !message.contains("无响应") {
        return;
    }
    let last = match previous {
        Reading::Available(reading) | Reading::Stale(reading) => reading,
        Reading::Unavailable(_) => return,
    };
    *new = Reading::Stale(last.clone());
}

#[derive(Clone, Debug)]
pub struct DeviceReading {
    pub kind: DeviceKind,
    pub model: &'static str,
    pub reading: Reading,
}

impl DeviceReading {
    pub fn loading(kind: DeviceKind) -> Self {
        let model = match kind {
            DeviceKind::Mouse => "A7 V2 Ultra+",
            DeviceKind::Keyboard => "G87 V2",
            DeviceKind::Headset => "V9 PRO",
        };
        Self {
            kind,
            model,
            reading: Reading::Unavailable("正在读取…".into()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub mouse: DeviceReading,
    pub keyboard: DeviceReading,
    pub headset: DeviceReading,
    pub initial: bool,
}

impl Snapshot {
    pub fn loading() -> Self {
        Self {
            mouse: DeviceReading::loading(DeviceKind::Mouse),
            keyboard: DeviceReading::loading(DeviceKind::Keyboard),
            headset: DeviceReading::loading(DeviceKind::Headset),
            initial: true,
        }
    }
}

fn find_config<'a>(
    api: &'a HidApi,
    vid: u16,
    pid: u16,
    usage_page: u16,
    usage: u16,
) -> Option<&'a DeviceInfo> {
    api.device_list().find(|info| {
        info.vendor_id() == vid
            && info.product_id() == pid
            && info.usage_page() == usage_page
            && info.usage() == usage
    })
}

fn find_mouse(api: &HidApi) -> Option<&DeviceInfo> {
    find_config(
        api,
        MCHOSE_VID,
        MOUSE_PID,
        A7_CONFIG_USAGE_PAGE,
        A7_CONFIG_USAGE,
    )
}

fn find_keyboard_receiver(api: &HidApi) -> Option<&DeviceInfo> {
    find_config(
        api,
        MCHOSE_VID,
        KEYBOARD_RECEIVER_PID,
        G87_CONFIG_USAGE_PAGE,
        G87_CONFIG_USAGE,
    )
}

fn find_headset_receiver(api: &HidApi) -> Option<&DeviceInfo> {
    find_config(
        api,
        HEADSET_VID,
        HEADSET_PID,
        V9_CONFIG_USAGE_PAGE,
        V9_CONFIG_USAGE,
    )
}

fn device_reading(kind: DeviceKind, model: &'static str, reading: Reading) -> DeviceReading {
    DeviceReading { kind, model, reading }
}

pub fn read_snapshot() -> Snapshot {
    let api = match HidApi::new() {
        Ok(api) => api,
        Err(error) => {
            let message = format!("HID 初始化失败：{error}");
            return Snapshot {
                mouse: device_reading(DeviceKind::Mouse, "A7 V2 Ultra+", Reading::Unavailable(message.clone())),
                keyboard: device_reading(DeviceKind::Keyboard, "G87 V2", Reading::Unavailable(message.clone())),
                headset: device_reading(DeviceKind::Headset, "V9 PRO", Reading::Unavailable(message)),
                initial: false,
            };
        }
    };

    let mouse = match find_mouse(&api) {
        Some(info) => read_mouse(&api, &info),
        None => Reading::Unavailable("未检测到接收器".into()),
    };

    let keyboard = match find_keyboard_receiver(&api) {
        Some(info) => read_keyboard(&api, &info),
        None => Reading::Unavailable("未检测到接收器".into()),
    };

    let headset = match find_headset_receiver(&api) {
        Some(info) => read_headset(&api, &info),
        None => Reading::Unavailable("未检测到耳机接收器".into()),
    };

    Snapshot {
        mouse: device_reading(DeviceKind::Mouse, "A7 V2 Ultra+", mouse),
        keyboard: device_reading(DeviceKind::Keyboard, "G87 V2", keyboard),
        headset: device_reading(DeviceKind::Headset, "V9 PRO", headset),
        initial: false,
    }
}

fn read_mouse(api: &HidApi, info: &DeviceInfo) -> Reading {
    let device = match info.open_device(api) {
        Ok(device) => device,
        Err(error) => return Reading::Unavailable(format!("设备接口打开失败：{error}")),
    };

    let request = a7_request();

    let mut previous: Option<BatteryReading> = None;
    let mut report = [0_u8; A7_FEATURE_LENGTH];

    for _ in 0..12 {
        if device.send_feature_report(&request).is_err() {
            thread::sleep(Duration::from_millis(90));
            continue;
        }
        thread::sleep(Duration::from_millis(90));

        report[0] = A7_REPORT_ID;
        let length = match device.get_feature_report(&mut report) {
            Ok(length) => length,
            Err(_) => continue,
        };
        let Some(reading) = parse_a7_response(&report[..length]) else {
            continue;
        };

        if previous.as_ref() == Some(&reading) {
            return Reading::Available(reading);
        }
        previous = Some(reading);
    }

    match previous {
        Some(reading) => Reading::Available(reading),
        None => Reading::Unavailable("无响应，动一下鼠标".into()),
    }
}

fn read_keyboard(api: &HidApi, info: &DeviceInfo) -> Reading {
    let device = match info.open_device(api) {
        Ok(device) => device,
        Err(error) => return Reading::Unavailable(format!("设备接口打开失败：{error}")),
    };

    let request = g87_request();
    if device
        .send_output_report(&request)
        .or_else(|_| device.write(&request).map(|_| ()))
        .is_err()
    {
        return Reading::Unavailable("无法发送电量请求".into());
    }

    let mut report = [0_u8; 64];
    for _ in 0..6 {
        match device.read_timeout(&mut report, 400) {
            Ok(0) => continue,
            Ok(length) => {
                if let Some(reading) = parse_g87_response(&report[..length]) {
                    return Reading::Available(reading);
                }
            }
            Err(_) => return Reading::Unavailable("读取失败，请重新插拔接收器".into()),
        }
    }

    Reading::Unavailable("无响应，按任意键唤醒".into())
}

fn g87_request() -> [u8; 20] {
    let mut request = [0_u8; 20];
    request[0] = G87_REPORT_ID;
    request[1] = 0x4A;
    request[2] = 0x01;
    let checksum = request[..19]
        .iter()
        .fold(0_u8, |sum, byte| sum.wrapping_add(*byte));
    request[19] = checksum;
    request
}

fn read_headset(api: &HidApi, info: &DeviceInfo) -> Reading {
    let device = match info.open_device(api) {
        Ok(device) => device,
        Err(error) => return Reading::Unavailable(format!("耳机接口打开失败：{error}")),
    };

    let request = v9_request();
    if device
        .send_output_report(&request)
        .or_else(|_| device.write(&request).map(|_| ()))
        .is_err()
    {
        return Reading::Unavailable("耳机请求发送失败".into());
    }

    let mut report = [0_u8; 64];
    for _ in 0..3 {
        match device.read_timeout(&mut report, 500) {
            Ok(0) => continue,
            Ok(length) => {
                if let Some(reading) = parse_v9_response(&report[..length]) {
                    return Reading::Available(reading);
                }
            }
            Err(_) => break,
        }
    }

    Reading::Unavailable("耳机未连接或已关闭".into())
}

fn v9_request() -> [u8; 64] {
    let mut request = [0_u8; 64];
    request[0] = 0x55;
    request[1] = 0x65;
    request[2] = 0x01;
    request
}

fn parse_v9_response(raw: &[u8]) -> Option<BatteryReading> {
    if raw.len() < 3 || raw[0] != 0x55 || raw[1] != 0x65 {
        return None;
    }
    let percent = raw[2];
    if percent > 100 {
        return None;
    }
    Some(BatteryReading { percent })
}

fn normalize_report_id(raw: &[u8], report_id: u8) -> &[u8] {
    raw.strip_prefix(&[report_id]).unwrap_or(raw)
}

fn a7_request() -> [u8; A7_FEATURE_LENGTH] {
    let mut request = [0_u8; A7_FEATURE_LENGTH];
    request[0] = A7_REPORT_ID;
    request[1] = 0x06 ^ 0xFF;
    request
}

fn parse_a7_response(raw: &[u8]) -> Option<BatteryReading> {
    let raw = normalize_report_id(raw, A7_REPORT_ID);
    if raw.len() < A7_MEANINGFUL_BYTES + 1 {
        return None;
    }
    if raw[0] ^ 0xFF != 0x06 {
        return None;
    }

    let payload: Vec<u8> = raw[1..].iter().map(|byte| byte ^ 0xFF).collect();
    if payload.len() < A7_MEANINGFUL_BYTES || !payload.iter().take(A7_MEANINGFUL_BYTES).any(|b| *b != 0) {
        return None;
    }

    let percent = payload[9];
    if percent > 100 {
        return None;
    }

    Some(BatteryReading { percent })
}

fn parse_g87_response(raw: &[u8]) -> Option<BatteryReading> {
    if raw.len() < 7 || raw[0] != G87_REPORT_ID {
        return None;
    }
    if raw.len() >= 8 {
        let checksum = raw[..raw.len() - 1]
            .iter()
            .fold(0_u8, |sum, byte| sum.wrapping_add(*byte));
        if checksum != *raw.last()? {
            return None;
        }
    }

    let payload = &raw[1..];
    if payload[0] != 0x4A {
        return None;
    }

    let percent = *payload.get(4)?;
    if percent > 100 {
        return None;
    }

    Some(BatteryReading { percent })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a7_request_fills_full_feature_report() {
        let request = a7_request();
        assert_eq!(request.len(), A7_FEATURE_LENGTH);
        assert_eq!(&request[..2], &[0x11, 0xF9]);
        assert!(request[2..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn a7_reader_accepts_verified_response() {
        let raw = [
            0x11, 0xF9, 0xC8, 0xC7, 0xDE, 0xBF, 0xD1, 0xFA, 0xFD, 0xFF, 0xF6, 0xA9, 0xFF, 0xFF,
            0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        ];
        assert_eq!(parse_a7_response(&raw), Some(BatteryReading { percent: 86 }));
    }

    #[test]
    fn a7_reader_rejects_invalid_values() {
        let mut raw = vec![0x11, 0xF9];
        raw.extend([0_u8; 19]);
        assert_eq!(parse_a7_response(&raw), None);

        let mut too_high = [0_u8; 21];
        too_high[0] = 0x11;
        for byte in &mut too_high[1..] {
            *byte = 0xFF;
        }
        too_high[1] = 0xF9;
        too_high[2] = 0x06 ^ 0xFF;
        too_high[11] = 101 ^ 0xFF;
        assert_eq!(parse_a7_response(&too_high), None);
    }

    #[test]
    fn g87_packet_parser_accepts_verified_response() {
        let raw = [
            0x13, 0x4A, 0x01, 0x00, 0x02, 0x46, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0xA6,
        ];
        assert_eq!(parse_g87_response(&raw), Some(BatteryReading { percent: 70 }));
    }

    #[test]
    fn g87_reader_rejects_bad_checksum_or_range() {
        let mut raw = [
            0x13, 0x4A, 0x01, 0x00, 0x02, 0x46, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0xA6,
        ];
        raw[19] ^= 0x01;
        assert_eq!(parse_g87_response(&raw), None);

        let mut too_high = raw;
        too_high[5] = 101;
        too_high[19] = too_high[..19]
            .iter()
            .fold(0_u8, |sum, byte| sum.wrapping_add(*byte));
        assert_eq!(parse_g87_response(&too_high), None);
    }

    #[test]
    fn v9_parser_accepts_verified_response() {
        let mut raw = [0_u8; 64];
        raw[0] = 0x55;
        raw[1] = 0x65;
        raw[2] = 80;
        raw[3] = 0x02;
        assert_eq!(parse_v9_response(&raw), Some(BatteryReading { percent: 80 }));
    }

    #[test]
    fn v9_parser_rejects_unknown_or_invalid_response() {
        assert_eq!(parse_v9_response(&[0x55, 0x65]), None);
        assert_eq!(parse_v9_response(&[0x56, 0x65, 80, 0]), None);
        assert_eq!(parse_v9_response(&[0x55, 0x65, 101, 0]), None);
    }

    #[test]
    fn v9_request_matches_read_only_query() {
        let request = v9_request();
        assert_eq!(request.len(), 64);
        assert_eq!(&request[..3], &[0x55, 0x65, 0x01]);
        assert!(request[3..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn timeout_keeps_last_known_reading() {
        let previous = Reading::Available(BatteryReading { percent: 68 });
        let mut next = Reading::Unavailable("无响应，按任意键唤醒".into());
        carry_last_known(&mut next, &previous);
        assert_eq!(next, Reading::Stale(BatteryReading { percent: 68 }));
    }

    #[test]
    fn removed_receiver_does_not_keep_last_reading() {
        let previous = Reading::Available(BatteryReading { percent: 68 });
        let mut next = Reading::Unavailable("未检测到接收器".into());
        carry_last_known(&mut next, &previous);
        assert_eq!(next, Reading::Unavailable("未检测到接收器".into()));
    }

    #[test]
    fn g87_request_matches_official_read_only_query() {
        assert_eq!(
            g87_request(),
            [
                0x13, 0x4A, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x5E,
            ]
        );
    }
}
