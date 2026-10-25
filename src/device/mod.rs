mod match_rules;
mod protocol;
mod spec;
mod transport;

pub use protocol::ReadErrorKind;
pub use spec::{DeviceSpec, RetainPolicy, Slot, SPECS};
pub use transport::{Clock, SystemClock, SystemLink};

use hidapi::{DeviceInfo, HidApi};

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

    fn all_unavailable(message: &str) -> Self {
        let reading = |kind| {
            DeviceReading::resolved(spec::spec_for(kind), Reading::Unavailable(message.into()), None, false)
        };
        Self {
            mouse: reading(DeviceKind::Mouse),
            keyboard: reading(DeviceKind::Keyboard),
            headset: reading(DeviceKind::Headset),
            initial: false,
        }
    }
}

pub fn read_snapshot() -> Snapshot {
    match HidApi::new() {
        Ok(api) => read_all(&api, &SystemClock),
        Err(error) => Snapshot::all_unavailable(&format!("HID 初始化失败：{error}")),
    }
}

fn read_all(api: &HidApi, clock: &dyn Clock) -> Snapshot {
    let mut mouse = DeviceReading::loading(DeviceKind::Mouse);
    let mut keyboard = DeviceReading::loading(DeviceKind::Keyboard);
    let mut headset = DeviceReading::loading(DeviceKind::Headset);

    for spec in SPECS.iter() {
        let reading = read_one(api, spec, clock);
        match spec.slot {
            Slot::Mouse => mouse = reading,
            Slot::Keyboard => keyboard = reading,
            Slot::Headset => headset = reading,
        }
    }

    Snapshot {
        mouse,
        keyboard,
        headset,
        initial: false,
    }
}

/// Locates and queries one device. A device that is not found, or whose interface cannot
/// be opened, is never written to.
fn read_one(api: &HidApi, spec: &DeviceSpec, clock: &dyn Clock) -> DeviceReading {
    let Some(info) = find(api, spec) else {
        return DeviceReading::resolved(
            spec,
            Reading::Unavailable(spec.missing.into()),
            None,
            false,
        );
    };

    let identity = Some(identity_of(info));
    let device = match info.open_device(api) {
        Ok(device) => device,
        Err(error) => {
            return DeviceReading::resolved(
                spec,
                Reading::Unavailable(format!("{}：{error}", spec.open_failure)),
                identity,
                false,
            );
        }
    };

    let mut link = SystemLink::new(device);
    let query = spec.protocol.implementation().read(&mut link, clock);
    match query {
        Ok(reading) => DeviceReading::resolved(spec, Reading::Available(reading), identity, false),
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
