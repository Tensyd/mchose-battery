//! Battery query protocols. Each implementation owns its request bytes, its retry budget
//! and its parser, and works only through `Link`, so it can be replayed offline.
//!
//! A protocol is not a model list: the same bytes are not evidence that a different
//! model, firmware or connection mode behaves the same way.

use crate::device::BatteryReading;
use crate::device::transport::{Clock, Link};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadErrorKind {
    /// The receiver for this device is not plugged in.
    NotPresent,
    /// The interface exists but could not be opened.
    OpenFailed,
    /// The query could not be written to the device.
    RequestFailed,
    /// The device accepted the query but returned nothing usable.
    NoResponse,
    /// A read failed outright, as opposed to timing out.
    ReadFailed,
}

impl ReadErrorKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::NotPresent => "未检测到接收器",
            Self::OpenFailed => "接口打开失败",
            Self::RequestFailed => "请求发送失败",
            Self::NoResponse => "无响应",
            Self::ReadFailed => "读取失败",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadError {
    pub kind: ReadErrorKind,
    pub detail: String,
}

impl ReadError {
    pub fn not_present(detail: impl Into<String>) -> Self {
        Self {
            kind: ReadErrorKind::NotPresent,
            detail: detail.into(),
        }
    }

    pub fn open_failed(detail: impl Into<String>) -> Self {
        Self {
            kind: ReadErrorKind::OpenFailed,
            detail: detail.into(),
        }
    }

    pub fn request_failed(detail: impl Into<String>) -> Self {
        Self {
            kind: ReadErrorKind::RequestFailed,
            detail: detail.into(),
        }
    }

    pub fn no_response(detail: impl Into<String>) -> Self {
        Self {
            kind: ReadErrorKind::NoResponse,
            detail: detail.into(),
        }
    }

    pub fn read_failed(detail: impl Into<String>) -> Self {
        Self {
            kind: ReadErrorKind::ReadFailed,
            detail: detail.into(),
        }
    }
}

pub trait Protocol: Sync {
    fn read(&self, link: &mut dyn Link, clock: &dyn Clock) -> Result<BatteryReading, ReadError>;
}

// ---------------------------------------------------------------------------
// A7 V2 Ultra+ — 81-byte feature report, inverted payload.
// ---------------------------------------------------------------------------

pub const A7_REPORT_ID: u8 = 0x11;
/// Full-length feature reports wake a mouse that has gone to standby; short 21-byte
/// requests are ignored while it sleeps. Measured on the A7 V2 Ultra+, not a rule for
/// other models.
pub const A7_FEATURE_LENGTH: usize = 81;
pub const A7_MEANINGFUL_BYTES: usize = 11;

const A7_ATTEMPTS: usize = 12;
const A7_ATTEMPT_GAP_MS: u64 = 90;

pub struct A7;

impl A7 {
    pub fn request() -> [u8; A7_FEATURE_LENGTH] {
        let mut request = [0_u8; A7_FEATURE_LENGTH];
        request[0] = A7_REPORT_ID;
        request[1] = 0x06 ^ 0xFF;
        request
    }

    pub fn parse(raw: &[u8]) -> Option<BatteryReading> {
        let raw = raw.strip_prefix(&[A7_REPORT_ID]).unwrap_or(raw);
        if raw.len() < A7_MEANINGFUL_BYTES + 1 {
            return None;
        }
        if raw[0] ^ 0xFF != 0x06 {
            return None;
        }

        let payload: Vec<u8> = raw[1..].iter().map(|byte| byte ^ 0xFF).collect();
        if payload.len() < A7_MEANINGFUL_BYTES
            || !payload.iter().take(A7_MEANINGFUL_BYTES).any(|b| *b != 0)
        {
            return None;
        }

        let percent = payload[9];
        if percent > 100 {
            return None;
        }

        Some(BatteryReading { percent })
    }
}

impl Protocol for A7 {
    fn read(&self, link: &mut dyn Link, clock: &dyn Clock) -> Result<BatteryReading, ReadError> {
        let request = Self::request();
        let mut previous: Option<BatteryReading> = None;
        let mut report = [0_u8; A7_FEATURE_LENGTH];

        for _ in 0..A7_ATTEMPTS {
            if link.send_feature(&request).is_err() {
                clock.sleep_ms(A7_ATTEMPT_GAP_MS);
                continue;
            }
            clock.sleep_ms(A7_ATTEMPT_GAP_MS);

            report[0] = A7_REPORT_ID;
            let length = match link.get_feature(&mut report) {
                Ok(length) => length,
                Err(_) => continue,
            };
            let Some(reading) = Self::parse(&report[..length]) else {
                continue;
            };

            // Two identical samples confirm the value; a single one is still used as a
            // last resort below.
            if previous.as_ref() == Some(&reading) {
                return Ok(reading);
            }
            previous = Some(reading);
        }

        previous.ok_or_else(|| ReadError::no_response("无响应，动一下鼠标"))
    }
}

pub static A7_PROTOCOL: A7 = A7;

// ---------------------------------------------------------------------------
// G87 V2 — 20-byte output report, additive checksum, framed reply.
// ---------------------------------------------------------------------------

pub const G87_REPORT_ID: u8 = 0x13;
pub const G87_REQUEST_LENGTH: usize = 20;

const G87_READ_ATTEMPTS: usize = 6;
const G87_READ_TIMEOUT_MS: i32 = 400;

pub struct G87;

impl G87 {
    pub fn request() -> [u8; G87_REQUEST_LENGTH] {
        let mut request = [0_u8; G87_REQUEST_LENGTH];
        request[0] = G87_REPORT_ID;
        request[1] = 0x4A;
        request[2] = 0x01;
        request[G87_REQUEST_LENGTH - 1] = request[..G87_REQUEST_LENGTH - 1]
            .iter()
            .fold(0_u8, |sum, byte| sum.wrapping_add(*byte));
        request
    }

    /// Current behaviour, kept deliberately: a seven-byte reply is accepted without a
    /// checksum because the checksum byte is the last one and short frames were seen in
    /// the field. Whether a short frame is trustworthy is still unconfirmed.
    pub fn parse(raw: &[u8]) -> Option<BatteryReading> {
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
}

impl Protocol for G87 {
    fn read(&self, link: &mut dyn Link, clock: &dyn Clock) -> Result<BatteryReading, ReadError> {
        let _ = clock;
        if link.send_output(&Self::request()).is_err() {
            return Err(ReadError::request_failed("无法发送电量请求"));
        }

        let mut report = [0_u8; 64];
        for _ in 0..G87_READ_ATTEMPTS {
            match link.read_timeout(&mut report, G87_READ_TIMEOUT_MS) {
                Ok(0) => continue,
                Ok(length) => {
                    if let Some(reading) = Self::parse(&report[..length]) {
                        return Ok(reading);
                    }
                }
                Err(_) => return Err(ReadError::read_failed("读取失败，请重新插拔接收器")),
            }
        }

        Err(ReadError::no_response("无响应，按任意键唤醒"))
    }
}

pub static G87_PROTOCOL: G87 = G87;

// ---------------------------------------------------------------------------
// V9 PRO — 64-byte output report, fixed signature reply.
// ---------------------------------------------------------------------------

pub const V9_REQUEST_LENGTH: usize = 64;
pub const V9_SIGNATURE: [u8; 2] = [0x55, 0x65];

const V9_READ_ATTEMPTS: usize = 3;
const V9_READ_TIMEOUT_MS: i32 = 500;

pub struct V9;

impl V9 {
    pub fn request() -> [u8; V9_REQUEST_LENGTH] {
        let mut request = [0_u8; V9_REQUEST_LENGTH];
        request[0] = V9_SIGNATURE[0];
        request[1] = V9_SIGNATURE[1];
        request[2] = 0x01;
        request
    }

    /// Current behaviour, kept deliberately: only the two-byte signature and the level
    /// range are checked, so an echoed request also parses. The status byte is not
    /// interpreted because its meaning is unconfirmed.
    pub fn parse(raw: &[u8]) -> Option<BatteryReading> {
        if raw.len() < 3 || raw[0] != V9_SIGNATURE[0] || raw[1] != V9_SIGNATURE[1] {
            return None;
        }
        let percent = raw[2];
        if percent > 100 {
            return None;
        }
        Some(BatteryReading { percent })
    }
}

impl Protocol for V9 {
    fn read(&self, link: &mut dyn Link, clock: &dyn Clock) -> Result<BatteryReading, ReadError> {
        let _ = clock;
        if link.send_output(&Self::request()).is_err() {
            return Err(ReadError::request_failed("耳机请求发送失败"));
        }

        let mut report = [0_u8; 64];
        for _ in 0..V9_READ_ATTEMPTS {
            match link.read_timeout(&mut report, V9_READ_TIMEOUT_MS) {
                Ok(0) => continue,
                Ok(length) => {
                    if let Some(reading) = Self::parse(&report[..length]) {
                        return Ok(reading);
                    }
                }
                Err(_) => break,
            }
        }

        Err(ReadError::no_response("耳机未连接或已关闭"))
    }
}

pub static V9_PROTOCOL: V9 = V9;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::transport::mock::{FakeClock, MockLink};

    fn verified_a7_reply() -> Vec<u8> {
        vec![
            0x11, 0xF9, 0xC8, 0xC7, 0xDE, 0xBF, 0xD1, 0xFA, 0xFD, 0xFF, 0xF6, 0xA9, 0xFF, 0xFF,
            0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        ]
    }

    fn verified_g87_reply() -> Vec<u8> {
        vec![
            0x13, 0x4A, 0x01, 0x00, 0x02, 0x46, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0xA6,
        ]
    }

    fn verified_v9_reply() -> Vec<u8> {
        let mut raw = vec![0_u8; 64];
        raw[0] = 0x55;
        raw[1] = 0x65;
        raw[2] = 80;
        raw[3] = 0x02;
        raw
    }

    // --- request bytes -----------------------------------------------------

    #[test]
    fn a7_request_fills_full_feature_report() {
        let request = A7::request();
        assert_eq!(request.len(), A7_FEATURE_LENGTH);
        assert_eq!(&request[..2], &[0x11, 0xF9]);
        assert!(request[2..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn g87_request_matches_official_read_only_query() {
        assert_eq!(
            G87::request(),
            [
                0x13, 0x4A, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x5E,
            ]
        );
    }

    #[test]
    fn v9_request_matches_read_only_query() {
        let request = V9::request();
        assert_eq!(request.len(), 64);
        assert_eq!(&request[..3], &[0x55, 0x65, 0x01]);
        assert!(request[3..].iter().all(|byte| *byte == 0));
    }

    // --- parsers -----------------------------------------------------------

    #[test]
    fn a7_parser_accepts_verified_reply() {
        assert_eq!(
            A7::parse(&verified_a7_reply()),
            Some(BatteryReading { percent: 86 })
        );
    }

    #[test]
    fn a7_parser_rejects_invalid_values() {
        let mut all_zero = vec![0x11, 0xF9];
        all_zero.extend([0_u8; 19]);
        assert_eq!(A7::parse(&all_zero), None);

        let mut too_high = [0_u8; 21];
        too_high.fill(0xFF);
        too_high[0] = 0x11;
        too_high[1] = 0xF9;
        too_high[2] = 0x06 ^ 0xFF;
        too_high[11] = 101 ^ 0xFF;
        assert_eq!(A7::parse(&too_high), None);

        // Shorter than the meaningful payload must be rejected, not panicked on.
        assert_eq!(A7::parse(&[]), None);
        assert_eq!(A7::parse(&[0x11, 0xF9]), None);
    }

    #[test]
    fn g87_parser_accepts_verified_reply() {
        assert_eq!(
            G87::parse(&verified_g87_reply()),
            Some(BatteryReading { percent: 70 })
        );
    }

    #[test]
    fn g87_parser_rejects_bad_checksum_or_range() {
        let mut bad_sum = verified_g87_reply();
        bad_sum[19] ^= 0x01;
        assert_eq!(G87::parse(&bad_sum), None);

        let mut too_high = verified_g87_reply();
        too_high[5] = 101;
        too_high[19] = too_high[..19]
            .iter()
            .fold(0_u8, |sum, byte| sum.wrapping_add(*byte));
        assert_eq!(G87::parse(&too_high), None);
    }

    /// Characterisation: a seven-byte frame skips the checksum. Recorded as current
    /// behaviour, not as a verified protocol rule.
    #[test]
    fn g87_parser_accepts_seven_byte_frame_without_checksum() {
        let short = [0x13, 0x4A, 0x01, 0x00, 0x02, 0x46, 0x00];
        assert_eq!(G87::parse(&short), Some(BatteryReading { percent: 70 }));

        let six = [0x13, 0x4A, 0x01, 0x00, 0x02, 0x46];
        assert_eq!(G87::parse(&six), None);
    }

    #[test]
    fn v9_parser_accepts_verified_reply() {
        assert_eq!(
            V9::parse(&verified_v9_reply()),
            Some(BatteryReading { percent: 80 })
        );
    }

    #[test]
    fn v9_parser_rejects_unknown_or_invalid_reply() {
        assert_eq!(V9::parse(&[0x55, 0x65]), None);
        assert_eq!(V9::parse(&[0x56, 0x65, 80, 0]), None);
        assert_eq!(V9::parse(&[0x55, 0x65, 101, 0]), None);
    }

    /// Characterisation: the request itself satisfies the reply parser. Recorded so a
    /// future tightening does not silently change behaviour; the true intent is pending
    /// evidence of whether the headset echoes requests.
    #[test]
    fn v9_parser_treats_request_echo_as_level() {
        assert_eq!(
            V9::parse(&V9::request()),
            Some(BatteryReading { percent: 1 })
        );
    }

    // --- transport interaction --------------------------------------------

    #[test]
    fn a7_reader_confirms_repeated_sample() {
        let mut link =
            MockLink::with_feature_replies(vec![Ok(verified_a7_reply()), Ok(verified_a7_reply())]);
        let clock = FakeClock::default();
        assert_eq!(
            A7.read(&mut link, &clock),
            Ok(BatteryReading { percent: 86 })
        );
        // One send per attempt, and the confirmed pair stops the loop early.
        assert_eq!(link.sent_features.len(), 2);
        assert_eq!(
            clock.slept_ms.load(std::sync::atomic::Ordering::Relaxed),
            2 * A7_ATTEMPT_GAP_MS
        );
    }

    #[test]
    fn a7_reader_keeps_last_valid_sample_when_no_pair_agrees() {
        let mut first = verified_a7_reply();
        first[11] = 84 ^ 0xFF;
        let mut second = verified_a7_reply();
        second[11] = 85 ^ 0xFF;
        let mut link = MockLink::with_feature_replies(vec![
            Ok(first),
            Ok(second),
            Err("read error".to_string()),
        ]);
        let clock = FakeClock::default();
        assert_eq!(
            A7.read(&mut link, &clock),
            Ok(BatteryReading { percent: 85 })
        );
        assert_eq!(link.sent_features.len(), A7_ATTEMPTS);
    }

    #[test]
    fn a7_reader_reports_timeout_without_any_valid_sample() {
        let mut link = MockLink {
            fail_feature_send: true,
            ..Default::default()
        };
        let clock = FakeClock::default();
        let error = A7.read(&mut link, &clock).unwrap_err();
        assert_eq!(error.kind, ReadErrorKind::NoResponse);
        assert_eq!(link.sent_features.len(), A7_ATTEMPTS);
        assert_eq!(
            clock.slept_ms.load(std::sync::atomic::Ordering::Relaxed),
            A7_ATTEMPTS as u64 * A7_ATTEMPT_GAP_MS
        );
    }

    #[test]
    fn g87_reader_falls_back_to_write_when_output_report_is_rejected() {
        let mut link = MockLink::with_input_replies(vec![Ok(verified_g87_reply())]);
        link.fail_output_report = true;
        let clock = FakeClock::default();
        assert_eq!(
            G87.read(&mut link, &clock),
            Ok(BatteryReading { percent: 70 })
        );
        assert_eq!(link.sent_outputs.len(), 1);
        assert_eq!(link.writes.len(), 1);
    }

    #[test]
    fn g87_reader_reports_request_failure() {
        let mut link = MockLink {
            fail_output_report: true,
            fail_write: true,
            ..Default::default()
        };
        let clock = FakeClock::default();
        let error = G87.read(&mut link, &clock).unwrap_err();
        assert_eq!(error.kind, ReadErrorKind::RequestFailed);
    }

    #[test]
    fn g87_reader_skips_empty_reads_then_succeeds() {
        let mut link = MockLink::with_input_replies(vec![
            Ok(Vec::new()),
            Ok(vec![0xFF; 8]),
            Ok(verified_g87_reply()),
        ]);
        let clock = FakeClock::default();
        assert_eq!(
            G87.read(&mut link, &clock),
            Ok(BatteryReading { percent: 70 })
        );
    }

    #[test]
    fn g87_reader_reports_read_error_immediately() {
        let mut link = MockLink {
            fail_input_read: true,
            ..Default::default()
        };
        let clock = FakeClock::default();
        let error = G87.read(&mut link, &clock).unwrap_err();
        assert_eq!(error.kind, ReadErrorKind::ReadFailed);
    }

    #[test]
    fn g87_reader_reports_timeout_when_nothing_arrives() {
        let mut link = MockLink::default();
        let clock = FakeClock::default();
        let error = G87.read(&mut link, &clock).unwrap_err();
        assert_eq!(error.kind, ReadErrorKind::NoResponse);
        assert_eq!(link.sent_outputs.len(), 1);
    }

    #[test]
    fn v9_reader_accepts_first_valid_reply() {
        let mut link =
            MockLink::with_input_replies(vec![Ok(vec![0x00; 4]), Ok(verified_v9_reply())]);
        let clock = FakeClock::default();
        assert_eq!(
            V9.read(&mut link, &clock),
            Ok(BatteryReading { percent: 80 })
        );
    }

    #[test]
    fn v9_reader_reports_timeout_for_silent_headset() {
        let mut link = MockLink::default();
        let clock = FakeClock::default();
        let error = V9.read(&mut link, &clock).unwrap_err();
        assert_eq!(error.kind, ReadErrorKind::NoResponse);
        assert_eq!(error.detail, "耳机未连接或已关闭");
        // Exactly one query goes out, and nothing is retried behind the user's back.
        assert_eq!(link.sent_outputs.len(), 1);
    }
}
