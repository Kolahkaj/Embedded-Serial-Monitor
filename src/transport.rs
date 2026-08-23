use std::{
    io::{ErrorKind, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream, UdpSocket},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use chrono::Utc;
use crossbeam_channel::{bounded, unbounded, Receiver, Sender, TrySendError};
use serialport::{DataBits, FlowControl, Parity, SerialPort, StopBits};

use crate::types::{
    BridgeConfig, ConnectionMode, FlowChoice, NetworkRole, ParityChoice, PortId, PortSettings,
    WorkerCommand, WorkerEvent,
};

pub struct SerialHandle {
    pub command_tx: Sender<WorkerCommand>,
}

const UI_BATCH_BYTES: usize = 16 * 1024;
const MAX_PENDING_UI_BYTES: usize = 256 * 1024;
const MAX_BATCH_LATENCY: Duration = Duration::from_millis(16);

fn flush_received_data(
    events: &Sender<WorkerEvent>,
    id: PortId,
    pending: &mut Vec<u8>,
    dropped_bytes: &mut u64,
    last_emit: &mut std::time::Instant,
    force: bool,
) -> bool {
    if pending.is_empty()
        || (!force && pending.len() < UI_BATCH_BYTES && last_emit.elapsed() < MAX_BATCH_LATENCY)
    {
        return true;
    }
    let bytes = std::mem::take(pending);
    let dropped = std::mem::take(dropped_bytes);
    match events.try_send(WorkerEvent::Data {
        id,
        bytes,
        timestamp: Utc::now(),
        dropped_bytes: dropped,
    }) {
        Ok(()) => {
            *last_emit = std::time::Instant::now();
            true
        }
        Err(TrySendError::Full(WorkerEvent::Data {
            bytes,
            dropped_bytes: dropped_on_event,
            ..
        })) => {
            *pending = bytes;
            *dropped_bytes = dropped_on_event;
            false
        }
        Err(TrySendError::Disconnected(_)) => false,
        Err(TrySendError::Full(_)) => false,
    }
}

pub fn open_serial_worker(
    id: PortId,
    settings: PortSettings,
    events: Sender<WorkerEvent>,
) -> SerialHandle {
    let (command_tx, command_rx) = unbounded::<WorkerCommand>();
    thread::Builder::new()
        .name(format!("serial-{}", id))
        .spawn(move || serial_loop(id, settings, command_rx, events))
        .expect("Start serial worker");
    SerialHandle { command_tx }
}

/// Opens the selected first-class connection mode. Serial retains its existing
/// worker while TCP and UDP use direct network workers with the same command and
/// event interfaces as a serial port.
pub fn open_connection_worker(
    id: PortId,
    settings: PortSettings,
    events: Sender<WorkerEvent>,
) -> SerialHandle {
    match settings.mode {
        ConnectionMode::Serial => open_serial_worker(id, settings, events),
        ConnectionMode::Tcp | ConnectionMode::Udp => {
            let (command_tx, command_rx) = unbounded::<WorkerCommand>();
            let name = format!("{}-{}", settings.mode.label().to_ascii_lowercase(), id);
            thread::Builder::new()
                .name(name)
                .spawn(move || match settings.mode {
                    ConnectionMode::Tcp => tcp_connection_loop(id, settings, command_rx, events),
                    ConnectionMode::Udp => udp_connection_loop(id, settings, command_rx, events),
                    ConnectionMode::Serial => unreachable!("serial mode is handled above"),
                })
                .expect("Start network connection worker");
            SerialHandle { command_tx }
        }
    }
}

fn serial_loop(
    id: PortId,
    settings: PortSettings,
    commands: Receiver<WorkerCommand>,
    events: Sender<WorkerEvent>,
) {
    let label = if settings.device.is_empty() {
        "serial port".into()
    } else {
        settings.device.clone()
    };
    'reconnect: loop {
        while let Ok(command) = commands.try_recv() {
            if matches!(command, WorkerCommand::Disconnect) {
                break 'reconnect;
            }
        }
        match open_port(&settings) {
            Ok(mut port) => {
                let _ = port.write_data_terminal_ready(settings.dtr);
                let _ = port.write_request_to_send(settings.rts);
                let _ = events.send(WorkerEvent::Status {
                    id,
                    message: format!("Connected: {} @ {}", label, settings.baud_rate),
                    connected: true,
                });
                let mut buffer = [0u8; 4096];
                let mut pending = Vec::with_capacity(UI_BATCH_BYTES * 2);
                let mut pending_dropped_bytes = 0u64;
                let mut last_emit = std::time::Instant::now();
                loop {
                    while let Ok(command) = commands.try_recv() {
                        match command {
                            WorkerCommand::Send(data) => {
                                if let Err(error) = port.write_all(&data).and_then(|_| port.flush())
                                {
                                    let _ = events.send(WorkerEvent::Status {
                                        id,
                                        message: format!("Write error: {error}"),
                                        connected: false,
                                    });
                                    break;
                                }
                            }
                            WorkerCommand::SetDtr(value) => {
                                let _ = port.write_data_terminal_ready(value);
                            }
                            WorkerCommand::SetRts(value) => {
                                let _ = port.write_request_to_send(value);
                            }
                            WorkerCommand::PulseDtr(ms) => {
                                let _ = port.write_data_terminal_ready(false);
                                thread::sleep(Duration::from_millis(ms));
                                let _ = port.write_data_terminal_ready(true);
                            }
                            WorkerCommand::SendBreak(ms) => {
                                let _ = port.set_break();
                                thread::sleep(Duration::from_millis(ms));
                                let _ = port.clear_break();
                            }
                            WorkerCommand::Disconnect => {
                                let _ = events.send(WorkerEvent::Status {
                                    id,
                                    message: "Disconnected by user".into(),
                                    connected: false,
                                });
                                return;
                            }
                        }
                    }
                    let _ = flush_received_data(
                        &events,
                        id,
                        &mut pending,
                        &mut pending_dropped_bytes,
                        &mut last_emit,
                        false,
                    );
                    match port.read(&mut buffer) {
                        Ok(count) if count > 0 => {
                            let available = MAX_PENDING_UI_BYTES.saturating_sub(pending.len());
                            let accepted = available.min(count);
                            pending.extend_from_slice(&buffer[..accepted]);
                            pending_dropped_bytes += (count - accepted) as u64;
                            let force_flush = pending.len() >= UI_BATCH_BYTES;
                            let _ = flush_received_data(
                                &events,
                                id,
                                &mut pending,
                                &mut pending_dropped_bytes,
                                &mut last_emit,
                                force_flush,
                            );
                        }
                        Ok(_) => {}
                        Err(error) if error.kind() == ErrorKind::TimedOut => {}
                        Err(error) => {
                            let _ = events.send(WorkerEvent::Status {
                                id,
                                message: format!("Port disconnected: {error}"),
                                connected: false,
                            });
                            break;
                        }
                    }
                }
            }
            Err(error) => {
                let _ = events.send(WorkerEvent::Status {
                    id,
                    message: format!("Open failed: {error}"),
                    connected: false,
                });
            }
        }
        if !settings.auto_reconnect {
            break;
        }
        for _ in 0..10 {
            if matches!(commands.try_recv(), Ok(WorkerCommand::Disconnect)) {
                return;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
}

fn append_network_bytes(
    events: &Sender<WorkerEvent>,
    id: PortId,
    pending: &mut Vec<u8>,
    dropped_bytes: &mut u64,
    last_emit: &mut std::time::Instant,
    bytes: &[u8],
) -> bool {
    let available = MAX_PENDING_UI_BYTES.saturating_sub(pending.len());
    let accepted = available.min(bytes.len());
    pending.extend_from_slice(&bytes[..accepted]);
    *dropped_bytes += (bytes.len() - accepted) as u64;
    flush_received_data(
        events,
        id,
        pending,
        dropped_bytes,
        last_emit,
        pending.len() >= UI_BATCH_BYTES,
    )
}

fn wait_for_reconnect_or_disconnect(commands: &Receiver<WorkerCommand>) -> bool {
    for _ in 0..10 {
        if matches!(commands.try_recv(), Ok(WorkerCommand::Disconnect)) {
            return true;
        }
        thread::sleep(Duration::from_millis(100));
    }
    false
}

fn tcp_connection_loop(
    id: PortId,
    settings: PortSettings,
    commands: Receiver<WorkerCommand>,
    events: Sender<WorkerEvent>,
) {
    let address = format!("{}:{}", settings.network_host.trim(), settings.network_port);
    'reconnect: loop {
        match settings.network_role {
            NetworkRole::Client => match TcpStream::connect(&address) {
                Ok(stream) => {
                    if tcp_stream_loop(id, stream, &commands, &events, &address, false) {
                        return;
                    }
                }
                Err(error) => {
                    let _ = events.send(WorkerEvent::Status {
                        id,
                        message: format!("TCP connection failed ({address}): {error}"),
                        connected: false,
                    });
                }
            },
            NetworkRole::Server => match TcpListener::bind(&address) {
                Ok(listener) => {
                    if listener.set_nonblocking(true).is_err() {
                        let _ = events.send(WorkerEvent::Status {
                            id,
                            message: "TCP listener could not enter nonblocking mode".into(),
                            connected: false,
                        });
                    } else {
                        let _ = events.send(WorkerEvent::Status {
                            id,
                            message: format!("TCP listening on {address}"),
                            connected: true,
                        });
                        loop {
                            while let Ok(command) = commands.try_recv() {
                                match command {
                                    WorkerCommand::Disconnect => {
                                        let _ = events.send(WorkerEvent::Status {
                                            id,
                                            message: "TCP listener stopped by user".into(),
                                            connected: false,
                                        });
                                        return;
                                    }
                                    WorkerCommand::Send(_) => {
                                        let _ = events.send(WorkerEvent::Status {
                                            id,
                                            message: "No TCP peer is connected to receive data"
                                                .into(),
                                            connected: true,
                                        });
                                    }
                                    _ => {}
                                }
                            }
                            match listener.accept() {
                                Ok((stream, peer)) => {
                                    let peer_address = peer.to_string();
                                    let _ = events.send(WorkerEvent::Status {
                                        id,
                                        message: format!("TCP peer connected: {peer_address}"),
                                        connected: true,
                                    });
                                    if tcp_stream_loop(
                                        id,
                                        stream,
                                        &commands,
                                        &events,
                                        &peer_address,
                                        true,
                                    ) {
                                        return;
                                    }
                                    let _ = events.send(WorkerEvent::Status {
                                        id,
                                        message: format!("TCP listening on {address}"),
                                        connected: true,
                                    });
                                }
                                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                                    thread::sleep(Duration::from_millis(15));
                                }
                                Err(error) => {
                                    let _ = events.send(WorkerEvent::Status {
                                        id,
                                        message: format!("TCP listener failed: {error}"),
                                        connected: false,
                                    });
                                    break;
                                }
                            }
                        }
                    }
                }
                Err(error) => {
                    let _ = events.send(WorkerEvent::Status {
                        id,
                        message: format!("TCP listener failed ({address}): {error}"),
                        connected: false,
                    });
                }
            },
        }
        if !settings.auto_reconnect || wait_for_reconnect_or_disconnect(&commands) {
            break 'reconnect;
        }
    }
}

/// Returns true only when the user explicitly disconnects the connection.
fn tcp_stream_loop(
    id: PortId,
    mut stream: TcpStream,
    commands: &Receiver<WorkerCommand>,
    events: &Sender<WorkerEvent>,
    peer: &str,
    server_mode: bool,
) -> bool {
    if let Err(error) = stream.set_nonblocking(true) {
        let _ = events.send(WorkerEvent::Status {
            id,
            message: format!("TCP setup failed: {error}"),
            connected: false,
        });
        return false;
    }
    if !server_mode {
        let _ = events.send(WorkerEvent::Status {
            id,
            message: format!("TCP connected: {peer}"),
            connected: true,
        });
    }
    let mut buffer = [0u8; 4096];
    let mut pending = Vec::with_capacity(UI_BATCH_BYTES * 2);
    let mut pending_dropped_bytes = 0u64;
    let mut last_emit = std::time::Instant::now();
    loop {
        while let Ok(command) = commands.try_recv() {
            match command {
                WorkerCommand::Send(data) => {
                    if let Err(error) = stream.write_all(&data) {
                        let _ = events.send(WorkerEvent::Status {
                            id,
                            message: format!("TCP write failed: {error}"),
                            connected: false,
                        });
                        return false;
                    }
                }
                WorkerCommand::Disconnect => {
                    let _ = events.send(WorkerEvent::Status {
                        id,
                        message: "TCP disconnected by user".into(),
                        connected: false,
                    });
                    return true;
                }
                _ => {}
            }
        }
        let _ = flush_received_data(
            events,
            id,
            &mut pending,
            &mut pending_dropped_bytes,
            &mut last_emit,
            false,
        );
        match stream.read(&mut buffer) {
            Ok(0) => {
                let _ = flush_received_data(
                    events,
                    id,
                    &mut pending,
                    &mut pending_dropped_bytes,
                    &mut last_emit,
                    true,
                );
                let _ = events.send(WorkerEvent::Status {
                    id,
                    message: format!("TCP peer disconnected: {peer}"),
                    connected: false,
                });
                return false;
            }
            Ok(count) => {
                let _ = append_network_bytes(
                    events,
                    id,
                    &mut pending,
                    &mut pending_dropped_bytes,
                    &mut last_emit,
                    &buffer[..count],
                );
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => {
                let _ = events.send(WorkerEvent::Status {
                    id,
                    message: format!("TCP read failed: {error}"),
                    connected: false,
                });
                return false;
            }
        }
    }
}

fn udp_connection_loop(
    id: PortId,
    settings: PortSettings,
    commands: Receiver<WorkerCommand>,
    events: Sender<WorkerEvent>,
) {
    let endpoint = format!("{}:{}", settings.network_host.trim(), settings.network_port);
    'reconnect: loop {
        let bind_address = if settings.network_role == NetworkRole::Server {
            endpoint.as_str()
        } else {
            "0.0.0.0:0"
        };
        match UdpSocket::bind(bind_address) {
            Ok(socket) => {
                if let Err(error) = socket.set_nonblocking(true) {
                    let _ = events.send(WorkerEvent::Status {
                        id,
                        message: format!("UDP setup failed: {error}"),
                        connected: false,
                    });
                } else {
                    let status = if settings.network_role == NetworkRole::Server {
                        format!("UDP listening on {endpoint}")
                    } else {
                        format!("UDP client ready for {endpoint}")
                    };
                    let _ = events.send(WorkerEvent::Status {
                        id,
                        message: status,
                        connected: true,
                    });
                    let mut buffer = [0u8; 4096];
                    let mut pending = Vec::with_capacity(UI_BATCH_BYTES * 2);
                    let mut pending_dropped_bytes = 0u64;
                    let mut last_emit = std::time::Instant::now();
                    let mut latest_peer: Option<SocketAddr> = None;
                    loop {
                        while let Ok(command) = commands.try_recv() {
                            match command {
                                WorkerCommand::Send(data) => {
                                    let result = if settings.network_role == NetworkRole::Client {
                                        socket.send_to(&data, &endpoint)
                                    } else if let Some(peer) = latest_peer {
                                        socket.send_to(&data, peer)
                                    } else {
                                        let _ = events.send(WorkerEvent::Status {
                                            id,
                                            message: "UDP listener has not received a peer yet"
                                                .into(),
                                            connected: true,
                                        });
                                        continue;
                                    };
                                    if let Err(error) = result {
                                        let _ = events.send(WorkerEvent::Status {
                                            id,
                                            message: format!("UDP send failed: {error}"),
                                            connected: false,
                                        });
                                    }
                                }
                                WorkerCommand::Disconnect => {
                                    let _ = events.send(WorkerEvent::Status {
                                        id,
                                        message: "UDP disconnected by user".into(),
                                        connected: false,
                                    });
                                    return;
                                }
                                _ => {}
                            }
                        }
                        let _ = flush_received_data(
                            &events,
                            id,
                            &mut pending,
                            &mut pending_dropped_bytes,
                            &mut last_emit,
                            false,
                        );
                        match socket.recv_from(&mut buffer) {
                            Ok((count, peer)) => {
                                latest_peer = Some(peer);
                                let _ = append_network_bytes(
                                    &events,
                                    id,
                                    &mut pending,
                                    &mut pending_dropped_bytes,
                                    &mut last_emit,
                                    &buffer[..count],
                                );
                            }
                            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                                thread::sleep(Duration::from_millis(5));
                            }
                            Err(error) => {
                                let _ = events.send(WorkerEvent::Status {
                                    id,
                                    message: format!("UDP receive failed: {error}"),
                                    connected: false,
                                });
                                break;
                            }
                        }
                    }
                }
            }
            Err(error) => {
                let _ = events.send(WorkerEvent::Status {
                    id,
                    message: format!("UDP bind failed ({bind_address}): {error}"),
                    connected: false,
                });
            }
        }
        if !settings.auto_reconnect || wait_for_reconnect_or_disconnect(&commands) {
            break 'reconnect;
        }
    }
}

fn open_port(settings: &PortSettings) -> Result<Box<dyn SerialPort>> {
    let baud = settings
        .baud_rate
        .trim()
        .parse::<u32>()
        .context("Baud rate must be a positive integer")?;
    let data_bits = match settings.data_bits {
        5 => DataBits::Five,
        6 => DataBits::Six,
        7 => DataBits::Seven,
        _ => DataBits::Eight,
    };
    let stop_bits = if settings.stop_bits == 2 {
        StopBits::Two
    } else {
        StopBits::One
    };
    let parity = match settings.parity {
        ParityChoice::None => Parity::None,
        ParityChoice::Odd => Parity::Odd,
        ParityChoice::Even => Parity::Even,
    };
    let flow = match settings.flow_control {
        FlowChoice::None => FlowControl::None,
        FlowChoice::Hardware => FlowControl::Hardware,
        FlowChoice::Software => FlowControl::Software,
    };
    serialport::new(&settings.device, baud)
        .data_bits(data_bits)
        .stop_bits(stop_bits)
        .parity(parity)
        .flow_control(flow)
        .timeout(Duration::from_millis(20))
        .open()
        .context("Open serial port")
}

pub struct BridgeHandle {
    pub outbound: Sender<Vec<u8>>,
    stop: Arc<AtomicBool>,
}

impl BridgeHandle {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

pub fn start_bridge(
    id: PortId,
    config: BridgeConfig,
    events: Sender<WorkerEvent>,
) -> Result<BridgeHandle> {
    let (out_tx, out_rx) = bounded::<Vec<u8>>(512);
    let stop = Arc::new(AtomicBool::new(false));
    if config.udp {
        start_udp_bridge(id, config, events, out_rx, Arc::clone(&stop))?;
    } else if config.tcp_server {
        start_tcp_server(id, config, events, out_rx, Arc::clone(&stop))?;
    } else {
        start_tcp_client(id, config, events, out_rx, Arc::clone(&stop))?;
    }
    Ok(BridgeHandle {
        outbound: out_tx,
        stop,
    })
}

fn start_tcp_server(
    id: PortId,
    config: BridgeConfig,
    events: Sender<WorkerEvent>,
    outbound: Receiver<Vec<u8>>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    let address = format!("{}:{}", config.bind_or_host, config.port);
    let listener =
        TcpListener::bind(&address).with_context(|| format!("Bind TCP server {address}"))?;
    listener.set_nonblocking(true)?;
    let clients: Arc<Mutex<Vec<TcpStream>>> = Arc::new(Mutex::new(Vec::new()));
    let client_list = Arc::clone(&clients);
    let input_events = events.clone();
    let accept_stop = Arc::clone(&stop);
    thread::spawn(move || loop {
        if accept_stop.load(Ordering::Relaxed) {
            break;
        }
        match listener.accept() {
            Ok((mut stream, peer)) => {
                let _ = stream.set_nonblocking(true);
                if let Ok(copy) = stream.try_clone() {
                    client_list.lock().unwrap().push(copy);
                }
                let event_sink = input_events.clone();
                let client_stop = Arc::clone(&accept_stop);
                thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    loop {
                        if client_stop.load(Ordering::Relaxed) {
                            break;
                        }
                        match stream.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                let _ = event_sink.try_send(WorkerEvent::NetworkData {
                                    id,
                                    bytes: buf[..n].to_vec(),
                                });
                            }
                            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                                thread::sleep(Duration::from_millis(10))
                            }
                            Err(_) => break,
                        }
                    }
                });
                let _ = input_events.send(WorkerEvent::BridgeStatus {
                    id,
                    message: format!("TCP peer connected: {peer}"),
                });
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(20)),
            Err(e) => {
                let _ = input_events.send(WorkerEvent::BridgeStatus {
                    id,
                    message: format!("TCP listener stopped: {e}"),
                });
                break;
            }
        }
    });
    thread::spawn(move || loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if let Ok(data) = outbound.recv_timeout(Duration::from_millis(50)) {
            let mut guard = clients.lock().unwrap();
            guard.retain_mut(|stream| stream.write_all(&data).is_ok());
        }
    });
    Ok(())
}

fn start_tcp_client(
    id: PortId,
    config: BridgeConfig,
    events: Sender<WorkerEvent>,
    outbound: Receiver<Vec<u8>>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    let address = format!("{}:{}", config.bind_or_host, config.port);
    let mut stream =
        TcpStream::connect(&address).with_context(|| format!("Connect TCP client {address}"))?;
    stream.set_nonblocking(true)?;
    let mut input = stream.try_clone()?;
    let input_events = events.clone();
    let input_stop = Arc::clone(&stop);
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            if input_stop.load(Ordering::Relaxed) {
                break;
            }
            match input.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let _ = input_events.try_send(WorkerEvent::NetworkData {
                        id,
                        bytes: buf[..n].to_vec(),
                    });
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10))
                }
                Err(_) => break,
            }
        }
    });
    thread::spawn(move || loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if let Ok(data) = outbound.recv_timeout(Duration::from_millis(50)) {
            let _ = stream.write_all(&data);
        }
    });
    let _ = events.send(WorkerEvent::BridgeStatus {
        id,
        message: format!("TCP bridge client connected: {address}"),
    });
    Ok(())
}

fn start_udp_bridge(
    id: PortId,
    config: BridgeConfig,
    events: Sender<WorkerEvent>,
    outbound: Receiver<Vec<u8>>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    let server_mode = config.tcp_server;
    let bind = if server_mode {
        format!("{}:{}", config.bind_or_host, config.port)
    } else {
        "0.0.0.0:0".into()
    };
    let socket = UdpSocket::bind(&bind).with_context(|| format!("Bind UDP socket {bind}"))?;
    socket.set_nonblocking(true)?;
    let destination = if server_mode {
        None
    } else {
        let address = format!("{}:{}", config.bind_or_host, config.port);
        Some(
            address
                .parse::<SocketAddr>()
                .with_context(|| format!("Invalid UDP destination {address}"))?,
        )
    };
    let latest_peer: Arc<Mutex<Option<SocketAddr>>> = Arc::new(Mutex::new(None));
    let input_peer = Arc::clone(&latest_peer);
    let read_socket = socket.try_clone()?;
    let input_events = events.clone();
    let input_stop = Arc::clone(&stop);
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            if input_stop.load(Ordering::Relaxed) {
                break;
            }
            match read_socket.recv_from(&mut buf) {
                Ok((n, peer)) => {
                    if server_mode {
                        *input_peer.lock().unwrap() = Some(peer);
                    }
                    let _ = input_events.try_send(WorkerEvent::NetworkData {
                        id,
                        bytes: buf[..n].to_vec(),
                    });
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10))
                }
                Err(error) => {
                    let _ = input_events.send(WorkerEvent::BridgeStatus {
                        id,
                        message: format!("UDP bridge stopped: {error}"),
                    });
                    break;
                }
            }
        }
    });
    let output_events = events.clone();
    thread::spawn(move || loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if let Ok(data) = outbound.recv_timeout(Duration::from_millis(50)) {
            let target = destination.or_else(|| *latest_peer.lock().unwrap());
            if let Some(target) = target {
                if let Err(error) = socket.send_to(&data, target) {
                    let _ = output_events.send(WorkerEvent::BridgeStatus {
                        id,
                        message: format!("UDP send failed: {error}"),
                    });
                }
            }
        }
    });
    let mode = if server_mode { "listening" } else { "client" };
    let detail = destination
        .map(|address| address.to_string())
        .unwrap_or(bind);
    let _ = events.send(WorkerEvent::BridgeStatus {
        id,
        message: format!("UDP bridge {mode}: {detail}"),
    });
    Ok(())
}

#[cfg(test)]
mod bridge_tests {
    use super::*;
    use std::{
        net::{TcpListener, TcpStream, UdpSocket},
        time::Instant,
    };

    fn free_udp_port() -> u16 {
        UdpSocket::bind("127.0.0.1:0")
            .expect("reserve UDP port")
            .local_addr()
            .expect("read UDP address")
            .port()
    }

    fn free_tcp_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .expect("reserve TCP port")
            .local_addr()
            .expect("read TCP address")
            .port()
    }

    fn wait_for_network_data(events: &Receiver<WorkerEvent>, id: PortId, expected: &[u8]) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            match events.recv_timeout(Duration::from_millis(50)) {
                Ok(WorkerEvent::NetworkData {
                    id: event_id,
                    bytes,
                }) if event_id == id && bytes == expected => return,
                Ok(_) | Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(error) => panic!("bridge event channel failed: {error}"),
            }
        }
        panic!("network data was not forwarded to the serial side");
    }

    #[test]
    fn tcp_server_forwards_inbound_data_and_replies_to_peer() {
        let port = free_tcp_port();
        let config = BridgeConfig {
            enabled: true,
            tcp_server: true,
            bind_or_host: "127.0.0.1".into(),
            port,
            udp: false,
        };
        let (events_tx, events_rx) = bounded(32);
        let bridge = start_bridge(43, config, events_tx).expect("start TCP bridge");
        let mut peer = TcpStream::connect(("127.0.0.1", port)).expect("connect test peer");
        peer.set_read_timeout(Some(Duration::from_secs(1)))
            .expect("set peer timeout");
        peer.write_all(b"to-serial").expect("send TCP data");
        wait_for_network_data(&events_rx, 43, b"to-serial");

        bridge
            .outbound
            .send(b"from-serial".to_vec())
            .expect("queue serial-to-TCP data");
        let mut buffer = [0u8; 64];
        let count = peer.read(&mut buffer).expect("receive TCP reply");
        assert_eq!(&buffer[..count], b"from-serial");
        bridge.stop();
    }

    #[test]
    fn udp_server_forwards_inbound_data_and_replies_to_latest_peer() {
        let port = free_udp_port();
        let config = BridgeConfig {
            enabled: true,
            tcp_server: true,
            bind_or_host: "127.0.0.1".into(),
            port,
            udp: true,
        };
        let (events_tx, events_rx) = bounded(32);
        let bridge = start_bridge(42, config, events_tx).expect("start UDP bridge");
        let peer = UdpSocket::bind("127.0.0.1:0").expect("bind test peer");
        peer.set_read_timeout(Some(Duration::from_secs(1)))
            .expect("set peer timeout");
        peer.send_to(b"to-serial", ("127.0.0.1", port))
            .expect("send inbound UDP data");

        wait_for_network_data(&events_rx, 42, b"to-serial");

        bridge
            .outbound
            .send(b"from-serial".to_vec())
            .expect("queue serial-to-UDP data");
        let mut buffer = [0u8; 64];
        let (count, _) = peer.recv_from(&mut buffer).expect("receive UDP reply");
        assert_eq!(&buffer[..count], b"from-serial");
        bridge.stop();
    }
}

#[cfg(test)]
mod direct_connection_tests {
    use super::*;
    use std::{
        net::{TcpListener, TcpStream, UdpSocket},
        time::Instant,
    };

    fn wait_for_connected(events: &Receiver<WorkerEvent>, id: PortId) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            match events.recv_timeout(Duration::from_millis(50)) {
                Ok(WorkerEvent::Status {
                    id: event_id,
                    connected: true,
                    ..
                }) if event_id == id => {
                    return;
                }
                Ok(_) | Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(error) => panic!("connection event channel failed: {error}"),
            }
        }
        panic!("direct connection did not become ready");
    }

    fn wait_for_data(events: &Receiver<WorkerEvent>, id: PortId, expected: &[u8]) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            match events.recv_timeout(Duration::from_millis(50)) {
                Ok(WorkerEvent::Data {
                    id: event_id,
                    bytes,
                    ..
                }) if event_id == id
                    && bytes.windows(expected.len()).any(|part| part == expected) =>
                {
                    return;
                }
                Ok(_) | Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(error) => panic!("connection event channel failed: {error}"),
            }
        }
        panic!("expected direct connection data was not received");
    }

    #[test]
    fn direct_tcp_client_sends_and_receives() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind TCP test listener");
        let port = listener.local_addr().expect("listener address").port();
        let peer = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept direct TCP client");
            stream.write_all(b"from-peer").expect("send TCP peer data");
            let mut buffer = [0u8; 64];
            let count = stream.read(&mut buffer).expect("receive direct TCP data");
            buffer[..count].to_vec()
        });

        let mut settings = PortSettings::default();
        settings.mode = ConnectionMode::Tcp;
        settings.network_role = NetworkRole::Client;
        settings.network_host = "127.0.0.1".into();
        settings.network_port = port;
        settings.auto_reconnect = false;
        let (events_tx, events_rx) = bounded(32);
        let handle = open_connection_worker(71, settings, events_tx);
        wait_for_data(&events_rx, 71, b"from-peer");
        handle
            .command_tx
            .send(WorkerCommand::Send(b"to-peer".to_vec()))
            .expect("queue direct TCP send");
        assert_eq!(peer.join().expect("join TCP peer"), b"to-peer");
        let _ = handle.command_tx.send(WorkerCommand::Disconnect);
    }

    #[test]
    fn direct_udp_listener_learns_peer_and_replies() {
        let reserve = UdpSocket::bind("127.0.0.1:0").expect("reserve UDP port");
        let port = reserve.local_addr().expect("reserved UDP address").port();
        drop(reserve);
        let mut settings = PortSettings::default();
        settings.mode = ConnectionMode::Udp;
        settings.network_role = NetworkRole::Server;
        settings.network_host = "127.0.0.1".into();
        settings.network_port = port;
        settings.auto_reconnect = false;
        let (events_tx, events_rx) = bounded(32);
        let handle = open_connection_worker(72, settings, events_tx);
        wait_for_connected(&events_rx, 72);
        let peer = UdpSocket::bind("127.0.0.1:0").expect("bind UDP peer");
        peer.set_read_timeout(Some(Duration::from_secs(1)))
            .expect("set UDP peer timeout");
        peer.send_to(b"to-listener", ("127.0.0.1", port))
            .expect("send UDP listener data");
        wait_for_data(&events_rx, 72, b"to-listener");
        handle
            .command_tx
            .send(WorkerCommand::Send(b"from-listener".to_vec()))
            .expect("queue UDP listener reply");
        let mut buffer = [0u8; 64];
        let (count, _) = peer
            .recv_from(&mut buffer)
            .expect("receive UDP listener reply");
        assert_eq!(&buffer[..count], b"from-listener");
        let _ = handle.command_tx.send(WorkerCommand::Disconnect);
    }
}
