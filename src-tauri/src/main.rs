use anyhow::{anyhow, Context, Result};
use ds4linux_core::{
    DaemonRequest, DaemonResponse, Profile, ProfilePatch, StatusSnapshot, DEFAULT_SOCKET_PATH,
};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::Command;

fn install_graphics_workarounds() {
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    if std::env::var_os("GDK_BACKEND").is_none()
        && std::env::var_os("WAYLAND_DISPLAY").is_some()
        && std::env::var_os("DISPLAY").is_some()
    {
        std::env::set_var("GDK_BACKEND", "x11");
    }
}

fn daemon_request(request: DaemonRequest) -> Result<DaemonResponse> {
    let mut stream = UnixStream::connect(DEFAULT_SOCKET_PATH)
        .with_context(|| format!("connect {DEFAULT_SOCKET_PATH}; is ds4linuxd running?"))?;
    let mut text = serde_json::to_string(&request).context("serialize request")?;
    text.push('\n');
    stream.write_all(text.as_bytes()).context("write request")?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).context("read response")?;
    serde_json::from_str(line.trim()).context("parse daemon response")
}

fn response_ok(response: DaemonResponse) -> Result<()> {
    match response {
        DaemonResponse::Ok => Ok(()),
        DaemonResponse::Error { message } => Err(anyhow!(message)),
        DaemonResponse::Status { .. } => Err(anyhow!("unexpected status response")),
    }
}

#[tauri::command]
fn daemon_status() -> Result<StatusSnapshot, String> {
    match daemon_request(DaemonRequest::GetStatus).map_err(|err| format!("{err:#}"))? {
        DaemonResponse::Status { status } => Ok(status),
        DaemonResponse::Error { message } => Err(message),
        DaemonResponse::Ok => Err("unexpected ok response".to_string()),
    }
}

#[tauri::command]
fn set_profile(profile: Profile) -> Result<(), String> {
    daemon_request(DaemonRequest::SetProfile { profile })
        .and_then(response_ok)
        .map_err(|err| format!("{err:#}"))
}

#[tauri::command]
fn patch_profile(patch: ProfilePatch) -> Result<(), String> {
    daemon_request(DaemonRequest::PatchProfile { patch })
        .and_then(response_ok)
        .map_err(|err| format!("{err:#}"))
}

#[tauri::command]
fn save_profile() -> Result<(), String> {
    daemon_request(DaemonRequest::SaveProfile)
        .and_then(response_ok)
        .map_err(|err| format!("{err:#}"))
}

#[tauri::command]
fn apply_polling(binterval: u8) -> Result<(), String> {
    daemon_request(DaemonRequest::ApplyPolling { binterval })
        .and_then(response_ok)
        .map_err(|err| format!("{err:#}"))
}

#[tauri::command]
fn set_hide_physical(enabled: bool) -> Result<(), String> {
    daemon_request(DaemonRequest::SetHidePhysical { enabled })
        .and_then(response_ok)
        .map_err(|err| format!("{err:#}"))
}

#[tauri::command]
fn service_action(action: String) -> Result<String, String> {
    let args: &[&str] = match action.as_str() {
        "start" => &["systemctl", "start", "ds4linuxd.service"],
        "stop" => &["systemctl", "stop", "ds4linuxd.service"],
        "restart" => &["systemctl", "restart", "ds4linuxd.service"],
        "enable" => &["systemctl", "enable", "--now", "ds4linuxd.service"],
        "disable" => &["systemctl", "disable", "--now", "ds4linuxd.service"],
        _ => return Err(format!("unknown service action: {action}")),
    };
    let output = Command::new("pkexec")
        .args(args)
        .output()
        .map_err(|err| format!("run pkexec: {err}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

#[tauri::command]
fn service_status() -> Result<String, String> {
    let output = Command::new("systemctl")
        .args(["is-active", "ds4linuxd.service"])
        .output()
        .map_err(|err| format!("systemctl is-active: {err}"))?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn main() {
    install_graphics_workarounds();

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            daemon_status,
            set_profile,
            patch_profile,
            save_profile,
            apply_polling,
            set_hide_physical,
            service_action,
            service_status
        ])
        .run(tauri::generate_context!())
        .expect("error while running ds4linux");
}
