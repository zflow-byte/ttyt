use serialport::{SerialPortInfo, SerialPortType};

use crate::error::CoreError;

/// A serial port discovered by [`scan`], with USB metadata resolved where
/// the OS/driver provides it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCandidate {
    pub port_name: String,
    pub usb_vendor: Option<String>,
    pub usb_product: Option<String>,
    pub suggested_bauds: Vec<u32>,
}

/// Enumerate serial ports on this machine, filtered to the current
/// platform's USB-serial naming convention (see [`is_callout_device`]).
///
/// `baud_candidates` (normally `Config::baud_candidates`) is attached to
/// every result as the set of bauds to offer for that port; the scanner
/// does not probe the device to guess a baud rate.
///
/// Note: this cannot be exercised end-to-end without physical USB-serial
/// hardware attached; `to_candidate`/`friendly_vendor_name`/
/// `is_callout_device_for` below carry the unit-testable logic, and this
/// function itself needs a manual hardware test to fully verify. macOS is
/// this project's only supported platform (per the design doc); the Linux
/// branch below is untested groundwork (Task 3.8), not a completed port --
/// the README's platform support section says so explicitly.
pub fn scan(baud_candidates: &[u32]) -> Result<Vec<DeviceCandidate>, CoreError> {
    let ports = serialport::available_ports()?;
    Ok(ports
        .into_iter()
        .filter(|p| is_callout_device(&p.port_name))
        .map(|p| to_candidate(p, baud_candidates))
        .collect())
}

fn is_callout_device(port_name: &str) -> bool {
    is_callout_device_for(port_name, std::env::consts::OS)
}

/// The actual filter logic, parameterized on a target-OS string rather
/// than reading `cfg!(target_os = ..)` directly, so both platforms' naming
/// conventions can be unit tested on whichever machine happens to be
/// building this crate -- a `#[cfg(target_os = "linux")]`-gated test would
/// simply never run in this (or most contributors') macOS-only dev
/// environment, silently going unverified.
fn is_callout_device_for(port_name: &str, target_os: &str) -> bool {
    match target_os {
        // `/dev/cu.*` per spec -- `/dev/tty.*` is the same underlying
        // hardware exposed a second time and would otherwise double-list
        // every adapter.
        "macos" => port_name.contains("cu."),
        // `/dev/ttyUSB*` (common USB-serial chipsets: FTDI, CP210x,
        // CH340) and `/dev/ttyACM*` (USB CDC-ACM, e.g. many Cisco USB
        // console cables) -- untested groundwork, see this function's
        // caller's doc comment. Deliberately excludes bare `/dev/ttyS*`
        // (built-in motherboard UARTs): this scanner is for USB-serial
        // console adapters, the same class of device macOS's `cu.*`
        // filter targets, not arbitrary onboard serial ports.
        "linux" => port_name.contains("ttyUSB") || port_name.contains("ttyACM"),
        // Windows/other: enumeration rules are future work (Task 3.9's
        // design note); until then, don't filter out ports.
        _ => true,
    }
}

fn to_candidate(port: SerialPortInfo, baud_candidates: &[u32]) -> DeviceCandidate {
    let (usb_vendor, usb_product) = match &port.port_type {
        SerialPortType::UsbPort(info) => (
            Some(
                info.manufacturer
                    .clone()
                    .unwrap_or_else(|| friendly_vendor_name(info.vid)),
            ),
            info.product.clone(),
        ),
        SerialPortType::PciPort | SerialPortType::BluetoothPort | SerialPortType::Unknown => {
            (None, None)
        }
    };

    DeviceCandidate {
        port_name: port.port_name,
        usb_vendor,
        usb_product,
        suggested_bauds: baud_candidates.to_vec(),
    }
}

/// Fallback vendor name lookup by USB VID, used when the OS/driver didn't
/// report a manufacturer string. Unrecognized VIDs return their raw hex
/// value rather than `None`, so unfamiliar-but-present hardware is still
/// visible to the user instead of silently disappearing.
fn friendly_vendor_name(vid: u16) -> String {
    match vid {
        0x0403 => "FTDI".to_string(),
        0x10C4 => "Silicon Labs (CP210x)".to_string(),
        0x1A86 => "QinHeng (CH340/CH341)".to_string(),
        0x067B => "Prolific".to_string(),
        other => format!("Unknown (VID 0x{other:04x})"),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use serialport::UsbPortInfo;

    fn usb_port(port_name: &str, vid: u16, manufacturer: Option<&str>) -> SerialPortInfo {
        SerialPortInfo {
            port_name: port_name.to_string(),
            port_type: SerialPortType::UsbPort(UsbPortInfo {
                vid,
                pid: 0x6001,
                serial_number: None,
                manufacturer: manufacturer.map(str::to_string),
                product: Some("USB-Serial Adapter".to_string()),
            }),
        }
    }

    #[test]
    fn known_ftdi_vid_maps_to_friendly_name_when_os_reports_none() {
        let port = usb_port("/dev/cu.usbserial-1410", 0x0403, None);
        let candidate = to_candidate(port, &[9600, 115200]);
        assert_eq!(candidate.usb_vendor.as_deref(), Some("FTDI"));
        assert_eq!(candidate.suggested_bauds, vec![9600, 115200]);
    }

    #[test]
    fn os_reported_manufacturer_takes_precedence_over_lookup_table() {
        let port = usb_port(
            "/dev/cu.usbserial-1410",
            0x0403,
            Some("Future Technology Devices"),
        );
        let candidate = to_candidate(port, &[9600]);
        assert_eq!(
            candidate.usb_vendor.as_deref(),
            Some("Future Technology Devices")
        );
    }

    #[test]
    fn unknown_vid_falls_back_to_hex_id_instead_of_none_or_panic() {
        let port = usb_port("/dev/cu.usbserial-9999", 0xFFFF, None);
        let candidate = to_candidate(port, &[9600]);
        assert_eq!(
            candidate.usb_vendor.as_deref(),
            Some("Unknown (VID 0xffff)")
        );
    }

    #[test]
    fn non_usb_port_has_no_vendor_or_product() {
        let port = SerialPortInfo {
            port_name: "/dev/cu.Bluetooth-Incoming-Port".to_string(),
            port_type: SerialPortType::BluetoothPort,
        };
        let candidate = to_candidate(port, &[9600]);
        assert_eq!(candidate.usb_vendor, None);
        assert_eq!(candidate.usb_product, None);
    }

    #[test]
    fn macos_filter_excludes_tty_dup_and_keeps_cu_device() {
        assert!(is_callout_device("/dev/cu.usbserial-1410"));
        if cfg!(target_os = "macos") {
            assert!(!is_callout_device("/dev/tty.usbserial-1410"));
        }
    }

    /// `is_callout_device_for` is parameterized specifically so both
    /// platforms' naming rules can be verified here regardless of which
    /// platform is actually building/running this test -- see its doc
    /// comment for why a `#[cfg(target_os = "linux")]`-gated test would be
    /// the wrong tool (it would never run on a macOS dev machine).
    #[test]
    fn macos_naming_rule_keeps_cu_and_excludes_tty_dup() {
        assert!(is_callout_device_for("/dev/cu.usbserial-1410", "macos"));
        assert!(!is_callout_device_for("/dev/tty.usbserial-1410", "macos"));
    }

    #[test]
    fn linux_naming_rule_keeps_ttyusb_and_ttyacm() {
        assert!(is_callout_device_for("/dev/ttyUSB0", "linux"));
        assert!(is_callout_device_for("/dev/ttyACM0", "linux"));
    }

    #[test]
    fn linux_naming_rule_excludes_bare_onboard_serial_ports() {
        assert!(!is_callout_device_for("/dev/ttyS0", "linux"));
    }

    #[test]
    fn unrecognized_platform_does_not_filter_out_any_port() {
        assert!(is_callout_device_for(
            "/dev/whatever-this-platform-calls-it",
            "windows"
        ));
    }
}
