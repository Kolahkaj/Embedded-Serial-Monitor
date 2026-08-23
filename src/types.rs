use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub type PortId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    Rx,
    Tx,
    System,
}

impl Direction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Rx => "RX",
            Self::Tx => "TX",
            Self::System => "SYS",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimestampMode {
    Off,
    Absolute,
    Relative,
    Delta,
}

impl TimestampMode {
    pub const ALL: [Self; 4] = [Self::Off, Self::Absolute, Self::Relative, Self::Delta];
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Absolute => "Absolute",
            Self::Relative => "Relative",
            Self::Delta => "Delta",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FramingMode {
    Raw,
    Idle,
    Delimited,
    FixedLength,
}

impl FramingMode {
    pub const ALL: [Self; 4] = [Self::Raw, Self::Idle, Self::Delimited, Self::FixedLength];
    pub fn label(self) -> &'static str {
        match self {
            Self::Raw => "Raw chunks",
            Self::Idle => "Idle timeout",
            Self::Delimited => "Start / end bytes",
            Self::FixedLength => "Fixed length",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParityChoice {
    None,
    Odd,
    Even,
}

impl ParityChoice {
    pub const ALL: [Self; 3] = [Self::None, Self::Odd, Self::Even];
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Odd => "Odd",
            Self::Even => "Even",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FlowChoice {
    None,
    Hardware,
    Software,
}

impl FlowChoice {
    pub const ALL: [Self; 3] = [Self::None, Self::Hardware, Self::Software];
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Hardware => "RTS/CTS",
            Self::Software => "XON/XOFF",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChecksumKind {
    None,
    Xor8,
    Add8,
    Lrc,
    ModbusCrc16,
    Crc32,
}

impl ChecksumKind {
    pub const ALL: [Self; 6] = [
        Self::None,
        Self::Xor8,
        Self::Add8,
        Self::Lrc,
        Self::ModbusCrc16,
        Self::Crc32,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Xor8 => "XOR-8",
            Self::Add8 => "ADD-8",
            Self::Lrc => "LRC",
            Self::ModbusCrc16 => "Modbus CRC-16",
            Self::Crc32 => "CRC-32",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SendEncoding {
    Ascii,
    Hex,
}

impl SendEncoding {
    pub const ALL: [Self; 2] = [Self::Ascii, Self::Hex];
    pub fn label(self) -> &'static str {
        match self {
            Self::Ascii => "ASCII / UTF-8",
            Self::Hex => "Hex bytes",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProtocolDecoder {
    None,
    ModbusRtu,
    Nmea0183,
    Dmx512,
    Midi,
    CanAdapter,
}

impl ProtocolDecoder {
    pub const ALL: [Self; 6] = [
        Self::None,
        Self::ModbusRtu,
        Self::Nmea0183,
        Self::Dmx512,
        Self::Midi,
        Self::CanAdapter,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::ModbusRtu => "MODBUS RTU",
            Self::Nmea0183 => "NMEA 0183",
            Self::Dmx512 => "DMX512",
            Self::Midi => "MIDI",
            Self::CanAdapter => "CAN serial adapter",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplaySettings {
    pub ascii: bool,
    pub hex: bool,
    pub octal: bool,
    pub binary: bool,
    pub decimal: bool,
    pub mixed: bool,
    pub timestamps: TimestampMode,
    pub word_wrap: bool,
    #[serde(default, alias = "live_filter")]
    pub keyword_filter: String,
    pub rate_limit_lines_per_sec: u32,
    pub max_buffer_mb: usize,
    pub anonymize: bool,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            ascii: true,
            hex: true,
            octal: false,
            binary: false,
            decimal: false,
            mixed: false,
            timestamps: TimestampMode::Relative,
            word_wrap: true,
            keyword_filter: String::new(),
            rate_limit_lines_per_sec: 2_000,
            max_buffer_mb: 50,
            anonymize: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ConnectionMode {
    #[default]
    Serial,
    Tcp,
    Udp,
}

impl ConnectionMode {
    pub const ALL: [Self; 3] = [Self::Serial, Self::Tcp, Self::Udp];

    pub fn label(self) -> &'static str {
        match self {
            Self::Serial => "Serial",
            Self::Tcp => "TCP",
            Self::Udp => "UDP",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum NetworkRole {
    #[default]
    Client,
    Server,
}

impl NetworkRole {
    pub const ALL: [Self; 2] = [Self::Client, Self::Server];

    pub fn label(self) -> &'static str {
        match self {
            Self::Client => "Client",
            Self::Server => "Server / listener",
        }
    }
}

fn default_network_host() -> String {
    "127.0.0.1".into()
}

fn default_network_port() -> u16 {
    3333
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortSettings {
    #[serde(default)]
    pub mode: ConnectionMode,
    #[serde(default)]
    pub network_role: NetworkRole,
    #[serde(default = "default_network_host")]
    pub network_host: String,
    #[serde(default = "default_network_port")]
    pub network_port: u16,
    pub device: String,
    pub baud_rate: String,
    pub data_bits: u8,
    pub stop_bits: u8,
    pub parity: ParityChoice,
    pub flow_control: FlowChoice,
    pub auto_reconnect: bool,
    pub framing: FramingMode,
    pub idle_timeout_ms: u64,
    pub start_bytes: String,
    pub end_bytes: String,
    pub fixed_length: usize,
    pub decoder: ProtocolDecoder,
    pub dtr: bool,
    pub rts: bool,
}

impl Default for PortSettings {
    fn default() -> Self {
        Self {
            mode: ConnectionMode::Serial,
            network_role: NetworkRole::Client,
            network_host: default_network_host(),
            network_port: default_network_port(),
            device: String::new(),
            baud_rate: "115200".into(),
            data_bits: 8,
            stop_bits: 1,
            parity: ParityChoice::None,
            flow_control: FlowChoice::None,
            auto_reconnect: true,
            framing: FramingMode::Raw,
            idle_timeout_ms: 20,
            start_bytes: "AA".into(),
            end_bytes: "55".into(),
            fixed_length: 8,
            decoder: ProtocolDecoder::None,
            dtr: true,
            rts: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColorRule {
    pub enabled: bool,
    pub pattern: String,
    pub color: [u8; 4],
    pub label: String,
}

impl Default for ColorRule {
    fn default() -> Self {
        Self {
            enabled: true,
            pattern: "(?i)error|fail|panic".into(),
            color: [235, 74, 74, 255],
            label: "Errors".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerRule {
    pub enabled: bool,
    pub incoming_pattern: String,
    pub response: String,
    pub encoding: SendEncoding,
}

impl Default for TriggerRule {
    fn default() -> Self {
        Self {
            enabled: false,
            incoming_pattern: "".into(),
            response: "".into(),
            encoding: SendEncoding::Ascii,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeConfig {
    pub enabled: bool,
    pub tcp_server: bool,
    pub bind_or_host: String,
    pub port: u16,
    pub udp: bool,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            tcp_server: true,
            bind_or_host: "0.0.0.0".into(),
            port: 3333,
            udp: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendSettings {
    pub encoding: SendEncoding,
    pub checksum: ChecksumKind,
    pub packet_size: usize,
    pub inter_packet_delay_ms: u64,
    pub repeat_interval_ms: u64,
    pub append_newline: bool,
}

impl Default for SendSettings {
    fn default() -> Self {
        Self {
            encoding: SendEncoding::Ascii,
            checksum: ChecksumKind::None,
            packet_size: 256,
            inter_packet_delay_ms: 0,
            repeat_interval_ms: 1000,
            append_newline: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogRecord {
    pub timestamp: DateTime<Utc>,
    pub relative_us: u128,
    pub delta_us: u128,
    pub port_label: String,
    pub direction: Direction,
    pub bytes: Vec<u8>,
    pub decoded: String,
    pub bookmarked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortSnapshot {
    pub settings: PortSettings,
    pub bridge: BridgeConfig,
    pub logs: Vec<LogRecord>,
    pub send_history: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionData {
    pub version: String,
    pub display: DisplaySettings,
    pub color_rules: Vec<ColorRule>,
    pub triggers: Vec<TriggerRule>,
    pub ports: Vec<PortSnapshot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Json,
    Sqlite,
}

impl ExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Json => "json",
            Self::Sqlite => "sqlite",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportByteView {
    Hex,
    Ascii,
    Octal,
    Binary,
    Decimal,
    Mixed,
}

impl ExportByteView {
    pub const ALL: [Self; 6] = [
        Self::Hex,
        Self::Ascii,
        Self::Octal,
        Self::Binary,
        Self::Decimal,
        Self::Mixed,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Hex => "HEX",
            Self::Ascii => "ASCII",
            Self::Octal => "Octal",
            Self::Binary => "Binary",
            Self::Decimal => "Decimal",
            Self::Mixed => "Mixed HEX + ASCII",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportScope {
    AllRetained,
    Received,
    Transmitted,
    Bookmarked,
}

impl ExportScope {
    pub const ALL: [Self; 4] = [
        Self::AllRetained,
        Self::Received,
        Self::Transmitted,
        Self::Bookmarked,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::AllRetained => "All retained records",
            Self::Received => "Received (RX) only",
            Self::Transmitted => "Transmitted (TX) only",
            Self::Bookmarked => "Bookmarked only",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppTheme {
    Dark,
    Light,
    Moonlight,
    Nord,
    Solarized,
}

impl AppTheme {
    pub const ALL: [Self; 5] = [
        Self::Dark,
        Self::Light,
        Self::Moonlight,
        Self::Nord,
        Self::Solarized,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Dark => "Dark",
            Self::Light => "Light",
            Self::Moonlight => "Moonlight",
            Self::Nord => "Nord",
            Self::Solarized => "Solarized",
        }
    }
}

#[derive(Debug, Clone)]
pub struct MacroCommand {
    pub name: String,
    pub body: String,
}

impl Default for MacroCommand {
    fn default() -> Self {
        Self {
            name: "Reset target".into(),
            body: "SEND_HEX 55 AA\nDELAY 100\nDTR_PULSE 50".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum WorkerEvent {
    Data {
        id: PortId,
        bytes: Vec<u8>,
        timestamp: DateTime<Utc>,
        dropped_bytes: u64,
    },
    Status {
        id: PortId,
        message: String,
        connected: bool,
    },
    NetworkData {
        id: PortId,
        bytes: Vec<u8>,
    },
    /// Informational or error status from the TCP/UDP bridge. This must not
    /// change the serial connection state.
    BridgeStatus {
        id: PortId,
        message: String,
    },
    AutoBaud {
        id: PortId,
        baud_rate: Option<String>,
        score: f32,
    },
    /// A macro send routed through the UI so it is visible in the TX log.
    MacroSend {
        id: PortId,
        bytes: Vec<u8>,
    },
    /// A macro electrical-control operation routed through the UI in script order.
    MacroControl {
        id: PortId,
        command: WorkerCommand,
        description: String,
    },
    /// Final macro result emitted after all preceding macro actions were queued.
    MacroComplete {
        id: PortId,
        name: String,
        sent: usize,
        controls: usize,
        errors: Vec<String>,
    },
}

#[derive(Debug, Clone)]
pub enum WorkerCommand {
    Send(Vec<u8>),
    SetDtr(bool),
    SetRts(bool),
    PulseDtr(u64),
    SendBreak(u64),
    Disconnect,
}
