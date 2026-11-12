//! IPC Client for runner, CLI, and MCP processes to communicate with the GUI.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use deming_core::RunEvent;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex};

use crate::error::{IpcError, Result};
use crate::message::IpcMessage;
use crate::server::PORT_FILE_NAME;

#[derive(Clone, Debug)]
struct ClientInner {
    tx: mpsc::Sender<IpcMessage>,
    rx: Arc<Mutex<mpsc::Receiver<IpcMessage>>>,
    connected: Arc<AtomicBool>,
}

/// Client used by CLI, MCP, and runner processes to publish events and receive commands.
///
/// Designed to gracefully fall back to a no-op if no GUI/server is currently listening.
#[derive(Clone, Debug, Default)]
pub struct IpcClient {
    inner: Option<ClientInner>,
}

impl IpcClient {
    /// Attempts to connect to an IPC server for the given workspace.
    ///
    /// If no server is running (port file missing or unreachable), returns a no-op client
    /// where `is_connected()` is `false` and `send()` returns `Ok(false)` without error.
    pub async fn connect(workspace_dir: impl AsRef<Path>) -> Result<Self> {
        let port_file = workspace_dir.as_ref().join(PORT_FILE_NAME);
        if !port_file.exists() {
            return Ok(Self::noop());
        }

        let content = match tokio::fs::read_to_string(&port_file).await {
            Ok(c) => c,
            Err(_) => return Ok(Self::noop()),
        };

        let port: u16 = match content.trim().parse() {
            Ok(p) => p,
            Err(_) => return Ok(Self::noop()),
        };

        Self::connect_to_port(port).await
    }

    /// Strict connection variant that returns [`IpcError::ServerNotRunning`] if no server is active.
    pub async fn connect_strict(workspace_dir: impl AsRef<Path>) -> Result<Self> {
        let port_file = workspace_dir.as_ref().join(PORT_FILE_NAME);
        if !port_file.exists() {
            return Err(IpcError::ServerNotRunning);
        }

        let content = tokio::fs::read_to_string(&port_file).await?;
        let port: u16 = content.trim().parse().map_err(|e: std::num::ParseIntError| {
            IpcError::InvalidPort(e.to_string())
        })?;

        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let stream = tokio::net::TcpStream::connect(addr)
            .await
            .map_err(|_| IpcError::ServerNotRunning)?;

        Ok(Self::from_stream(stream))
    }

    /// Connects to a specific port on localhost, falling back to a no-op client on failure.
    pub async fn connect_to_port(port: u16) -> Result<Self> {
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let connect_timeout = Duration::from_millis(500);
        match tokio::time::timeout(connect_timeout, TcpStream::connect(addr)).await {
            Ok(Ok(stream)) => Ok(Self::from_stream(stream)),
            _ => Ok(Self::noop()),
        }
    }

    /// Explicitly creates a no-op client.
    pub fn noop() -> Self {
        Self { inner: None }
    }

    /// Returns `true` if this client is currently connected to an active server.
    pub fn is_connected(&self) -> bool {
        self.inner
            .as_ref()
            .map(|i| i.connected.load(Ordering::SeqCst))
            .unwrap_or(false)
    }

    fn from_stream(stream: TcpStream) -> Self {
        let (read_half, mut write_half) = stream.into_split();
        let (tx, mut write_rx) = mpsc::channel::<IpcMessage>(128);
        let (read_tx, rx) = mpsc::channel::<IpcMessage>(128);
        let connected = Arc::new(AtomicBool::new(true));

        let connected_writer = connected.clone();
        tokio::spawn(async move {
            while let Some(msg) = write_rx.recv().await {
                match msg.to_json_line() {
                    Ok(line) => {
                        if write_half.write_all(line.as_bytes()).await.is_err()
                            || write_half.flush().await.is_err()
                        {
                            connected_writer.store(false, Ordering::SeqCst);
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            connected_writer.store(false, Ordering::SeqCst);
        });

        let connected_reader = connected.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(read_half);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        if let Ok(msg) = IpcMessage::from_json_line(trimmed) {
                            if read_tx.send(msg).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            connected_reader.store(false, Ordering::SeqCst);
        });

        Self {
            inner: Some(ClientInner {
                tx,
                rx: Arc::new(Mutex::new(rx)),
                connected,
            }),
        }
    }

    /// Sends an [`IpcMessage`] to the server.
    ///
    /// Returns `Ok(true)` if sent, `Ok(false)` if no-op or disconnected.
    pub async fn send(&self, msg: &IpcMessage) -> Result<bool> {
        if let Some(inner) = &self.inner {
            if !inner.connected.load(Ordering::SeqCst) {
                return Ok(false);
            }
            match inner.tx.send(msg.clone()).await {
                Ok(()) => Ok(true),
                Err(_) => {
                    inner.connected.store(false, Ordering::SeqCst);
                    Ok(false)
                }
            }
        } else {
            Ok(false)
        }
    }

    /// Receives the next incoming [`IpcMessage`] from the server.
    ///
    /// Returns `Ok(None)` if no-op or connection closed.
    pub async fn recv(&self) -> Result<Option<IpcMessage>> {
        if let Some(inner) = &self.inner {
            let mut rx = inner.rx.lock().await;
            Ok(rx.recv().await)
        } else {
            Ok(None)
        }
    }

    /// Receives the next incoming [`IpcMessage`] with a timeout.
    pub async fn recv_timeout(&self, timeout: Duration) -> Result<Option<IpcMessage>> {
        if self.inner.is_none() {
            return Ok(None);
        }
        match tokio::time::timeout(timeout, self.recv()).await {
            Ok(res) => res,
            Err(_) => Ok(None),
        }
    }

    /// Helper to send a [`RunEvent`].
    pub async fn publish_event(&self, run_id: impl Into<String>, event: RunEvent) -> Result<bool> {
        self.send(&IpcMessage::PublishEvent {
            run_id: run_id.into(),
            event,
        })
        .await
    }

    /// Helper to send a `RunStarted` notification.
    pub async fn run_started(&self, run_id: impl Into<String>, name: impl Into<String>) -> Result<bool> {
        self.send(&IpcMessage::RunStarted {
            run_id: run_id.into(),
            name: name.into(),
        })
        .await
    }

    /// Helper to request run abort.
    pub async fn abort_run(&self, run_id: impl Into<String>) -> Result<bool> {
        self.send(&IpcMessage::AbortRun {
            run_id: run_id.into(),
        })
        .await
    }

    /// Helper to request aborting all runs.
    pub async fn abort_all(&self) -> Result<bool> {
        self.send(&IpcMessage::AbortAll).await
    }

    /// Helper to request screenshot capture.
    pub async fn request_screenshot(
        &self,
        run_id: impl Into<String>,
        instance_id: impl Into<String>,
    ) -> Result<bool> {
        self.send(&IpcMessage::RequestScreenshot {
            run_id: run_id.into(),
            instance_id: instance_id.into(),
        })
        .await
    }

    /// Helper to send screenshot result.
    pub async fn screenshot_result(
        &self,
        run_id: impl Into<String>,
        instance_id: impl Into<String>,
        path: Option<String>,
        error: Option<String>,
    ) -> Result<bool> {
        self.send(&IpcMessage::ScreenshotResult {
            run_id: run_id.into(),
            instance_id: instance_id.into(),
            path,
            error,
        })
        .await
    }

    /// Sends a `Ping` message to the server.
    pub async fn ping(&self) -> Result<bool> {
        self.send(&IpcMessage::Ping).await
    }

    /// Sends a `Ping` and waits for a `Pong` response within `timeout`.
    pub async fn ping_roundtrip(&self, timeout: Duration) -> Result<bool> {
        if !self.is_connected() {
            return Ok(false);
        }
        if !self.send(&IpcMessage::Ping).await? {
            return Ok(false);
        }
        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            let remaining = timeout.saturating_sub(start.elapsed());
            if let Some(msg) = self.recv_timeout(remaining).await? {
                if matches!(msg, IpcMessage::Pong) {
                    return Ok(true);
                }
            } else {
                break;
            }
        }
        Ok(false)
    }
}
