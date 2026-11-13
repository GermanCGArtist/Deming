//! Native WebView popup (`tao` + `wry`) for one-click Microsoft OAuth sign-in.

use std::io::Write;
use std::path::PathBuf;

use deming_auth::MsaClient;
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::{Icon, WindowBuilder};
use wry::{WebContext, WebViewBuilder};

use crate::load_embedded_icon_rgba;
use crate::runner_bridge::DEFAULT_MSA_CLIENT_ID;

const REDIRECT_PREFIX: &str = "https://login.live.com/oauth20_desktop.srf";

/// Returns `true` if `url` is the Microsoft desktop OAuth redirect carrying either `code=` or `error=`.
pub fn is_oauth_callback_url(url: &str) -> bool {
    url.starts_with(REDIRECT_PREFIX) && (url.contains("code=") || url.contains("error="))
}

fn webview_data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("deming")
        .join("login-webview2")
}

/// Opens a dedicated native WebView window for Microsoft sign-in, prints the captured
/// redirect URL (`https://login.live.com/oauth20_desktop.srf?code=...`) to stdout, and exits.
pub fn run_popup_and_print_url() -> Result<(), String> {
    let auth_url = MsaClient::new(DEFAULT_MSA_CLIENT_ID).browser_authorize_url();

    let event_loop = EventLoopBuilder::<String>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let mut window_builder = WindowBuilder::new()
        .with_title("Sign in with Microsoft — Deming")
        .with_inner_size(LogicalSize::new(520.0, 680.0));

    if let Ok((w, h, rgba)) = load_embedded_icon_rgba() {
        if let Ok(icon) = Icon::from_rgba(rgba, w, h) {
            window_builder = window_builder.with_window_icon(Some(icon));
        }
    }

    let window = window_builder
        .build(&event_loop)
        .map_err(|e| format!("failed to create login popup window: {e}"))?;

    let data_dir = webview_data_dir();
    let _ = std::fs::create_dir_all(&data_dir);
    let mut web_context = WebContext::new(Some(data_dir));

    let _webview = WebViewBuilder::new_with_web_context(&mut web_context)
        .with_url(&auth_url)
        .with_navigation_handler(move |url: String| {
            if is_oauth_callback_url(&url) {
                let _ = proxy.send_event(url);
                return false;
            }
            true
        })
        .build(&window)
        .map_err(|e| format!("failed to initialize WebView: {e}"))?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(captured_url) => {
                println!("{captured_url}");
                let _ = std::io::stdout().flush();
                *control_flow = ControlFlow::Exit;
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    });
}
