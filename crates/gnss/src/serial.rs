//! The desktop's port: a serial device — almost always a USB-to-UART bridge on
//! the receiver's board (FTDI, Silicon Labs, WCH) or the receiver's own USB
//! CDC-ACM interface.
//!
//! **"Automatic" picks a USB serial port, preferring the bridges GNSS boards
//! use.** A laptop has few serial ports and a receiver plugged in is usually
//! the only USB one, so the guess is right in the common case and the setting
//! names a port for the rest. The guess is re-made on every open, so a board
//! that comes back as `ttyUSB1` after a replug is still found.
//!
//! **On Linux the commonest failure is a permission, not a fault.** The device
//! node belongs to a group (`uucp` on Arch, `dialout` on Debian and Ubuntu) and
//! a fresh account is not in it. That error is reported with the group's name
//! in it, because "Permission denied" alone sends people looking for a broken
//! cable.

use std::io::{Read, Write};
use std::time::Duration;

use crate::receiver::Port;

/// USB vendor ids of the bridges and receivers GNSS boards are built with.
const GNSS_VENDORS: &[(u16, &str)] = &[
    (0x0403, "FTDI"),
    (0x10C4, "Silicon Labs"),
    (0x1A86, "WCH"),
    (0x1546, "u-blox"),
    (0x067B, "Prolific"),
];

/// A port a person could choose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortInfo {
    /// What to open, e.g. `/dev/ttyUSB0` or `COM4`.
    pub path: String,
    /// How to show it: the path, and the USB product where known.
    pub label: String,
    pub usb: bool,
    /// A bridge GNSS boards are known to use.
    pub likely: bool,
}

/// The serial ports on this machine, the likeliest receiver first.
#[must_use]
pub fn ports() -> Vec<PortInfo> {
    let mut out: Vec<PortInfo> = serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .map(|p| match p.port_type {
            serialport::SerialPortType::UsbPort(u) => {
                let vendor = GNSS_VENDORS.iter().find(|(v, _)| *v == u.vid);
                let product = u
                    .product
                    .or_else(|| vendor.map(|(_, n)| (*n).to_string()))
                    .unwrap_or_else(|| format!("USB {:04x}:{:04x}", u.vid, u.pid));
                PortInfo {
                    label: format!("{product} ({})", p.port_name),
                    path: p.port_name,
                    usb: true,
                    likely: vendor.is_some(),
                }
            }
            _ => PortInfo {
                label: p.port_name.clone(),
                path: p.port_name,
                usb: false,
                likely: false,
            },
        })
        .collect();
    // macOS lists every device twice, as a dial-in `tty.` and a call-out `cu.`;
    // only the call-out one opens without waiting for a carrier.
    out.retain(|p| !p.path.starts_with("/dev/tty."));
    out.sort_by_key(|p| (!p.likely, !p.usb, p.path.clone()));
    out
}

/// Open `path` (or, when empty, the likeliest USB port) at `baud`.
pub fn open(path: &str, baud: u32) -> Result<(Box<dyn Port>, String), String> {
    let (path, label) = if path.trim().is_empty() {
        let found = ports().into_iter().find(|p| p.usb).ok_or_else(|| {
            "no USB serial device found — is the receiver plugged in?".to_string()
        })?;
        (found.path, found.label)
    } else {
        (path.trim().to_string(), path.trim().to_string())
    };
    let port = serialport::new(&path, baud)
        .timeout(Duration::from_millis(100))
        .open()
        .map_err(|e| describe_error(&path, &e))?;
    Ok((Box::new(Serial(port)), format!("{label}, {baud} baud")))
}

fn describe_error(path: &str, e: &serialport::Error) -> String {
    let denied = matches!(
        e.kind(),
        serialport::ErrorKind::Io(std::io::ErrorKind::PermissionDenied)
    ) || e.description.contains("Permission denied");
    if denied {
        #[cfg(unix)]
        if let Some(group) = owning_group(path) {
            return format!(
                "no permission to open {path} — add yourself to the “{group}” group \
                 (sudo usermod -aG {group} $USER) and log in again"
            );
        }
        return format!("no permission to open {path}");
    }
    format!("{path}: {}", e.description)
}

/// The name of the group that owns a device node, from `/etc/group`.
#[cfg(unix)]
fn owning_group(path: &str) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let gid = std::fs::metadata(path).ok()?.gid();
    let groups = std::fs::read_to_string("/etc/group").ok()?;
    group_name(&groups, gid)
}

#[cfg(unix)]
fn group_name(etc_group: &str, gid: u32) -> Option<String> {
    etc_group.lines().find_map(|l| {
        let mut f = l.split(':');
        let name = f.next()?;
        f.next()?;
        (f.next()?.parse::<u32>().ok()? == gid).then(|| name.to_string())
    })
}

struct Serial(Box<dyn serialport::SerialPort>);

impl Port for Serial {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        match self.0.read(buf) {
            // A tty that reads zero bytes without a timeout has hung up.
            Ok(0) => Err("the receiver was disconnected".into()),
            Ok(n) => Ok(n),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Ok(0),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => Ok(0),
            Err(e) => Err(e.to_string()),
        }
    }

    fn write(&mut self, data: &[u8]) -> Result<(), String> {
        self.0.write_all(data).map_err(|e| e.to_string())?;
        self.0.flush().map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn the_group_is_named_from_etc_group() {
        let etc = "root:x:0:root\nuucp:x:14:\ndialout:x:20:jeroen\n";
        assert_eq!(super::group_name(etc, 14).as_deref(), Some("uucp"));
        assert_eq!(super::group_name(etc, 99), None);
    }
}
