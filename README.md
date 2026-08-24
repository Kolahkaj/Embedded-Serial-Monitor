# Embedded Serial Monitor

A cross-platform desktop application for monitoring, logging, and interacting with devices over Serial, TCP, and UDP connections. Whether you're debugging an embedded system, analyzing sensor data, or building a network bridge, this tool provides a comprehensive interface for your communication needs.

## Key Features

*   **Multiple Connection Modes:** Connect via Serial (COM port), TCP (client or server), or UDP.
*   **Live Data Display:** View RX, TX, and system records with powerful filtering, timestamping, and configurable byte rendering.
*   **Flexible Framing:** Define message boundaries using raw chunks, idle timeouts, start/end bytes, or fixed-length frames.
*   **Data Logging & Export:** Log incoming data to files or export selected records (all, RX, TX, or bookmarks) to CSV, JSON, or SQLite formats.
*   **Real-time Plotting:** Parse structured numeric data from incoming records for time-series or X/Y scatter plots.
*   **Serial-to-Network Bridge:** Forward data between a Serial port and a TCP or UDP endpoint.
*   **Trigger Actions:** Automatically send configured responses (ASCII or hex) when incoming text matches a regular expression.
*   **Macro Scripting:** Automate repetitive tasks with simple scripts for sending data, delays, and hardware control signals.

## Connection Modes

### 🔌 Serial
Open a COM or device port with full control over baud rate, data bits, parity, stop bits, and flow control.

### 🌐 TCP
Connect to a remote TCP server as a client, or start a local server to listen for incoming TCP connections.

### 📨 UDP
Send and receive UDP datagrams. Act as a client or create a local listener that automatically replies to the most recent peer.

## Downloads

Get the latest version for your operating system:

| Platform | File |
| :--- | :--- |
| **Windows 64-bit** | [embedded-serial-monitor-win64-setup.exe](https://kolahkaj.github.io/Embedded-Serial-Monitor/) |
| **Windows 32-bit** | [embedded-serial-monitor-win32-setup.exe](https://kolahkaj.github.io/Embedded-Serial-Monitor/) |
| **Debian / Ubuntu** | [embedded-serial-monitor_0.1.0_amd64.deb](https://kolahkaj.github.io/Embedded-Serial-Monitor/) |
| **Arch Linux** | [embedded-serial-monitor-0.1.0-1-x86_64.pkg.tar.zst](https://kolahkaj.github.io/Embedded-Serial-Monitor/) |
| **Portable Linux** | [embedded-serial-monitor-0.1.0-linux-x86_64-portable.tar.gz](https://kolahkaj.github.io/Embedded-Serial-Monitor/) |

## Quick Start Guide

### Plotting Data from a Device
The plot feature can visualize structured numeric data from your device's output.

**1. Supported Data Format:**
The parser reads complete brace-delimited objects `{ ... }` from a single received record. Each value must be a finite number.

*   **Valid Frame:** `status { temperature: 24.6, rpm: 1250, voltage: -3.25 }`
*   **Invalid Frames:**
    *   `{ temperature: 24.6` (No closing brace)
    *   `{ temp C: 24 }` (Space in key name)
    *   `{ mode: auto }` (Non-numeric value)

**2. Configure Framing:**
Ensure the **Framing** setting is configured so a complete JSON-like object arrives in one record. The parser will ignore incomplete objects split across records.

### Using Macro Scripts
Automate sending commands with simple scripts.

**Supported Commands:**
*   `SEND text` – Send text as written.
*   `SEND_HEX AA 55` – Send hexadecimal bytes (e.g., `AA 55`).
*   `DELAY 100` – Wait for 100 milliseconds (alias: `WAIT`).
*   `DTR_PULSE 50` – Pulse the DTR signal for 50ms (Serial only).
*   `BREAK 100` – Send a serial BREAK condition for 100ms (Serial only).
*   `REPEAT 3 … END` – Repeat the enclosed block of commands (max count: 10,000).

**Example Script:**
```
# Reset target device
SEND_HEX 55 AA
DELAY 100
DTR_PULSE 50

REPEAT 3
  SEND PING
  DELAY 200
END
```

## Operational Notes & Limits

*   **High Data Rates:** At very high input rates, the application may apply display and retention limits to maintain performance. Always check system records and dropped-data counters before treating the visible log as a complete record.
*   **Serial-to-Network Bridge:** Use this mode specifically when you need to expose a Serial device over a network. The direct TCP/UDP monitor modes do not require a Serial port.
*   **UDP Listener:** A UDP listener will only reply to the most recent peer after it has received at least one datagram.
*   **Macro Runner:**
    *   Macros run in order and apply only to the selected connected Serial port.
    *   There are no variables, conditions, or receive-wait commands. Use `DELAY` for pacing.
    *   Commands are case-insensitive; empty lines and lines starting with `#` are ignored.

## Contributing

If you have any questions, encounter a bug, or would like to suggest a new feature, please open an issue on the project's [GitHub repository](https://github.com/kolahkaj/Embedded-Serial-Monitor). Contributions are welcome!

## License

This project is open-source and available under the [MIT License](LICENSE).
