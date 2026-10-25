//! Facts about a single HID interface, detached from hidapi so that matching can be
//! tested without hardware.

use crate::device::spec::DeviceSpec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InterfaceFacts {
    pub vendor_id: u16,
    pub product_id: u16,
    pub usage_page: u16,
    pub usage: u16,
}

impl InterfaceFacts {
    pub fn new(vendor_id: u16, product_id: u16, usage_page: u16, usage: u16) -> Self {
        Self {
            vendor_id,
            product_id,
            usage_page,
            usage,
        }
    }
}

impl DeviceSpec {
    /// Four-field match. This locates a candidate interface; it is not proof that a
    /// particular model, firmware or connection mode speaks the protocol.
    pub fn matches(&self, facts: &InterfaceFacts) -> bool {
        facts.vendor_id == self.vendor_id
            && facts.product_id == self.product_id
            && facts.usage_page == self.usage_page
            && facts.usage == self.usage
    }
}
