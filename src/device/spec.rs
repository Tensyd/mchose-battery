//! The device table. One row per supported device, and nothing else: a row is a promise
//! that the model, the interface and the protocol were checked together on real hardware.
//!
//! Adding a row is not how a model becomes supported. A candidate needs protocol evidence
//! and, separately, a pass on the target hardware in this program; an unverified model
//! would only turn a working card into a wrong number.

use crate::device::DeviceKind;
use crate::device::protocol::{A7_PROTOCOL, G87_PROTOCOL, Protocol, V9_PROTOCOL};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetainPolicy {
    /// Keep the last reading, greyed out, when the device stops answering. Only used
    /// where a sleeping device is a normal state rather than a failure.
    OnTimeout,
    /// Never show a value the device did not report in this refresh.
    Never,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolId {
    A7,
    G87,
    V9,
}

impl ProtocolId {
    pub fn implementation(self) -> &'static dyn Protocol {
        match self {
            ProtocolId::A7 => &A7_PROTOCOL,
            ProtocolId::G87 => &G87_PROTOCOL,
            ProtocolId::V9 => &V9_PROTOCOL,
        }
    }
}

pub struct DeviceSpec {
    pub kind: DeviceKind,
    pub model: &'static str,
    pub vendor_id: u16,
    pub product_id: u16,
    pub usage_page: u16,
    pub usage: u16,
    pub protocol: ProtocolId,
    pub retain: RetainPolicy,
    /// Prefix for an open failure, so the message still names the device.
    pub open_failure: &'static str,
    /// Shown when the receiver is absent.
    pub missing: &'static str,
}

/// Supported devices, verified on hardware in this program. Order matches the UI.
pub static SPECS: [DeviceSpec; 3] = [
    DeviceSpec {
        kind: DeviceKind::Mouse,
        model: "A7 V2 Ultra+",
        vendor_id: 0x3837,
        product_id: 0x100B,
        usage_page: 0xFF01,
        usage: 1,
        protocol: ProtocolId::A7,
        retain: RetainPolicy::OnTimeout,
        open_failure: "设备接口打开失败",
        missing: "未检测到接收器",
    },
    DeviceSpec {
        kind: DeviceKind::Keyboard,
        model: "G87 V2",
        vendor_id: 0x3837,
        product_id: 0x201B,
        usage_page: 0xFF02,
        usage: 2,
        protocol: ProtocolId::G87,
        retain: RetainPolicy::OnTimeout,
        open_failure: "设备接口打开失败",
        missing: "未检测到接收器",
    },
    DeviceSpec {
        kind: DeviceKind::Headset,
        model: "V9 PRO",
        vendor_id: 0x291D,
        product_id: 0x385D,
        usage_page: 0xFF90,
        usage: 1,
        protocol: ProtocolId::V9,
        // A receiver cannot tell a switched-off headset from an out-of-range one, and a
        // stale headset value would be a guess, so this slot never carries.
        retain: RetainPolicy::Never,
        open_failure: "耳机接口打开失败",
        missing: "未检测到耳机接收器",
    },
];

pub fn spec_for(kind: DeviceKind) -> &'static DeviceSpec {
    SPECS
        .iter()
        .find(|spec| spec.kind == kind)
        .expect("every device kind has a spec")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::match_rules::InterfaceFacts;

    #[test]
    fn table_has_one_row_per_slot_and_kind() {
        assert_eq!(SPECS.len(), 3);
        let mut models: Vec<&str> = SPECS.iter().map(|spec| spec.model).collect();
        models.sort_unstable();
        assert_eq!(models, ["A7 V2 Ultra+", "G87 V2", "V9 PRO"]);

        for kind in [DeviceKind::Mouse, DeviceKind::Keyboard, DeviceKind::Headset] {
            assert_eq!(spec_for(kind).kind, kind);
        }
    }

    #[test]
    fn every_usage_page_is_a_vendor_page() {
        for spec in SPECS.iter() {
            assert!(
                spec.usage_page >= 0xFF00,
                "{} must match a vendor collection",
                spec.model
            );
        }
    }

    #[test]
    fn matching_requires_all_four_fields() {
        let mouse = spec_for(DeviceKind::Mouse);
        assert!(mouse.matches(&InterfaceFacts::new(0x3837, 0x100B, 0xFF01, 1)));

        // Each field independently rejects, so a sibling collection on the same
        // interface is never queried by accident.
        assert!(!mouse.matches(&InterfaceFacts::new(0x3838, 0x100B, 0xFF01, 1)));
        assert!(!mouse.matches(&InterfaceFacts::new(0x3837, 0x100C, 0xFF01, 1)));
        assert!(!mouse.matches(&InterfaceFacts::new(0x3837, 0x100B, 0xFF02, 1)));
        assert!(!mouse.matches(&InterfaceFacts::new(0x3837, 0x100B, 0xFF01, 2)));
    }

    #[test]
    fn unknown_and_sibling_interfaces_do_not_match_any_spec() {
        let observations = [
            // The keyboard receiver's own interface.
            InterfaceFacts::new(0x3837, 0x201B, 0xFF02, 2),
            // The mouse's dead sibling collection seen on this hardware.
            InterfaceFacts::new(0x3837, 0x100B, 0xFF0B, 0x104),
            // A model nobody has verified here.
            InterfaceFacts::new(0x5253, 0x1020, 0xFF01, 1),
        ];

        // Only the keyboard row is expected to match its own interface.
        let matches: Vec<&str> = SPECS
            .iter()
            .filter(|spec| observations.iter().any(|facts| spec.matches(facts)))
            .map(|spec| spec.model)
            .collect();
        assert_eq!(matches, ["G87 V2"]);
    }

    #[test]
    fn headset_never_carries_a_stale_value() {
        assert_eq!(spec_for(DeviceKind::Headset).retain, RetainPolicy::Never);
        assert_eq!(spec_for(DeviceKind::Mouse).retain, RetainPolicy::OnTimeout);
        assert_eq!(
            spec_for(DeviceKind::Keyboard).retain,
            RetainPolicy::OnTimeout
        );
    }
}
