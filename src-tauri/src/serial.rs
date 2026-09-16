//! Serial console session over the `serialport` crate. Console cables are
//! "8N1 at 9600" nine times out of ten, so those are the defaults.

use crate::session::{Session, SessionManager};
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serialport::{DataBits, Parity, SerialPort, StopBits};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::AppHandle;

/// Line settings stored on a `serial` node. Everything but `port` is optional.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SerialSettings {
    pub port: Option<String>,
    pub baud: Option<u32>,
    /// 5, 6, 7 or 8.
    pub data_bits: Option<u8>,
    /// "none" | "odd" | "even".
    pub parity: Option<String>,
    /// 1 or 2.
    pub stop_bits: Option<u8>,
}

impl SerialSettings {
    /// Resolve defaults (9600 8N1) and validate.
    pub fn resolve(&self) -> Result<(String, u32, DataBits, Parity, StopBits)> {
        let port = self.port.clone().filter(|p| !p.trim().is_empty()).ok_or_else(|| anyhow!("serial port required"))?;
        let baud = self.baud.unwrap_or(9600);
        let data_bits = match self.data_bits.unwrap_or(8) {
            5 => DataBits::Five,
            6 => DataBits::Six,
            7 => DataBits::Seven,
            8 => DataBits::Eight,
            n => return Err(anyhow!("data bits must be 5-8, got {n}")),
        };
        let parity = match self.parity.as_deref().unwrap_or("none").to_ascii_lowercase().as_str() {
            "none" | "n" => Parity::None,
            "odd" | "o" => Parity::Odd,
            "even" | "e" => Parity::Even,
            p => return Err(anyhow!("parity must be none/odd/even, got {p:?}")),
        };
        let stop_bits = match self.stop_bits.unwrap_or(1) {
            1 => StopBits::One,
            2 => StopBits::Two,
            n => return Err(anyhow!("stop bits must be 1 or 2, got {n}")),
        };
        Ok((port, baud, data_bits, parity, stop_bits))
    }

    /// "COM3 9600 8N1" for tab titles.
    pub fn label(&self) -> String {
        let parity = self.parity.as_deref().unwrap_or("none").chars().next().unwrap_or('n').to_ascii_uppercase();
        format!(
            "{} {} {}{}{}",
            self.port.as_deref().unwrap_or("?"),
            self.baud.unwrap_or(9600),
            self.data_bits.unwrap_or(8),
            parity,
            self.stop_bits.unwrap_or(1)
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PortInfo {
    pub name: String,
    /// USB product/manufacturer when known, else the port type.
    pub description: String,
}

pub fn list_ports() -> Result<Vec<PortInfo>> {
    let mut ports: Vec<PortInfo> = serialport::available_ports()
        .context("enumerate serial ports")?
        .into_iter()
        .map(|p| {
            let description = match p.port_type {
                serialport::SerialPortType::UsbPort(u) => {
                    let mut parts = Vec::new();
                    if let Some(m) = u.manufacturer { parts.push(m); }
                    if let Some(pr) = u.product { parts.push(pr); }
                    if parts.is_empty() { format!("USB {:04x}:{:04x}", u.vid, u.pid) } else { parts.join(" ") }
                }
                serialport::SerialPortType::PciPort => "PCI".into(),
                serialport::SerialPortType::BluetoothPort => "Bluetooth".into(),
                serialport::SerialPortType::Unknown => String::new(),
            };
            PortInfo { name: p.port_name, description }
        })
        .collect();
    ports.sort_by(|a, b| natural_key(&a.name).cmp(&natural_key(&b.name)));
    Ok(ports)
}

/// Sort COM3 before COM10.
fn natural_key(s: &str) -> (String, u64) {
    let digits: String = s.chars().rev().take_while(|c| c.is_ascii_digit()).collect::<Vec<_>>().into_iter().rev().collect();
    let prefix = &s[..s.len() - digits.len()];
    (prefix.to_string(), digits.parse().unwrap_or(0))
}

pub struct SerialSession {
    writer: Mutex<Box<dyn SerialPort>>,
    stop: Arc<AtomicBool>,
}

impl SerialSession {
    pub fn open(app: AppHandle, id: String, settings: &SerialSettings) -> Result<Arc<Self>> {
        let (port, baud, data_bits, parity, stop_bits) = settings.resolve()?;
        let mut reader = serialport::new(&port, baud)
            .data_bits(data_bits)
            .parity(parity)
            .stop_bits(stop_bits)
            .timeout(Duration::from_millis(100))
            .open()
            .with_context(|| format!("open {port} at {baud}"))?;
        let writer = reader.try_clone().context("clone serial handle")?;

        let stop = Arc::new(AtomicBool::new(false));
        {
            let stop = stop.clone();
            std::thread::Builder::new()
                .name(format!("serial-{port}"))
                .spawn(move || {
                    let mut buf = [0u8; 4096];
                    let reason = loop {
                        if stop.load(Ordering::Relaxed) {
                            break "closed";
                        }
                        match reader.read(&mut buf) {
                            Ok(0) => {}
                            Ok(n) => SessionManager::emit_data(&app, &id, &buf[..n]),
                            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                            Err(e) => {
                                tracing::warn!(port = %port, "serial read failed: {e}");
                                break "device disconnected";
                            }
                        }
                    };
                    SessionManager::emit_closed(&app, &id, reason);
                })
                .context("spawn serial reader thread")?;
        }
        Ok(Arc::new(Self { writer: Mutex::new(writer), stop }))
    }
}

#[async_trait]
impl Session for SerialSession {
    async fn write(&self, data: &[u8]) -> Result<()> {
        let mut w = self.writer.lock().unwrap();
        w.write_all(data)?;
        w.flush()?;
        Ok(())
    }

    /// Serial lines have no window size; the remote device cannot be told.
    async fn resize(&self, _cols: u16, _rows: u16) -> Result<()> {
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        self.stop.store(true, Ordering::Relaxed);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_9600_8n1() {
        let s = SerialSettings { port: Some("COM3".into()), ..Default::default() };
        let (port, baud, d, p, st) = s.resolve().unwrap();
        assert_eq!((port.as_str(), baud), ("COM3", 9600));
        assert!(matches!((d, p, st), (DataBits::Eight, Parity::None, StopBits::One)));
        assert_eq!(s.label(), "COM3 9600 8N1");
    }

    #[test]
    fn validates_line_settings() {
        assert!(SerialSettings::default().resolve().is_err());
        let bad = SerialSettings { port: Some("/dev/ttyUSB0".into()), data_bits: Some(9), ..Default::default() };
        assert!(bad.resolve().unwrap_err().to_string().contains("data bits"));
        let odd = SerialSettings { port: Some("x".into()), parity: Some("Even".into()), stop_bits: Some(2), baud: Some(115200), ..Default::default() };
        let (_, baud, _, p, st) = odd.resolve().unwrap();
        assert_eq!(baud, 115200);
        assert!(matches!((p, st), (Parity::Even, StopBits::Two)));
        assert_eq!(odd.label(), "x 115200 8E2");
    }

    #[test]
    fn ports_sort_naturally() {
        let mut names = vec!["COM10", "COM3", "COM1", "/dev/ttyUSB1", "/dev/ttyUSB0"];
        names.sort_by_key(|n| natural_key(n));
        assert_eq!(names, vec!["/dev/ttyUSB0", "/dev/ttyUSB1", "COM1", "COM3", "COM10"]);
    }

    #[test]
    fn list_ports_does_not_fail() {
        // Whatever this box has (possibly nothing), enumeration itself must work.
        list_ports().unwrap();
    }
}
