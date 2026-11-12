![Deming Banner](assets/deming-banner.png)

# Deming

> **High-Throughput Parallel Minecraft Java Edition Mod Testing & Orchestration Engine**  
> *Pure Rust desktop app, JSON CLI, and Model Context Protocol (MCP) server for automated, deterministic mod testing across Vanilla, Fabric, NeoForge, Forge, and Quilt.*

---

## Overview

**Deming** is a high-performance orchestration and test-automation platform tailored specifically for Minecraft Java Edition mod developers, modpack authors, CI/CD pipelines, and AI coding agents.

Named in honor of W. Edwards Deming and the continuous improvement philosophy (PDCA), Deming transforms manual mod debugging and compatibility matrices into deterministic, repeatable, and automated execution jobs. Deming lets you launch, coordinate, evaluate, and snapshot multiple Minecraft client and server instances concurrently on a single machine—all backed by strict OS-level resource isolation and failure safety.

---

## Key Features

- 🤖 **AI-First JSON CLI & Model Context Protocol (MCP) Server**  
  Built from the ground up for agentic workflows. Exposes standard stdio JSON-RPC MCP endpoints (`get_config`, `set_config`, `add_offline_account`, `submit_test_run`, `get_run_status`, `list_runs`, `tail_instance_logs`, `capture_screenshot`, `abort_run`) and clean JSON CLI subcommands, enabling LLM agents to autonomously verify mod builds, inspect crashes, and capture visual proof.

- ⚡ **Multi-Loader Matrix Testing**  
  First-class support across the entire modern Minecraft Java ecosystem:
  - **Vanilla**
  - **Fabric**
  - **NeoForge**
  - **Forge**
  - **Quilt**  
  Resolves loader meta manifests, Maven library dependencies, server runtimes, and local or Modrinth mod sources seamlessly.

- 🔒 **OS-Enforced Limits & Windows Job Objects**  
  Uses native Windows Job Objects (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`) to guarantee zero orphaned `javaw.exe` instances. Fine-grained CPU core affinity, process priority controls (`Normal`, `BelowNormal`, `Idle`), RAM caps, and instant sub-millisecond **Stop / Abort All** job termination.

- 🌐 **Orchestrated Dedicated Server + Multi-Client Auto-Connect**  
  Automate complex multiplayer scenarios. Deming launches a temporary local dedicated server, waits for the world ready indicator, and spins up multiple client instances configured to automatically connect (`--quickPlayMultiplayer` or legacy parameters).

- 🔑 **Multi-Account Management**  
  Manage offline development accounts (with deterministic UUID v3 generation) for fast local iteration, side-by-side with Microsoft Authentication (MSA) via OAuth 2.0 Device-Code flow (Xbox Live, XSTS, and Minecraft session tokens) for online compatibility testing.

- 📸 **Native Window Screenshot Capture**  
  Captures native OS window buffers (`PrintWindow` / `BitBlt`) directly via HWND discovery and PID matching to generate visual artifacts of main menus, in-game states, or crash HUDs without requiring in-game mods.

- 📦 **Structured Run Archiving & Retention**  
  Every test run is isolated into a structured directory with `manifest.json`, stdout/stderr logs, screenshots, JUnit/GameTest XML reports, and rule evaluation outcomes. Automatic retention policies handle pruning and `.zip` archiving.

- 🖥️ **Obsidian-Themed Native GUI**  
  A dark obsidian (`#181A20`) desktop application built with `eframe`/`egui` featuring an emergency abort dashboard, visual test builder, searchable log inspection, screenshot gallery, and live IPC syncing with CLI/MCP tasks.

---

## Workspace Crates Architecture

Deming is engineered as a clean, modular Rust workspace:

```
crates/
├── deming-core      # Domain models, configuration schemas, job specs, and event definitions
├── deming-fetch     # Mojang metadata, loader resolvers, Adoptium Java JRE manager, Modrinth client
├── deming-auth      # Microsoft Device-Code OAuth, Xbox Live/XSTS, offline UUID accounts & keystore
├── deming-runner    # Windows Job Objects, process scheduler, server/client runners, HWND capture
├── deming-archive   # Run storage, manifest serialization, log streaming, zip compression & pruning
├── deming-ipc       # Inter-process communication protocol & server for GUI/CLI/MCP live sync
├── deming-cli       # Unified CLI tool and stdio MCP server for terminal & AI agent automation
└── deming-gui       # Native desktop application (egui/eframe) with live dashboard & test builder
```

---

## Quickstart

### Prerequisites

- **Rust** 1.78+ (stable toolchain recommended)
- **Windows 10 / 11** (for native Windows Job Object isolation and HWND screen capture)
- **Git**

### Building from Source

Build the entire workspace in release mode:

```bash
cargo build --workspace --release
```

Compiled binaries will be available in `target/release/`:
- `deming-cli.exe`
- `deming-gui.exe`

---

## Usage

### 1. Launching the GUI

Start the graphical desktop environment:

```bash
cargo run --bin deming-gui --release
```

The GUI connects to the local Deming IPC bus to monitor runs submitted via the CLI or MCP in real time, configure accounts, and build test suites interactively.

### 2. Using the CLI

Deming provides a structured, scriptable CLI:

#### Manage Configuration
```bash
# View current global configuration
deming config get

# Set resource limits
deming config set --max-ram-mb 16384 --max-concurrent-instances 4
```

#### Manage Accounts
```bash
# List all configured accounts
deming account list

# Add an offline development account
deming account add-offline --username DevPlayer1

# Start Microsoft Device-Code login
deming account login-msa
```

#### Submit a Test Run
Submit a test specification file (`job.json`):

```bash
deming run submit --spec job.json
```

Example `job.json`:
```json
{
  "name": "fabric-1.20.4-smoke-test",
  "timeout_seconds": 180,
  "instances": [
    {
      "name": "client-1",
      "minecraft_version": "1.20.4",
      "mod_loader": "Fabric",
      "loader_version": null,
      "ram_mb": 4096,
      "cpu_cores": 4,
      "account": { "type": "Offline", "username": "DevTester" },
      "mods": [
        { "type": "LocalPath", "path": "./build/libs/my-mod-1.0.0.jar" }
      ],
      "completion_rule": {
        "type": "LogPattern",
        "pattern": "Done \\(.*s\\)! For help, type \"help\""
      }
    }
  ]
}
```

#### Query Run Status & Inspect Logs
```bash
# List previous test runs
deming run list

# Check status of a specific run
deming run status <RUN_ID>

# Tail logs of an instance
deming run logs <RUN_ID> client-1 --tail 100
```

### 3. Model Context Protocol (MCP) Server

Deming includes a built-in MCP server for integration with Claude Desktop, Cursor, Gemini, or any MCP-compatible AI agent harness:

```bash
deming mcp
```

#### Config Example (`claude_desktop_config.json`):
```json
{
  "mcpServers": {
    "deming": {
      "command": "F:\\workflow\\Deming\\target\\release\\deming-cli.exe",
      "args": ["mcp"]
    }
  }
}
```

**Exposed MCP Tools:**
- `get_config`: Retrieve current Deming settings and resource limits.
- `set_config`: Update workspace directory, memory ceilings, or concurrency parameters.
- `list_accounts`: List stored offline and Microsoft accounts.
- `add_offline_account`: Register a new offline player username.
- `submit_test_run`: Schedule and execute a parallel test job matrix.
- `get_run_status`: Poll execution progress, instance states, and test pass/fail metrics.
- `list_runs`: Review historical test runs and archive metadata.
- `tail_instance_logs`: Fetch stdout/stderr logs from running or completed instances.
- `capture_screenshot`: Take an immediate native window capture of an active client instance.
- `abort_run`: Instantly terminate a test run and clean up all child processes.

---

## License

This project is licensed under the Apache License 2.0 or MIT License at your option.
