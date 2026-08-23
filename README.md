I don't normally put my vibe coded projects out there, but this one felt worth sharing.
Most serial monitors I've tried choke on high data rates. This one doesn't.
A few features are just me being picky, a few came from AI suggestions — but together they make something I actually use.
I really didn't have enough time to check all the features. I'd be happy if you let me know, if there are any problems.

# Embedded Serial Monitor: Final Developer Guide

## Purpose and final scope

Embedded Serial Monitor is a native Rust desktop terminal for embedded-device traffic. A connection tab can directly operate in **Serial**, **TCP**, or **UDP** mode. The completed project deliberately does **not** include a Modbus simulator. Existing checksum and display-decoder utilities remain available as normal terminal tools; they do not create a protocol simulator.

The application is structured to keep I/O away from the immediate-mode user-interface thread. Transport workers run in background threads and report events through bounded channels. The GUI drains a capped number of events per frame, frames received bytes, retains a bounded rolling log, and then applies rendering, filtering, copying, plotting, and export features.

| Source file | Responsibility | Main concepts |
|---|---|---|
| `src/main.rs` | Desktop application, state, layouts, settings pages, log rendering, transmission, persistence orchestration | `SerialMonitorApp`, `PortState`, `ui_left_settings`, `connect_selected`, `process_events` |
| `src/transport.rs` | Background Serial/TCP/UDP I/O plus the independent serial-to-network bridge | `open_connection_worker`, `serial_loop`, `tcp_connection_loop`, `udp_connection_loop`, `start_bridge` |
| `src/engine.rs` | Pure byte formatting, framing, checksums, decoders, exports, and testable helpers | `Framer`, `render_record`, `checksum`, `export_records` |
| `src/types.rs` | Serde-persisted data model and message contracts | `PortSettings`, `ConnectionMode`, `WorkerCommand`, `WorkerEvent`, `SessionData` |

## Connection model

`PortSettings.mode` is the direct endpoint selector. The only final options are `Serial`, `Tcp`, and `Udp`. The Connect page uses this enum to present only relevant controls. Serial mode exposes device, baud rate, framing, parity, flow control, and electrical controls. TCP and UDP mode expose host/bind address, port, and client/server role. Connection mode changes are disabled while a tab is active, so visible configuration cannot diverge from the worker already running.

`connect_selected` validates the selected mode, creates a `Framer` from display framing preferences, then calls `transport::open_connection_worker`. That function dispatches to the serial worker or a direct TCP/UDP worker but returns the same `SerialHandle` command channel type. This common command interface keeps Transmit, repeat sends, macros, triggered responses, and disconnect behavior independent of the selected transport.

| Mode | Worker behavior | Receive and send semantics |
|---|---|---|
| Serial | Opens the configured OS serial device and applies baud/framing/electrical settings | Reads device bytes; `WorkerCommand::Send` writes bytes to the device |
| TCP client | Connects to the configured remote host and port; optionally retries after failure | Reads and writes a single connected TCP stream |
| TCP server/listener | Binds the configured host and port; accepts one peer at a time | Writes to the active peer; resumes listening after peer loss |
| UDP client | Binds an ephemeral local port and targets the configured endpoint | Receives datagrams and sends transmit bytes to configured host/port |
| UDP server/listener | Binds the configured host and port | Remembers the most recent datagram peer and sends transmit bytes back to that peer |

The Network page is intentionally distinct from direct TCP/UDP connections. It is a **Serial Network Bridge**: it forwards bytes between a physical serial connection and a separately configured TCP or UDP endpoint. It is hidden from direct TCP/UDP tabs to avoid overlapping connection paths.

## Worker event pipeline

Every worker owns a `Receiver<WorkerCommand>` and publishes `WorkerEvent` values. Data is batched with a byte-size and latency limit before publication. The receive side keeps a capped pending buffer so high-rate traffic cannot grow unbounded while the interface is busy.

`process_events` in `main.rs` drains no more than `MAX_EVENTS_PER_FRAME` events on each interface frame. `WorkerEvent::Data` first enters the configured `Framer`; the resulting records are passed to `push_received`. Outbound user data is recorded as `Direction::Tx` when `send_to_port` places bytes onto a worker command channel. This separation means transport work cannot block repaint or interaction.

> **Pause affects visualization and retained log/plot ingestion, not transport.** Serial, TCP, UDP, and bridge workers continue to move bytes while the displayed log is paused.

## Framing, logs, filtering, and memory control

`engine::Framer` supports raw chunks, idle-timeout records, delimited records, and fixed-length records. Raw chunks are additionally split on CR, LF, or CRLF sequences before presentation, which makes text terminals readable while keeping non-text payloads available in raw byte views.

Each `PortState` stores its rolling log in a `VecDeque`. Retention is estimated from allocated byte/string capacity rather than content length alone. When the configured memory budget is exceeded, the oldest records are removed in a batch down to a lower target rather than repeatedly shifting a `Vec` at the threshold. Rendering is capped to recent records, iterates records by reference, and only constructs a filter search string when a keyword filter is active. These choices prevent the earlier high-volume memory-boundary failure.

The Live Data pane uses two-axis scrolling. Records do not wrap; long serial or network messages remain on one line with a horizontal scrollbar. Rows support text selection and a dedicated Copy action. The keyword filter is case-insensitive and accepts comma-separated alternatives; a record is displayed when it contains any entered keyword.

## TCP/UDP implementation notes

Direct TCP and UDP workers use nonblocking sockets and short sleeps while idle so they can promptly handle commands and shutdown. TCP server mode holds the listener while clients come and go. UDP server mode stores the last peer address after a received datagram, providing expected request/reply behavior without inventing a peer before traffic arrives. The worker sends status messages separately from bridge-status messages; network bridge updates cannot incorrectly alter the direct serial connection state.

The local regression suite creates real loopback listeners and sockets. It verifies direct TCP client round trips, direct UDP server receive/reply behavior, and serial-network bridge TCP/UDP behavior. These tests exercise the same worker loops used by the desktop application.

## UI architecture

The eframe/egui application uses a compact widget shell with a left navigation panel and a central working surface. `WorkspaceTab` selects Monitor, Connect, Network, Export, Display, Advanced, Plot, or Theme. The bottom transmit dock is resizable; the center monitor retains the greatest share of space at small window sizes.

The theme system applies full-surface palettes for Dark, Light, Moonlight, Nord, and Solarized, including background, panels, controls, separators, code/log colors, and status colors. Theme selection is persisted in session data.

## Session data and compatibility

`SessionData` serializes the list of port settings, display preferences, color rules, triggers, scripts, themes, and other user state. Newly added direct network fields are protected by serde defaults so sessions saved before TCP/UDP mode support remain loadable. The final schema contains no simulator-specific fields.

## Build and packaging

The project is built with stable Rust. `.cargo/config.toml` provides GNU Windows target linker configuration for `x86_64-pc-windows-gnu` and `i686-pc-windows-gnu`. `packaging/build-distributions.sh` builds native Linux, Windows x64, Windows x86, Debian amd64, and Arch x86_64 artifacts. The script invokes NSIS for Windows installers, `dpkg-deb` for Debian packages, and `bsdtar`/zstd-compatible assembly for Arch packages.

| Output | Installation method |
|---|---|
| `embedded-serial-monitor-win64-setup.exe` | Run on 64-bit Windows; installer creates shortcuts and an uninstaller |
| `embedded-serial-monitor-win32-setup.exe` | Run on 32-bit Windows |
| `embedded-serial-monitor-win64.exe` / `win32.exe` | Portable Windows executables; copy and run |
| `embedded-serial-monitor_0.1.0_amd64.deb` | `sudo apt install ./file.deb` |
| `embedded-serial-monitor-0.1.0-1-x86_64.pkg.tar.zst` | `sudo pacman -U ./file.pkg.tar.zst` |

## Line-by-line annotated source

The `documented-source/` directory is generated from the clean source by `tools/generate_annotated_source.py`. It contains a syntactically valid copy of every Rust source file. Before each original line, the generator inserts an annotation identifying the original line number and an explanation based on the line’s Rust structure. This lets a reader inspect the code in exact original order without polluting the production source with redundant comments.

Run the generator after source changes:

```bash
python3 tools/generate_annotated_source.py
```

The final release package includes both the clean, buildable production source and the full line-level annotated copy.




