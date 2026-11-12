//! IPC Server for GUI and daemon listeners.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::error::Result;
use crate::message::IpcMessage;

pub const PORT_FILE_NAME: &str = "deming-ipc.port";

/// Helper handle to broadcast messages to all connected IPC clients.
#[derive(Clone, Debug)]
pub struct IpcBroadcaster {
    clients: Arc<Mutex<Vec<mpsc::Sender<IpcMessage>>>>,
}

impl IpcBroadcaster {
    /// Broadcasts an [`IpcMessage`] to all connected clients.
    ///
    /// Returns the number of clients to which the message was successfully dispatched.
    pub async fn broadcast(&self, msg: &IpcMessage) -> Result<usize> {
        let mut clients = self.clients.lock().await;
        let mut sent = 0;
        let mut active = Vec::with_capacity(clients.len());

        for client in clients.drain(..) {
            if client.send(msg.clone()).await.is_ok() {
                sent += 1;
                active.push(client);
            }
        }
        *clients = active;
        Ok(sent)
    }

    /// Returns the number of currently active client connections.
    pub async fn client_count(&self) -> usize {
        let mut clients = self.clients.lock().await;
        clients.retain(|c| !c.is_closed());
        clients.len()
    }
}

/// Localhost IPC TCP server for receiving events and dispatching commands.
pub struct IpcServer {
    workspace_dir: PathBuf,
    port_file: PathBuf,
    local_addr: SocketAddr,
    clients: Arc<Mutex<Vec<mpsc::Sender<IpcMessage>>>>,
    incoming_rx: mpsc::Receiver<IpcMessage>,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

impl IpcServer {
    /// Binds an IPC server on `127.0.0.1:0` for the specified workspace.
    ///
    /// Automatically writes the assigned port to `<workspace_dir>/deming-ipc.port`.
    pub async fn bind(workspace_dir: impl AsRef<Path>) -> Result<Self> {
        let workspace_dir = workspace_dir.as_ref().to_path_buf();
        tokio::fs::create_dir_all(&workspace_dir).await?;

        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let local_addr = listener.local_addr()?;
        let port = local_addr.port();

        let port_file = workspace_dir.join(PORT_FILE_NAME);
        tokio::fs::write(&port_file, format!("{}\n", port)).await?;

        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
        let (incoming_tx, incoming_rx) = mpsc::channel::<IpcMessage>(512);
        let clients: Arc<Mutex<Vec<mpsc::Sender<IpcMessage>>>> = Arc::new(Mutex::new(Vec::new()));

        let clients_accept = clients.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => {
                        break;
                    }
                    accept_res = listener.accept() => {
                        match accept_res {
                            Ok((stream, _peer_addr)) => {
                                let (read_half, mut write_half) = stream.into_split();
                                let (client_tx, mut client_rx) = mpsc::channel::<IpcMessage>(128);

                                {
                                    let mut lock = clients_accept.lock().await;
                                    lock.push(client_tx.clone());
                                }

                                // Client writer task
                                tokio::spawn(async move {
                                    while let Some(msg) = client_rx.recv().await {
                                        match msg.to_json_line() {
                                            Ok(line) => {
                                                if write_half.write_all(line.as_bytes()).await.is_err()
                                                    || write_half.flush().await.is_err()
                                                {
                                                    break;
                                                }
                                            }
                                            Err(_) => break,
                                        }
                                    }
                                });

                                // Client reader task
                                let incoming_tx = incoming_tx.clone();
                                let client_tx_pong = client_tx.clone();
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
                                                match IpcMessage::from_json_line(trimmed) {
                                                    Ok(IpcMessage::Ping) => {
                                                        let _ = client_tx_pong.send(IpcMessage::Pong).await;
                                                        let _ = incoming_tx.send(IpcMessage::Ping).await;
                                                    }
                                                    Ok(msg) => {
                                                        if incoming_tx.send(msg).await.is_err() {
                                                            break;
                                                        }
                                                    }
                                                    Err(_) => {
                                                        // Ignore or skip corrupted line
                                                    }
                                                }
                                            }
                                            Err(_) => break,
                                        }
                                    }
                                });
                            }
                            Err(_) => {
                                break;
                            }
                        }
                    }
                }
            }
        });

        Ok(Self {
            workspace_dir,
            port_file,
            local_addr,
            clients,
            incoming_rx,
            shutdown_tx: Some(shutdown_tx),
        })
    }

    /// Returns the local port number this server is listening on.
    pub fn port(&self) -> u16 {
        self.local_addr.port()
    }

    /// Returns the local socket address this server is bound to.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Returns the workspace directory this server is associated with.
    pub fn workspace_dir(&self) -> &Path {
        &self.workspace_dir
    }

    /// Returns the path to the `deming-ipc.port` file.
    pub fn port_file(&self) -> &Path {
        &self.port_file
    }

    /// Returns an [`IpcBroadcaster`] clone that can be used from other tasks.
    pub fn broadcaster(&self) -> IpcBroadcaster {
        IpcBroadcaster {
            clients: self.clients.clone(),
        }
    }

    /// Broadcasts an [`IpcMessage`] to all currently connected clients.
    pub async fn broadcast(&self, msg: &IpcMessage) -> Result<usize> {
        self.broadcaster().broadcast(msg).await
    }

    /// Returns the number of currently connected clients.
    pub async fn connected_clients(&self) -> usize {
        self.broadcaster().client_count().await
    }

    /// Receives the next incoming [`IpcMessage`] from any connected client.
    ///
    /// Returns `None` if the server is shutting down and all clients disconnected.
    pub async fn recv(&mut self) -> Option<IpcMessage> {
        self.incoming_rx.recv().await
    }

    /// Receives the next incoming [`IpcMessage`] with a timeout.
    pub async fn recv_timeout(&mut self, timeout: Duration) -> Option<IpcMessage> {
        match tokio::time::timeout(timeout, self.recv()).await {
            Ok(msg) => msg,
            Err(_) => None,
        }
    }

    /// Explicitly stops the server and deletes the port file.
    pub async fn stop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if self.port_file.exists() {
            if let Ok(content) = tokio::fs::read_to_string(&self.port_file).await {
                if content.trim() == self.local_addr.port().to_string() {
                    let _ = tokio::fs::remove_file(&self.port_file).await;
                }
            }
        }
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if self.port_file.exists() {
            if let Ok(content) = std::fs::read_to_string(&self.port_file) {
                if content.trim() == self.local_addr.port().to_string() {
                    let _ = std::fs::remove_file(&self.port_file);
                }
            }
        }
    }
}
