use std::path::PathBuf;
use std::time::Duration;

use chrono::Utc;
use deming_core::{InstanceState, RunEvent, RunStatus};
use deming_ipc::{IpcClient, IpcMessage, IpcServer, PORT_FILE_NAME};
use uuid::Uuid;

fn temp_workspace() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("deming-ipc-test-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn test_server_bind_and_cleanup() {
    let workspace = temp_workspace();
    let port_file = workspace.join(PORT_FILE_NAME);

    let mut server = IpcServer::bind(&workspace).await.unwrap();
    assert!(port_file.exists());

    let content = std::fs::read_to_string(&port_file).unwrap();
    let port: u16 = content.trim().parse().unwrap();
    assert_eq!(port, server.port());
    assert!(port > 0);

    // Explicit stop removes the port file
    server.stop().await;
    assert!(!port_file.exists());

    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn test_server_drop_cleans_up_port_file() {
    let workspace = temp_workspace();
    let port_file = workspace.join(PORT_FILE_NAME);

    {
        let server = IpcServer::bind(&workspace).await.unwrap();
        assert!(port_file.exists());
        assert_eq!(server.port(), std::fs::read_to_string(&port_file).unwrap().trim().parse::<u16>().unwrap());
    } // server dropped here

    // Verify drop cleaned up port file
    assert!(!port_file.exists());
    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn test_client_fallback_when_no_server_running() {
    let workspace = temp_workspace();

    // No server exists
    let client = IpcClient::connect(&workspace).await.unwrap();
    assert!(!client.is_connected());

    // Sending must gracefully return Ok(false)
    let sent = client
        .publish_event(
            "run-none",
            RunEvent::RunFinished {
                run_id: Uuid::new_v4(),
                status: RunStatus::Passed,
                timestamp: Utc::now(),
            },
        )
        .await
        .unwrap();
    assert!(!sent);

    // Recv must return Ok(None)
    let recv = client.recv().await.unwrap();
    assert_eq!(recv, None);

    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn test_client_fallback_with_stale_port_file() {
    let workspace = temp_workspace();
    let port_file = workspace.join(PORT_FILE_NAME);

    // Point to a likely unused high port
    std::fs::write(&port_file, "65534\n").unwrap();

    let client = IpcClient::connect(&workspace).await.unwrap();
    assert!(!client.is_connected());

    let sent = client.ping().await.unwrap();
    assert!(!sent);

    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn test_server_client_roundtrip_events_and_commands() {
    let workspace = temp_workspace();
    let mut server = IpcServer::bind(&workspace).await.unwrap();

    let client = IpcClient::connect(&workspace).await.unwrap();
    assert!(client.is_connected());

    // 1. Client publishes RunStarted to server
    let run_id = "run-test-123";
    let sent = client.run_started(run_id, "my-job").await.unwrap();
    assert!(sent);

    let msg = server.recv_timeout(Duration::from_secs(2)).await.expect("server did not receive RunStarted");
    assert_eq!(
        msg,
        IpcMessage::RunStarted {
            run_id: run_id.to_string(),
            name: "my-job".to_string()
        }
    );

    // 2. Client publishes PublishEvent
    let event_uuid = Uuid::new_v4();
    let event = RunEvent::InstanceStateChanged {
        run_id: event_uuid,
        instance_id: "inst-1".into(),
        state: InstanceState::Running,
        message: Some("Ready".into()),
        timestamp: Utc::now(),
    };

    let sent = client.publish_event(run_id, event.clone()).await.unwrap();
    assert!(sent);

    let msg = server.recv_timeout(Duration::from_secs(2)).await.expect("server did not receive PublishEvent");
    assert_eq!(
        msg,
        IpcMessage::PublishEvent {
            run_id: run_id.to_string(),
            event,
        }
    );

    // 3. Server broadcasts command to client
    let broadcast_count = server
        .broadcast(&IpcMessage::AbortRun {
            run_id: run_id.to_string(),
        })
        .await
        .unwrap();
    assert_eq!(broadcast_count, 1);

    let client_msg = client
        .recv_timeout(Duration::from_secs(2))
        .await
        .unwrap()
        .expect("client did not receive AbortRun");
    assert_eq!(
        client_msg,
        IpcMessage::AbortRun {
            run_id: run_id.to_string()
        }
    );

    // 4. Ping-pong roundtrip
    let ping_ok = client.ping_roundtrip(Duration::from_secs(2)).await.unwrap();
    assert!(ping_ok);

    server.stop().await;
    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn test_multiple_clients_broadcast() {
    let workspace = temp_workspace();
    let server = IpcServer::bind(&workspace).await.unwrap();

    let client1 = IpcClient::connect(&workspace).await.unwrap();
    let client2 = IpcClient::connect(&workspace).await.unwrap();
    assert!(client1.is_connected());
    assert!(client2.is_connected());

    // Give connection tasks a brief tick to register in server's client list
    tokio::time::sleep(Duration::from_millis(50)).await;

    let sent = server.broadcast(&IpcMessage::AbortAll).await.unwrap();
    assert_eq!(sent, 2);

    let msg1 = client1.recv_timeout(Duration::from_secs(2)).await.unwrap().unwrap();
    let msg2 = client2.recv_timeout(Duration::from_secs(2)).await.unwrap().unwrap();

    assert_eq!(msg1, IpcMessage::AbortAll);
    assert_eq!(msg2, IpcMessage::AbortAll);

    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn test_screenshot_request_and_result() {
    let workspace = temp_workspace();
    let mut server = IpcServer::bind(&workspace).await.unwrap();
    let client = IpcClient::connect(&workspace).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Server requests screenshot
    server
        .broadcast(&IpcMessage::RequestScreenshot {
            run_id: "run-007".into(),
            instance_id: "inst-alpha".into(),
        })
        .await
        .unwrap();

    let cmd = client.recv_timeout(Duration::from_secs(2)).await.unwrap().unwrap();
    assert_eq!(
        cmd,
        IpcMessage::RequestScreenshot {
            run_id: "run-007".into(),
            instance_id: "inst-alpha".into()
        }
    );

    // Client sends screenshot result
    client
        .screenshot_result("run-007", "inst-alpha", Some("C:/shot.png".into()), None)
        .await
        .unwrap();

    let res = server.recv_timeout(Duration::from_secs(2)).await.unwrap();
    assert_eq!(
        res,
        IpcMessage::ScreenshotResult {
            run_id: "run-007".into(),
            instance_id: "inst-alpha".into(),
            path: Some("C:/shot.png".into()),
            error: None,
        }
    );

    let _ = std::fs::remove_dir_all(&workspace);
}
