//! The only place that talks to the operating system. Protocols work against `Link`, so
//! every exchange can be replayed offline without a device.

use std::thread;
use std::time::Duration;

use hidapi::HidDevice;

/// A bidirectional HID link. Every method is a single synchronous exchange, matching what
/// hidapi offers on Windows.
pub trait Link {
    fn send_feature(&mut self, report: &[u8]) -> Result<(), String>;
    fn get_feature(&mut self, buffer: &mut [u8]) -> Result<usize, String>;
    fn send_output(&mut self, report: &[u8]) -> Result<(), String>;
    fn read_timeout(&mut self, buffer: &mut [u8], timeout_ms: i32) -> Result<usize, String>;
}

/// Sleeping is injected so that offline tests do not spend the protocol's real budget.
pub trait Clock: Sync {
    fn sleep_ms(&self, millis: u64);
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn sleep_ms(&self, millis: u64) {
        thread::sleep(Duration::from_millis(millis));
    }
}

pub struct SystemLink {
    device: HidDevice,
}

impl SystemLink {
    pub fn new(device: HidDevice) -> Self {
        Self { device }
    }
}

impl Link for SystemLink {
    fn send_feature(&mut self, report: &[u8]) -> Result<(), String> {
        self.device
            .send_feature_report(report)
            .map_err(|error| error.to_string())
    }

    fn get_feature(&mut self, buffer: &mut [u8]) -> Result<usize, String> {
        self.device
            .get_feature_report(buffer)
            .map_err(|error| error.to_string())
    }

    fn send_output(&mut self, report: &[u8]) -> Result<(), String> {
        self.device
            .send_output_report(report)
            .or_else(|_| self.device.write(report).map(|_| ()))
            .map_err(|error| error.to_string())
    }

    fn read_timeout(&mut self, buffer: &mut [u8], timeout_ms: i32) -> Result<usize, String> {
        self.device
            .read_timeout(buffer, timeout_ms)
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
pub(crate) mod mock {
    use super::{Clock, Link};

    /// A scripted link: replies are consumed in order, and every write is recorded so a
    /// test can assert that nothing was sent.
    #[derive(Default)]
    pub struct MockLink {
        pub feature_replies: Vec<Result<Vec<u8>, String>>,
        pub input_replies: Vec<Result<Vec<u8>, String>>,
        pub sent_features: Vec<Vec<u8>>,
        pub sent_outputs: Vec<Vec<u8>>,
        pub writes: Vec<Vec<u8>>,
        pub fail_feature_send: bool,
        /// The output report is rejected, so the transport must retry with `write`.
        pub fail_output_report: bool,
        /// The `write` retry also fails; only meaningful with `fail_output_report`.
        pub fail_write: bool,
        pub fail_input_read: bool,
    }

    impl MockLink {
        pub fn with_feature_replies(replies: Vec<Result<Vec<u8>, String>>) -> Self {
            Self {
                feature_replies: replies,
                ..Default::default()
            }
        }

        pub fn with_input_replies(replies: Vec<Result<Vec<u8>, String>>) -> Self {
            Self {
                input_replies: replies,
                ..Default::default()
            }
        }
    }

    impl Link for MockLink {
        fn send_feature(&mut self, report: &[u8]) -> Result<(), String> {
            self.sent_features.push(report.to_vec());
            if self.fail_feature_send {
                return Err("mock feature send failed".to_string());
            }
            Ok(())
        }

        fn get_feature(&mut self, buffer: &mut [u8]) -> Result<usize, String> {
            if self.feature_replies.is_empty() {
                return Err("mock has no feature reply".to_string());
            }
            let reply = self.feature_replies.remove(0)?;
            let length = reply.len().min(buffer.len());
            buffer[..length].copy_from_slice(&reply[..length]);
            Ok(length)
        }

        fn send_output(&mut self, report: &[u8]) -> Result<(), String> {
            // Same composition as the system link: output report first, `write` as the
            // fallback, so a protocol test exercises the real retry path.
            self.sent_outputs.push(report.to_vec());
            if !self.fail_output_report {
                return Ok(());
            }
            self.writes.push(report.to_vec());
            if self.fail_write {
                return Err("mock write failed".to_string());
            }
            Ok(())
        }

        fn read_timeout(&mut self, buffer: &mut [u8], _timeout_ms: i32) -> Result<usize, String> {
            if self.fail_input_read {
                return Err("mock read failed".to_string());
            }
            if self.input_replies.is_empty() {
                return Ok(0);
            }
            let reply = self.input_replies.remove(0)?;
            let length = reply.len().min(buffer.len());
            buffer[..length].copy_from_slice(&reply[..length]);
            Ok(length)
        }
    }

    /// Records the requested delays instead of waiting for them.
    #[derive(Default)]
    pub struct FakeClock {
        pub slept_ms: std::sync::atomic::AtomicU64,
    }

    impl Clock for FakeClock {
        fn sleep_ms(&self, millis: u64) {
            self.slept_ms
                .fetch_add(millis, std::sync::atomic::Ordering::Relaxed);
        }
    }
}
