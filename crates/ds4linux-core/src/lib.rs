use serde::{Deserialize, Serialize};

pub const DEFAULT_SOCKET_PATH: &str = "/run/ds4linux/ds4linux.sock";
pub const USB_OC_PARAMETER: &str = "/sys/module/usb_oc/parameters/interrupt_interval_override";
pub const DUALSENSE_VID: &str = "054c";
pub const DUALSENSE_PID: &str = "0ce6";
pub const VIRTUAL_XBOX360_VID: u16 = 0x045e;
pub const VIRTUAL_XBOX360_PID: u16 = 0x028e;
pub const VIRTUAL_XBOX360_NAME: &str = "DS4Linux Virtual Xbox 360 Controller";
pub const VIRTUAL_LEGACY_NAME: &str = "DS4Linux Virtual Controller";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeadzoneShape {
    Radial,
    Axial,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct StickProfile {
    pub deadzone: f32,
    pub anti_deadzone: f32,
    pub deadzone_shape: DeadzoneShape,
    pub square: bool,
}

impl Default for StickProfile {
    fn default() -> Self {
        Self {
            deadzone: 0.08,
            anti_deadzone: 0.08,
            deadzone_shape: DeadzoneShape::Radial,
            square: false,
        }
    }
}

impl StickProfile {
    pub fn sanitize(&mut self) {
        self.deadzone = self.deadzone.clamp(0.0, 0.99);
        self.anti_deadzone = self.anti_deadzone.clamp(0.0, 1.0);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Profile {
    pub enabled: bool,
    pub selected_device_path: Option<String>,
    pub left_stick: StickProfile,
    pub right_stick: StickProfile,
    pub hide_physical: bool,
    pub disable_controller_audio: bool,
    pub polling_binterval: u8,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            enabled: true,
            selected_device_path: None,
            left_stick: StickProfile {
                square: true,
                ..StickProfile::default()
            },
            right_stick: StickProfile::default(),
            hide_physical: true,
            disable_controller_audio: false,
            polling_binterval: 4,
        }
    }
}

impl Profile {
    pub fn sanitize(&mut self) {
        self.left_stick.sanitize();
        self.right_stick.sanitize();
        self.polling_binterval = self.polling_binterval.clamp(1, 16);
    }

    pub fn polling_override_value(&self) -> String {
        format!("{DUALSENSE_VID}:{DUALSENSE_PID}:{}", self.polling_binterval)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProfilePatch {
    pub enabled: Option<bool>,
    pub selected_device_path: Option<Option<String>>,
    pub left_stick: Option<StickProfile>,
    pub right_stick: Option<StickProfile>,
    pub hide_physical: Option<bool>,
    pub disable_controller_audio: Option<bool>,
    pub polling_binterval: Option<u8>,
}

impl ProfilePatch {
    pub fn apply_to(self, profile: &mut Profile) {
        if let Some(value) = self.enabled {
            profile.enabled = value;
        }
        if let Some(value) = self.selected_device_path {
            profile.selected_device_path = value;
        }
        if let Some(value) = self.left_stick {
            profile.left_stick = value;
        }
        if let Some(value) = self.right_stick {
            profile.right_stick = value;
        }
        if let Some(value) = self.hide_physical {
            profile.hide_physical = value;
        }
        if let Some(value) = self.disable_controller_audio {
            profile.disable_controller_audio = value;
        }
        if let Some(value) = self.polling_binterval {
            profile.polling_binterval = value;
        }
        profile.sanitize();
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct Stick2<T> {
    pub x: T,
    pub y: T,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct ControllerSample {
    pub left: Stick2<i32>,
    pub right: Stick2<i32>,
    pub l2: i32,
    pub r2: i32,
    pub hat_x: i32,
    pub hat_y: i32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Metrics {
    pub input_hz: f32,
    pub output_hz: f32,
    pub hidraw_hz: f32,
    pub configured_polling_hz: Option<f32>,
    pub configured_polling_ms: Option<f32>,
    pub physical_path: Option<String>,
    pub virtual_path: Option<String>,
    pub hidraw_path: Option<String>,
    pub last_error: Option<String>,
    pub usb_oc_value: Option<String>,
    pub usb_oc_loaded: bool,
    pub usb_oc_persistent: bool,
    pub physical_hidden: bool,
    pub controller_audio_disabled: bool,
    pub running: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct InputDeviceInfo {
    pub path: String,
    pub name: String,
    pub vendor_id: Option<String>,
    pub product_id: Option<String>,
    pub is_selected: bool,
    pub is_virtual: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StatusSnapshot {
    pub profile: Profile,
    pub devices: Vec<InputDeviceInfo>,
    pub raw: ControllerSample,
    pub output: ControllerSample,
    pub metrics: Metrics,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonRequest {
    GetStatus,
    SetProfile { profile: Profile },
    PatchProfile { patch: ProfilePatch },
    SaveProfile,
    ApplyPolling { binterval: u8 },
    SetHidePhysical { enabled: bool },
    Stop,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonResponse {
    Ok,
    Status { status: StatusSnapshot },
    Error { message: String },
}

pub fn raw_255_to_unit(value: i32) -> f32 {
    let value = value.clamp(0, 255);
    if value >= 128 {
        ((value - 128) as f32 / 127.0).clamp(0.0, 1.0)
    } else {
        ((value - 128) as f32 / 128.0).clamp(-1.0, 0.0)
    }
}

pub fn unit_to_i16(value: f32) -> i32 {
    let value = value.clamp(-1.0, 1.0);
    if value >= 0.0 {
        (value * 32767.0).round() as i32
    } else {
        (value * 32768.0).round() as i32
    }
}

pub fn transform_left_stick(raw: Stick2<i32>, profile: &Profile) -> Stick2<i32> {
    transform_stick(raw, &profile.left_stick, profile.enabled)
}

pub fn transform_right_stick(raw: Stick2<i32>, profile: &Profile) -> Stick2<i32> {
    transform_stick(raw, &profile.right_stick, profile.enabled)
}

pub fn transform_stick(raw: Stick2<i32>, stick: &StickProfile, enabled: bool) -> Stick2<i32> {
    if !enabled {
        return passthrough_stick(raw);
    }

    let x = raw_255_to_unit(raw.x);
    let y = raw_255_to_unit(raw.y);
    let (mut x, mut y) = match stick.deadzone_shape {
        DeadzoneShape::Radial => {
            apply_ds4windows_radial_deadzone(x, y, stick.deadzone, stick.anti_deadzone)
        }
        DeadzoneShape::Axial => (
            apply_axis_deadzone(x, stick.deadzone, stick.anti_deadzone),
            apply_axis_deadzone(y, stick.deadzone, stick.anti_deadzone),
        ),
    };

    if stick.square {
        (x, y) = circle_to_square(x, y);
    }

    Stick2 {
        x: unit_to_i16(x),
        y: unit_to_i16(y),
    }
}

pub fn passthrough_stick(raw: Stick2<i32>) -> Stick2<i32> {
    Stick2 {
        x: unit_to_i16(raw_255_to_unit(raw.x)),
        y: unit_to_i16(raw_255_to_unit(raw.y)),
    }
}

pub fn high_speed_interval_ms_from_binterval(binterval: u8) -> f32 {
    let binterval = binterval.clamp(1, 16);
    2_f32.powi(i32::from(binterval) - 1) / 8.0
}

pub fn high_speed_hz_from_binterval(binterval: u8) -> f32 {
    1000.0 / high_speed_interval_ms_from_binterval(binterval)
}

fn apply_axis_deadzone(value: f32, deadzone: f32, anti_deadzone: f32) -> f32 {
    let sign = value.signum();
    let magnitude = value.abs();
    if magnitude <= deadzone {
        return 0.0;
    }
    let scaled = (magnitude - deadzone) / (1.0 - deadzone);
    sign * (anti_deadzone + scaled * (1.0 - anti_deadzone)).clamp(0.0, 1.0)
}

fn apply_ds4windows_radial_deadzone(
    x: f32,
    y: f32,
    deadzone: f32,
    anti_deadzone: f32,
) -> (f32, f32) {
    let radius = x.hypot(y);
    if radius <= deadzone {
        return (0.0, 0.0);
    }
    if radius <= f32::EPSILON {
        return (0.0, 0.0);
    }

    let abs_x = x.abs().min(1.0);
    let abs_y = y.abs().min(1.0);
    let cos = abs_x / radius;
    let sin = abs_y / radius;

    let out_x = apply_radial_axis(abs_x, cos, deadzone, anti_deadzone) * x.signum();
    let out_y = apply_radial_axis(abs_y, sin, deadzone, anti_deadzone) * y.signum();
    (out_x, out_y)
}

fn apply_radial_axis(magnitude: f32, angle_ratio: f32, deadzone: f32, anti_deadzone: f32) -> f32 {
    if magnitude <= f32::EPSILON || angle_ratio <= f32::EPSILON {
        return 0.0;
    }

    let axis_deadzone = angle_ratio * deadzone;
    let usable_range = (1.0 - axis_deadzone).max(f32::EPSILON);
    let scaled = ((magnitude - axis_deadzone) / usable_range).clamp(0.0, 1.0);
    let axis_anti_deadzone = (angle_ratio * anti_deadzone).clamp(0.0, 1.0);
    ((1.0 - axis_anti_deadzone) * scaled + axis_anti_deadzone).clamp(0.0, 1.0)
}

fn circle_to_square(x: f32, y: f32) -> (f32, f32) {
    let radius = x.hypot(y);
    if radius <= f32::EPSILON {
        return (0.0, 0.0);
    }
    let ux = x / radius;
    let uy = y / radius;
    let boundary = 1.0 / ux.abs().max(uy.abs()).max(f32::EPSILON);
    (ux * radius * boundary, uy * radius * boundary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_is_zero() {
        let out = transform_left_stick(Stick2 { x: 128, y: 128 }, &Profile::default());
        assert_eq!(out, Stick2 { x: 0, y: 0 });
    }

    #[test]
    fn deadzone_holds_small_motion() {
        let out = transform_left_stick(Stick2 { x: 138, y: 128 }, &Profile::default());
        assert_eq!(out, Stick2 { x: 0, y: 0 });
    }

    #[test]
    fn square_left_hits_corners() {
        let out = transform_left_stick(Stick2 { x: 255, y: 0 }, &Profile::default());
        assert_eq!(out.x, 32767);
        assert_eq!(out.y, -32768);
    }

    #[test]
    fn dualsense_high_speed_binterval_four_is_1000hz() {
        assert_eq!(high_speed_interval_ms_from_binterval(4), 1.0);
        assert_eq!(high_speed_hz_from_binterval(4), 1000.0);
    }

    #[test]
    fn radial_deadzone_keeps_diagonal_direction() {
        let profile = Profile {
            left_stick: StickProfile {
                deadzone_shape: DeadzoneShape::Radial,
                square: false,
                ..StickProfile::default()
            },
            ..Profile::default()
        };
        let out = transform_left_stick(Stick2 { x: 255, y: 0 }, &profile);
        assert!(out.x > 30000);
        assert!(out.y < -30000);
    }
}
