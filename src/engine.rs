use std::{fs::File, io::Write, path::Path, time::Duration};

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use regex::Regex;
use rusqlite::{params, Connection};

use crate::types::{
    ChecksumKind, Direction, DisplaySettings, ExportByteView, ExportFormat, FramingMode, LogRecord,
    ProtocolDecoder, TimestampMode,
};

pub fn parse_hex(input: &str) -> Result<Vec<u8>> {
    let cleaned: String = input
        .chars()
        .filter(|c| !c.is_whitespace() && *c != ':' && *c != ',')
        .collect();
    let stripped = cleaned.replace("0x", "").replace("0X", "");
    if stripped.is_empty() {
        return Ok(Vec::new());
    }
    if stripped.len() % 2 != 0 {
        return Err(anyhow!("Hex data needs an even number of digits"));
    }
    hex::decode(&stripped).context("Invalid hexadecimal input")
}

pub fn ascii(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| match *b {
            0x20..=0x7e => *b as char,
            b'\n' => '↵',
            b'\r' => '␍',
            b'\t' => '⇥',
            _ => '·',
        })
        .collect()
}

pub fn hex_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}
pub fn octal_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:03o}"))
        .collect::<Vec<_>>()
        .join(" ")
}
pub fn binary_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:08b}"))
        .collect::<Vec<_>>()
        .join(" ")
}
pub fn decimal_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:03}"))
        .collect::<Vec<_>>()
        .join(" ")
}
pub fn mixed_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| {
            let c = if (0x20..=0x7e).contains(b) {
                *b as char
            } else {
                '·'
            };
            format!("[{b:02X}] {c}")
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

pub fn checksum(bytes: &[u8], kind: ChecksumKind) -> Vec<u8> {
    match kind {
        ChecksumKind::None => vec![],
        ChecksumKind::Xor8 => vec![bytes.iter().fold(0u8, |a, b| a ^ b)],
        ChecksumKind::Add8 => vec![bytes.iter().fold(0u8, |a, b| a.wrapping_add(*b))],
        ChecksumKind::Lrc => {
            vec![0u8.wrapping_sub(bytes.iter().fold(0u8, |a, b| a.wrapping_add(*b)))]
        }
        ChecksumKind::ModbusCrc16 => {
            let mut crc = 0xFFFFu16;
            for b in bytes {
                crc ^= *b as u16;
                for _ in 0..8 {
                    crc = if crc & 1 != 0 {
                        (crc >> 1) ^ 0xA001
                    } else {
                        crc >> 1
                    };
                }
            }
            crc.to_le_bytes().to_vec()
        }
        ChecksumKind::Crc32 => {
            let mut crc = 0xFFFF_FFFFu32;
            for b in bytes {
                crc ^= *b as u32;
                for _ in 0..8 {
                    crc = if crc & 1 != 0 {
                        (crc >> 1) ^ 0xEDB8_8320
                    } else {
                        crc >> 1
                    };
                }
            }
            (!crc).to_be_bytes().to_vec()
        }
    }
}

pub fn decode(bytes: &[u8], decoder: ProtocolDecoder) -> String {
    match decoder {
        ProtocolDecoder::None => String::new(),
        ProtocolDecoder::Nmea0183 => {
            let s = String::from_utf8_lossy(bytes);
            if s.starts_with('$') || s.starts_with('!') {
                let sentence = s.split('*').next().unwrap_or(&s);
                let fields: Vec<&str> = sentence.split(',').collect();
                if fields.len() > 1 {
                    format!("NMEA {} | {} field(s)", fields[0], fields.len() - 1)
                } else {
                    "NMEA sentence".into()
                }
            } else {
                "NMEA: waiting for sentence".into()
            }
        }
        ProtocolDecoder::ModbusRtu => {
            if bytes.len() >= 4 {
                format!(
                    "MODBUS addr={} function=0x{:02X} payload={} byte(s)",
                    bytes[0],
                    bytes[1],
                    bytes.len().saturating_sub(4)
                )
            } else {
                "MODBUS: incomplete frame".into()
            }
        }
        ProtocolDecoder::Dmx512 => {
            if bytes.is_empty() {
                "DMX: empty".into()
            } else {
                format!(
                    "DMX start code={} · {} channel byte(s)",
                    bytes[0],
                    bytes.len().saturating_sub(1)
                )
            }
        }
        ProtocolDecoder::Midi => {
            if let Some(status) = bytes.first() {
                let kind = match status & 0xF0 {
                    0x80 => "Note Off",
                    0x90 => "Note On",
                    0xA0 => "Poly Pressure",
                    0xB0 => "Control Change",
                    0xC0 => "Program Change",
                    0xD0 => "Channel Pressure",
                    0xE0 => "Pitch Bend",
                    _ => "System / Data",
                };
                format!("MIDI {kind} · channel {}", (status & 0x0F) + 1)
            } else {
                "MIDI: empty".into()
            }
        }
        ProtocolDecoder::CanAdapter => {
            let text = String::from_utf8_lossy(bytes);
            if text.starts_with('t') || text.starts_with('T') {
                format!("CAN adapter frame: {}", text.trim())
            } else {
                "CAN adapter: waiting for ASCII frame".into()
            }
        }
    }
}

pub fn anonymize(text: &str) -> String {
    let mac = Regex::new(r"(?i)\b(?:[0-9a-f]{2}:){5}[0-9a-f]{2}\b").unwrap();
    let serial = Regex::new(r"(?i)\b(?:sn|serial|imei|uuid)\s*[:=]\s*[^\s,;]+").unwrap();
    let text = mac.replace_all(text, "[MAC-REDACTED]");
    serial
        .replace_all(&text, "[IDENTIFIER-REDACTED]")
        .into_owned()
}

pub fn render_record(record: &LogRecord, display: &DisplaySettings) -> String {
    let mut columns = Vec::new();
    let timestamp = match display.timestamps {
        TimestampMode::Off => String::new(),
        TimestampMode::Absolute => record.timestamp.format("%H:%M:%S%.6f").to_string(),
        TimestampMode::Relative => format!("+{:>10.6}s", record.relative_us as f64 / 1_000_000.0),
        TimestampMode::Delta => format!("Δ{:>10.6}s", record.delta_us as f64 / 1_000_000.0),
    };
    if !timestamp.is_empty() {
        columns.push(timestamp);
    }
    if display.mixed {
        columns.push(mixed_bytes(&record.bytes));
    } else {
        if display.hex {
            columns.push(hex_bytes(&record.bytes));
        }
        if display.ascii {
            columns.push(ascii(&record.bytes));
        }
        if display.octal {
            columns.push(octal_bytes(&record.bytes));
        }
        if display.binary {
            columns.push(binary_bytes(&record.bytes));
        }
        if display.decimal {
            columns.push(decimal_bytes(&record.bytes));
        }
    }
    if !record.decoded.is_empty() {
        columns.push(format!("⟪{}⟫", record.decoded));
    }
    columns.join("  │  ")
}

const MAX_FRAMES_PER_EXTRACTION: usize = 64;
const MAX_FRAME_BUFFER_BYTES: usize = 1024 * 1024;

pub struct Framer {
    mode: FramingMode,
    buffer: Vec<u8>,
    last_data: std::time::Instant,
    idle: Duration,
    start: Vec<u8>,
    end: Vec<u8>,
    fixed_length: usize,
}

impl Framer {
    fn append_bounded(&mut self, data: &[u8]) {
        self.buffer.extend_from_slice(data);
        if self.buffer.len() > MAX_FRAME_BUFFER_BYTES {
            let excess = self.buffer.len() - MAX_FRAME_BUFFER_BYTES;
            self.buffer.drain(..excess);
        }
    }

    pub fn new(
        mode: FramingMode,
        idle_ms: u64,
        start: Vec<u8>,
        end: Vec<u8>,
        fixed_length: usize,
    ) -> Self {
        Self {
            mode,
            buffer: Vec::new(),
            last_data: std::time::Instant::now(),
            idle: Duration::from_millis(idle_ms.max(1)),
            start,
            end,
            fixed_length: fixed_length.max(1),
        }
    }
    pub fn push(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        self.last_data = std::time::Instant::now();
        match self.mode {
            FramingMode::Raw => vec![data.to_vec()],
            FramingMode::Idle => {
                self.append_bounded(data);
                vec![]
            }
            FramingMode::FixedLength => {
                self.append_bounded(data);
                self.drain_ready_frames()
            }
            FramingMode::Delimited => {
                self.append_bounded(data);
                self.drain_ready_frames()
            }
        }
    }

    pub fn drain_ready_frames(&mut self) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        match self.mode {
            FramingMode::FixedLength => {
                while self.buffer.len() >= self.fixed_length
                    && frames.len() < MAX_FRAMES_PER_EXTRACTION
                {
                    frames.push(self.buffer.drain(..self.fixed_length).collect());
                }
            }
            FramingMode::Delimited => {
                while frames.len() < MAX_FRAMES_PER_EXTRACTION {
                    let Some(start_idx) = find_subsequence(&self.buffer, &self.start) else {
                        if self.buffer.len() > 4096 {
                            self.buffer.clear();
                        }
                        break;
                    };
                    if start_idx > 0 {
                        self.buffer.drain(..start_idx);
                    }
                    let search_start = self.start.len();
                    let Some(end_rel) = find_subsequence(&self.buffer[search_start..], &self.end)
                    else {
                        break;
                    };
                    let end_idx = search_start + end_rel + self.end.len();
                    frames.push(self.buffer.drain(..end_idx).collect());
                }
            }
            FramingMode::Raw | FramingMode::Idle => {}
        }
        frames
    }
    pub fn tick(&mut self) -> Option<Vec<u8>> {
        if self.mode == FramingMode::Idle
            && !self.buffer.is_empty()
            && self.last_data.elapsed() >= self.idle
        {
            Some(std::mem::take(&mut self.buffer))
        } else {
            None
        }
    }
}

fn find_subsequence(data: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    data.windows(needle.len())
        .position(|window| window == needle)
}

pub fn export_byte_view(bytes: &[u8], view: ExportByteView) -> String {
    match view {
        ExportByteView::Hex => hex_bytes(bytes),
        ExportByteView::Ascii => ascii(bytes),
        ExportByteView::Octal => octal_bytes(bytes),
        ExportByteView::Binary => binary_bytes(bytes),
        ExportByteView::Decimal => decimal_bytes(bytes),
        ExportByteView::Mixed => mixed_bytes(bytes),
    }
}

pub fn export_records(
    path: &Path,
    format: ExportFormat,
    byte_view: ExportByteView,
    records: &[LogRecord],
) -> Result<()> {
    match format {
        ExportFormat::Json => {
            let file = File::create(path).context("Create JSON export")?;
            let exported: Vec<_> = records
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "timestamp": r.timestamp.to_rfc3339(),
                        "relative_us": r.relative_us,
                        "delta_us": r.delta_us,
                        "port": r.port_label,
                        "direction": r.direction.label(),
                        "byte_view": byte_view.label(),
                        "payload": export_byte_view(&r.bytes, byte_view),
                        "decoded": r.decoded,
                        "bookmarked": r.bookmarked,
                    })
                })
                .collect();
            serde_json::to_writer_pretty(file, &exported).context("Write JSON export")?;
        }
        ExportFormat::Csv => {
            let mut file = File::create(path).context("Create CSV export")?;
            writeln!(
                file,
                "timestamp,relative_us,delta_us,port,direction,byte_view,payload,decoded,bookmarked"
            )?;
            for r in records {
                let esc = |v: String| format!("\"{}\"", v.replace('"', "\"\""));
                writeln!(
                    file,
                    "{},{},{},{},{},{},{},{},{}",
                    r.timestamp.to_rfc3339(),
                    r.relative_us,
                    r.delta_us,
                    esc(r.port_label.clone()),
                    r.direction.label(),
                    byte_view.label(),
                    esc(export_byte_view(&r.bytes, byte_view)),
                    esc(r.decoded.clone()),
                    r.bookmarked
                )?;
            }
        }
        ExportFormat::Sqlite => {
            let mut conn = Connection::open(path).context("Open SQLite export")?;
            conn.execute_batch("CREATE TABLE IF NOT EXISTS serial_records (timestamp TEXT, relative_us INTEGER, delta_us INTEGER, port TEXT, direction TEXT, byte_view TEXT, payload TEXT, decoded TEXT, bookmarked INTEGER);")?;
            let tx = conn.transaction()?;
            {
                let mut stmt = tx.prepare(
                    "INSERT INTO serial_records VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                )?;
                for r in records {
                    stmt.execute(params![
                        r.timestamp.to_rfc3339(),
                        r.relative_us as i64,
                        r.delta_us as i64,
                        r.port_label,
                        r.direction.label(),
                        byte_view.label(),
                        export_byte_view(&r.bytes, byte_view),
                        r.decoded,
                        r.bookmarked as i32
                    ])?;
                }
            }
            tx.commit()?;
        }
    }
    Ok(())
}

pub fn now_system_record(message: impl Into<String>) -> LogRecord {
    LogRecord {
        timestamp: Utc::now(),
        relative_us: 0,
        delta_us: 0,
        port_label: "APP".into(),
        direction: Direction::System,
        bytes: vec![],
        decoded: message.into(),
        bookmarked: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flexible_hex_input() {
        assert_eq!(parse_hex("0xAA:55 01").unwrap(), vec![0xAA, 0x55, 0x01]);
        assert!(parse_hex("ABC").is_err());
    }

    #[test]
    fn produces_known_modbus_crc() {
        assert_eq!(
            checksum(
                &[0x01, 0x03, 0x00, 0x00, 0x00, 0x0A],
                ChecksumKind::ModbusCrc16
            ),
            vec![0xC5, 0xCD]
        );
    }

    #[test]
    fn fixed_framer_preserves_remainder() {
        let mut framer = Framer::new(FramingMode::FixedLength, 10, vec![], vec![], 3);
        assert_eq!(framer.push(&[1, 2, 3, 4]), vec![vec![1, 2, 3]]);
        assert_eq!(framer.push(&[5, 6]), vec![vec![4, 5, 6]]);
    }

    #[test]
    fn delimited_framer_recovers_frames() {
        let mut framer = Framer::new(FramingMode::Delimited, 10, vec![0xAA], vec![0x55], 1);
        assert_eq!(
            framer.push(&[0, 0xAA, 1, 0x55, 0xAA, 2, 0x55]),
            vec![vec![0xAA, 1, 0x55], vec![0xAA, 2, 0x55]]
        );
    }

    #[test]
    fn protocol_decoders_handle_representative_wire_frames() {
        let nmea = decode(
            b"$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*47",
            ProtocolDecoder::Nmea0183,
        );
        assert!(nmea.contains("NMEA $GPGGA"));
        assert!(nmea.contains("14 field(s)"));

        let modbus = decode(
            &[0x01, 0x03, 0x04, 0x00, 0x2A, 0x00, 0x10, 0xDB, 0xF7],
            ProtocolDecoder::ModbusRtu,
        );
        assert!(modbus.contains("addr=1"));
        assert!(modbus.contains("function=0x03"));
        assert!(modbus.contains("payload=5 byte(s)"));

        let dmx = decode(&[0x00, 0xFF, 0x80, 0x01], ProtocolDecoder::Dmx512);
        assert_eq!(dmx, "DMX start code=0 · 3 channel byte(s)");

        let midi = decode(&[0x90, 60, 100], ProtocolDecoder::Midi);
        assert_eq!(midi, "MIDI Note On · channel 1");

        let can = decode(b"t1232AABB\r", ProtocolDecoder::CanAdapter);
        assert_eq!(can, "CAN adapter frame: t1232AABB");
    }

    #[test]
    fn export_byte_view_matches_requested_representation() {
        assert_eq!(
            export_byte_view(&[0x41, 0x0A], ExportByteView::Hex),
            "41 0A"
        );
        assert_eq!(export_byte_view(&[0x41, 0x0A], ExportByteView::Ascii), "A↵");
    }
}
