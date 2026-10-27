mod match_rules;
mod protocol;
mod spec;
mod transport;

pub use match_rules::InterfaceFacts;
pub use protocol::{ReadError, ReadErrorKind};
pub use spec::{DeviceSpec, RetainPolicy, SPECS};
pub use transport::{Clock, SystemClock, SystemLink};

use hidapi::{DeviceInfo, HidApi};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Upper bound for one refresh. A wedged HID call cannot be cancelled, so this is what
/// keeps the window usable instead of leaving it on "reading" until the process restarts.
const READ_DEADLINE: Duration = Duration::from_secs(8);

/// A stuck read thread never exits, so cap how many may pile up across retries.
const MAX_OUTSTANDING_READS: usize = 4;
static OUTSTANDING_READS: AtomicUsize = AtomicUsize::new(0);

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

#[derive(Clone, Debug)]
pub struct DeviceReading {
    pub kind: DeviceKind,
    pub model: &'static str,
    pub reading: Reading,
    /// Opaque identity of the interface this reading came from. Used to stop one physical
    /// device's reading being carried onto a different one; never shown.
    pub identity: Option<String>,
    retain: RetainPolicy,
    timed_out: bool,
}

impl DeviceReading {
    fn resolved(
        spec: &DeviceSpec,
        reading: Reading,
        identity: Option<String>,
        timed_out: bool,
    ) -> Self {
        Self {
            kind: spec.kind,
            model: spec.model,
            reading,
            identity,
            retain: spec.retain,
            timed_out,
        }
    }

    pub fn loading(kind: DeviceKind) -> Self {
        let spec = spec::spec_for(kind);
        Self::resolved(
            spec,
            Reading::Unavailable("正在读取…".into()),
            None,
            false,
        )
    }

    /// Builds a display-only reading for tests, without a device behind it.
    #[cfg(test)]
    pub fn from_parts(kind: DeviceKind, model: &'static str, reading: Reading) -> Self {
        Self {
            kind,
            model,
            reading,
            identity: None,
            retain: RetainPolicy::Never,
            timed_out: false,
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

pub fn read_snapshot() -> Snapshot {
    read_all()
}

fn read_all() -> Snapshot {
    let mut results: Vec<Option<DeviceReading>> = SPECS.iter().map(|_| None).collect();
    let (sender, receiver) = mpsc::channel();

    // Queries are independent, so run them at the same time. Done one after another the
    // window waits for the sum of every retry budget — about six seconds with all three
    // devices asleep, which is exactly when the user is staring at it.
    //
    // The threads are detached on purpose: a HID call can wedge inside the driver and no
    // API cancels it, so waiting for a join would freeze the window forever. The deadline
    // below is what actually bounds a refresh.
    let mut started = 0;
    for (index, spec) in SPECS.iter().enumerate() {
        if OUTSTANDING_READS.load(Ordering::Relaxed) >= MAX_OUTSTANDING_READS {
            break;
        }
        OUTSTANDING_READS.fetch_add(1, Ordering::Relaxed);
        let sender = sender.clone();
        let spawned = std::thread::Builder::new()
            .stack_size(96 * 1024)
            .spawn(move || {
                let reading = read_with_own_api(spec, &SystemClock);
                OUTSTANDING_READS.fetch_sub(1, Ordering::Relaxed);
                let _ = sender.send((index, reading));
            });
        if spawned.is_ok() {
            started += 1;
        } else {
            OUTSTANDING_READS.fetch_sub(1, Ordering::Relaxed);
        }
    }
    drop(sender);

    let deadline = Instant::now() + READ_DEADLINE;
    for _ in 0..started {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining) {
            Ok((index, reading)) => results[index] = Some(reading),
            // Out of time: whatever has not answered is reported as such, and the flag
            // that gates the next refresh is released by the caller as usual.
            Err(_) => break,
        }
    }

    assemble(results)
}

fn assemble(results: Vec<Option<DeviceReading>>) -> Snapshot {
    let mut snapshot = Snapshot {
        mouse: DeviceReading::loading(DeviceKind::Mouse),
        keyboard: DeviceReading::loading(DeviceKind::Keyboard),
        headset: DeviceReading::loading(DeviceKind::Headset),
        initial: false,
    };

    for (index, result) in results.into_iter().enumerate() {
        let reading = result.unwrap_or_else(|| timed_out_reading(&SPECS[index]));
        match reading.kind {
            DeviceKind::Mouse => snapshot.mouse = reading,
            DeviceKind::Keyboard => snapshot.keyboard = reading,
            DeviceKind::Headset => snapshot.headset = reading,
        }
    }

    snapshot
}

/// What a device shows when its query never came back. Identity is unknown, so the previous
/// value is not carried forward: it cannot be attributed to whatever is plugged in now.
fn timed_out_reading(spec: &DeviceSpec) -> DeviceReading {
    DeviceReading::resolved(
        spec,
        Reading::Unavailable("读取超时：设备未在限定时间内响应，请重新插拔接收器".into()),
        None,
        true,
    )
}

/// Each thread opens its own hidapi instance: `HidApi` is not `Sync`, and sharing one would
/// serialise the enumeration anyway.
fn read_with_own_api(spec: &DeviceSpec, clock: &dyn Clock) -> DeviceReading {
    match HidApi::new() {
        Ok(api) => read_one(&api, spec, clock),
        Err(error) => DeviceReading::resolved(
            spec,
            Reading::Unavailable(format!("HID 初始化失败：{error}")),
            None,
            false,
        ),
    }
}

/// The result of a single read-only battery query, kept structured for diagnostics.
#[derive(Clone, Debug)]
pub struct ProbeResult {
    pub model: &'static str,
    /// Whether a matching interface is plugged in. A missing receiver is a different
    /// problem from an unreachable device, and the report should not conflate them.
    pub present: bool,
    pub outcome: Result<u8, ReadError>,
}

/// Locates and queries one device. A device that is not found, or whose interface cannot
/// be opened, is never written to.
fn probe_one(api: &HidApi, spec: &DeviceSpec, clock: &dyn Clock) -> ProbeResult {
    let Some(info) = find(api, spec) else {
        return ProbeResult {
            model: spec.model,
            present: false,
            outcome: Err(ReadError::not_present(spec.missing)),
        };
    };

    let device = match info.open_device(api) {
        Ok(device) => device,
        Err(error) => {
            return ProbeResult {
                model: spec.model,
                present: true,
                outcome: Err(ReadError::open_failed(format!(
                    "{}：{error}",
                    spec.open_failure
                ))),
            };
        }
    };

    let mut link = SystemLink::new(device);
    let outcome = spec
        .protocol
        .implementation()
        .read(&mut link, clock)
        .map(|reading| reading.percent);

    ProbeResult {
        model: spec.model,
        present: true,
        outcome,
    }
}

/// Performs one read-only query per supported device and returns structured results.
///
/// Used by `--diagnose --probe`: the snapshot machinery flattens errors into display
/// text, but a bug report needs to tell "receiver missing" apart from "device asleep".
pub fn probe() -> Result<Vec<ProbeResult>, String> {
    let api = HidApi::new().map_err(|error| format!("HID 初始化失败：{error}"))?;
    Ok(SPECS
        .iter()
        .map(|spec| probe_one(&api, spec, &SystemClock))
        .collect())
}

fn read_one(api: &HidApi, spec: &DeviceSpec, clock: &dyn Clock) -> DeviceReading {
    let identity = find(api, spec).map(identity_of);
    match probe_one(api, spec, clock).outcome {
        Ok(percent) => DeviceReading::resolved(
            spec,
            Reading::Available(BatteryReading { percent }),
            identity,
            false,
        ),
        Err(error) => {
            let timed_out = error.kind == ReadErrorKind::NoResponse;
            DeviceReading::resolved(spec, Reading::Unavailable(error.detail), identity, timed_out)
        }
    }
}

fn find<'a>(api: &'a HidApi, spec: &DeviceSpec) -> Option<&'a DeviceInfo> {
    api.device_list().find(|info| {
        spec.matches(&match_rules::InterfaceFacts::new(
            info.vendor_id(),
            info.product_id(),
            info.usage_page(),
            info.usage(),
        ))
    })
}

fn identity_of(info: &DeviceInfo) -> String {
    use std::fmt::Write;

    let mut identity = String::new();
    for byte in info.path().to_bytes() {
        let _ = write!(identity, "{byte:02x}");
    }
    identity
}

/// Carries readings forward according to each device's policy. A value from a different
/// physical interface is never carried: the previous sample cannot be attributed to the
/// device now answering.
pub fn carry_forward(next: &mut Snapshot, previous: &Snapshot) {
    let pairs = [
        (&mut next.mouse, &previous.mouse),
        (&mut next.keyboard, &previous.keyboard),
        (&mut next.headset, &previous.headset),
    ];

    for (target, source) in pairs {
        carry_slot(target, source);
    }
}

fn carry_slot(next: &mut DeviceReading, previous: &DeviceReading) {
    if next.retain != RetainPolicy::OnTimeout || !next.timed_out {
        return;
    }
    if next.identity.is_none() || next.identity != previous.identity {
        return;
    }
    let last = match &previous.reading {
        Reading::Available(reading) | Reading::Stale(reading) => reading,
        Reading::Unavailable(_) => return,
    };
    next.reading = Reading::Stale(last.clone());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_land_in_their_own_slots() {
        let mouse = device(DeviceKind::Mouse, reading(42), "a", false);
        let keyboard = device(DeviceKind::Keyboard, reading(7), "b", false);
        let headset = device(
            DeviceKind::Headset,
            Reading::Unavailable("耳机未连接或已关闭".into()),
            "c",
            false,
        );

        let snapshot = assemble(vec![Some(mouse), Some(keyboard), Some(headset)]);

        assert_eq!(snapshot.mouse.reading, reading(42));
        assert_eq!(snapshot.keyboard.reading, reading(7));
        assert_eq!(
            snapshot.headset.reading,
            Reading::Unavailable("耳机未连接或已关闭".into())
        );
    }

    #[test]
    fn a_missing_result_is_reported_as_a_timeout_instead_of_loading_forever() {
        let snapshot = assemble(vec![None, None, None]);

        for reading in [&snapshot.mouse, &snapshot.keyboard, &snapshot.headset] {
            assert!(
                matches!(&reading.reading, Reading::Unavailable(message) if message.contains("读取超时")),
                "expected a timeout message, got {:?}",
                reading.reading
            );
        }
    }

    #[test]
    fn a_timeout_does_not_reuse_the_previous_value() {
        let previous = snapshot(
            device(DeviceKind::Keyboard, reading(77), "wireless-1234", false),
            device(DeviceKind::Mouse, reading(50), "mouse", false),
            device(
                DeviceKind::Headset,
                Reading::Unavailable("x".into()),
                "headset",
                false,
            ),
        );

        let mut next = assemble(vec![None, None, None]);
        carry_forward(&mut next, &previous);

        // Identity is unknown after a timeout, so nothing may be carried onto it.
        assert!(!matches!(next.keyboard.reading, Reading::Stale(_)));
    }

    /// Not part of the suite: it talks to real hardware. Run it by hand to see how long a
    /// full refresh takes with whatever is plugged in.
    #[test]
    #[ignore = "talks to real devices; run with --ignored --nocapture"]
    fn measure_full_read_latency() {
        let started = std::time::Instant::now();
        let snapshot = read_snapshot();
        let elapsed = started.elapsed();
        println!(
            "full read: {elapsed:?} | mouse {:?} | keyboard {:?} | headset {:?}",
            snapshot.mouse.reading, snapshot.keyboard.reading, snapshot.headset.reading
        );
    }

    fn reading(percent: u8) -> Reading {
        Reading::Available(BatteryReading { percent })
    }

    fn device(kind: DeviceKind, reading: Reading, identity: &str, timed_out: bool) -> DeviceReading {
        let spec = spec::spec_for(kind);
        DeviceReading::resolved(spec, reading, Some(identity.to_string()), timed_out)
    }

    fn snapshot(mouse: DeviceReading, keyboard: DeviceReading, headset: DeviceReading) -> Snapshot {
        Snapshot {
            mouse,
            keyboard,
            headset,
            initial: false,
        }
    }

    #[test]
    fn timeout_keeps_last_known_reading_for_the_same_interface() {
        let previous = snapshot(
            device(DeviceKind::Mouse, reading(68), "mouse-a", false),
            device(DeviceKind::Keyboard, reading(70), "keyboard-a", false),
            device(DeviceKind::Headset, reading(80), "headset-a", false),
        );
        let mut next = snapshot(
            device(DeviceKind::Mouse, Reading::Unavailable("无响应".into()), "mouse-a", true),
            device(DeviceKind::Keyboard, reading(70), "keyboard-a", false),
            device(DeviceKind::Headset, Reading::Unavailable("无响应".into()), "headset-a", true),
        );

        carry_forward(&mut next, &previous);

        assert_eq!(next.mouse.reading, Reading::Stale(BatteryReading { percent: 68 }));
        assert_eq!(next.keyboard.reading, reading(70));
        // The headset policy is `Never`: an unanswered headset does not keep a value.
        assert_eq!(next.headset.reading, Reading::Unavailable("无响应".into()));
    }

    #[test]
    fn removed_receiver_does_not_keep_last_reading() {
        let previous = snapshot(
            device(DeviceKind::Mouse, reading(68), "mouse-a", false),
            device(DeviceKind::Keyboard, reading(70), "keyboard-a", false),
            device(DeviceKind::Headset, reading(80), "headset-a", false),
        );
        let missing = DeviceReading::resolved(
            spec::spec_for(DeviceKind::Mouse),
            Reading::Unavailable("未检测到接收器".into()),
            None,
            false,
        );
        let mut next = snapshot(missing, previous.keyboard.clone(), previous.headset.clone());

        carry_forward(&mut next, &previous);

        assert_eq!(next.mouse.reading, Reading::Unavailable("未检测到接收器".into()));
    }

    /// The regression this guards: replacing the receiver must not inherit the reading of
    /// the device that just left.
    #[test]
    fn a_different_interface_never_inherits_the_previous_reading() {
        let previous = snapshot(
            device(DeviceKind::Mouse, reading(68), "mouse-a", false),
            device(DeviceKind::Keyboard, reading(70), "keyboard-a", false),
            device(DeviceKind::Headset, reading(80), "headset-a", false),
        );
        let mut next = snapshot(
            device(DeviceKind::Mouse, Reading::Unavailable("无响应".into()), "mouse-b", true),
            device(DeviceKind::Keyboard, Reading::Unavailable("无响应".into()), "keyboard-b", true),
            device(DeviceKind::Headset, reading(80), "headset-a", false),
        );

        carry_forward(&mut next, &previous);

        assert_eq!(next.mouse.reading, Reading::Unavailable("无响应".into()));
        assert_eq!(next.keyboard.reading, Reading::Unavailable("无响应".into()));
    }

    #[test]
    fn a_stale_reading_stays_stale_on_the_next_timeout() {
        let previous = snapshot(
            device(DeviceKind::Mouse, Reading::Stale(BatteryReading { percent: 68 }), "mouse-a", false),
            device(DeviceKind::Keyboard, reading(70), "keyboard-a", false),
            device(DeviceKind::Headset, reading(80), "headset-a", false),
        );
        let mut next = snapshot(
            device(DeviceKind::Mouse, Reading::Unavailable("无响应".into()), "mouse-a", true),
            device(DeviceKind::Keyboard, reading(70), "keyboard-a", false),
            device(DeviceKind::Headset, reading(80), "headset-a", false),
        );

        carry_forward(&mut next, &previous);

        assert_eq!(next.mouse.reading, Reading::Stale(BatteryReading { percent: 68 }));
    }

    #[test]
    fn a_failure_that_is_not_a_timeout_does_not_carry() {
        let previous = snapshot(
            device(DeviceKind::Keyboard, reading(70), "keyboard-a", false),
            device(DeviceKind::Keyboard, reading(70), "keyboard-a", false),
            device(DeviceKind::Headset, reading(80), "headset-a", false),
        );
        let mut next = snapshot(
            device(
                DeviceKind::Keyboard,
                Reading::Unavailable("读取失败，请重新插拔接收器".into()),
                "keyboard-a",
                false,
            ),
            device(DeviceKind::Keyboard, reading(70), "keyboard-a", false),
            device(DeviceKind::Headset, reading(80), "headset-a", false),
        );

        carry_forward(&mut next, &previous);

        assert_eq!(
            next.mouse.reading,
            Reading::Unavailable("读取失败，请重新插拔接收器".into())
        );
    }

    #[test]
    fn first_refresh_has_nothing_to_carry() {
        let previous = Snapshot::loading();
        let mut next = snapshot(
            device(DeviceKind::Mouse, Reading::Unavailable("无响应".into()), "mouse-a", true),
            DeviceReading::loading(DeviceKind::Keyboard),
            DeviceReading::loading(DeviceKind::Headset),
        );

        carry_forward(&mut next, &previous);

        assert_eq!(next.mouse.reading, Reading::Unavailable("无响应".into()));
    }
}
