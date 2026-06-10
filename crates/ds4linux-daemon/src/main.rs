use anyhow::{anyhow, Context, Result};
use ds4linux_core::{
    high_speed_hz_from_binterval, high_speed_interval_ms_from_binterval, transform_left_stick,
    transform_right_stick, ControllerSample, DaemonRequest, DaemonResponse, InputDeviceInfo,
    Metrics, Profile, ProfilePatch, StatusSnapshot, DEFAULT_SOCKET_PATH, DUALSENSE_PID,
    DUALSENSE_VID, USB_OC_PARAMETER, VIRTUAL_LEGACY_NAME, VIRTUAL_XBOX360_NAME,
    VIRTUAL_XBOX360_PID, VIRTUAL_XBOX360_VID,
};
use evdev::uinput::VirtualDevice;
use evdev::{
    enumerate, AbsInfo, AbsoluteAxisCode, AttributeSet, BusType, Device, EventSummary, EventType,
    InputEvent, InputId, KeyCode, SynchronizationCode, UinputAbsSetup,
};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tracing::{error, info, warn};

const CONFIG_PATH: &str = "/etc/ds4linux/config.json";
const HIDE_RULE_PATH: &str = "/etc/udev/rules.d/99-ds4linux-hide-physical.rules";
const AUDIO_RULE_PATH: &str = "/etc/udev/rules.d/99-ds4linux-disable-controller-audio.rules";
const USB_OC_MODULES_LOAD_PATH: &str = "/etc/modules-load.d/usb_oc.conf";
const USB_OC_MODPROBE_PATH: &str = "/etc/modprobe.d/usb_oc.conf";
const SOCKET_GROUP: &str = "users";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct DaemonConfig {
    socket_path: String,
    profile: Profile,
    target_user: String,
    socket_group: String,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        let target_user = default_target_user();
        let socket_group = default_socket_group(&target_user);
        Self {
            socket_path: DEFAULT_SOCKET_PATH.to_string(),
            profile: Profile::default(),
            target_user,
            socket_group,
        }
    }
}

#[derive(Debug)]
struct SharedState {
    config: DaemonConfig,
    devices: Vec<InputDeviceInfo>,
    raw: ControllerSample,
    output: ControllerSample,
    metrics: Metrics,
}

impl SharedState {
    fn snapshot(&self) -> StatusSnapshot {
        StatusSnapshot {
            profile: self.config.profile.clone(),
            devices: self.devices.clone(),
            raw: self.raw,
            output: self.output,
            metrics: self.metrics.clone(),
        }
    }
}

#[derive(Clone)]
struct App {
    state: Arc<Mutex<SharedState>>,
    stop: Arc<AtomicBool>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_target(false)
        .init();

    let config = load_config().unwrap_or_else(|err| {
        warn!("using default config: {err:#}");
        DaemonConfig::default()
    });

    fs::create_dir_all("/run/ds4linux").context("create /run/ds4linux")?;
    let socket_path = config.socket_path.clone();
    let socket_group = config.socket_group.clone();
    let state = Arc::new(Mutex::new(SharedState {
        config,
        devices: Vec::new(),
        raw: ControllerSample::default(),
        output: ControllerSample::default(),
        metrics: Metrics::default(),
    }));
    let stop = Arc::new(AtomicBool::new(false));

    let app = App {
        state: state.clone(),
        stop: stop.clone(),
    };

    let metrics_app = app.clone();
    thread::spawn(move || hidraw_rate_loop(metrics_app));

    thread::spawn(move || {
        if let Err(err) = remapper_loop(app) {
            error!("remapper stopped: {err:#}");
        }
    });

    serve_socket(state, stop, socket_path, socket_group).await
}

fn load_config() -> Result<DaemonConfig> {
    let text = fs::read_to_string(CONFIG_PATH).with_context(|| format!("read {CONFIG_PATH}"))?;
    let mut config: DaemonConfig = serde_json::from_str(&text).context("parse config")?;
    sanitize_config(&mut config);
    Ok(config)
}

fn save_config(config: &DaemonConfig) -> Result<()> {
    fs::create_dir_all("/etc/ds4linux").context("create /etc/ds4linux")?;
    let text = serde_json::to_string_pretty(config).context("serialize config")?;
    fs::write(CONFIG_PATH, text).with_context(|| format!("write {CONFIG_PATH}"))?;
    Ok(())
}

fn sanitize_config(config: &mut DaemonConfig) {
    config.profile.sanitize();
    if config.target_user.trim().is_empty() {
        config.target_user = default_target_user();
    }
    if config.socket_group.trim().is_empty() {
        config.socket_group = default_socket_group(&config.target_user);
    }
}

fn default_target_user() -> String {
    env_value("DS4LINUX_TARGET_USER")
        .or_else(|| env_value("SUDO_USER").filter(|value| value != "root"))
        .or_else(|| env_value("USER"))
        .unwrap_or_else(|| "root".to_string())
}

fn default_socket_group(target_user: &str) -> String {
    env_value("DS4LINUX_SOCKET_GROUP")
        .or_else(|| primary_group_for_user(target_user))
        .unwrap_or_else(|| SOCKET_GROUP.to_string())
}

fn env_value(name: &str) -> Option<String> {
    env::var(name).ok().and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn primary_group_for_user(user: &str) -> Option<String> {
    if user.trim().is_empty() {
        return None;
    }
    let output = Command::new("id").args(["-gn", user]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let group = String::from_utf8(output.stdout).ok()?;
    let group = group.trim();
    (!group.is_empty()).then(|| group.to_string())
}

async fn serve_socket(
    state: Arc<Mutex<SharedState>>,
    stop: Arc<AtomicBool>,
    socket_path: String,
    socket_group: String,
) -> Result<()> {
    let path = PathBuf::from(socket_path);
    if path.exists() {
        fs::remove_file(&path).with_context(|| format!("remove old socket {}", path.display()))?;
    }

    let listener = UnixListener::bind(&path).with_context(|| format!("bind {}", path.display()))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o660)).context("chmod socket")?;
    let _ = Command::new("chgrp").arg(socket_group).arg(&path).status();
    info!("listening on {}", path.display());

    while !stop.load(Ordering::Relaxed) {
        let (stream, _) = listener.accept().await.context("accept client")?;
        let state = state.clone();
        let stop = stop.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_client(stream, state, stop).await {
                warn!("client error: {err:#}");
            }
        });
    }

    Ok(())
}

async fn handle_client(
    stream: UnixStream,
    state: Arc<Mutex<SharedState>>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    reader.read_line(&mut line).await.context("read request")?;
    let request: DaemonRequest = serde_json::from_str(line.trim()).context("parse request")?;
    let response =
        handle_request(request, &state, &stop).unwrap_or_else(|err| DaemonResponse::Error {
            message: format!("{err:#}"),
        });
    let mut text = serde_json::to_string(&response).context("serialize response")?;
    text.push('\n');
    writer
        .write_all(text.as_bytes())
        .await
        .context("write response")?;
    Ok(())
}

fn handle_request(
    request: DaemonRequest,
    state: &Arc<Mutex<SharedState>>,
    stop: &Arc<AtomicBool>,
) -> Result<DaemonResponse> {
    match request {
        DaemonRequest::GetStatus => Ok(DaemonResponse::Status {
            status: state.lock().unwrap().snapshot(),
        }),
        DaemonRequest::SetProfile { mut profile } => {
            profile.sanitize();
            let (selected, target_user) = {
                let mut guard = state.lock().unwrap();
                let selected = selected_device_for_profile(&profile, &guard.devices);
                let target_user = guard.config.target_user.clone();
                guard.config.profile = profile.clone();
                save_config(&guard.config)?;
                (selected, target_user)
            };
            apply_polling_for_device(&profile, selected.as_ref())?;
            apply_hide_for_device(profile.hide_physical, selected.as_ref(), &target_user)?;
            apply_controller_audio_disable(
                profile.disable_controller_audio,
                selected.as_ref(),
                &target_user,
            )?;
            Ok(DaemonResponse::Ok)
        }
        DaemonRequest::PatchProfile { patch } => {
            let (old_hide, old_audio, old_polling, old_path, profile, selected, target_user) = {
                let mut state = state.lock().unwrap();
                let old_hide = state.config.profile.hide_physical;
                let old_audio = state.config.profile.disable_controller_audio;
                let old_polling = state.config.profile.polling_binterval;
                let old_path = state.config.profile.selected_device_path.clone();
                patch.apply_to(&mut state.config.profile);
                let profile = state.config.profile.clone();
                let selected = selected_device_for_profile(&profile, &state.devices);
                let target_user = state.config.target_user.clone();
                save_config(&state.config)?;
                (
                    old_hide,
                    old_audio,
                    old_polling,
                    old_path,
                    profile,
                    selected,
                    target_user,
                )
            };

            if old_polling != profile.polling_binterval {
                apply_polling_for_device(&profile, selected.as_ref())?;
            }
            if old_hide != profile.hide_physical || old_path != profile.selected_device_path {
                apply_hide_for_device(profile.hide_physical, selected.as_ref(), &target_user)?;
            }
            if old_audio != profile.disable_controller_audio
                || old_path != profile.selected_device_path
            {
                apply_controller_audio_disable(
                    profile.disable_controller_audio,
                    selected.as_ref(),
                    &target_user,
                )?;
            }
            Ok(DaemonResponse::Ok)
        }
        DaemonRequest::SaveProfile => {
            let state = state.lock().unwrap();
            save_config(&state.config)?;
            Ok(DaemonResponse::Ok)
        }
        DaemonRequest::ApplyPolling { binterval } => {
            let mut patch = ProfilePatch::default();
            patch.polling_binterval = Some(binterval);
            drop(handle_request(
                DaemonRequest::PatchProfile { patch },
                state,
                stop,
            )?);
            Ok(DaemonResponse::Ok)
        }
        DaemonRequest::SetHidePhysical { enabled } => {
            let mut patch = ProfilePatch::default();
            patch.hide_physical = Some(enabled);
            drop(handle_request(
                DaemonRequest::PatchProfile { patch },
                state,
                stop,
            )?);
            Ok(DaemonResponse::Ok)
        }
        DaemonRequest::Stop => {
            stop.store(true, Ordering::Relaxed);
            Ok(DaemonResponse::Ok)
        }
    }
}

fn selected_device_for_profile(
    profile: &Profile,
    devices: &[InputDeviceInfo],
) -> Option<InputDeviceInfo> {
    profile
        .selected_device_path
        .as_ref()
        .and_then(|path| devices.iter().find(|device| &device.path == path).cloned())
        .or_else(|| {
            devices
                .iter()
                .find(|device| {
                    device.vendor_id.as_deref() == Some(DUALSENSE_VID)
                        && device.product_id.as_deref() == Some(DUALSENSE_PID)
                })
                .cloned()
        })
        .or_else(|| devices.iter().find(|device| !device.is_virtual).cloned())
}

fn remapper_loop(app: App) -> Result<()> {
    let mut profile = app.state.lock().unwrap().config.profile.clone();
    let mut target_user = app.state.lock().unwrap().config.target_user.clone();
    let mut raw = ControllerSample::default();
    let mut output = ControllerSample::default();
    let mut current_path: Option<String> = None;
    let mut device: Option<Device> = None;
    let mut virtual_device = create_virtual_device().context("create virtual controller")?;
    let mut virtual_path = virtual_device_path(&mut virtual_device);
    let mut devices: Vec<InputDeviceInfo> = Vec::new();
    let mut last_device_scan = Instant::now() - Duration::from_secs(2);
    let mut last_control_sync = Instant::now() - Duration::from_secs(1);
    let mut input_count = 0u32;
    let mut output_count = 0u32;
    let mut last_rate_tick = Instant::now();
    let mut last_status_publish = Instant::now() - Duration::from_millis(50);
    let mut pending_events: Vec<InputEvent> = Vec::with_capacity(16);

    loop {
        if app.stop.load(Ordering::Relaxed) {
            return Ok(());
        }

        if last_control_sync.elapsed() >= Duration::from_millis(10) {
            if let Ok(state) = app.state.try_lock() {
                profile = state.config.profile.clone();
                target_user = state.config.target_user.clone();
            }
            last_control_sync = Instant::now();
        }

        if device.is_none()
            && (devices.is_empty() || last_device_scan.elapsed() >= Duration::from_secs(1))
        {
            devices = enumerate_controllers();
            last_device_scan = Instant::now();
            publish_devices(&app.state, &profile, &devices);
        }

        let selected = selected_device_for_profile(&profile, &devices);
        let wanted = selected.as_ref().map(|device| device.path.clone());

        if wanted != current_path {
            if let Some(mut old) = device.take() {
                let _ = old.ungrab();
            }
            pending_events.clear();
            current_path = wanted.clone();
            if let Some(path) = wanted {
                match open_physical_device(&path) {
                    Ok(opened) => {
                        info!("selected controller {path}");
                        if let Err(err) = apply_polling_for_device(&profile, selected.as_ref()) {
                            warn!("apply polling override failed: {err:#}");
                        }
                        if let Err(err) = apply_hide_for_device(
                            profile.hide_physical,
                            selected.as_ref(),
                            &target_user,
                        ) {
                            warn!("apply physical hide setting failed: {err:#}");
                        }
                        if let Err(err) = apply_controller_audio_disable(
                            profile.disable_controller_audio,
                            selected.as_ref(),
                            &target_user,
                        ) {
                            warn!("apply controller audio setting failed: {err:#}");
                        }
                        let hidraw_path = selected.as_ref().and_then(find_hidraw_for_device);
                        device = Some(opened);
                        if let Ok(mut state) = app.state.try_lock() {
                            state.metrics.physical_path = Some(path);
                            state.metrics.hidraw_path = hidraw_path;
                            state.metrics.virtual_path = virtual_path.clone();
                            state.metrics.physical_hidden = profile.hide_physical;
                            state.metrics.controller_audio_disabled =
                                profile.disable_controller_audio;
                            state.metrics.last_error = None;
                        }
                    }
                    Err(err) => {
                        if let Ok(mut state) = app.state.try_lock() {
                            state.metrics.physical_path = None;
                            state.metrics.hidraw_path = None;
                            state.metrics.last_error = Some(format!("{err:#}"));
                        }
                    }
                }
            } else {
                if let Ok(mut state) = app.state.try_lock() {
                    state.metrics.physical_path = None;
                    state.metrics.hidraw_path = None;
                }
            }
        }

        let mut drop_device = false;
        if let Some(dev) = device.as_mut() {
            let fd = dev.as_raw_fd();
            match dev.fetch_events() {
                Ok(events) => {
                    for event in events {
                        match event.destructure() {
                            EventSummary::AbsoluteAxis(_, axis, value) => {
                                update_axis(
                                    axis,
                                    value,
                                    &mut raw,
                                    &mut output,
                                    &profile,
                                    &mut pending_events,
                                );
                            }
                            EventSummary::Key(_, key, value) => {
                                if is_gamepad_button(key) {
                                    let key = output_key_for_xbox(key);
                                    pending_events.push(InputEvent::new(
                                        EventType::KEY.0,
                                        key.0,
                                        value,
                                    ));
                                }
                            }
                            EventSummary::Synchronization(
                                _,
                                SynchronizationCode::SYN_REPORT,
                                _,
                            ) => {
                                input_count += 1;
                                if !pending_events.is_empty() {
                                    virtual_device.emit(&pending_events)?;
                                    pending_events.clear();
                                    output_count += 1;
                                }
                            }
                            EventSummary::Synchronization(
                                _,
                                SynchronizationCode::SYN_DROPPED,
                                _,
                            ) => {
                                pending_events.clear();
                                warn!("input events dropped; resyncing from current device state is pending");
                            }
                            _ => {}
                        }
                    }
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => {
                    let _ = wait_for_input(fd, 2);
                }
                Err(err) => {
                    if let Ok(mut state) = app.state.try_lock() {
                        state.metrics.last_error = Some(format!(
                            "read {} failed: {err}",
                            current_path.as_deref().unwrap_or("?")
                        ));
                    }
                    drop_device = true;
                    thread::sleep(Duration::from_millis(250));
                }
            }
        } else {
            thread::sleep(Duration::from_millis(250));
        }
        if drop_device {
            device = None;
            current_path = None;
        }

        if last_rate_tick.elapsed() >= Duration::from_secs(1) {
            let elapsed = last_rate_tick.elapsed().as_secs_f32();
            if let Ok(mut state) = app.state.try_lock() {
                state.metrics.input_hz = input_count as f32 / elapsed;
                state.metrics.output_hz = output_count as f32 / elapsed;
                state.metrics.running = true;
            }
            last_rate_tick = Instant::now();
            input_count = 0;
            output_count = 0;
            if virtual_path.is_none() {
                virtual_path = virtual_device_path(&mut virtual_device);
            }
        }

        if last_status_publish.elapsed() >= Duration::from_millis(16) {
            if let Ok(mut state) = app.state.try_lock() {
                state.raw = raw;
                state.output = output;
                state.metrics.virtual_path = virtual_path.clone();
                state.metrics.physical_hidden = profile.hide_physical;
                state.metrics.controller_audio_disabled = profile.disable_controller_audio;
            }
            last_status_publish = Instant::now();
        }
    }
}

fn publish_devices(
    state: &Arc<Mutex<SharedState>>,
    profile: &Profile,
    devices: &[InputDeviceInfo],
) {
    if let Ok(mut state) = state.try_lock() {
        let selected = selected_device_for_profile(profile, devices);
        state.devices = mark_selected(
            devices.to_vec(),
            selected.as_ref().map(|device| device.path.as_str()),
        );
    }
}

fn hidraw_rate_loop(app: App) {
    let mut current_path: Option<String> = None;
    let mut file: Option<File> = None;
    let mut buffer = [0u8; 256];
    let mut count = 0u32;
    let mut last_rate_tick = Instant::now();
    let mut last_path_check = Instant::now() - Duration::from_secs(1);

    loop {
        if app.stop.load(Ordering::Relaxed) {
            return;
        }

        if last_path_check.elapsed() >= Duration::from_millis(250) {
            let wanted_path = app
                .state
                .try_lock()
                .ok()
                .and_then(|state| state.metrics.hidraw_path.clone());

            if wanted_path != current_path {
                file = None;
                current_path = wanted_path.clone();
                if let Some(path) = wanted_path {
                    match open_hidraw_device(&path) {
                        Ok(opened) => file = Some(opened),
                        Err(err) => {
                            warn!("open hidraw rate source failed: {err:#}");
                        }
                    }
                }
            }
            last_path_check = Instant::now();
        }

        let mut drop_hidraw = false;
        if let Some(open_file) = file.as_mut() {
            match drain_hidraw_reports(open_file, &mut buffer) {
                Ok(read) => count += read,
                Err(err) => {
                    warn!("hidraw rate read failed: {err:#}");
                    drop_hidraw = true;
                }
            }
        }
        if drop_hidraw {
            file = None;
            current_path = None;
        }

        if last_rate_tick.elapsed() >= Duration::from_secs(1) {
            let elapsed = last_rate_tick.elapsed().as_secs_f32();
            if let Ok(mut state) = app.state.try_lock() {
                let usb_oc_loaded = Path::new(USB_OC_PARAMETER).exists();
                let usb_oc_value = fs::read_to_string(USB_OC_PARAMETER)
                    .ok()
                    .map(|value| value.trim().to_string());
                let binterval = usb_oc_value
                    .as_deref()
                    .and_then(parse_usb_oc_binterval)
                    .unwrap_or(state.config.profile.polling_binterval);
                state.metrics.hidraw_hz = count as f32 / elapsed;
                state.metrics.configured_polling_hz = Some(high_speed_hz_from_binterval(binterval));
                state.metrics.configured_polling_ms =
                    Some(high_speed_interval_ms_from_binterval(binterval));
                state.metrics.usb_oc_value = usb_oc_value;
                state.metrics.usb_oc_loaded = usb_oc_loaded;
                state.metrics.usb_oc_persistent = usb_oc_persistent();
            }
            count = 0;
            last_rate_tick = Instant::now();
        }

        thread::sleep(Duration::from_millis(1));
    }
}

fn parse_usb_oc_binterval(value: &str) -> Option<u8> {
    value.trim().rsplit(':').next()?.parse().ok()
}

fn usb_oc_persistent() -> bool {
    file_contains_line(USB_OC_MODULES_LOAD_PATH, "usb_oc")
        && file_contains_text(USB_OC_MODPROBE_PATH, "interrupt_interval_override=")
}

fn file_contains_line(path: &str, wanted: &str) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    text.lines().any(|line| line.trim() == wanted)
}

fn file_contains_text(path: &str, wanted: &str) -> bool {
    fs::read_to_string(path)
        .map(|text| text.contains(wanted))
        .unwrap_or(false)
}

fn open_physical_device(path: &str) -> Result<Device> {
    let mut device = Device::open(path).with_context(|| format!("open {path}"))?;
    set_nonblocking(device.as_raw_fd()).context("set nonblocking")?;
    device.grab().with_context(|| format!("grab {path}"))?;
    Ok(device)
}

fn open_hidraw_device(path: &str) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .open(path)
        .with_context(|| format!("open {path}"))?;
    set_nonblocking(file.as_raw_fd()).context("set hidraw nonblocking")?;
    Ok(file)
}

fn drain_hidraw_reports(file: &mut File, buffer: &mut [u8]) -> Result<u32> {
    let mut count = 0;
    loop {
        match file.read(buffer) {
            Ok(0) => return Ok(count),
            Ok(_) => count += 1,
            Err(err) if err.kind() == ErrorKind::WouldBlock => return Ok(count),
            Err(err) => return Err(err).context("read hidraw report"),
        }
    }
}

fn set_nonblocking(fd: i32) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(anyhow!("F_GETFL failed"));
    }
    let ret = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if ret < 0 {
        return Err(anyhow!("F_SETFL O_NONBLOCK failed"));
    }
    Ok(())
}

fn wait_for_input(fd: i32, timeout_ms: i32) -> Result<bool> {
    let mut pollfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
    if result < 0 {
        return Err(anyhow!("poll failed"));
    }
    Ok(result > 0 && pollfd.revents & libc::POLLIN != 0)
}

fn update_axis(
    axis: AbsoluteAxisCode,
    value: i32,
    raw: &mut ControllerSample,
    output: &mut ControllerSample,
    profile: &Profile,
    pending_events: &mut Vec<InputEvent>,
) {
    match axis {
        AbsoluteAxisCode::ABS_X | AbsoluteAxisCode::ABS_Y => {
            if axis == AbsoluteAxisCode::ABS_X {
                raw.left.x = value;
            } else {
                raw.left.y = value;
            }
            let left = transform_left_stick(raw.left, profile);
            output.left = left;
            pending_events.push(abs_event(
                AbsoluteAxisCode::ABS_X,
                stick_axis_for_xbox(left.x),
            ));
            pending_events.push(abs_event(
                AbsoluteAxisCode::ABS_Y,
                stick_axis_for_xbox(left.y),
            ));
        }
        AbsoluteAxisCode::ABS_RX | AbsoluteAxisCode::ABS_RY => {
            if axis == AbsoluteAxisCode::ABS_RX {
                raw.right.x = value;
            } else {
                raw.right.y = value;
            }
            let right = transform_right_stick(raw.right, profile);
            output.right = right;
            pending_events.push(abs_event(
                AbsoluteAxisCode::ABS_RX,
                stick_axis_for_xbox(right.x),
            ));
            pending_events.push(abs_event(
                AbsoluteAxisCode::ABS_RY,
                stick_axis_for_xbox(right.y),
            ));
        }
        AbsoluteAxisCode::ABS_Z => {
            raw.l2 = value.clamp(0, 255);
            output.l2 = raw.l2;
            pending_events.push(abs_event(AbsoluteAxisCode::ABS_Z, output.l2));
        }
        AbsoluteAxisCode::ABS_RZ => {
            raw.r2 = value.clamp(0, 255);
            output.r2 = raw.r2;
            pending_events.push(abs_event(AbsoluteAxisCode::ABS_RZ, output.r2));
        }
        AbsoluteAxisCode::ABS_HAT0X => {
            raw.hat_x = value.clamp(-1, 1);
            output.hat_x = raw.hat_x;
            pending_events.push(abs_event(AbsoluteAxisCode::ABS_HAT0X, output.hat_x));
        }
        AbsoluteAxisCode::ABS_HAT0Y => {
            raw.hat_y = value.clamp(-1, 1);
            output.hat_y = raw.hat_y;
            pending_events.push(abs_event(AbsoluteAxisCode::ABS_HAT0Y, output.hat_y));
        }
        _ => {}
    }
}

fn abs_event(axis: AbsoluteAxisCode, value: i32) -> InputEvent {
    InputEvent::new(EventType::ABSOLUTE.0, axis.0, value)
}

fn stick_axis_for_xbox(value: i32) -> i32 {
    value.clamp(-32768, 32767)
}

fn create_virtual_device() -> Result<VirtualDevice> {
    let mut keys = AttributeSet::<KeyCode>::new();
    for key in GAMEPAD_BUTTONS {
        keys.insert(*key);
    }

    let stick = AbsInfo::new(0, -32768, 32767, 0, 0, 0);
    let trigger = AbsInfo::new(0, 0, 255, 0, 0, 0);
    let hat = AbsInfo::new(0, -1, 1, 0, 0, 0);
    let axes = [
        UinputAbsSetup::new(AbsoluteAxisCode::ABS_X, stick),
        UinputAbsSetup::new(AbsoluteAxisCode::ABS_Y, stick),
        UinputAbsSetup::new(AbsoluteAxisCode::ABS_RX, stick),
        UinputAbsSetup::new(AbsoluteAxisCode::ABS_RY, stick),
        UinputAbsSetup::new(AbsoluteAxisCode::ABS_Z, trigger),
        UinputAbsSetup::new(AbsoluteAxisCode::ABS_RZ, trigger),
        UinputAbsSetup::new(AbsoluteAxisCode::ABS_HAT0X, hat),
        UinputAbsSetup::new(AbsoluteAxisCode::ABS_HAT0Y, hat),
    ];

    let mut builder = VirtualDevice::builder()?
        .name(VIRTUAL_XBOX360_NAME)
        .input_id(InputId::new(
            BusType::BUS_USB,
            VIRTUAL_XBOX360_VID,
            VIRTUAL_XBOX360_PID,
            0x0110,
        ))
        .with_keys(&keys)?;

    for axis in axes {
        builder = builder.with_absolute_axis(&axis)?;
    }

    builder.build().context("build uinput virtual device")
}

fn virtual_device_path(device: &mut VirtualDevice) -> Option<String> {
    device
        .enumerate_dev_nodes_blocking()
        .ok()
        .and_then(|mut nodes| nodes.find_map(Result::ok))
        .map(|path| path.display().to_string())
}

const GAMEPAD_BUTTONS: &[KeyCode] = &[
    KeyCode::BTN_SOUTH,
    KeyCode::BTN_EAST,
    KeyCode::BTN_NORTH,
    KeyCode::BTN_WEST,
    KeyCode::BTN_TL,
    KeyCode::BTN_TR,
    KeyCode::BTN_TL2,
    KeyCode::BTN_TR2,
    KeyCode::BTN_SELECT,
    KeyCode::BTN_START,
    KeyCode::BTN_MODE,
    KeyCode::BTN_THUMBL,
    KeyCode::BTN_THUMBR,
];

fn is_gamepad_button(key: KeyCode) -> bool {
    GAMEPAD_BUTTONS.contains(&key)
}

fn output_key_for_xbox(key: KeyCode) -> KeyCode {
    match key {
        KeyCode::BTN_NORTH => KeyCode::BTN_WEST,
        KeyCode::BTN_WEST => KeyCode::BTN_NORTH,
        _ => key,
    }
}

fn enumerate_controllers() -> Vec<InputDeviceInfo> {
    let mut devices = Vec::new();
    for (path, device) in enumerate() {
        let Some(abs) = device.supported_absolute_axes() else {
            continue;
        };
        if !(abs.contains(AbsoluteAxisCode::ABS_X) && abs.contains(AbsoluteAxisCode::ABS_Y)) {
            continue;
        }
        let Some(keys) = device.supported_keys() else {
            continue;
        };
        if !GAMEPAD_BUTTONS.iter().any(|key| keys.contains(*key)) {
            continue;
        }
        let input_id = device.input_id();
        let name = device.name().unwrap_or("Unknown controller").to_string();
        let is_virtual = name == VIRTUAL_LEGACY_NAME
            || name == VIRTUAL_XBOX360_NAME
            || (input_id.vendor() == VIRTUAL_XBOX360_VID
                && input_id.product() == VIRTUAL_XBOX360_PID);
        devices.push(InputDeviceInfo {
            path: path.display().to_string(),
            name,
            vendor_id: Some(format!("{:04x}", input_id.vendor())),
            product_id: Some(format!("{:04x}", input_id.product())),
            is_selected: false,
            is_virtual,
        });
    }
    devices.sort_by(|a, b| a.path.cmp(&b.path));
    devices
}

fn mark_selected(
    mut devices: Vec<InputDeviceInfo>,
    selected: Option<&str>,
) -> Vec<InputDeviceInfo> {
    if let Some(selected) = selected {
        for device in &mut devices {
            device.is_selected = device.path == selected;
        }
    }
    devices
}

fn find_hidraw_for_device(device: &InputDeviceInfo) -> Option<String> {
    let vid = device.vendor_id.as_deref().unwrap_or(DUALSENSE_VID);
    let pid = device.product_id.as_deref().unwrap_or(DUALSENSE_PID);
    let mut candidates = Vec::new();

    let entries = fs::read_dir("/dev").ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with("hidraw") {
            continue;
        }
        let Some(props) = device_node_props(&path) else {
            continue;
        };
        let matches_vid_pid = has_prop(&props, "ID_VENDOR_ID", vid)
            && has_prop(&props, "ID_MODEL_ID", pid)
            || has_prop(&props, "ID_USB_VENDOR_ID", vid)
                && has_prop(&props, "ID_USB_MODEL_ID", pid);
        if matches_vid_pid {
            let score = if has_prop(&props, "ID_USB_INTERFACE_NUM", "03") {
                0
            } else {
                1
            };
            candidates.push((score, path.display().to_string()));
        }
    }

    candidates.sort_by(|a, b| a.cmp(b));
    candidates.into_iter().map(|(_, path)| path).next()
}

fn apply_polling_for_device(profile: &Profile, selected: Option<&InputDeviceInfo>) -> Result<()> {
    let vid = selected
        .and_then(|device| device.vendor_id.as_deref())
        .unwrap_or(DUALSENSE_VID);
    let pid = selected
        .and_then(|device| device.product_id.as_deref())
        .unwrap_or(DUALSENSE_PID);
    let value = format!("{vid}:{pid}:{}", profile.polling_binterval);
    fs::write(USB_OC_PARAMETER, value.as_bytes())
        .with_context(|| format!("write {USB_OC_PARAMETER}; is usb_oc loaded?"))?;
    Ok(())
}

fn apply_hide_for_device(
    enabled: bool,
    selected: Option<&InputDeviceInfo>,
    target_user: &str,
) -> Result<()> {
    if !enabled {
        let vid = selected
            .and_then(|device| device.vendor_id.as_deref())
            .unwrap_or(DUALSENSE_VID);
        let pid = selected
            .and_then(|device| device.product_id.as_deref())
            .unwrap_or(DUALSENSE_PID);
        let _ = fs::remove_file(HIDE_RULE_PATH);
        run("udevadm", &["control", "--reload"])?;
        let _ = Command::new("udevadm")
            .args(["trigger", "--action=change", "--subsystem-match=input"])
            .status();
        let _ = Command::new("udevadm")
            .args(["trigger", "--action=change", "--subsystem-match=hidraw"])
            .status();
        let _ = Command::new("udevadm").arg("settle").status();
        restore_user_access_for_vid_pid(vid, pid, target_user)?;
        return Ok(());
    }

    let selected = selected.ok_or_else(|| anyhow!("no selected physical controller to hide"))?;
    let vid = selected.vendor_id.as_deref().unwrap_or(DUALSENSE_VID);
    let pid = selected.product_id.as_deref().unwrap_or(DUALSENSE_PID);
    let rules = format!(
        "ACTION==\"add|change\", SUBSYSTEM==\"input\", KERNEL==\"event*\", ENV{{ID_VENDOR_ID}}==\"{vid}\", ENV{{ID_MODEL_ID}}==\"{pid}\", ENV{{ID_INPUT_JOYSTICK}}=\"0\", TAG-=\"uaccess\", MODE=\"0600\"\n\
         ACTION==\"add|change\", SUBSYSTEM==\"hidraw\", ENV{{ID_VENDOR_ID}}==\"{vid}\", ENV{{ID_MODEL_ID}}==\"{pid}\", TAG-=\"uaccess\", MODE=\"0600\"\n"
    );
    fs::write(HIDE_RULE_PATH, rules).with_context(|| format!("write {HIDE_RULE_PATH}"))?;
    run("udevadm", &["control", "--reload"])?;
    let _ = Command::new("udevadm")
        .args(["trigger", "--action=change", "--subsystem-match=input"])
        .status();
    let _ = Command::new("udevadm")
        .args(["trigger", "--action=change", "--subsystem-match=hidraw"])
        .status();
    let _ = Command::new("udevadm").arg("settle").status();
    strip_user_access_for_vid_pid(vid, pid, target_user)?;
    Ok(())
}

fn apply_controller_audio_disable(
    enabled: bool,
    selected: Option<&InputDeviceInfo>,
    target_user: &str,
) -> Result<()> {
    if !enabled {
        let vid = selected
            .and_then(|device| device.vendor_id.as_deref())
            .unwrap_or(DUALSENSE_VID);
        let pid = selected
            .and_then(|device| device.product_id.as_deref())
            .unwrap_or(DUALSENSE_PID);
        let _ = fs::remove_file(AUDIO_RULE_PATH);
        set_usb_audio_interfaces_authorized(vid, pid, true)?;
        reload_sound_udev()?;
        restore_audio_access_for_vid_pid(vid, pid, target_user)?;
        return Ok(());
    }

    let selected = selected.ok_or_else(|| anyhow!("no selected physical controller for audio"))?;
    let vid = selected.vendor_id.as_deref().unwrap_or(DUALSENSE_VID);
    let pid = selected.product_id.as_deref().unwrap_or(DUALSENSE_PID);
    let rules = format!(
        "ACTION==\"add|change\", SUBSYSTEM==\"usb\", DEVTYPE==\"usb_interface\", ATTR{{bInterfaceClass}}==\"01\", ATTRS{{idVendor}}==\"{vid}\", ATTRS{{idProduct}}==\"{pid}\", ATTR{{authorized}}=\"0\"\n\
         ACTION==\"add|change\", SUBSYSTEM==\"sound\", KERNEL==\"card*\", ATTRS{{idVendor}}==\"{vid}\", ATTRS{{idProduct}}==\"{pid}\", ENV{{ACP_IGNORE}}=\"1\", ENV{{PULSE_IGNORE}}=\"1\"\n\
         ACTION==\"add|change\", SUBSYSTEM==\"sound\", KERNEL==\"controlC*|pcmC*\", ATTRS{{idVendor}}==\"{vid}\", ATTRS{{idProduct}}==\"{pid}\", ENV{{ACP_IGNORE}}=\"1\", ENV{{PULSE_IGNORE}}=\"1\", TAG-=\"uaccess\", MODE=\"0000\"\n"
    );
    fs::write(AUDIO_RULE_PATH, rules).with_context(|| format!("write {AUDIO_RULE_PATH}"))?;
    set_usb_audio_interfaces_authorized(vid, pid, false)?;
    reload_sound_udev()?;
    strip_audio_access_for_vid_pid(vid, pid, target_user)?;
    Ok(())
}

fn reload_sound_udev() -> Result<()> {
    run("udevadm", &["control", "--reload"])?;
    let _ = Command::new("udevadm")
        .args(["trigger", "--action=change", "--subsystem-match=sound"])
        .status();
    let _ = Command::new("udevadm").arg("settle").status();
    Ok(())
}

fn set_usb_audio_interfaces_authorized(vid: &str, pid: &str, authorized: bool) -> Result<()> {
    let value = if authorized { "1" } else { "0" };
    for path in usb_audio_interface_paths_for_vid_pid(vid, pid)? {
        let authorized_path = path.join("authorized");
        if authorized_path.exists() {
            fs::write(&authorized_path, value)
                .with_context(|| format!("write {}", authorized_path.display()))?;
        }
    }
    Ok(())
}

fn usb_audio_interface_paths_for_vid_pid(vid: &str, pid: &str) -> Result<Vec<PathBuf>> {
    let mut interfaces = Vec::new();
    let dir = Path::new("/sys/bus/usb/devices");
    for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some((device_name, _interface)) = name.split_once(':') else {
            continue;
        };
        if read_trimmed(path.join("bInterfaceClass")).as_deref() != Some("01") {
            continue;
        }
        let device_path = dir.join(device_name);
        if read_trimmed(device_path.join("idVendor")).as_deref() == Some(vid)
            && read_trimmed(device_path.join("idProduct")).as_deref() == Some(pid)
        {
            interfaces.push(path);
        }
    }
    interfaces.sort();
    Ok(interfaces)
}

fn restore_user_access_for_vid_pid(vid: &str, pid: &str, target_user: &str) -> Result<()> {
    for pattern in ["/dev/input", "/dev"] {
        let dir = Path::new(pattern);
        for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
            let entry = entry?;
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if pattern == "/dev/input" && !name.starts_with("event") {
                continue;
            }
            if pattern == "/dev" && !name.starts_with("hidraw") {
                continue;
            }
            if device_node_matches(&path, vid, pid) {
                let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o660));
                let _ = Command::new("setfacl")
                    .args(["-m", &format!("u:{target_user}:rw")])
                    .arg(&path)
                    .status();
                let _ = Command::new("setfacl")
                    .args(["-m", "g::rw-,m::rw-"])
                    .arg(&path)
                    .status();
            }
        }
    }
    Ok(())
}

fn restore_audio_access_for_vid_pid(vid: &str, pid: &str, target_user: &str) -> Result<()> {
    for path in sound_device_nodes_for_vid_pid(vid, pid)? {
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o660));
        let _ = Command::new("setfacl")
            .args(["-m", &format!("u:{target_user}:rw")])
            .arg(&path)
            .status();
        let _ = Command::new("setfacl")
            .args(["-m", "g::rw-,m::rw-"])
            .arg(&path)
            .status();
    }
    Ok(())
}

fn strip_user_access_for_vid_pid(vid: &str, pid: &str, target_user: &str) -> Result<()> {
    for pattern in ["/dev/input", "/dev"] {
        let dir = Path::new(pattern);
        for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
            let entry = entry?;
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if pattern == "/dev/input" && !name.starts_with("event") {
                continue;
            }
            if pattern == "/dev" && !name.starts_with("hidraw") {
                continue;
            }
            if device_node_matches(&path, vid, pid) {
                let _ = Command::new("setfacl")
                    .args(["-x", &format!("u:{target_user}")])
                    .arg(&path)
                    .status();
                let _ = Command::new("setfacl")
                    .args(["-m", "g::---,m::---"])
                    .arg(&path)
                    .status();
                let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
            }
        }
    }
    Ok(())
}

fn strip_audio_access_for_vid_pid(vid: &str, pid: &str, target_user: &str) -> Result<()> {
    for path in sound_device_nodes_for_vid_pid(vid, pid)? {
        let _ = Command::new("setfacl")
            .args(["-x", &format!("u:{target_user}")])
            .arg(&path)
            .status();
        let _ = Command::new("setfacl")
            .args(["-m", "g::---,m::---"])
            .arg(&path)
            .status();
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o000));
    }
    Ok(())
}

fn sound_device_nodes_for_vid_pid(vid: &str, pid: &str) -> Result<Vec<PathBuf>> {
    let cards = sound_cards_for_vid_pid(vid, pid)?;
    if cards.is_empty() {
        return Ok(Vec::new());
    }

    let mut nodes = Vec::new();
    let dir = Path::new("/dev/snd");
    for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if cards.iter().any(|card| {
            name == format!("controlC{card}") || name.starts_with(&format!("pcmC{card}D"))
        }) {
            nodes.push(path);
        }
    }
    nodes.sort();
    Ok(nodes)
}

fn sound_cards_for_vid_pid(vid: &str, pid: &str) -> Result<Vec<String>> {
    let mut cards = Vec::new();
    let dir = Path::new("/sys/class/sound");
    for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(card) = name.strip_prefix("card") else {
            continue;
        };
        if card.is_empty() || card.chars().any(|ch| !ch.is_ascii_digit()) {
            continue;
        }
        let Some(props) = sysfs_node_props(&path) else {
            continue;
        };
        if has_prop(&props, "ID_VENDOR_ID", vid) && has_prop(&props, "ID_MODEL_ID", pid)
            || has_prop(&props, "ID_USB_VENDOR_ID", vid) && has_prop(&props, "ID_USB_MODEL_ID", pid)
        {
            cards.push(card.to_string());
        }
    }
    cards.sort();
    Ok(cards)
}

fn device_node_matches(path: &Path, vid: &str, pid: &str) -> bool {
    let Some(props) = device_node_props(path) else {
        return false;
    };
    has_prop(&props, "ID_VENDOR_ID", vid) && has_prop(&props, "ID_MODEL_ID", pid)
        || has_prop(&props, "ID_USB_VENDOR_ID", vid) && has_prop(&props, "ID_USB_MODEL_ID", pid)
}

fn device_node_props(path: &Path) -> Option<String> {
    let output = Command::new("udevadm")
        .args(["info", "-q", "property", "-n"])
        .arg(path)
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn sysfs_node_props(path: &Path) -> Option<String> {
    let output = Command::new("udevadm")
        .args(["info", "-q", "property", "-p"])
        .arg(path)
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn read_trimmed(path: PathBuf) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
}

fn has_prop(props: &str, key: &str, value: &str) -> bool {
    props.lines().any(|line| line == format!("{key}={value}"))
}

fn run(command: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(command)
        .args(args)
        .status()
        .with_context(|| format!("run {command}"))?;
    if !status.success() {
        return Err(anyhow!("{command} exited with {status}"));
    }
    Ok(())
}
