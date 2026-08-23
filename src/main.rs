#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod engine;
mod transport;
mod types;

use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    io::{BufWriter, Read, Write},
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use chrono::Utc;
use crossbeam_channel::{bounded, Receiver, Sender};
use eframe::egui::{self, Color32, RichText, TextStyle};
use egui_plot::{Legend, Line, Plot, PlotPoints, Points};
use rand::RngCore;
use regex::Regex;
use rfd::FileDialog;
use rustfft::{num_complex::Complex, FftPlanner};

use crate::{
    engine::{
        anonymize, checksum, export_records, now_system_record, parse_hex, render_record, Framer,
    },
    transport::{BridgeHandle, SerialHandle},
    types::*,
};

const APP_NAME: &str = "Embedded Serial Monitor";
const SESSION_VERSION: &str = "4";

#[derive(Clone, Copy)]
struct DiagnosticPalette {
    rx: Color32,
    tx: Color32,
    system: Color32,
    muted: Color32,
    success: Color32,
    warning: Color32,
    error: Color32,
    accent: Color32,
}

fn diagnostic_palette(theme: AppTheme) -> DiagnosticPalette {
    match theme {
        AppTheme::Light => DiagnosticPalette {
            rx: Color32::from_rgb(0, 84, 150),
            tx: Color32::from_rgb(166, 77, 0),
            system: Color32::from_rgb(67, 84, 104),
            muted: Color32::from_rgb(67, 84, 104),
            success: Color32::from_rgb(0, 112, 62),
            warning: Color32::from_rgb(161, 82, 0),
            error: Color32::from_rgb(180, 28, 28),
            accent: Color32::from_rgb(2, 132, 199),
        },
        AppTheme::Dark => DiagnosticPalette {
            rx: Color32::from_rgb(96, 204, 255),
            tx: Color32::from_rgb(255, 193, 92),
            system: Color32::from_rgb(178, 190, 206),
            muted: Color32::from_rgb(178, 190, 206),
            success: Color32::from_rgb(106, 218, 134),
            warning: Color32::from_rgb(255, 205, 98),
            error: Color32::from_rgb(255, 103, 103),
            accent: Color32::from_rgb(34, 211, 238),
        },
        AppTheme::Moonlight => DiagnosticPalette {
            rx: Color32::from_rgb(125, 211, 252),
            tx: Color32::from_rgb(251, 191, 36),
            system: Color32::from_rgb(165, 180, 210),
            muted: Color32::from_rgb(165, 180, 210),
            success: Color32::from_rgb(134, 239, 172),
            warning: Color32::from_rgb(253, 224, 71),
            error: Color32::from_rgb(252, 129, 129),
            accent: Color32::from_rgb(192, 132, 252),
        },
        AppTheme::Nord => DiagnosticPalette {
            rx: Color32::from_rgb(136, 192, 208),
            tx: Color32::from_rgb(235, 203, 139),
            system: Color32::from_rgb(174, 185, 202),
            muted: Color32::from_rgb(174, 185, 202),
            success: Color32::from_rgb(163, 190, 140),
            warning: Color32::from_rgb(235, 203, 139),
            error: Color32::from_rgb(191, 97, 106),
            accent: Color32::from_rgb(129, 161, 193),
        },
        AppTheme::Solarized => DiagnosticPalette {
            rx: Color32::from_rgb(38, 139, 210),
            tx: Color32::from_rgb(203, 75, 22),
            system: Color32::from_rgb(101, 123, 131),
            muted: Color32::from_rgb(101, 123, 131),
            success: Color32::from_rgb(133, 153, 0),
            warning: Color32::from_rgb(181, 137, 0),
            error: Color32::from_rgb(220, 50, 47),
            accent: Color32::from_rgb(42, 161, 152),
        },
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkspaceTab {
    Monitor,
    Connection,
    Network,
    Display,
    Advanced,
    Plot,
    Appearance,
}

impl WorkspaceTab {
    const ALL: [Self; 7] = [
        Self::Monitor,
        Self::Connection,
        Self::Network,
        Self::Display,
        Self::Advanced,
        Self::Plot,
        Self::Appearance,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Monitor => "Monitor",
            Self::Connection => "Connect",
            Self::Network => "Network",
            Self::Display => "Display",
            Self::Advanced => "Advanced",
            Self::Plot => "Plot",
            Self::Appearance => "Appearance",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PlotMode {
    TimeSeries,
    Xy,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SignalFilter {
    Raw,
    MovingAverage,
    Median,
    LowPass,
}

impl SignalFilter {
    const ALL: [Self; 4] = [Self::Raw, Self::MovingAverage, Self::Median, Self::LowPass];

    fn label(self) -> &'static str {
        match self {
            Self::Raw => "Raw",
            Self::MovingAverage => "Moving average",
            Self::Median => "Median",
            Self::LowPass => "Low-pass",
        }
    }
}

struct PlotAnalysisState {
    field: String,
    filter: SignalFilter,
    filter_window: usize,
    low_pass_cutoff_hz: f64,
    fft_window: usize,
    fft_in_db: bool,
    spectrum_visible: bool,
    reset_spectrum_view: bool,
}

impl Default for PlotAnalysisState {
    fn default() -> Self {
        Self {
            field: String::new(),
            filter: SignalFilter::Raw,
            filter_window: 5,
            low_pass_cutoff_hz: 5.0,
            fft_window: 512,
            fft_in_db: false,
            spectrum_visible: true,
            reset_spectrum_view: false,
        }
    }
}

#[derive(Clone)]
struct PlotSample {
    elapsed_s: f64,
    /// Values parsed from this individual record, used for time-series traces.
    values: BTreeMap<String, f64>,
    /// Latest known value for every field at this sample time, used for X/Y pairing.
    xy_values: BTreeMap<String, f64>,
}

struct PlotState {
    samples: VecDeque<PlotSample>,
    fields: BTreeMap<String, bool>,
    colors: BTreeMap<String, [u8; 4]>,
    mode: PlotMode,
    x_field: String,
    y_field: String,
    history_limit: usize,
    paused: bool,
    auto_y: bool,
    y_min: f64,
    y_max: f64,
    line_width: f32,
    reset_view: bool,
    started_at: Instant,
    last_ingest: Instant,
    sample_rate_hz: u32,
    latest_values: BTreeMap<String, f64>,
    analysis: PlotAnalysisState,
}

impl Default for PlotState {
    fn default() -> Self {
        Self {
            samples: VecDeque::new(),
            fields: BTreeMap::new(),
            colors: BTreeMap::new(),
            mode: PlotMode::TimeSeries,
            x_field: String::new(),
            y_field: String::new(),
            history_limit: 10_000,
            paused: false,
            auto_y: true,
            y_min: -1.0,
            y_max: 1.0,
            line_width: 1.8,
            reset_view: false,
            started_at: Instant::now(),
            last_ingest: Instant::now(),
            sample_rate_hz: 60,
            latest_values: BTreeMap::new(),
            analysis: PlotAnalysisState::default(),
        }
    }
}

impl PlotState {
    fn known_fields(&self) -> Vec<String> {
        self.fields.keys().cloned().collect()
    }

    fn color_for(&mut self, field: &str) -> Color32 {
        let next = self.colors.len();
        let color = self.colors.entry(field.to_owned()).or_insert_with(|| {
            const COLORS: [[u8; 4]; 8] = [
                [0, 170, 255, 255],
                [255, 152, 0, 255],
                [67, 200, 126, 255],
                [190, 92, 255, 255],
                [255, 90, 120, 255],
                [0, 188, 170, 255],
                [255, 205, 70, 255],
                [120, 155, 255, 255],
            ];
            COLORS[next % COLORS.len()]
        });
        Color32::from_rgba_unmultiplied(color[0], color[1], color[2], color[3])
    }
}

struct PortState {
    id: PortId,
    alias: String,
    settings: PortSettings,
    bridge: BridgeConfig,
    connected: bool,
    status: String,
    logs: Vec<LogRecord>,
    memory_estimate: usize,
    serial: Option<SerialHandle>,
    bridge_handle: Option<BridgeHandle>,
    framer: Framer,
    connected_since: Option<Instant>,
    last_record: Option<Instant>,
    rate_window: Instant,
    rate_count: u32,
    rate_dropped: u64,
    input_dropped_bytes: u64,
    send_history: Vec<String>,
    break_ms: u64,
}

impl PortState {
    fn new(id: PortId) -> Self {
        let settings = PortSettings::default();
        Self {
            id,
            alias: format!("Port {id}"),
            framer: make_framer(&settings),
            settings,
            bridge: BridgeConfig::default(),
            connected: false,
            status: "Disconnected".into(),
            logs: Vec::new(),
            memory_estimate: 0,
            serial: None,
            bridge_handle: None,
            connected_since: None,
            last_record: None,
            rate_window: Instant::now(),
            rate_count: 0,
            rate_dropped: 0,
            input_dropped_bytes: 0,
            send_history: Vec::new(),
            break_ms: 100,
        }
    }
}

fn make_framer(settings: &PortSettings) -> Framer {
    Framer::new(
        settings.framing,
        settings.idle_timeout_ms,
        parse_hex(&settings.start_bytes).unwrap_or_default(),
        parse_hex(&settings.end_bytes).unwrap_or_default(),
        settings.fixed_length,
    )
}

fn connection_display_label(port: &PortState) -> String {
    match port.settings.mode {
        ConnectionMode::Serial => {
            if port.settings.device.is_empty() {
                port.alias.clone()
            } else {
                port.settings.device.clone()
            }
        }
        ConnectionMode::Tcp => format!(
            "TCP {}:{}",
            port.settings.network_host, port.settings.network_port
        ),
        ConnectionMode::Udp => format!(
            "UDP {}:{}",
            port.settings.network_host, port.settings.network_port
        ),
    }
}

pub struct SerialMonitorApp {
    ports: Vec<PortState>,
    selected: Option<PortId>,
    next_port_id: PortId,
    serial_choices: Vec<String>,
    events_tx: Sender<WorkerEvent>,
    events_rx: Receiver<WorkerEvent>,
    display: DisplaySettings,
    color_rules: Vec<ColorRule>,
    triggers: Vec<TriggerRule>,
    send_buffer: String,
    send_settings: SendSettings,
    macro_command: MacroCommand,
    macro_running: bool,
    macro_dispatch_errors: Vec<String>,
    export_format: ExportFormat,
    export_byte_view: ExportByteView,
    export_scope: ExportScope,
    export_dialog_open: bool,
    logging: Option<BufWriter<fs::File>>,
    logging_path: Option<std::path::PathBuf>,
    app_theme: AppTheme,
    app_status: String,
    paused: bool,
    scheduled_enabled: bool,
    last_scheduled: Instant,
    fuzz_size: usize,
    fuzz_count: usize,
    active_tab: WorkspaceTab,
    plot: PlotState,
}

impl Default for SerialMonitorApp {
    fn default() -> Self {
        let (events_tx, events_rx) = bounded::<WorkerEvent>(256);
        let first = PortState::new(1);
        Self {
            ports: vec![first],
            selected: Some(1),
            next_port_id: 2,
            serial_choices: refresh_serial_choices(),
            events_tx,
            events_rx,
            display: DisplaySettings::default(),
            color_rules: vec![
                ColorRule::default(),
                ColorRule {
                    enabled: true,
                    pattern: "(?i)ack|success|ok".into(),
                    color: [82, 201, 119, 255],
                    label: "Acknowledgements".into(),
                },
            ],
            triggers: vec![TriggerRule::default()],
            send_buffer: String::new(),
            send_settings: SendSettings::default(),
            macro_command: MacroCommand::default(),
            macro_running: false,
            macro_dispatch_errors: Vec::new(),
            export_format: ExportFormat::Csv,
            export_byte_view: ExportByteView::Hex,
            export_scope: ExportScope::AllRetained,
            export_dialog_open: false,
            logging: None,
            logging_path: None,
            app_theme: AppTheme::Dark,
            app_status: "Ready · choose a port and connect".into(),
            paused: false,
            scheduled_enabled: false,
            last_scheduled: Instant::now(),
            fuzz_size: 32,
            fuzz_count: 100,
            active_tab: WorkspaceTab::Monitor,
            plot: PlotState::default(),
        }
    }
}

impl SerialMonitorApp {
    fn selected_index(&self) -> Option<usize> {
        self.selected
            .and_then(|id| self.ports.iter().position(|p| p.id == id))
    }
    fn selected_port(&self) -> Option<&PortState> {
        self.selected_index().and_then(|i| self.ports.get(i))
    }
    fn selected_port_mut(&mut self) -> Option<&mut PortState> {
        self.selected_index().and_then(|i| self.ports.get_mut(i))
    }

    fn add_port(&mut self) {
        let id = self.next_port_id;
        self.next_port_id += 1;
        self.ports.push(PortState::new(id));
        self.selected = Some(id);
    }

    fn close_selected(&mut self) {
        let Some(index) = self.selected_index() else {
            return;
        };
        self.disconnect_index(index);
        self.ports.remove(index);
        self.selected = self.ports.first().map(|p| p.id);
    }

    fn connect_selected(&mut self) {
        let Some(index) = self.selected_index() else {
            return;
        };
        if self.ports[index].connected {
            self.disconnect_index(index);
            return;
        }
        let p = &mut self.ports[index];
        match p.settings.mode {
            ConnectionMode::Serial if p.settings.device.trim().is_empty() => {
                self.app_status = "Choose a serial device before connecting.".into();
                return;
            }
            ConnectionMode::Tcp | ConnectionMode::Udp
                if p.settings.network_host.trim().is_empty() =>
            {
                self.app_status = "Enter a host or bind address before connecting.".into();
                return;
            }

            _ => {}
        }
        let requested_baud = p
            .settings
            .baud_rate
            .trim()
            .parse::<u32>()
            .unwrap_or_default();
        if p.settings.mode == ConnectionMode::Serial
            && requested_baud >= 1_000_000
            && !p.settings.dtr
        {
            p.settings.dtr = true;
        }
        p.framer = make_framer(&p.settings);
        p.status = match p.settings.mode {
            ConnectionMode::Serial if requested_baud >= 1_000_000 => {
                format!("Opening at {} baud with DTR asserted…", requested_baud)
            }
            ConnectionMode::Serial => "Opening serial port…".into(),
            ConnectionMode::Tcp => format!(
                "Opening TCP {} {}:{}…",
                p.settings.network_role.label().to_ascii_lowercase(),
                p.settings.network_host,
                p.settings.network_port
            ),
            ConnectionMode::Udp => format!(
                "Opening UDP {} {}:{}…",
                p.settings.network_role.label().to_ascii_lowercase(),
                p.settings.network_host,
                p.settings.network_port
            ),
        };
        p.serial = Some(transport::open_connection_worker(
            p.id,
            p.settings.clone(),
            self.events_tx.clone(),
        ));
        p.connected_since = Some(Instant::now());
        self.app_status = p.status.clone();
        if p.settings.mode == ConnectionMode::Serial
            && p.bridge.enabled
            && p.bridge_handle.is_none()
        {
            match transport::start_bridge(p.id, p.bridge.clone(), self.events_tx.clone()) {
                Ok(handle) => p.bridge_handle = Some(handle),
                Err(error) => {
                    self.app_status = format!("Serial opening; bridge not started: {error}")
                }
            }
        }
    }

    fn restart_bridge(&mut self, index: usize) {
        let (id, config) = {
            let port = &mut self.ports[index];
            if let Some(handle) = port.bridge_handle.take() {
                handle.stop();
            }
            (port.id, port.bridge.clone())
        };
        if self.ports[index].settings.mode != ConnectionMode::Serial {
            self.ports[index].status = "Serial bridge is available only in Serial mode".into();
            self.app_status = "Switch to Serial mode to use the Network Bridge.".into();
            return;
        }
        if !config.enabled {
            self.ports[index].status = "Bridge disabled".into();
            self.app_status = "Network bridge disabled.".into();
            return;
        }
        match transport::start_bridge(id, config, self.events_tx.clone()) {
            Ok(handle) => {
                self.ports[index].bridge_handle = Some(handle);
                self.app_status = "Network bridge applied.".into();
            }
            Err(error) => {
                self.ports[index].status = format!("Bridge start failed: {error}");
                self.app_status = format!("Network bridge did not start: {error}");
            }
        }
    }

    fn disconnect_index(&mut self, index: usize) {
        if let Some(handle) = self.ports[index].serial.take() {
            let _ = handle.command_tx.send(WorkerCommand::Disconnect);
        }
        if let Some(handle) = self.ports[index].bridge_handle.take() {
            handle.stop();
        }
        self.ports[index].connected = false;
        self.ports[index].status = "Disconnected".into();
    }

    fn selected_send(&mut self) {
        let result = self.outbound_from_buffer();
        match result {
            Ok(bytes) if bytes.is_empty() => self.app_status = "Nothing to send.".into(),
            Ok(bytes) => {
                let history = self.send_buffer.clone();
                if let Some(port) = self.selected_port_mut() {
                    let raw = bytes.clone();
                    send_to_port(port, raw);
                    port.send_history.push(history);
                    port.send_history.truncate(100);
                }
            }
            Err(error) => self.app_status = format!("Send input error: {error}"),
        }
    }

    fn outbound_from_buffer(&self) -> Result<Vec<u8>> {
        let mut data = match self.send_settings.encoding {
            SendEncoding::Ascii => self.send_buffer.as_bytes().to_vec(),
            SendEncoding::Hex => parse_hex(&self.send_buffer)?,
        };
        if self.send_settings.append_newline {
            data.push(b'\n');
        }
        let crc = checksum(&data, self.send_settings.checksum);
        data.extend_from_slice(&crc);
        Ok(data)
    }

    fn add_log(&mut self, id: PortId, mut record: LogRecord) {
        self.ingest_plot_record(&record);
        let now = Instant::now();
        let mut trigger_payloads = Vec::new();
        if let Some(port) = self.ports.iter_mut().find(|p| p.id == id) {
            if self.paused {
                return;
            }
            if now.duration_since(port.rate_window) >= Duration::from_secs(1) {
                if port.rate_dropped > 0 {
                    let dropped = now_system_record(format!(
                        "Rate limiter dropped {} displayed packet(s) on {}",
                        port.rate_dropped, port.alias
                    ));
                    port.memory_estimate += record_weight(&dropped);
                    port.logs.push(dropped);
                    port.rate_dropped = 0;
                }
                port.rate_window = now;
                port.rate_count = 0;
            }
            let cap = self.display.rate_limit_lines_per_sec;
            if cap > 0 && port.rate_count >= cap {
                port.rate_dropped += 1;
                return;
            }
            port.rate_count += 1;
            record.relative_us = port
                .connected_since
                .map(|t| t.elapsed().as_micros())
                .unwrap_or(0);
            record.delta_us = port
                .last_record
                .map(|t| now.duration_since(t).as_micros())
                .unwrap_or(0);
            port.last_record = Some(now);
            if self.display.anonymize {
                record.decoded = anonymize(&record.decoded);
            }
            let event_text = format!(
                "{} {} {}",
                record.direction.label(),
                engine::ascii(&record.bytes),
                engine::hex_bytes(&record.bytes)
            );
            for trigger in &self.triggers {
                if trigger.enabled && !trigger.incoming_pattern.trim().is_empty() {
                    if let Ok(re) = Regex::new(&trigger.incoming_pattern) {
                        if re.is_match(&event_text) {
                            let result = match trigger.encoding {
                                SendEncoding::Ascii => Ok(trigger.response.as_bytes().to_vec()),
                                SendEncoding::Hex => parse_hex(&trigger.response),
                            };
                            if let Ok(payload) = result {
                                trigger_payloads.push(payload);
                            }
                        }
                    }
                }
            }
            port.memory_estimate += record_weight(&record);
            port.logs.push(record.clone());
            let budget = self.display.max_buffer_mb.max(1) * 1024 * 1024;
            trim_port_logs_to_budget(port, budget);
        }
        self.write_log_record(&record);
        for payload in trigger_payloads {
            if let Some(port) = self.ports.iter_mut().find(|p| p.id == id) {
                send_to_port(port, payload);
            }
        }
    }

    fn append_plot_values(&mut self, values: BTreeMap<String, f64>, elapsed_s: f64) {
        for field in values.keys() {
            self.plot.fields.entry(field.clone()).or_insert(true);
            if self.plot.x_field.is_empty() {
                self.plot.x_field = field.clone();
            } else if self.plot.y_field.is_empty() && self.plot.x_field != *field {
                self.plot.y_field = field.clone();
            }
        }
        self.plot
            .latest_values
            .extend(values.iter().map(|(field, value)| (field.clone(), *value)));
        self.plot.samples.push_back(PlotSample {
            elapsed_s,
            values,
            xy_values: self.plot.latest_values.clone(),
        });
        while self.plot.samples.len() > self.plot.history_limit.max(100) {
            self.plot.samples.pop_front();
        }
    }

    fn rebuild_plot_from_history(&mut self) {
        let records: Vec<LogRecord> = self
            .selected_port()
            .map(|port| {
                port.logs
                    .iter()
                    .filter(|record| record.direction == Direction::Rx)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        self.plot.samples.clear();
        self.plot.fields.clear();
        self.plot.latest_values.clear();
        self.plot.x_field.clear();
        self.plot.y_field.clear();
        self.plot.started_at = Instant::now();
        for record in records {
            let values = parse_structured_values(&record.bytes);
            if !values.is_empty() {
                self.append_plot_values(values, self.plot.started_at.elapsed().as_secs_f64());
            }
        }
    }

    fn ingest_plot_record(&mut self, record: &LogRecord) {
        if self.plot.paused || record.direction != Direction::Rx {
            return;
        }
        let min_interval = Duration::from_secs_f64(1.0 / self.plot.sample_rate_hz.max(1) as f64);
        if self.plot.last_ingest.elapsed() < min_interval {
            return;
        }
        self.plot.last_ingest = Instant::now();
        let values = parse_structured_values(&record.bytes);
        if values.is_empty() {
            return;
        }
        self.append_plot_values(values, self.plot.started_at.elapsed().as_secs_f64());
    }

    fn drain_events(&mut self) {
        const MAX_EVENTS_PER_FRAME: usize = 48;
        for _ in 0..MAX_EVENTS_PER_FRAME {
            let Ok(event) = self.events_rx.try_recv() else {
                break;
            };
            match event {
                WorkerEvent::Data {
                    id,
                    bytes,
                    timestamp,
                    dropped_bytes,
                } => {
                    let frames = if let Some(port) = self.ports.iter_mut().find(|p| p.id == id) {
                        if dropped_bytes > 0 {
                            port.input_dropped_bytes += dropped_bytes;
                            port.status = format!(
                                "Receiving; {} bytes shed to protect UI",
                                port.input_dropped_bytes
                            );
                        }
                        port.framer.push(&bytes)
                    } else {
                        Vec::new()
                    };
                    for frame in frames {
                        self.push_received(id, frame, timestamp);
                    }
                }
                WorkerEvent::NetworkData { id, bytes } => {
                    if let Some(port) = self.ports.iter_mut().find(|p| p.id == id) {
                        send_to_port(port, bytes);
                    }
                }
                WorkerEvent::BridgeStatus { id, message } => {
                    if let Some(port) = self.ports.iter_mut().find(|p| p.id == id) {
                        port.status = message.clone();
                    }
                    self.app_status = message;
                }
                WorkerEvent::Status {
                    id,
                    message,
                    connected,
                } => {
                    if let Some(port) = self.ports.iter_mut().find(|p| p.id == id) {
                        port.connected = connected;
                        port.status = message.clone();
                    }
                    self.app_status = message;
                }
                WorkerEvent::MacroSend { id, bytes } => {
                    if let Some(port) = self.ports.iter_mut().find(|port| port.id == id) {
                        if !send_to_port(port, bytes) {
                            let message = "Macro send was not queued because the serial port is disconnected.";
                            self.macro_dispatch_errors.push(message.into());
                            append_system_record(port, message);
                        }
                    } else {
                        self.macro_dispatch_errors
                            .push(format!("Macro requested unknown port {id}."));
                    }
                }
                WorkerEvent::MacroControl {
                    id,
                    command,
                    description,
                } => {
                    if let Some(port) = self.ports.iter_mut().find(|port| port.id == id) {
                        let queued = port
                            .serial
                            .as_ref()
                            .is_some_and(|handle| handle.command_tx.send(command).is_ok());
                        if queued {
                            append_system_record(port, format!("Macro: {description}"));
                        } else {
                            let message = format!("Macro control was not queued: {description}");
                            self.macro_dispatch_errors.push(message.clone());
                            append_system_record(port, message);
                        }
                    } else {
                        self.macro_dispatch_errors
                            .push(format!("Macro requested unknown port {id}."));
                    }
                }
                WorkerEvent::MacroComplete {
                    id,
                    name,
                    sent,
                    controls,
                    mut errors,
                } => {
                    errors.append(&mut self.macro_dispatch_errors);
                    self.macro_running = false;
                    let summary = if errors.is_empty() {
                        format!("Macro ‘{name}’ completed: {sent} send(s), {controls} control action(s).")
                    } else {
                        format!(
                            "Macro ‘{name}’ completed with {} error(s): {}",
                            errors.len(),
                            errors.join(" | ")
                        )
                    };
                    if let Some(port) = self.ports.iter_mut().find(|port| port.id == id) {
                        append_system_record(port, summary.clone());
                    }
                    self.app_status = summary;
                }
                WorkerEvent::AutoBaud {
                    id,
                    baud_rate,
                    score,
                } => match baud_rate {
                    Some(baud_rate) => {
                        if let Some(port) = self.ports.iter_mut().find(|p| p.id == id) {
                            port.settings.baud_rate = baud_rate.clone();
                            port.status =
                                format!("Auto baud: {baud_rate} ({:.0}% printable)", score * 100.0);
                        }
                        self.app_status = format!(
                            "Baud scan selected {baud_rate}; printable-data score {:.0}%. Verify protocol data before connecting.",
                            score * 100.0
                        );
                    }
                    None => {
                        self.app_status = "No data was captured during baud scanning. Confirm the device is transmitting and try again.".into();
                    }
                },
            }
        }
        let ids: Vec<PortId> = self.ports.iter().map(|p| p.id).collect();
        for id in ids {
            let ready_frames = self
                .ports
                .iter_mut()
                .find(|p| p.id == id)
                .map(|p| p.framer.drain_ready_frames())
                .unwrap_or_default();
            for frame in ready_frames {
                self.push_received(id, frame, Utc::now());
            }
            let frame = self
                .ports
                .iter_mut()
                .find(|p| p.id == id)
                .and_then(|p| p.framer.tick());
            if let Some(frame) = frame {
                self.push_received(id, frame, Utc::now());
            }
        }
    }

    fn push_received(&mut self, id: PortId, bytes: Vec<u8>, timestamp: chrono::DateTime<Utc>) {
        if let Some(port) = self.ports.iter().find(|p| p.id == id) {
            if let Some(bridge) = &port.bridge_handle {
                let _ = bridge.outbound.try_send(bytes.clone());
            }
        }
        let label = self
            .ports
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.alias.clone())
            .unwrap_or_else(|| "Port".into());
        for line in split_line_terminated_records(&bytes) {
            self.add_log(
                id,
                LogRecord {
                    timestamp,
                    relative_us: 0,
                    delta_us: 0,
                    port_label: label.clone(),
                    direction: Direction::Rx,
                    bytes: line,
                    decoded: String::new(),
                    bookmarked: false,
                },
            );
        }
    }

    fn auto_detect(&mut self) {
        let Some(port) = self.selected_port() else {
            return;
        };
        if port.connected {
            self.app_status = "Disconnect before running a baud-rate scan.".into();
            return;
        }
        if port.settings.device.is_empty() {
            self.app_status = "Choose a device before scanning.".into();
            return;
        }

        let id = port.id;
        let device = port.settings.device.clone();
        let parity = port.settings.parity;
        let flow = port.settings.flow_control;
        let events = self.events_tx.clone();
        self.app_status = "Scanning common baud rates using printable-data scoring…".into();
        thread::spawn(move || {
            let rates = [
                300u32, 1200, 2400, 4800, 9600, 19200, 38400, 57600, 115200, 230400, 460800,
                921600, 1_000_000, 1_500_000, 2_000_000, 3_000_000,
            ];
            let parity = match parity {
                ParityChoice::None => serialport::Parity::None,
                ParityChoice::Odd => serialport::Parity::Odd,
                ParityChoice::Even => serialport::Parity::Even,
            };
            let flow = match flow {
                FlowChoice::None => serialport::FlowControl::None,
                FlowChoice::Hardware => serialport::FlowControl::Hardware,
                FlowChoice::Software => serialport::FlowControl::Software,
            };
            let mut winner: Option<(u32, f32)> = None;

            for baud in rates {
                if let Ok(mut handle) = serialport::new(&device, baud)
                    .parity(parity)
                    .flow_control(flow)
                    .timeout(Duration::from_millis(40))
                    .open()
                {
                    let mut received = Vec::with_capacity(4_096);
                    let deadline = Instant::now() + Duration::from_millis(260);
                    let mut chunk = [0u8; 512];
                    while Instant::now() < deadline && received.len() < 8_192 {
                        match handle.read(&mut chunk) {
                            Ok(count) if count > 0 => received.extend_from_slice(&chunk[..count]),
                            Ok(_) => {}
                            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {}
                            Err(_) => break,
                        }
                    }
                    if received.is_empty() {
                        continue;
                    }
                    let score = autobaud_confidence(&received);
                    if score >= 0.82 && winner.map(|(_, best)| score > best).unwrap_or(true) {
                        winner = Some((baud, score));
                    }
                }
            }
            let _ = events.send(WorkerEvent::AutoBaud {
                id,
                baud_rate: winner.map(|(baud, _)| baud.to_string()),
                score: winner.map(|(_, score)| score).unwrap_or(0.0),
            });
        });
    }

    fn run_macro(&mut self) {
        if self.macro_running {
            self.app_status = "A macro is already running.".into();
            return;
        }
        let Some(port) = self.selected_port() else {
            return;
        };
        let Some(_handle) = &port.serial else {
            self.app_status = "Connect a serial port before running a macro.".into();
            return;
        };
        let id = port.id;
        let script = self.macro_command.body.clone();
        let name = self.macro_command.name.trim().to_owned();
        let events = self.events_tx.clone();
        self.macro_running = true;
        self.macro_dispatch_errors.clear();
        self.app_status = format!("Running macro ‘{}’…", name);
        thread::spawn(move || run_macro_script(script, id, name, events));
    }

    fn stream_file(&mut self) {
        let Some(path) = FileDialog::new()
            .add_filter("Firmware / binary", &["bin", "hex", "s19", "srec", "txt"])
            .pick_file()
        else {
            return;
        };
        let Some(port) = self.selected_port() else {
            return;
        };
        let Some(handle) = &port.serial else {
            self.app_status = "Connect a serial port before file streaming.".into();
            return;
        };
        let tx = handle.command_tx.clone();
        let packet = self.send_settings.packet_size.max(1);
        let delay = self.send_settings.inter_packet_delay_ms;
        self.app_status = format!(
            "Streaming {} in {}-byte packets…",
            path.file_name().and_then(|v| v.to_str()).unwrap_or("file"),
            packet
        );
        thread::spawn(move || {
            if let Ok(mut file) = fs::File::open(path) {
                let mut buffer = vec![0u8; packet];
                loop {
                    match file.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(count) => {
                            if tx
                                .send(WorkerCommand::Send(buffer[..count].to_vec()))
                                .is_err()
                            {
                                break;
                            }
                            if delay > 0 {
                                thread::sleep(Duration::from_millis(delay));
                            }
                        }
                    }
                }
            }
        });
    }

    fn fuzz(&mut self) {
        let Some(port) = self.selected_port() else {
            return;
        };
        let Some(handle) = &port.serial else {
            self.app_status = "Connect a serial port before fuzzing.".into();
            return;
        };
        let tx = handle.command_tx.clone();
        let size = self.fuzz_size.clamp(1, 65_536);
        let count = self.fuzz_count.clamp(1, 1_000_000);
        self.app_status = format!("Fuzzing target with {count} random frames of {size} bytes.");
        thread::spawn(move || {
            let mut rng = rand::thread_rng();
            for _ in 0..count {
                let mut bytes = vec![0u8; size];
                rng.fill_bytes(&mut bytes);
                if tx.send(WorkerCommand::Send(bytes)).is_err() {
                    break;
                }
            }
        });
    }

    fn scheduled_tick(&mut self) {
        if !self.scheduled_enabled
            || self.last_scheduled.elapsed()
                < Duration::from_millis(self.send_settings.repeat_interval_ms.max(1))
        {
            return;
        }
        self.last_scheduled = Instant::now();
        self.selected_send();
    }

    fn save_session(&mut self) {
        let path = FileDialog::new()
            .add_filter("Serial Monitor Session", &["json"])
            .set_file_name("serial-session.json")
            .save_file();
        let Some(path) = path else {
            return;
        };
        let session = SessionData {
            version: SESSION_VERSION.into(),
            display: self.display.clone(),
            color_rules: self.color_rules.clone(),
            triggers: self.triggers.clone(),
            ports: self
                .ports
                .iter()
                .map(|p| PortSnapshot {
                    settings: p.settings.clone(),
                    bridge: p.bridge.clone(),
                    logs: p.logs.clone(),
                    send_history: p.send_history.clone(),
                })
                .collect(),
        };
        match serde_json::to_writer_pretty(fs::File::create(&path).unwrap(), &session) {
            Ok(_) => self.app_status = format!("Session saved to {}", path.display()),
            Err(e) => self.app_status = format!("Session save failed: {e}"),
        }
    }

    fn load_session(&mut self) {
        let path = FileDialog::new()
            .add_filter("Serial Monitor Session", &["json"])
            .pick_file();
        let Some(path) = path else {
            return;
        };
        match fs::File::open(&path)
            .ok()
            .and_then(|f| serde_json::from_reader::<_, SessionData>(f).ok())
        {
            Some(session) => {
                for index in (0..self.ports.len()).rev() {
                    self.disconnect_index(index);
                }
                self.display = session.display;
                self.color_rules = session.color_rules;
                self.triggers = session.triggers;
                self.ports = session
                    .ports
                    .into_iter()
                    .map(|snapshot| {
                        let id = self.next_port_id;
                        self.next_port_id += 1;
                        let estimate = snapshot.logs.iter().map(record_weight).sum();
                        PortState {
                            id,
                            alias: format!("Port {id}"),
                            settings: snapshot.settings.clone(),
                            bridge: snapshot.bridge,
                            connected: false,
                            status: "Restored · disconnected".into(),
                            logs: snapshot.logs,
                            memory_estimate: estimate,
                            serial: None,
                            bridge_handle: None,
                            framer: make_framer(&snapshot.settings),
                            connected_since: None,
                            last_record: None,
                            rate_window: Instant::now(),
                            rate_count: 0,
                            rate_dropped: 0,
                            input_dropped_bytes: 0,
                            send_history: snapshot.send_history,
                            break_ms: 100,
                        }
                    })
                    .collect();
                if self.ports.is_empty() {
                    self.add_port();
                }
                self.selected = self.ports.first().map(|p| p.id);
                self.app_status = format!("Session restored from {}", path.display());
            }
            None => self.app_status = "Unable to read this session file.".into(),
        }
    }

    fn export_selected(&mut self) {
        let format = self.export_format;
        let byte_view = self.export_byte_view;
        let scope = self.export_scope;
        let Some(port) = self.selected_port() else {
            return;
        };
        let path = FileDialog::new()
            .add_filter("Export", &[format.extension()])
            .set_file_name(format!("serial-export.{}", format.extension()))
            .save_file();
        let Some(path) = path else {
            return;
        };
        let records: Vec<LogRecord> = port
            .logs
            .iter()
            .filter(|record| match scope {
                ExportScope::AllRetained => true,
                ExportScope::Received => record.direction == Direction::Rx,
                ExportScope::Transmitted => record.direction == Direction::Tx,
                ExportScope::Bookmarked => record.bookmarked,
            })
            .cloned()
            .collect();
        match export_records(&path, format, byte_view, &records) {
            Ok(_) => {
                self.app_status =
                    format!("Exported {} records to {}", records.len(), path.display())
            }
            Err(error) => self.app_status = format!("Export failed: {error}"),
        }
    }

    fn toggle_logging(&mut self) {
        if self.logging.is_some() {
            self.logging = None;
            let path = self
                .logging_path
                .take()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "log file".into());
            self.app_status = format!("Stopped logging to {path}");
            return;
        }
        let Some(path) = FileDialog::new()
            .add_filter("Text log", &["txt", "log"])
            .set_file_name("serial-live-data.log")
            .save_file()
        else {
            return;
        };
        match fs::File::create(&path) {
            Ok(file) => {
                self.logging = Some(BufWriter::new(file));
                self.logging_path = Some(path.clone());
                self.app_status = format!("Logging live data to {}", path.display());
            }
            Err(error) => self.app_status = format!("Unable to start logging: {error}"),
        }
    }

    fn write_log_record(&mut self, record: &LogRecord) {
        let Some(writer) = self.logging.as_mut() else {
            return;
        };
        let line = render_record(record, &self.display);
        if let Err(error) = writeln!(writer, "{line}") {
            let path = self
                .logging_path
                .take()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "log file".into());
            self.logging = None;
            self.app_status = format!("Logging stopped ({path}): {error}");
        } else if let Err(error) = writer.flush() {
            let path = self
                .logging_path
                .take()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "log file".into());
            self.logging = None;
            self.app_status = format!("Logging stopped ({path}): {error}");
        }
    }

    fn ui_toolbar(&mut self, ui: &mut egui::Ui) {
        let palette = diagnostic_palette(self.app_theme);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("SERIAL / MONITOR").strong().color(palette.accent));
            ui.separator();
            if ui.button("Save").clicked() {
                self.save_session();
            }
            if ui.button("Open").clicked() {
                self.load_session();
            }
            if ui.button(if self.paused { "Resume" } else { "Pause" })
                .on_hover_text("Pause or resume retained live-log ingestion. Serial and network bridge transport remain connected.")
                .clicked()
            {
                self.paused = !self.paused;
            }
        });
    }

    fn ui_navigation(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            for view in WorkspaceTab::ALL {
                let label = match view {
                    WorkspaceTab::Appearance => "Theme",
                    _ => view.label(),
                };
                ui.selectable_value(&mut self.active_tab, view, label);
            }
        });
    }

    fn ui_port_tabs(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            for port in &self.ports {
                let title = format!(
                    "{} {}",
                    if port.connected { "●" } else { "○" },
                    connection_display_label(port)
                );
                if ui
                    .selectable_label(self.selected == Some(port.id), title)
                    .clicked()
                {
                    self.selected = Some(port.id);
                }
            }
            if ui
                .button("+ New")
                .on_hover_text("Create a new serial connection")
                .clicked()
            {
                self.add_port();
                self.active_tab = WorkspaceTab::Connection;
            }
            if self.ports.len() > 1 && ui.button("× Close current").clicked() {
                self.close_selected();
            }
        });
    }

    fn ui_left_settings(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
        let Some(index) = self.selected_index() else {
            return;
        };
        let mut do_scan_ports = false;
        let mut do_connect = false;
        let mut do_autobaud = false;
        let mut dtr_change = None;
        let mut rts_change = None;
        let mut break_request = None;
        let palette = diagnostic_palette(self.app_theme);
        let port = &mut self.ports[index];
        section_heading(
            ui,
            "Connection",
            "Choose a direct Serial, TCP, or UDP endpoint for this connection tab.",
        );
        ui.add_enabled_ui(!port.connected, |ui| {
            egui::ComboBox::from_label("Mode")
                .selected_text(port.settings.mode.label())
                .show_ui(ui, |ui| {
                    for mode in ConnectionMode::ALL {
                        ui.selectable_value(&mut port.settings.mode, mode, mode.label());
                    }
                });
        });
        let serial_mode = port.settings.mode == ConnectionMode::Serial;
        if serial_mode {
            ui.horizontal_wrapped(|ui| {
                ui.label("Serial device");
                egui::ComboBox::from_id_salt("serial-device")
                    .selected_text(if port.settings.device.is_empty() {
                        "Select device"
                    } else {
                        &port.settings.device
                    })
                    .show_ui(ui, |ui| {
                        for name in &self.serial_choices {
                            ui.selectable_value(&mut port.settings.device, name.clone(), name);
                        }
                    });
                if ui.button("Scan ports").clicked() {
                    do_scan_ports = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label("Custom baud");
                ui.add(tall_singleline(&mut port.settings.baud_rate).desired_width(110.0));
            });
            ui.horizontal(|ui| {
                ui.label("Data");
                egui::ComboBox::from_id_salt("data_bits")
                    .selected_text(port.settings.data_bits.to_string())
                    .show_ui(ui, |ui| {
                        for b in [5u8, 6, 7, 8] {
                            ui.selectable_value(&mut port.settings.data_bits, b, b.to_string());
                        }
                    });
                ui.label("Stop");
                egui::ComboBox::from_id_salt("stop_bits")
                    .selected_text(port.settings.stop_bits.to_string())
                    .show_ui(ui, |ui| {
                        for b in [1u8, 2] {
                            ui.selectable_value(&mut port.settings.stop_bits, b, b.to_string());
                        }
                    });
            });
            egui::ComboBox::from_label("Parity")
                .selected_text(port.settings.parity.label())
                .show_ui(ui, |ui| {
                    for value in ParityChoice::ALL {
                        ui.selectable_value(&mut port.settings.parity, value, value.label());
                    }
                });
            egui::ComboBox::from_label("Flow control")
                .selected_text(port.settings.flow_control.label())
                .show_ui(ui, |ui| {
                    for value in FlowChoice::ALL {
                        ui.selectable_value(&mut port.settings.flow_control, value, value.label());
                    }
                });
        } else {
            ui.horizontal(|ui| {
                ui.label("Host / bind");
                ui.add(tall_singleline(&mut port.settings.network_host).desired_width(220.0));
            });
            ui.horizontal(|ui| {
                ui.label("Port");
                ui.add(egui::DragValue::new(&mut port.settings.network_port).range(1..=65_535));
            });
            egui::ComboBox::from_label("Role")
                .selected_text(port.settings.network_role.label())
                .show_ui(ui, |ui| {
                    for role in NetworkRole::ALL {
                        ui.selectable_value(&mut port.settings.network_role, role, role.label());
                    }
                });
            ui.label(if port.settings.network_role == NetworkRole::Server {
                "Server/listener binds Host / bind and accepts incoming peers."
            } else {
                "Client connects or sends to Host / bind and Port."
            });
        }
        ui.checkbox(
            &mut port.settings.auto_reconnect,
            if serial_mode {
                "Auto-reconnect serial device"
            } else {
                "Auto-reconnect network endpoint"
            },
        );
        ui.label(
            RichText::new(&port.status)
                .small()
                .color(if port.connected {
                    palette.success
                } else {
                    palette.muted
                }),
        );
        ui.horizontal_wrapped(|ui| {
            if ui
                .button(if port.connected {
                    "Disconnect"
                } else {
                    "Connect"
                })
                .clicked()
            {
                do_connect = true;
            }
            if port.settings.mode == ConnectionMode::Serial && ui.button("Auto baud scan").clicked()
            {
                do_autobaud = true;
            }
        });

        ui.add_space(8.0);
        ui.separator();
        section_heading(
            ui,
            "Framing",
            "Choose how received bytes are separated into records.",
        );
        egui::ComboBox::from_label("Packet framing")
            .selected_text(port.settings.framing.label())
            .show_ui(ui, |ui| {
                for value in FramingMode::ALL {
                    ui.selectable_value(&mut port.settings.framing, value, value.label());
                }
            });
        match port.settings.framing {
            FramingMode::Idle => {
                ui.horizontal(|ui| {
                    ui.label("Idle ms");
                    ui.add(
                        egui::DragValue::new(&mut port.settings.idle_timeout_ms).range(1..=10_000),
                    );
                });
            }
            FramingMode::Delimited => {
                ui.label("Start / end are hexadecimal bytes.");
                ui.add(tall_singleline(&mut port.settings.start_bytes));
                ui.add(tall_singleline(&mut port.settings.end_bytes));
            }
            FramingMode::FixedLength => {
                ui.horizontal(|ui| {
                    ui.label("Bytes per frame");
                    ui.add(egui::DragValue::new(&mut port.settings.fixed_length).range(1..=65_536));
                });
            }
            FramingMode::Raw => {}
        }
        if serial_mode {
            ui.add_space(8.0);
            ui.separator();
            section_heading(
                ui,
                "Electrical control",
                "DTR, RTS, and BREAK apply only to physical serial ports.",
            );
            if ui.checkbox(&mut port.settings.dtr, "DTR").changed() {
                dtr_change = Some(port.settings.dtr);
            }
            if ui.checkbox(&mut port.settings.rts, "RTS").changed() {
                rts_change = Some(port.settings.rts);
            }
            ui.horizontal(|ui| {
                ui.label("Break ms");
                ui.add(egui::DragValue::new(&mut port.break_ms).range(1..=10_000));
                if ui.button("Inject BREAK").clicked() {
                    break_request = Some(port.break_ms);
                }
            });

            if let Some(handle) = &port.serial {
                if let Some(value) = dtr_change {
                    let _ = handle.command_tx.send(WorkerCommand::SetDtr(value));
                }
                if let Some(value) = rts_change {
                    let _ = handle.command_tx.send(WorkerCommand::SetRts(value));
                }
                if let Some(ms) = break_request {
                    let _ = handle.command_tx.send(WorkerCommand::SendBreak(ms));
                }
            }
        }
        if do_connect {
            self.connect_selected();
        }
        if do_autobaud {
            self.auto_detect();
        }
        if do_scan_ports {
            self.serial_choices = refresh_serial_choices();
            self.app_status = "Refreshed available serial devices.".into();
        }
    }

    fn ui_network_settings(&mut self, ui: &mut egui::Ui) {
        let Some(index) = self.selected_index() else {
            ui.label("Select a connection first.");
            return;
        };
        let palette = diagnostic_palette(self.app_theme);
        let mut apply = false;
        let port = &mut self.ports[index];
        section_heading(
            ui,
            "Network bridge",
            "Forward bytes between a physical serial connection and a separate TCP or UDP endpoint.",
        );
        if port.settings.mode != ConnectionMode::Serial {
            ui.label("This tab is a direct network connection. Its TCP or UDP settings are in Connect; the Serial Network Bridge is not used.");
            return;
        }
        ui.label("The bridge is independent of the live-log display and remains available while logging is off.");
        ui.checkbox(&mut port.bridge.enabled, "Enable serial bridge");
        ui.horizontal(|ui| {
            ui.label("Transport");
            ui.selectable_value(&mut port.bridge.udp, false, "TCP");
            ui.selectable_value(&mut port.bridge.udp, true, "UDP");
        });
        ui.horizontal(|ui| {
            ui.label("Mode");
            ui.selectable_value(&mut port.bridge.tcp_server, true, "Server / listener");
            ui.selectable_value(&mut port.bridge.tcp_server, false, "Client");
        });
        ui.horizontal(|ui| {
            ui.label("Host / bind");
            ui.add(tall_singleline(&mut port.bridge.bind_or_host).desired_width(240.0));
        });
        ui.horizontal(|ui| {
            ui.label("Port");
            ui.add(egui::DragValue::new(&mut port.bridge.port).range(1..=65_535));
        });
        ui.separator();
        ui.label(
            RichText::new(if port.bridge_handle.is_some() {
                "Bridge status: active"
            } else if port.bridge.enabled && port.connected {
                "Bridge status: waiting for reconnect/apply"
            } else {
                "Bridge status: inactive"
            })
            .color(if port.bridge_handle.is_some() {
                palette.success
            } else {
                palette.muted
            }),
        );
        if ui.button("Apply bridge").clicked() {
            apply = true;
        }
        ui.label(
            RichText::new("Use only with authorized equipment and networks.")
                .small()
                .color(palette.warning),
        );
        if apply {
            self.restart_bridge(index);
        }
    }

    fn ui_export_settings(&mut self, ui: &mut egui::Ui) {
        section_heading(
            ui,
            "Export retained records",
            "Choose the file representation and record scope for the selected connection.",
        );
        if let Some(port) = self.selected_port() {
            ui.label(format!(
                "Target connection: {} · {} retained records",
                if port.settings.device.is_empty() {
                    &port.alias
                } else {
                    &port.settings.device
                },
                port.logs.len()
            ));
        } else {
            ui.label("Select a connection before exporting.");
        }
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            ui.label("File type");
            ui.selectable_value(&mut self.export_format, ExportFormat::Csv, "CSV");
            ui.selectable_value(&mut self.export_format, ExportFormat::Json, "JSON");
            ui.selectable_value(&mut self.export_format, ExportFormat::Sqlite, "SQLite");
        });
        ui.label("Output bytes");
        egui::ComboBox::from_id_salt("export-popup-byte-view")
            .selected_text(self.export_byte_view.label())
            .show_ui(ui, |ui| {
                for value in ExportByteView::ALL {
                    ui.selectable_value(&mut self.export_byte_view, value, value.label());
                }
            });
        ui.label("Record scope");
        egui::ComboBox::from_id_salt("export-popup-scope")
            .selected_text(self.export_scope.label())
            .show_ui(ui, |ui| {
                for value in ExportScope::ALL {
                    ui.selectable_value(&mut self.export_scope, value, value.label());
                }
            });
        ui.label("Export does not change or clear the retained live log.");
    }

    fn ui_display_settings(&mut self, ui: &mut egui::Ui) {
        section_heading(
            ui,
            "Display & safeguards",
            "Choose byte representations, timestamps, and bounded display budgets.",
        );
        ui.checkbox(&mut self.display.ascii, "ASCII");
        ui.checkbox(&mut self.display.hex, "HEX");
        ui.checkbox(&mut self.display.octal, "Octal");
        ui.checkbox(&mut self.display.binary, "Binary");
        ui.checkbox(&mut self.display.decimal, "Decimal");
        ui.checkbox(&mut self.display.mixed, "Mixed [HEX] ASCII");
        egui::ComboBox::from_label("Timestamp")
            .selected_text(self.display.timestamps.label())
            .show_ui(ui, |ui| {
                for value in TimestampMode::ALL {
                    ui.selectable_value(&mut self.display.timestamps, value, value.label());
                }
            });
        ui.horizontal(|ui| {
            ui.label("Lines/sec limit");
            ui.add(
                egui::DragValue::new(&mut self.display.rate_limit_lines_per_sec).range(0..=100_000),
            );
        });
        ui.horizontal(|ui| {
            ui.label("RAM buffer MiB");
            ui.add(egui::DragValue::new(&mut self.display.max_buffer_mb).range(1..=4096));
        });
        ui.checkbox(
            &mut self.display.anonymize,
            "Mask MAC / identifiers in decoded text",
        );
        ui.separator();
        section_heading(ui, "Colour rules", "Optional regular-expression rules highlight matching displayed records without changing captured bytes.");
        for rule in &mut self.color_rules {
            ui.horizontal(|ui| {
                ui.checkbox(&mut rule.enabled, "");
                let mut color = Color32::from_rgba_unmultiplied(
                    rule.color[0],
                    rule.color[1],
                    rule.color[2],
                    rule.color[3],
                );
                if ui.color_edit_button_srgba(&mut color).changed() {
                    rule.color = color.to_array();
                }
                ui.add(tall_singleline(&mut rule.label).desired_width(130.0));
                ui.add(tall_singleline(&mut rule.pattern).desired_width(190.0));
            });
        }
        if ui.button("Add colour rule").clicked() {
            self.color_rules.push(ColorRule {
                enabled: true,
                pattern: String::new(),
                color: [168, 85, 247, 255],
                label: "New rule".into(),
            });
        }
        ui.label(
            RichText::new("Select a swatch to set a matching rule’s colour.")
                .small()
                .color(diagnostic_palette(self.app_theme).muted),
        );
    }

    fn ui_right_tools(&mut self, ui: &mut egui::Ui) {
        section_heading(ui, "Display & safeguards", "Choose the byte representations, timestamps, and bounded display budgets. Lower the line limit for very high-rate traffic.");
        ui.checkbox(&mut self.display.ascii, "ASCII");
        ui.checkbox(&mut self.display.hex, "HEX");
        ui.checkbox(&mut self.display.octal, "Octal");
        ui.checkbox(&mut self.display.binary, "Binary");
        ui.checkbox(&mut self.display.decimal, "Decimal");
        ui.checkbox(&mut self.display.mixed, "Mixed [HEX] ASCII");
        egui::ComboBox::from_label("Timestamp")
            .selected_text(self.display.timestamps.label())
            .show_ui(ui, |ui| {
                for value in TimestampMode::ALL {
                    ui.selectable_value(&mut self.display.timestamps, value, value.label());
                }
            });
        ui.horizontal(|ui| {
            ui.label("Lines/sec limit");
            ui.add(
                egui::DragValue::new(&mut self.display.rate_limit_lines_per_sec).range(0..=100_000),
            );
        });
        ui.horizontal(|ui| {
            ui.label("RAM buffer MiB");
            ui.add(egui::DragValue::new(&mut self.display.max_buffer_mb).range(1..=4096));
        });
        ui.checkbox(
            &mut self.display.anonymize,
            "Mask MAC / identifiers in decoded text",
        );
        ui.separator();
        section_heading(ui, "Colour rules", "Optional regular-expression rules highlight matching displayed records without changing captured bytes.");
        for rule in &mut self.color_rules {
            ui.horizontal(|ui| {
                ui.checkbox(&mut rule.enabled, "");
                let mut color = Color32::from_rgba_unmultiplied(
                    rule.color[0],
                    rule.color[1],
                    rule.color[2],
                    rule.color[3],
                );
                if ui.color_edit_button_srgba(&mut color).changed() {
                    rule.color = color.to_array();
                }
                ui.add(tall_singleline(&mut rule.label).desired_width(130.0));
                ui.add(tall_singleline(&mut rule.pattern).desired_width(190.0));
            });
        }
        if ui.button("Add colour rule").clicked() {
            self.color_rules.push(ColorRule {
                enabled: true,
                pattern: "".into(),
                color: [168, 85, 247, 255],
                label: "New rule".into(),
            });
        }
        ui.label(
            RichText::new("Select a swatch to set a matching rule’s colour.")
                .small()
                .color(diagnostic_palette(self.app_theme).muted),
        );
        ui.separator();
        section_heading(
            ui,
            "Export format",
            "Configure the file type, byte representation, and record set used by the single Export action in the top toolbar.",
        );
        ui.horizontal_wrapped(|ui| {
            ui.label("File type");
            ui.selectable_value(&mut self.export_format, ExportFormat::Csv, "CSV");
            ui.selectable_value(&mut self.export_format, ExportFormat::Json, "JSON");
            ui.selectable_value(&mut self.export_format, ExportFormat::Sqlite, "SQLite");
        });
        egui::ComboBox::from_label("Output bytes")
            .selected_text(self.export_byte_view.label())
            .show_ui(ui, |ui| {
                for value in ExportByteView::ALL {
                    ui.selectable_value(&mut self.export_byte_view, value, value.label());
                }
            });
        egui::ComboBox::from_label("What to export")
            .selected_text(self.export_scope.label())
            .show_ui(ui, |ui| {
                for value in ExportScope::ALL {
                    ui.selectable_value(&mut self.export_scope, value, value.label());
                }
            });
        ui.label(
            RichText::new(
                "Use Export in the top toolbar to save this connection’s selected records with the chosen byte representation.",
            )
            .small()
            .color(diagnostic_palette(self.app_theme).muted),
        );
    }

    fn ui_advanced_tools(&mut self, ui: &mut egui::Ui) {
        let palette = diagnostic_palette(self.app_theme);
        section_heading(
            ui,
            "Triggered response",
            "Automatically send a response when an incoming record matches a regular expression.",
        );
        for (trigger_index, trigger) in self.triggers.iter_mut().enumerate() {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.checkbox(&mut trigger.enabled, "Enable responder");
                    egui::ComboBox::from_id_salt(("advanced-trigger-encoding", trigger_index))
                        .selected_text(trigger.encoding.label())
                        .show_ui(ui, |ui| {
                            for value in SendEncoding::ALL {
                                ui.selectable_value(&mut trigger.encoding, value, value.label());
                            }
                        });
                });
                ui.label("Incoming regex");
                ui.add(
                    tall_singleline(&mut trigger.incoming_pattern)
                        .desired_width(ui.available_width())
                        .hint_text("e.g. READY|OK"),
                );
                ui.label("Response payload");
                ui.add(
                    tall_singleline(&mut trigger.response)
                        .desired_width(ui.available_width())
                        .hint_text("response to send"),
                );
            });
        }
        if ui.button("Add responder").clicked() {
            self.triggers.push(TriggerRule::default());
        }
        ui.separator();
        section_heading(
            ui,
            "Macro / script engine",
            "Run ordered SEND, SEND_HEX, DELAY, DTR_PULSE, BREAK, and REPEAT/END sequences.",
        );
        ui.horizontal_wrapped(|ui| {
            ui.label("Name");
            ui.add(tall_singleline(&mut self.macro_command.name).desired_width(180.0));
            if ui
                .add_enabled(!self.macro_running, egui::Button::new("Run macro"))
                .clicked()
            {
                self.run_macro();
            }
            if self.macro_running {
                ui.colored_label(palette.warning, "Running…");
            }
        });
        ui.add(
            egui::TextEdit::multiline(&mut self.macro_command.body)
                .font(TextStyle::Monospace)
                .desired_rows(5)
                .hint_text(
                    "SEND text\nSEND_HEX AA 55\nDELAY 50\nDTR_PULSE 100\nREPEAT 3\nSEND PING\nEND",
                ),
        );
        ui.label(
            RichText::new("Commands: SEND, SEND_HEX, DELAY, DTR_PULSE, BREAK, REPEAT n … END.")
                .small()
                .color(palette.muted),
        );
        ui.separator();
        section_heading(ui, "Fuzz & stress testing", "Queues randomized frames for authorized robustness testing. This can destabilize or reset hardware.");
        ui.horizontal_wrapped(|ui| {
            ui.label("Random frame bytes");
            ui.add(egui::DragValue::new(&mut self.fuzz_size).range(1..=65_536));
            ui.label("count");
            ui.add(egui::DragValue::new(&mut self.fuzz_count).range(1..=1_000_000));
            if ui.button("Fuzz target").clicked() {
                self.fuzz();
            }
        });
    }

    fn ui_send_panel(&mut self, ui: &mut egui::Ui) {
        section_heading(
            ui,
            "Transmit",
            "Compose ASCII or hexadecimal data and send it to the selected connected serial port.",
        );
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("encoding")
                .selected_text(self.send_settings.encoding.label())
                .show_ui(ui, |ui| {
                    for value in SendEncoding::ALL {
                        ui.selectable_value(&mut self.send_settings.encoding, value, value.label());
                    }
                });
            egui::ComboBox::from_id_salt("checksum")
                .selected_text(self.send_settings.checksum.label())
                .show_ui(ui, |ui| {
                    for value in ChecksumKind::ALL {
                        ui.selectable_value(&mut self.send_settings.checksum, value, value.label());
                    }
                });
            ui.checkbox(&mut self.send_settings.append_newline, "Append LF");
        });
        ui.add(
            egui::TextEdit::multiline(&mut self.send_buffer)
                .font(TextStyle::Monospace)
                .desired_rows(4)
                .hint_text("Send ASCII text, or hexadecimal bytes such as AA 55 01 00"),
        );
        ui.horizontal_wrapped(|ui| {
            if ui.button(RichText::new("Send  F5").strong()).clicked() {
                self.selected_send();
            }
            ui.checkbox(&mut self.scheduled_enabled, "Repeat");
            ui.label("every ms");
            ui.add(
                egui::DragValue::new(&mut self.send_settings.repeat_interval_ms)
                    .range(1..=86_400_000),
            );
            if ui.button("Stream file…").clicked() {
                self.stream_file();
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("File packet bytes");
            ui.add(egui::DragValue::new(&mut self.send_settings.packet_size).range(1..=65_536));
            ui.label("delay ms");
            ui.add(
                egui::DragValue::new(&mut self.send_settings.inter_packet_delay_ms)
                    .range(0..=10_000),
            );
        });
    }

    fn ui_log_view(&mut self, ui: &mut egui::Ui) {
        let Some(index) = self.selected_index() else {
            return;
        };
        let palette = diagnostic_palette(self.app_theme);
        section_heading(
            ui,
            "Live data",
            "The selected port’s retained RX, TX, and system records.",
        );
        ui.vertical(|ui| {
            let filter_width = ui.available_width().min(420.0);
            ui.label("Filter keywords (comma-separated)");
            ui.add(
                tall_singleline(&mut self.display.keyword_filter)
                    .hint_text("e.g. temperature, error, ACK")
                    .desired_width(filter_width),
            );
            ui.horizontal_wrapped(|ui| {
                if ui.button("Clear").clicked() {
                    if let Some(port) = self.ports.get_mut(index) {
                        port.logs.clear();
                        port.memory_estimate = 0;
                        port.rate_dropped = 0;
                        port.input_dropped_bytes = 0;
                    }
                    self.app_status = "Cleared selected live-data buffer".into();
                }
                if ui
                    .button(if self.logging.is_some() {
                        "Stop logging"
                    } else {
                        "Start logging"
                    })
                    .clicked()
                {
                    self.toggle_logging();
                }
                if ui.button("Export…").clicked() {
                    self.export_dialog_open = true;
                }
            });
        });
        let port = &mut self.ports[index];
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!(
                    "{}",
                    if port.settings.device.is_empty() {
                        &port.alias
                    } else {
                        &port.settings.device
                    }
                ))
                .strong(),
            );
            ui.label(
                RichText::new(format!(
                    "{} records · {:.1} MiB",
                    port.logs.len(),
                    port.memory_estimate as f64 / 1024.0 / 1024.0
                ))
                .small()
                .color(palette.muted),
            );
            ui.colored_label(palette.rx, "RX");
            ui.colored_label(palette.tx, "TX");
            ui.colored_label(palette.system, "SYS");
            ui.colored_label(palette.success, "ACK");
            ui.colored_label(palette.error, "ERR");
            if port.rate_dropped > 0 {
                ui.label(
                    RichText::new(format!("rate limiter: {} pending drops", port.rate_dropped))
                        .color(palette.warning),
                );
            }
            if port.input_dropped_bytes > 0 {
                ui.label(
                    RichText::new(format!(
                        "burst protection: {} bytes shed",
                        port.input_dropped_bytes
                    ))
                    .color(palette.warning),
                );
            }
        });
        const MAX_RENDERED_RECORDS: usize = 600;
        let display = self.display.clone();
        let keyword_filter = display.keyword_filter.trim().to_ascii_lowercase();
        let rules: Vec<(Regex, Color32)> = self
            .color_rules
            .iter()
            .filter(|rule| rule.enabled)
            .filter_map(|rule| {
                Regex::new(&rule.pattern).ok().map(|regex| {
                    (
                        regex,
                        Color32::from_rgba_unmultiplied(
                            rule.color[0],
                            rule.color[1],
                            rule.color[2],
                            rule.color[3],
                        ),
                    )
                })
            })
            .collect();
        let first_rendered = port.logs.len().saturating_sub(MAX_RENDERED_RECORDS);
        egui::ScrollArea::both()
            .id_salt("live-data-records")
            .max_height(ui.available_height().clamp(280.0, 560.0))
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for i in first_rendered..port.logs.len() {
                    let record = port.logs[i].clone();
                    let haystack = format!(
                        "{} {} {}",
                        record.direction.label(),
                        engine::ascii(&record.bytes),
                        engine::hex_bytes(&record.bytes)
                    );
                    if !keyword_filter_matches(&keyword_filter, &haystack) {
                        continue;
                    }
                    let mut color = match record.direction {
                        Direction::Rx => palette.rx,
                        Direction::Tx => palette.tx,
                        Direction::System => palette.system,
                    };
                    for (rule, rule_color) in &rules {
                        if rule.is_match(&haystack) {
                            color = *rule_color;
                            break;
                        }
                    }
                    let rendered = render_record(&record, &display);
                    ui.horizontal(|ui| {
                        let bookmark = if record.bookmarked { "★" } else { "☆" };
                        if ui.small_button(bookmark).clicked() {
                            port.logs[i].bookmarked = !port.logs[i].bookmarked;
                        }
                        if ui
                            .small_button("Copy")
                            .on_hover_text("Copy this record")
                            .clicked()
                        {
                            ui.ctx().copy_text(rendered.clone());
                        }
                        ui.add(
                            egui::Label::new(RichText::new(rendered).monospace().color(color))
                                .selectable(true)
                                .wrap_mode(egui::TextWrapMode::Extend),
                        );
                    });
                }
            });
    }

    fn ui_plotting_placeholder(&mut self, ui: &mut egui::Ui) {
        section_heading(
            ui,
            "Plot values",
            "RX records containing complete brace-delimited numeric fields become interactive value plots.",
        );
        ui.label(
            "Receive values such as {temperature: 24.6, pressure: 101.3, rpm: 1250} to plot them.",
        );
        ui.horizontal_wrapped(|ui| {
            if ui.button("Refresh from retained RX log").clicked() {
                self.rebuild_plot_from_history();
            }
            ui.selectable_value(&mut self.plot.mode, PlotMode::TimeSeries, "Time series");
            ui.selectable_value(&mut self.plot.mode, PlotMode::Xy, "X / Y");
            ui.checkbox(&mut self.plot.paused, "Pause capture");
            if ui.button("Clear values").clicked() {
                self.plot.samples.clear();
                self.plot.latest_values.clear();
                self.plot.fields.clear();
                self.plot.colors.clear();
                self.plot.x_field.clear();
                self.plot.y_field.clear();
                self.plot.started_at = Instant::now();
                self.plot.reset_view = true;
            }
            if ui.button("Reset view").clicked() {
                self.plot.reset_view = true;
            }
        });

        let fields = self.plot.known_fields();
        if fields.is_empty() {
            ui.add_space(20.0);
            ui.label(RichText::new("No numeric values captured yet.").strong());
            ui.label(
                "The Plot widget updates when a received line contains a complete numeric object.",
            );
            return;
        }

        ui.separator();
        match self.plot.mode {
            PlotMode::TimeSeries => {
                ui.label("Visible values");
                ui.horizontal_wrapped(|ui| {
                    for field in &fields {
                        let mut enabled = self.plot.fields.get(field).copied().unwrap_or(true);
                        if ui.checkbox(&mut enabled, field).changed() {
                            self.plot.fields.insert(field.clone(), enabled);
                        }
                        let mut color = self.plot.color_for(field);
                        if ui.color_edit_button_srgba(&mut color).changed() {
                            self.plot.colors.insert(field.clone(), color.to_array());
                        }
                    }
                });
            }
            PlotMode::Xy => {
                if self.plot.x_field.is_empty() || !fields.contains(&self.plot.x_field) {
                    self.plot.x_field = fields[0].clone();
                }
                if self.plot.y_field.is_empty() || !fields.contains(&self.plot.y_field) {
                    self.plot.y_field = fields.get(1).cloned().unwrap_or_else(|| fields[0].clone());
                }
                ui.horizontal_wrapped(|ui| {
                    egui::ComboBox::from_label("X value")
                        .selected_text(&self.plot.x_field)
                        .show_ui(ui, |ui| {
                            for field in &fields {
                                ui.selectable_value(&mut self.plot.x_field, field.clone(), field);
                            }
                        });
                    egui::ComboBox::from_label("Y value")
                        .selected_text(&self.plot.y_field)
                        .show_ui(ui, |ui| {
                            for field in &fields {
                                ui.selectable_value(&mut self.plot.y_field, field.clone(), field);
                            }
                        });
                });
            }
        }

        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.plot.auto_y, "Auto Y range");
            if !self.plot.auto_y {
                ui.label("Y min");
                ui.add(egui::DragValue::new(&mut self.plot.y_min).speed(0.1));
                ui.label("Y max");
                ui.add(egui::DragValue::new(&mut self.plot.y_max).speed(0.1));
                if self.plot.y_max <= self.plot.y_min {
                    self.plot.y_max = self.plot.y_min + 1.0;
                }
            }
            ui.label("Line width");
            ui.add(
                egui::DragValue::new(&mut self.plot.line_width)
                    .range(0.5..=8.0)
                    .speed(0.1),
            );
            ui.label(format!(
                "{} samples · {} values",
                self.plot.samples.len(),
                fields.len()
            ));
        });

        let mut traces: Vec<(String, Vec<[f64; 2]>, Color32)> = Vec::new();
        let (x_label, y_label) = match self.plot.mode {
            PlotMode::TimeSeries => {
                for field in &fields {
                    if !self.plot.fields.get(field).copied().unwrap_or(true) {
                        continue;
                    }
                    let points = collect_field_points(&self.plot.samples, field);
                    if !points.is_empty() {
                        traces.push((field.clone(), points, self.plot.color_for(field)));
                    }
                }
                ("Elapsed seconds".to_owned(), "Value".to_owned())
            }
            PlotMode::Xy => {
                let x_field = self.plot.x_field.clone();
                let y_field = self.plot.y_field.clone();
                let points = plot_xy_points(&self.plot.samples, &x_field, &y_field);
                if !points.is_empty() {
                    traces.push((
                        format!("{} vs {}", y_field, x_field),
                        points,
                        self.plot.color_for(&y_field),
                    ));
                }
                (x_field, y_field)
            }
        };

        let mut plot = Plot::new("value-plot")
            .legend(Legend::default())
            .height(360.0)
            .allow_drag(true)
            .allow_zoom(true)
            .allow_scroll(true)
            .allow_boxed_zoom(true)
            .x_axis_label(x_label)
            .y_axis_label(y_label)
            .show_grid(true);
        if !self.plot.auto_y {
            plot = plot
                .auto_bounds([true, false].into())
                .include_y(self.plot.y_min)
                .include_y(self.plot.y_max);
        }
        if self.plot.reset_view {
            plot = plot.reset();
            self.plot.reset_view = false;
        }
        let width = self.plot.line_width;
        plot.show(ui, |plot_ui| {
            for (name, points, color) in traces {
                plot_ui.line(
                    Line::new(PlotPoints::from(points.clone()))
                        .name(name)
                        .color(color)
                        .width(width),
                );
                plot_ui.points(
                    Points::new(PlotPoints::from(points))
                        .color(color)
                        .radius(3.0),
                );
            }
        });
    }
}

fn autobaud_confidence(bytes: &[u8]) -> f32 {
    if bytes.len() < 24 {
        return 0.0;
    }
    let printable = bytes
        .iter()
        .filter(|byte| {
            **byte == b'\r' || **byte == b'\n' || **byte == b'\t' || (0x20..=0x7e).contains(&**byte)
        })
        .count() as f32
        / bytes.len() as f32;
    let structure = bytes
        .iter()
        .filter(|byte| {
            matches!(
                **byte,
                b'\r' | b'\n' | b'{' | b'}' | b',' | b':' | b'$' | b'!'
            )
        })
        .count() as f32
        / bytes.len() as f32;
    let mut seen = [false; 256];
    for byte in bytes {
        seen[*byte as usize] = true;
    }
    let distinct = seen.iter().filter(|used| **used).count() as f32;
    let variety = (distinct / bytes.len().min(32) as f32).min(1.0);
    let utf8_bonus = if std::str::from_utf8(bytes).is_ok() {
        0.05
    } else {
        0.0
    };
    (printable * 0.70 + structure.min(0.15) + variety * 0.10 + utf8_bonus).min(1.0)
}

fn split_line_terminated_records(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut records = Vec::new();
    let mut line = Vec::new();
    for &byte in bytes {
        if matches!(byte, b'\r' | b'\n') {
            if !line.is_empty() {
                records.push(std::mem::take(&mut line));
            }
        } else {
            line.push(byte);
        }
    }
    if !line.is_empty() {
        records.push(line);
    }
    records
}

fn send_to_port(port: &mut PortState, bytes: Vec<u8>) -> bool {
    let queued = port.serial.as_ref().is_some_and(|handle| {
        handle
            .command_tx
            .send(WorkerCommand::Send(bytes.clone()))
            .is_ok()
    });
    if !queued {
        return false;
    }
    let now = Instant::now();
    let timestamp = Utc::now();
    let relative_us = port
        .connected_since
        .map(|t| t.elapsed().as_micros())
        .unwrap_or(0);
    let delta_us = port
        .last_record
        .map(|t| now.duration_since(t).as_micros())
        .unwrap_or(0);
    for line in split_line_terminated_records(&bytes) {
        let record = LogRecord {
            timestamp,
            relative_us,
            delta_us,
            port_label: port.alias.clone(),
            direction: Direction::Tx,
            bytes: line,
            decoded: String::new(),
            bookmarked: false,
        };
        port.memory_estimate += record_weight(&record);
        port.logs.push(record);
    }
    port.last_record = Some(now);
    true
}

fn append_system_record(port: &mut PortState, message: impl Into<String>) {
    let mut record = now_system_record(format!("{}: {}", port.alias, message.into()));
    record.relative_us = port
        .connected_since
        .map(|started| started.elapsed().as_micros())
        .unwrap_or(0);
    port.memory_estimate += record_weight(&record);
    port.logs.push(record);
}

fn record_weight(record: &LogRecord) -> usize {
    record.bytes.capacity()
        + record.decoded.capacity()
        + record.port_label.capacity()
        + std::mem::size_of::<LogRecord>()
        + 64
}

fn trim_port_logs_to_budget(port: &mut PortState, budget: usize) {
    if port.memory_estimate <= budget {
        return;
    }
    // Trim below the cap in one batch. Draining one record at a time from the
    // front of a Vec shifts every later record and becomes prohibitively costly
    // once the buffer reaches tens of MiB.
    let target = budget.saturating_mul(85) / 100;
    let mut remove_count = 0usize;
    let mut freed = 0usize;
    while port.memory_estimate.saturating_sub(freed) > target && remove_count < port.logs.len() {
        freed += record_weight(&port.logs[remove_count]);
        remove_count += 1;
    }
    if remove_count > 0 {
        port.logs.drain(0..remove_count);
        port.memory_estimate = port.memory_estimate.saturating_sub(freed);
        port.logs.shrink_to(port.logs.len().saturating_add(1_024));
    }
}

fn refresh_serial_choices() -> Vec<String> {
    serialport::available_ports()
        .map(|ports| ports.into_iter().map(|p| p.port_name).collect())
        .unwrap_or_default()
}

/// Produces X/Y points from the latest value snapshot captured for each record.
/// This supports firmware that reports X and Y in alternating messages instead of one object.
fn plot_xy_points(samples: &VecDeque<PlotSample>, x_field: &str, y_field: &str) -> Vec<[f64; 2]> {
    samples
        .iter()
        .filter_map(|sample| {
            Some([
                *sample.xy_values.get(x_field)?,
                *sample.xy_values.get(y_field)?,
            ])
        })
        .collect()
}

#[derive(Clone)]
struct FrequencyMeasurement {
    sample_rate_hz: f64,
    frequency_hz: Option<f64>,
    period_s: Option<f64>,
    min: f64,
    max: f64,
    mean: f64,
    rms: f64,
}

struct SpectrumResult {
    sample_rate_hz: f64,
    dominant_frequency_hz: f64,
    peak_magnitude: f64,
    points: Vec<[f64; 2]>,
}

fn collect_field_points(samples: &VecDeque<PlotSample>, field: &str) -> Vec<[f64; 2]> {
    samples
        .iter()
        .filter_map(|sample| {
            sample
                .values
                .get(field)
                .map(|value| [sample.elapsed_s, *value])
        })
        .collect()
}

fn sample_rate_hz(points: &[[f64; 2]]) -> Option<f64> {
    let mut intervals: Vec<f64> = points
        .windows(2)
        .filter_map(|pair| {
            let delta = pair[1][0] - pair[0][0];
            (delta.is_finite() && delta > 0.0).then_some(delta)
        })
        .collect();
    if intervals.is_empty() {
        return None;
    }
    intervals.sort_by(f64::total_cmp);
    let median = intervals[intervals.len() / 2];
    (median > 0.0).then_some(1.0 / median)
}

fn filter_signal(
    points: &[[f64; 2]],
    filter: SignalFilter,
    window: usize,
    low_pass_cutoff_hz: f64,
) -> Vec<[f64; 2]> {
    if points.is_empty() || filter == SignalFilter::Raw {
        return points.to_vec();
    }
    let window = window.clamp(1, 251);
    match filter {
        SignalFilter::Raw => points.to_vec(),
        SignalFilter::MovingAverage => points
            .iter()
            .enumerate()
            .map(|(index, point)| {
                let start = index.saturating_add(1).saturating_sub(window);
                let values = &points[start..=index];
                let average =
                    values.iter().map(|value| value[1]).sum::<f64>() / values.len() as f64;
                [point[0], average]
            })
            .collect(),
        SignalFilter::Median => points
            .iter()
            .enumerate()
            .map(|(index, point)| {
                let start = index.saturating_add(1).saturating_sub(window);
                let mut values: Vec<f64> =
                    points[start..=index].iter().map(|value| value[1]).collect();
                values.sort_by(f64::total_cmp);
                let middle = values.len() / 2;
                let median = if values.len() % 2 == 0 {
                    (values[middle - 1] + values[middle]) / 2.0
                } else {
                    values[middle]
                };
                [point[0], median]
            })
            .collect(),
        SignalFilter::LowPass => {
            let cutoff = low_pass_cutoff_hz.max(0.001);
            let rc = 1.0 / (std::f64::consts::TAU * cutoff);
            let fallback_dt = sample_rate_hz(points)
                .map(|rate| 1.0 / rate)
                .unwrap_or(0.01);
            let mut previous = points[0][1];
            points
                .iter()
                .enumerate()
                .map(|(index, point)| {
                    if index > 0 {
                        let dt = (point[0] - points[index - 1][0]).max(fallback_dt * 0.1);
                        let alpha = dt / (rc + dt);
                        previous += alpha * (point[1] - previous);
                    }
                    [point[0], previous]
                })
                .collect()
        }
    }
}

fn measure_frequency(points: &[[f64; 2]]) -> Option<FrequencyMeasurement> {
    if points.len() < 3 {
        return None;
    }
    let values: Vec<f64> = points.iter().map(|point| point[1]).collect();
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let rms = (values.iter().map(|value| value * value).sum::<f64>() / values.len() as f64).sqrt();

    let rising_crossings: Vec<f64> = points
        .windows(2)
        .filter_map(|pair| {
            let (left, right) = (pair[0], pair[1]);
            if left[1] <= mean && right[1] > mean && right[1] != left[1] {
                let fraction = (mean - left[1]) / (right[1] - left[1]);
                Some(left[0] + fraction * (right[0] - left[0]))
            } else {
                None
            }
        })
        .collect();
    let periods: Vec<f64> = rising_crossings
        .windows(2)
        .filter_map(|pair| {
            let period = pair[1] - pair[0];
            (period > 0.0 && period.is_finite()).then_some(period)
        })
        .collect();
    let period_s =
        (!periods.is_empty()).then(|| periods.iter().sum::<f64>() / periods.len() as f64);
    let frequency_hz = period_s.map(|period| 1.0 / period);

    Some(FrequencyMeasurement {
        sample_rate_hz: sample_rate_hz(points).unwrap_or(0.0),
        frequency_hz,
        period_s,
        min,
        max,
        mean,
        rms,
    })
}

fn compute_spectrum(points: &[[f64; 2]], requested_window: usize) -> Option<SpectrumResult> {
    let available = points.len().min(requested_window.clamp(16, 4096));
    let next_power = available.checked_next_power_of_two()?;
    let size = if available.is_power_of_two() {
        available
    } else {
        next_power >> 1
    };
    if size < 16 {
        return None;
    }
    let points = &points[points.len() - size..];
    let sample_rate_hz = sample_rate_hz(points)?;
    let mean = points.iter().map(|point| point[1]).sum::<f64>() / size as f64;
    let mut input: Vec<Complex<f64>> = points
        .iter()
        .enumerate()
        .map(|(index, point)| {
            let hann =
                0.5 * (1.0 - (std::f64::consts::TAU * index as f64 / (size - 1) as f64).cos());
            Complex::new((point[1] - mean) * hann, 0.0)
        })
        .collect();
    FftPlanner::<f64>::new()
        .plan_fft_forward(size)
        .process(&mut input);

    let mut dominant_frequency_hz = 0.0;
    let mut peak_magnitude = 0.0;
    let points: Vec<[f64; 2]> = (1..size / 2)
        .map(|bin| {
            let magnitude = 2.0 * input[bin].norm() / size as f64;
            if magnitude > peak_magnitude {
                peak_magnitude = magnitude;
                dominant_frequency_hz = bin as f64 * sample_rate_hz / size as f64;
            }
            [bin as f64 * sample_rate_hz / size as f64, magnitude]
        })
        .collect();
    Some(SpectrumResult {
        sample_rate_hz,
        dominant_frequency_hz,
        peak_magnitude,
        points,
    })
}

/// Matches a Live Data record against any comma-separated keyword, case-insensitively.
/// A blank field leaves all records visible.
fn keyword_filter_matches(keywords: &str, text: &str) -> bool {
    let keywords: Vec<&str> = keywords
        .split(',')
        .map(str::trim)
        .filter(|keyword| !keyword.is_empty())
        .collect();
    keywords.is_empty() || {
        let text = text.to_ascii_lowercase();
        keywords.iter().any(|keyword| text.contains(keyword))
    }
}

/// Parses every complete `{name: value}` object contained in one received record.
/// Parsing is intentionally record-local: incomplete or malformed objects are ignored instead
/// of being joined to a later raw serial chunk and accidentally creating new field names.
fn parse_structured_values(bytes: &[u8]) -> BTreeMap<String, f64> {
    let text = String::from_utf8_lossy(bytes);
    let mut values = BTreeMap::new();
    let mut remaining = text.as_ref();

    while let Some(open) = remaining.find('{') {
        let after_open = &remaining[open + 1..];
        let Some(close) = after_open.find('}') else {
            break;
        };
        values.extend(parse_structured_object(&after_open[..close]));
        remaining = &after_open[close + 1..];
    }

    values
}

fn parse_structured_object(body: &str) -> BTreeMap<String, f64> {
    let mut values = BTreeMap::new();
    for entry in body.split(',') {
        let Some((raw_key, raw_value)) = entry.split_once(':') else {
            continue;
        };
        let key = raw_key.trim().trim_matches(|ch| ch == '\"' || ch == '\'');
        if key.is_empty()
            || !key
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        {
            continue;
        }
        if let Ok(value) = raw_value
            .trim()
            .trim_matches(|ch| ch == '\"' || ch == '\'')
            .parse::<f64>()
        {
            if value.is_finite() {
                values.insert(key.to_owned(), value);
            }
        }
    }
    values
}

#[derive(Default)]
struct MacroExecutionReport {
    sent: usize,
    controls: usize,
    errors: Vec<String>,
}

fn run_macro_script(script: String, id: PortId, name: String, events: Sender<WorkerEvent>) {
    let lines: Vec<String> = script
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(ToOwned::to_owned)
        .collect();
    let mut report = MacroExecutionReport::default();
    if lines.is_empty() {
        report
            .errors
            .push("Macro contains no executable commands.".into());
    } else {
        execute_macro_lines(&lines, &events, id, 0, &mut report);
    }
    let _ = events.send(WorkerEvent::MacroComplete {
        id,
        name,
        sent: report.sent,
        controls: report.controls,
        errors: report.errors,
    });
}

fn queue_macro_event(
    events: &Sender<WorkerEvent>,
    event: WorkerEvent,
    report: &mut MacroExecutionReport,
) -> bool {
    if events.send(event).is_ok() {
        true
    } else {
        report
            .errors
            .push("Macro stopped because the application event channel closed.".into());
        false
    }
}

fn execute_macro_lines(
    lines: &[String],
    events: &Sender<WorkerEvent>,
    id: PortId,
    depth: usize,
    report: &mut MacroExecutionReport,
) {
    let mut i = 0usize;
    while i < lines.len() {
        let line = &lines[i];
        let mut parts = line.splitn(2, char::is_whitespace);
        let op = parts.next().unwrap_or("").to_ascii_uppercase();
        let arg = parts.next().unwrap_or("").trim();
        let line_number = i + 1;
        match op.as_str() {
            "SEND" => {
                if arg.is_empty() {
                    report
                        .errors
                        .push(format!("Line {line_number}: SEND needs text."));
                } else if queue_macro_event(
                    events,
                    WorkerEvent::MacroSend {
                        id,
                        bytes: arg.as_bytes().to_vec(),
                    },
                    report,
                ) {
                    report.sent += 1;
                } else {
                    return;
                }
            }
            "SEND_HEX" => match parse_hex(arg) {
                Ok(data) if !data.is_empty() => {
                    if queue_macro_event(events, WorkerEvent::MacroSend { id, bytes: data }, report)
                    {
                        report.sent += 1;
                    } else {
                        return;
                    }
                }
                Ok(_) => report.errors.push(format!(
                    "Line {line_number}: SEND_HEX needs at least one byte."
                )),
                Err(error) => report.errors.push(format!(
                    "Line {line_number}: invalid SEND_HEX payload: {error}"
                )),
            },
            "DELAY" | "WAIT" => match arg.parse::<u64>() {
                Ok(ms) => thread::sleep(Duration::from_millis(ms)),
                Err(_) => report.errors.push(format!(
                    "Line {line_number}: {op} requires an unsigned millisecond value."
                )),
            },
            "DTR_PULSE" => match arg.parse::<u64>() {
                Ok(ms) => {
                    if queue_macro_event(
                        events,
                        WorkerEvent::MacroControl {
                            id,
                            command: WorkerCommand::PulseDtr(ms),
                            description: format!("DTR pulse for {ms} ms"),
                        },
                        report,
                    ) {
                        report.controls += 1;
                    } else {
                        return;
                    }
                }
                Err(_) => report.errors.push(format!(
                    "Line {line_number}: DTR_PULSE requires an unsigned millisecond value."
                )),
            },
            "BREAK" => match arg.parse::<u64>() {
                Ok(ms) => {
                    if queue_macro_event(
                        events,
                        WorkerEvent::MacroControl {
                            id,
                            command: WorkerCommand::SendBreak(ms),
                            description: format!("BREAK for {ms} ms"),
                        },
                        report,
                    ) {
                        report.controls += 1;
                    } else {
                        return;
                    }
                }
                Err(_) => report.errors.push(format!(
                    "Line {line_number}: BREAK requires an unsigned millisecond value."
                )),
            },
            "REPEAT" if depth < 2 => {
                let Ok(count) = arg.parse::<usize>() else {
                    report.errors.push(format!(
                        "Line {line_number}: REPEAT requires an integer count from 1 to 10,000."
                    ));
                    i += 1;
                    continue;
                };
                if !(1..=10_000).contains(&count) {
                    report.errors.push(format!(
                        "Line {line_number}: REPEAT count must be from 1 to 10,000."
                    ));
                    i += 1;
                    continue;
                }
                let mut end = i + 1;
                let mut nested = 0usize;
                while end < lines.len() {
                    let nested_op = lines[end]
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .to_ascii_uppercase();
                    if nested_op == "REPEAT" {
                        nested += 1;
                    } else if nested_op == "END" {
                        if nested == 0 {
                            break;
                        }
                        nested -= 1;
                    }
                    end += 1;
                }
                if end == lines.len() {
                    report
                        .errors
                        .push(format!("Line {line_number}: REPEAT has no matching END."));
                    return;
                }
                for _ in 0..count {
                    execute_macro_lines(&lines[i + 1..end], events, id, depth + 1, report);
                    if !report.errors.is_empty()
                        && report
                            .errors
                            .last()
                            .is_some_and(|error| error.contains("event channel closed"))
                    {
                        return;
                    }
                }
                i = end;
            }
            "REPEAT" => report.errors.push(format!(
                "Line {line_number}: nested REPEAT depth exceeds the supported limit."
            )),
            "END" => report
                .errors
                .push(format!("Line {line_number}: END has no matching REPEAT.")),
            _ => report
                .errors
                .push(format!("Line {line_number}: unknown macro command ‘{op}’.")),
        }
        i += 1;
    }
}

fn section_heading(ui: &mut egui::Ui, title: &str, help: &str) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(title)
                .size(18.0)
                .strong()
                .color(ui.visuals().strong_text_color()),
        );
        ui.small_button("ⓘ").on_hover_text(help);
    });
    ui.add_space(2.0);
}

fn tall_singleline<'a>(text: &'a mut String) -> egui::TextEdit<'a> {
    egui::TextEdit::singleline(text)
}

fn theme_panel_color(theme: AppTheme) -> Color32 {
    match theme {
        AppTheme::Dark => Color32::from_rgb(18, 24, 33),
        AppTheme::Light => Color32::from_rgb(245, 247, 250),
        AppTheme::Moonlight => Color32::from_rgb(26, 27, 50),
        AppTheme::Nord => Color32::from_rgb(46, 52, 64),
        AppTheme::Solarized => Color32::from_rgb(253, 246, 227),
    }
}

fn native_panel(theme: AppTheme) -> egui::Frame {
    egui::Frame::none()
        .fill(theme_panel_color(theme))
        .inner_margin(8.0)
}
fn chrome_frame(theme: AppTheme) -> egui::Frame {
    egui::Frame::none()
        .fill(theme_panel_color(theme))
        .inner_margin(6.0)
}
fn canvas_frame(theme: AppTheme) -> egui::Frame {
    egui::Frame::none().fill(theme_panel_color(theme))
}

fn apply_theme(ctx: &egui::Context, theme: AppTheme) {
    let is_light = matches!(theme, AppTheme::Light | AppTheme::Solarized);
    ctx.set_theme(if is_light {
        egui::Theme::Light
    } else {
        egui::Theme::Dark
    });

    let mut visuals = if is_light {
        egui::Visuals::light()
    } else {
        egui::Visuals::dark()
    };

    let (panel, window, extreme, faint, text, selection) = match theme {
        AppTheme::Dark => (
            Color32::from_rgb(18, 24, 33),
            Color32::from_rgb(27, 36, 49),
            Color32::from_rgb(10, 14, 20),
            Color32::from_rgb(35, 46, 62),
            Color32::from_rgb(225, 233, 242),
            Color32::from_rgb(34, 211, 238),
        ),
        AppTheme::Light => (
            Color32::from_rgb(245, 247, 250),
            Color32::from_rgb(255, 255, 255),
            Color32::from_rgb(232, 236, 241),
            Color32::from_rgb(225, 230, 236),
            Color32::from_rgb(25, 32, 42),
            Color32::from_rgb(2, 132, 199),
        ),
        AppTheme::Moonlight => (
            Color32::from_rgb(26, 27, 50),
            Color32::from_rgb(36, 38, 68),
            Color32::from_rgb(18, 19, 38),
            Color32::from_rgb(48, 48, 82),
            Color32::from_rgb(235, 236, 255),
            Color32::from_rgb(192, 132, 252),
        ),
        AppTheme::Nord => (
            Color32::from_rgb(46, 52, 64),
            Color32::from_rgb(59, 66, 82),
            Color32::from_rgb(36, 41, 51),
            Color32::from_rgb(67, 76, 94),
            Color32::from_rgb(236, 239, 244),
            Color32::from_rgb(136, 192, 208),
        ),
        AppTheme::Solarized => (
            Color32::from_rgb(253, 246, 227),
            Color32::from_rgb(238, 232, 213),
            Color32::from_rgb(246, 240, 221),
            Color32::from_rgb(238, 232, 213),
            Color32::from_rgb(88, 110, 117),
            Color32::from_rgb(42, 161, 152),
        ),
    };

    visuals.panel_fill = panel;
    visuals.window_fill = window;
    visuals.extreme_bg_color = extreme;
    visuals.faint_bg_color = faint;
    visuals.override_text_color = Some(text);
    visuals.selection.bg_fill = selection;
    visuals.hyperlink_color = selection;
    ctx.set_visuals(visuals);
}

fn theme_config_path() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|base| base.join("Embedded Serial Monitor").join("theme.json"))
}

fn load_theme() -> AppTheme {
    theme_config_path()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|value| serde_json::from_str(&value).ok())
        .unwrap_or(AppTheme::Dark)
}

fn persist_theme(theme: AppTheme) {
    let Some(path) = theme_config_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(value) = serde_json::to_string(&theme) {
        let _ = fs::write(path, value);
    }
}

impl eframe::App for SerialMonitorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();
        self.scheduled_tick();
        if ctx.input(|i| i.key_pressed(egui::Key::F5)) {
            self.selected_send();
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.paused = true;
        }
        egui::TopBottomPanel::top("toolbar")
            .frame(chrome_frame(self.app_theme))
            .show(ctx, |ui| self.ui_toolbar(ui));
        egui::TopBottomPanel::top("port-tabs")
            .frame(chrome_frame(self.app_theme))
            .show(ctx, |ui| self.ui_port_tabs(ui));
        egui::TopBottomPanel::bottom("status")
            .frame(chrome_frame(self.app_theme))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(diagnostic_palette(self.app_theme).accent, "●");
                    ui.label(
                        RichText::new(&self.app_status)
                            .small()
                            .color(diagnostic_palette(self.app_theme).muted),
                    );
                });
            });

        egui::SidePanel::left("widget-navigation")
            .frame(chrome_frame(self.app_theme))
            .resizable(false)
            .default_width(86.0)
            .min_width(72.0)
            .max_width(112.0)
            .show(ctx, |ui| self.ui_navigation(ui));

        if self.active_tab == WorkspaceTab::Monitor {
            egui::TopBottomPanel::bottom("main-transmit-bar")
                .frame(native_panel(self.app_theme))
                .resizable(true)
                .default_height(104.0)
                .min_height(82.0)
                .max_height(170.0)
                .show(ctx, |ui| self.ui_send_panel(ui));
        }
        egui::CentralPanel::default()
            .frame(canvas_frame(self.app_theme))
            .show(ctx, |ui| match self.active_tab {
                WorkspaceTab::Monitor => {
                    native_panel(self.app_theme).show(ui, |ui| self.ui_log_view(ui));
                }
                WorkspaceTab::Connection => {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        native_panel(self.app_theme).show(ui, |ui| self.ui_left_settings(ui, ctx));
                    });
                }
                WorkspaceTab::Network => {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        native_panel(self.app_theme).show(ui, |ui| self.ui_network_settings(ui));
                    });
                }
                WorkspaceTab::Display => {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        native_panel(self.app_theme).show(ui, |ui| self.ui_display_settings(ui));
                    });
                }
                WorkspaceTab::Advanced => {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        native_panel(self.app_theme).show(ui, |ui| self.ui_advanced_tools(ui));
                    });
                }
                WorkspaceTab::Plot => {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        native_panel(self.app_theme)
                            .show(ui, |ui| self.ui_plotting_placeholder(ui));
                    });
                }
                WorkspaceTab::Appearance => {
                    native_panel(self.app_theme).show(ui, |ui| {
                        section_heading(
                            ui,
                            "Appearance",
                            "Choose a readable diagnostic theme. Changes apply immediately.",
                        );
                        ui.horizontal_wrapped(|ui| {
                            for theme in AppTheme::ALL {
                                if ui
                                    .selectable_label(self.app_theme == theme, theme.label())
                                    .clicked()
                                {
                                    self.app_theme = theme;
                                    persist_theme(self.app_theme);
                                    apply_theme(ctx, self.app_theme);
                                }
                            }
                        });
                        ui.add_space(6.0);
                        ui.label(
                            "Custom diagnostic palettes include Moonlight, Nord, and Solarized.",
                        );
                    });
                }
            });

        if self.export_dialog_open {
            let mut open = true;
            let mut save = false;
            let mut cancel = false;
            egui::Window::new("Export")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    self.ui_export_settings(ui);
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() {
                            save = true;
                        }
                        if ui.button("Cancel").clicked() {
                            cancel = true;
                        }
                    });
                });
            if save {
                self.export_selected();
            }
            if save || cancel || !open {
                self.export_dialog_open = false;
            }
        }
        ctx.request_repaint_after(Duration::from_millis(16));
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([640.0, 540.0])
            .with_min_inner_size([560.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native(
        APP_NAME,
        options,
        Box::new(|cc| {
            let theme = load_theme();
            apply_theme(&cc.egui_ctx, theme);
            let mut app = SerialMonitorApp::default();
            app.app_theme = theme;
            Ok(Box::new(app))
        }),
    )
}

#[cfg(test)]
mod plotting_tests {
    use super::{
        compute_spectrum, filter_signal, keyword_filter_matches, measure_frequency,
        parse_structured_values, plot_xy_points, run_macro_script, PlotSample, SignalFilter,
        WorkerCommand, WorkerEvent,
    };
    use crossbeam_channel::unbounded;
    use std::collections::{BTreeMap, VecDeque};

    #[test]
    fn memory_budget_trims_logs_in_amortized_batches() {
        let mut port = super::PortState::new(1);
        for _ in 0..20 {
            let record = super::LogRecord {
                timestamp: chrono::Utc::now(),
                relative_us: 0,
                delta_us: 0,
                port_label: "Port 1".into(),
                direction: super::Direction::Rx,
                bytes: vec![b'X'; 512],
                decoded: "decoded".repeat(32),
                bookmarked: false,
            };
            port.memory_estimate += super::record_weight(&record);
            port.logs.push(record);
        }
        let budget = 2_048;
        super::trim_port_logs_to_budget(&mut port, budget);
        assert!(port.memory_estimate <= budget * 85 / 100);
        assert!(port.logs.len() < 20);
    }

    #[test]
    fn autobaud_confidence_prefers_structured_text_over_false_positives() {
        let structured = b"{hello: value, count: 42}\r\n{hello: value, count: 43}\r\n";
        assert!(super::autobaud_confidence(structured) >= 0.82);
        assert!(super::autobaud_confidence(&vec![b'+'; 64]) < 0.82);
        assert!(super::autobaud_confidence(&[0x00, 0xFF, 0x80, 0x01].repeat(16)) < 0.82);
    }

    #[test]
    fn line_terminators_split_into_readable_records() {
        let records = super::split_line_terminated_records(b"{a: 1}\r\n{b: 2}\n{c: 3}\r{d: 4}");
        assert_eq!(
            records,
            vec![
                b"{a: 1}".to_vec(),
                b"{b: 2}".to_vec(),
                b"{c: 3}".to_vec(),
                b"{d: 4}".to_vec()
            ]
        );
        assert!(super::split_line_terminated_records(b"\r\n").is_empty());
    }

    #[test]
    fn macro_executor_reports_actions_completion_and_errors() {
        let (events_tx, events_rx) = unbounded();
        run_macro_script(
            "SEND HELLO\nSEND_HEX AA 55\nDTR_PULSE 4\nREPEAT 2\nSEND X\nEND".into(),
            7,
            "test".into(),
            events_tx,
        );
        let events: Vec<WorkerEvent> = events_rx.try_iter().collect();
        assert_eq!(events.len(), 6);
        assert!(matches!(
            &events[0],
            WorkerEvent::MacroSend { id: 7, bytes } if bytes == b"HELLO"
        ));
        assert!(matches!(
            &events[1],
            WorkerEvent::MacroSend { id: 7, bytes } if bytes == &[0xAA, 0x55]
        ));
        assert!(matches!(
            &events[2],
            WorkerEvent::MacroControl {
                id: 7,
                command: WorkerCommand::PulseDtr(4),
                ..
            }
        ));
        assert!(matches!(
            &events[3],
            WorkerEvent::MacroSend { id: 7, bytes } if bytes == b"X"
        ));
        assert!(matches!(
            &events[4],
            WorkerEvent::MacroSend { id: 7, bytes } if bytes == b"X"
        ));
        assert!(matches!(
            &events[5],
            WorkerEvent::MacroComplete {
                id: 7,
                sent: 4,
                controls: 1,
                errors,
                ..
            } if errors.is_empty()
        ));

        let (events_tx, events_rx) = unbounded();
        run_macro_script("SEND_HEX XYZ\nUNKNOWN".into(), 7, "bad".into(), events_tx);
        let complete = events_rx
            .try_iter()
            .find_map(|event| match event {
                WorkerEvent::MacroComplete { errors, .. } => Some(errors),
                _ => None,
            })
            .expect("macro completion event");
        assert_eq!(complete.len(), 2);
    }

    #[test]
    fn analysis_filters_measure_frequency_and_find_fft_peak() {
        let sample_rate = 64.0;
        let expected_frequency = 4.0;
        let points: Vec<[f64; 2]> = (0..128)
            .map(|index| {
                let time = index as f64 / sample_rate;
                [
                    time,
                    (std::f64::consts::TAU * expected_frequency * time).sin(),
                ]
            })
            .collect();
        let moving_average = filter_signal(
            &[[0.0, 0.0], [1.0, 10.0], [2.0, 0.0]],
            SignalFilter::MovingAverage,
            2,
            1.0,
        );
        assert_eq!(moving_average, vec![[0.0, 0.0], [1.0, 5.0], [2.0, 5.0]]);

        let measurement = measure_frequency(&points).expect("sine wave can be measured");
        assert!((measurement.sample_rate_hz - sample_rate).abs() < 1e-9);
        assert!(
            (measurement.frequency_hz.expect("crossings exist") - expected_frequency).abs() < 1e-9
        );

        let spectrum = compute_spectrum(&points, 128).expect("sine wave has an FFT");
        assert!((spectrum.dominant_frequency_hz - expected_frequency).abs() < 1e-9);
    }

    #[test]
    fn structured_parser_rejects_partial_and_cross_record_fragments() {
        assert!(parse_structured_values(b"noise hello: 2}").is_empty());
        assert!(parse_structured_values(b"{hi: 1, he").is_empty());
        assert!(parse_structured_values(b"llo: 2}").is_empty());
        assert_eq!(
            parse_structured_values(b"{hi: 1, hello: 2}"),
            BTreeMap::from([("hello".to_owned(), 2.0), ("hi".to_owned(), 1.0)])
        );
    }

    #[test]
    fn xy_points_use_latest_value_snapshots() {
        let samples = VecDeque::from([PlotSample {
            elapsed_s: 0.0,
            values: BTreeMap::from([("x".to_owned(), 10.0)]),
            xy_values: BTreeMap::from([("x".to_owned(), 10.0), ("y".to_owned(), 20.0)]),
        }]);
        assert_eq!(plot_xy_points(&samples, "x", "y"), vec![[10.0, 20.0]]);
    }

    #[test]
    fn keyword_filter_is_case_insensitive_and_matches_any_comma_separated_term() {
        assert!(keyword_filter_matches("", "temperature: 24.6"));
        assert!(keyword_filter_matches(
            "error, temperature",
            "Temperature: 24.6"
        ));
        assert!(keyword_filter_matches("error, timeout", "serial TIMEOUT"));
        assert!(!keyword_filter_matches(
            "error, timeout",
            "temperature: 24.6"
        ));
    }

    #[test]
    fn parses_numeric_brace_delimited_fields() {
        let values =
            parse_structured_values(b"status { temperature: 24.6, rpm: 1250, voltage: -3.25 }");
        assert_eq!(values.get("temperature"), Some(&24.6));
        assert_eq!(values.get("rpm"), Some(&1250.0));
        assert_eq!(values.get("voltage"), Some(&-3.25));
    }

    #[test]
    fn ignores_invalid_names_and_non_numeric_values() {
        let values =
            parse_structured_values(b"{ good: 1, bad value: 2, text: nope, nan: NaN, later: 3.5 }");
        assert_eq!(values.get("good"), Some(&1.0));
        assert_eq!(values.get("later"), Some(&3.5));
        assert!(!values.contains_key("bad value"));
        assert!(!values.contains_key("text"));
        assert!(!values.contains_key("nan"));
    }
}
