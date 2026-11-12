//! Integration test for the full Microsoft Device-Code -> XBL -> XSTS -> MC Services -> Profile flow
//! against a local mock HTTP server.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use deming_auth::{AccountStore, MsaClient, MsaEndpoints, StoredAccount};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn spawn_mock_auth_server() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock server");
    let addr = listener.local_addr().expect("local addr");
    let token_poll_count = Arc::new(AtomicUsize::new(0));
    let poll_counter = Arc::clone(&token_poll_count);

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let counter = Arc::clone(&poll_counter);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16 * 1024];
                let n = match socket.read(&mut buf).await {
                    Ok(n) if n > 0 => n,
                    _ => return,
                };
                let req_str = String::from_utf8_lossy(&buf[..n]);
                let mut lines = req_str.split("\r\n");
                let request_line = lines.next().unwrap_or("");
                let mut parts = request_line.split_whitespace();
                let method = parts.next().unwrap_or("");
                let path = parts.next().unwrap_or("");

                let body_str = req_str.split("\r\n\r\n").nth(1).unwrap_or("");

                let (status, resp_body) = match (method, path) {
                    ("POST", "/oauth2/v2.0/devicecode") => {
                        assert!(body_str.contains("client_id=test-client-id"));
                        (
                            "200 OK",
                            json!({
                                "user_code": "ABCD-1234",
                                "device_code": "dev-code-xyz",
                                "verification_uri": "https://microsoft.com/devicelogin",
                                "expires_in": 60,
                                "interval": 0
                            })
                            .to_string(),
                        )
                    }
                    ("POST", "/oauth2/v2.0/token") => {
                        if body_str.contains("grant_type=refresh_token") {
                            assert!(body_str.contains("refresh_token=msa-refresh-1"));
                            (
                                "200 OK",
                                json!({
                                    "access_token": "msa-access-refreshed",
                                    "refresh_token": "msa-refresh-2",
                                    "expires_in": 3600
                                })
                                .to_string(),
                            )
                        } else {
                            let attempt = counter.fetch_add(1, Ordering::SeqCst);
                            if attempt == 0 {
                                (
                                    "400 Bad Request",
                                    json!({
                                        "error": "authorization_pending",
                                        "error_description": "User has not completed authorization yet."
                                    })
                                    .to_string(),
                                )
                            } else {
                                assert!(body_str.contains("device_code=dev-code-xyz"));
                                (
                                    "200 OK",
                                    json!({
                                        "access_token": "msa-access-1",
                                        "refresh_token": "msa-refresh-1",
                                        "expires_in": 3600
                                    })
                                    .to_string(),
                                )
                            }
                        }
                    }
                    ("POST", "/user/authenticate") => {
                        let v: Value = serde_json::from_str(body_str).expect("xbl json");
                        let rps = v["Properties"]["RpsTicket"].as_str().unwrap_or("");
                        assert!(rps == "d=msa-access-1" || rps == "d=msa-access-refreshed");
                        (
                            "200 OK",
                            json!({
                                "Token": "xbl-token-1",
                                "DisplayClaims": {
                                    "xui": [{ "uhs": "userhash-999" }]
                                }
                            })
                            .to_string(),
                        )
                    }
                    ("POST", "/xsts/authorize") => {
                        let v: Value = serde_json::from_str(body_str).expect("xsts json");
                        assert_eq!(v["Properties"]["UserTokens"][0], "xbl-token-1");
                        assert_eq!(v["RelyingParty"], "rp://api.minecraftservices.com/");
                        (
                            "200 OK",
                            json!({
                                "Token": "xsts-token-1",
                                "DisplayClaims": {
                                    "xui": [{ "uhs": "userhash-999" }]
                                }
                            })
                            .to_string(),
                        )
                    }
                    ("POST", "/authentication/login_with_xbox") => {
                        let v: Value = serde_json::from_str(body_str).expect("mc login json");
                        assert_eq!(v["identityToken"], "XBL3.0 x=userhash-999;xsts-token-1");
                        (
                            "200 OK",
                            json!({
                                "access_token": "mc-jwt-token-final",
                                "expires_in": 86400
                            })
                            .to_string(),
                        )
                    }
                    ("GET", "/minecraft/profile") => {
                        assert!(req_str
                            .to_ascii_lowercase()
                            .contains("authorization: bearer mc-jwt-token-final"));
                        (
                            "200 OK",
                            json!({
                                "id": "853c80ef3c3749fdaa49938b674adae6",
                                "name": "jeb_"
                            })
                            .to_string(),
                        )
                    }
                    _ => (
                        "404 Not Found",
                        json!({ "error": format!("unexpected {method} {path}") }).to_string(),
                    ),
                };

                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp_body}",
                    resp_body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });

    (format!("http://{addr}"), token_poll_count)
}

#[tokio::test]
async fn full_device_code_and_refresh_flow_against_mock_server() {
    let (base_url, poll_count) = spawn_mock_auth_server().await;
    let client = MsaClient::with_endpoints("test-client-id", MsaEndpoints::from_mock_base(&base_url));

    let prompt = client
        .start_device_code()
        .await
        .expect("start_device_code");
    assert_eq!(prompt.user_code, "ABCD-1234");
    assert_eq!(prompt.device_code, "dev-code-xyz");
    assert_eq!(prompt.verification_uri, "https://microsoft.com/devicelogin");

    let session = client
        .poll_and_complete_login(&prompt)
        .await
        .expect("poll_and_complete_login");
    assert_eq!(poll_count.load(Ordering::SeqCst), 2);
    assert_eq!(session.uuid, "853c80ef-3c37-49fd-aa49-938b674adae6");
    assert_eq!(session.username, "jeb_");
    assert_eq!(session.mc_access_token, "mc-jwt-token-final");
    assert_eq!(session.msa_refresh_token, "msa-refresh-1");
    assert!(!session.is_expired());

    // Persist to AccountStore and verify round-trip
    let store_path = std::env::temp_dir().join(format!(
        "deming-auth-msa-test-{}.json",
        uuid::Uuid::new_v4()
    ));
    let store = AccountStore::new(&store_path);
    store
        .upsert(StoredAccount::from_msa(&session))
        .expect("persist msa account");
    let loaded = store.load().expect("load persisted account");
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].username(), "jeb_");

    // Refresh flow
    let refreshed = client
        .refresh_token(&session.msa_refresh_token)
        .await
        .expect("refresh_token");
    assert_eq!(refreshed.msa_refresh_token, "msa-refresh-2");
    assert_eq!(refreshed.mc_access_token, "mc-jwt-token-final");
    assert_eq!(refreshed.uuid, "853c80ef-3c37-49fd-aa49-938b674adae6");

    let _ = std::fs::remove_file(&store_path);
}
