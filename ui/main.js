const invoke = window.__TAURI__.core.invoke;

const el = {
  daemonState: document.querySelector("#daemonState"),
  deviceSelect: document.querySelector("#deviceSelect"),
  physicalPath: document.querySelector("#physicalPath"),
  virtualPath: document.querySelector("#virtualPath"),
  hidrawPath: document.querySelector("#hidrawPath"),
  usbOcValue: document.querySelector("#usbOcValue"),
  audioState: document.querySelector("#audioState"),
  enabled: document.querySelector("#enabled"),
  hidePhysical: document.querySelector("#hidePhysical"),
  disableControllerAudio: document.querySelector("#disableControllerAudio"),
  polling: document.querySelector("#polling"),
  leftShape: document.querySelector("#leftShape"),
  leftSquare: document.querySelector("#leftSquare"),
  leftDeadzone: document.querySelector("#leftDeadzone"),
  leftDeadzoneNumber: document.querySelector("#leftDeadzoneNumber"),
  leftAntiDeadzone: document.querySelector("#leftAntiDeadzone"),
  leftAntiDeadzoneNumber: document.querySelector("#leftAntiDeadzoneNumber"),
  rightShape: document.querySelector("#rightShape"),
  rightSquare: document.querySelector("#rightSquare"),
  rightDeadzone: document.querySelector("#rightDeadzone"),
  rightDeadzoneNumber: document.querySelector("#rightDeadzoneNumber"),
  rightAntiDeadzone: document.querySelector("#rightAntiDeadzone"),
  rightAntiDeadzoneNumber: document.querySelector("#rightAntiDeadzoneNumber"),
  configuredRate: document.querySelector("#configuredRate"),
  hidrawRate: document.querySelector("#hidrawRate"),
  outputRate: document.querySelector("#outputRate"),
  hiddenState: document.querySelector("#hiddenState"),
  runningState: document.querySelector("#runningState"),
  leftCanvas: document.querySelector("#leftCanvas"),
  rightCanvas: document.querySelector("#rightCanvas"),
  leftValues: document.querySelector("#leftValues"),
  rightValues: document.querySelector("#rightValues"),
  lastError: document.querySelector("#lastError"),
  applyProfile: document.querySelector("#applyProfile"),
  restartService: document.querySelector("#restartService"),
  enableService: document.querySelector("#enableService"),
  stopService: document.querySelector("#stopService")
};

const DEFAULT_STICK = {
  deadzone: 0.08,
  anti_deadzone: 0.08,
  deadzone_shape: "radial",
  square: false
};

const DEADZONE_TYPES = new Set(["radial", "axial"]);

const DEFAULT_PROFILE = {
  enabled: true,
  selected_device_path: null,
  left_stick: { ...DEFAULT_STICK, square: true },
  right_stick: { ...DEFAULT_STICK },
  hide_physical: true,
  disable_controller_audio: false,
  polling_binterval: 4
};

let currentStatus = null;
let draftProfile = structuredClone(DEFAULT_PROFILE);
let dirty = false;
let refreshInFlight = false;
let serviceRefreshInFlight = false;
let lastServiceStatus = "checking";

function normalizeStick(stick, fallback = DEFAULT_STICK) {
  const deadzoneType = stick?.deadzone_shape ?? fallback.deadzone_shape;
  return {
    deadzone: Number(stick?.deadzone ?? fallback.deadzone),
    anti_deadzone: Number(stick?.anti_deadzone ?? fallback.anti_deadzone),
    deadzone_shape: DEADZONE_TYPES.has(deadzoneType) ? deadzoneType : fallback.deadzone_shape,
    square: Boolean(stick?.square ?? fallback.square)
  };
}

function normalizeProfile(profile) {
  return {
    enabled: Boolean(profile?.enabled ?? DEFAULT_PROFILE.enabled),
    selected_device_path: profile?.selected_device_path ?? null,
    left_stick: normalizeStick(profile?.left_stick, DEFAULT_PROFILE.left_stick),
    right_stick: normalizeStick(profile?.right_stick, DEFAULT_PROFILE.right_stick),
    hide_physical: Boolean(profile?.hide_physical ?? DEFAULT_PROFILE.hide_physical),
    disable_controller_audio: Boolean(profile?.disable_controller_audio ?? DEFAULT_PROFILE.disable_controller_audio),
    polling_binterval: Number(profile?.polling_binterval ?? DEFAULT_PROFILE.polling_binterval)
  };
}

function clampNumber(value, min, max) {
  const number = Number(value);
  if (!Number.isFinite(number)) return min;
  return Math.max(min, Math.min(max, number));
}

function fmtHz(value) {
  return `${Math.round(value || 0)} Hz`;
}

function fmtPolling(hz, ms) {
  if (!hz || !ms) return "-";
  const msText = ms < 1 ? ms.toFixed(1) : ms.toFixed(0);
  return `${Math.round(hz)} Hz / ${msText} ms`;
}

function fmtUsbOc(metrics) {
  const loaded = metrics.usb_oc_loaded ? "loaded" : "missing";
  const persistent = metrics.usb_oc_persistent ? "persistent" : "not persistent";
  const value = metrics.usb_oc_value ? ` (${metrics.usb_oc_value})` : "";
  return `${loaded}, ${persistent}${value}`;
}

function rawToUnit(value) {
  if (value >= 128) return Math.max(0, Math.min(1, (value - 128) / 127));
  return Math.max(-1, Math.min(0, (value - 128) / 128));
}

function i16ToUnit(value) {
  if (value >= 0) return Math.max(0, Math.min(1, value / 32767));
  return Math.max(-1, Math.min(0, value / 32768));
}

function deviceLabel(device) {
  const id = device.vendor_id && device.product_id ? `${device.vendor_id}:${device.product_id}` : "unknown";
  return `${device.name} (${id})`;
}

function setDirty(value) {
  dirty = value;
  el.applyProfile.disabled = !dirty;
  el.applyProfile.classList.toggle("dirty", dirty);
}

function setStickControls(prefix, stick) {
  el[`${prefix}Shape`].value = DEADZONE_TYPES.has(stick.deadzone_shape) ? stick.deadzone_shape : "radial";
  el[`${prefix}Square`].checked = stick.square;
  setNumberPair(prefix, "Deadzone", stick.deadzone);
  setNumberPair(prefix, "AntiDeadzone", stick.anti_deadzone);
}

function setNumberPair(prefix, name, value) {
  const range = el[`${prefix}${name}`];
  const number = el[`${prefix}${name}Number`];
  const rounded = Number(value).toFixed(3);
  range.value = Math.min(Number(value), Number(range.max));
  number.value = rounded;
}

function readStickControls(prefix) {
  return {
    deadzone: clampNumber(el[`${prefix}DeadzoneNumber`].value, 0, 0.99),
    anti_deadzone: clampNumber(el[`${prefix}AntiDeadzoneNumber`].value, 0, 1),
    deadzone_shape: el[`${prefix}Shape`].value,
    square: el[`${prefix}Square`].checked
  };
}

function syncFormFromProfile(profile) {
  el.deviceSelect.value = profile.selected_device_path || currentStatus?.metrics?.physical_path || "";
  el.enabled.checked = profile.enabled;
  el.hidePhysical.checked = profile.hide_physical;
  el.disableControllerAudio.checked = profile.disable_controller_audio;
  el.polling.value = profile.polling_binterval;
  setStickControls("left", profile.left_stick);
  setStickControls("right", profile.right_stick);
}

function readDraftFromForm() {
  draftProfile = {
    enabled: el.enabled.checked,
    selected_device_path: el.deviceSelect.value || null,
    left_stick: readStickControls("left"),
    right_stick: readStickControls("right"),
    hide_physical: el.hidePhysical.checked,
    disable_controller_audio: el.disableControllerAudio.checked,
    polling_binterval: Number(el.polling.value)
  };
}

function updateDeviceSelect(status) {
  const visibleDevices = status.devices.filter((device) => !device.is_virtual);
  const knownOptions = new Set([...el.deviceSelect.options].map((option) => option.value));
  const wantedOptions = new Set(visibleDevices.map((device) => device.path));
  if (knownOptions.size === wantedOptions.size && [...wantedOptions].every((path) => knownOptions.has(path))) {
    return;
  }

  el.deviceSelect.replaceChildren(
    ...visibleDevices.map((device) => {
      const option = document.createElement("option");
      option.value = device.path;
      option.textContent = deviceLabel(device);
      return option;
    })
  );
}

function drawDeadzone(ctx, cx, cy, r, stick) {
  const dz = stick.deadzone * r;
  ctx.save();
  ctx.strokeStyle = "#d19b55";
  ctx.lineWidth = 2;

  if (stick.deadzone_shape === "radial") {
    ctx.fillStyle = "rgba(209, 155, 85, 0.10)";
    ctx.beginPath();
    ctx.arc(cx, cy, dz, 0, Math.PI * 2);
    ctx.fill();
    ctx.stroke();
  } else if (stick.deadzone_shape === "axial") {
    ctx.fillStyle = "rgba(209, 155, 85, 0.08)";
    ctx.fillRect(cx - dz, cy - r, dz * 2, r * 2);
    ctx.fillRect(cx - r, cy - dz, r * 2, dz * 2);

    ctx.strokeStyle = "rgba(209, 155, 85, 0.95)";
    ctx.beginPath();
    ctx.moveTo(cx - dz, cy - r);
    ctx.lineTo(cx - dz, cy + r);
    ctx.moveTo(cx + dz, cy - r);
    ctx.lineTo(cx + dz, cy + r);
    ctx.moveTo(cx - r, cy - dz);
    ctx.lineTo(cx + r, cy - dz);
    ctx.moveTo(cx - r, cy + dz);
    ctx.lineTo(cx + r, cy + dz);
    ctx.stroke();
  }

  ctx.restore();
}

function drawPoint(ctx, cx, cy, r, x, y, color, radius) {
  ctx.fillStyle = color;
  ctx.beginPath();
  ctx.arc(cx + x * r, cy + y * r, radius, 0, Math.PI * 2);
  ctx.fill();
}

function drawStick(canvas, title, raw, output, stick) {
  const ctx = canvas.getContext("2d");
  const w = canvas.width;
  const h = canvas.height;
  const cx = w / 2;
  const cy = h / 2 + 8;
  const r = Math.min(w, h) * 0.36;

  const rawX = rawToUnit(raw.x);
  const rawY = rawToUnit(raw.y);
  const outX = i16ToUnit(output.x);
  const outY = i16ToUnit(output.y);

  ctx.clearRect(0, 0, w, h);
  ctx.fillStyle = "#171a1d";
  ctx.fillRect(0, 0, w, h);

  ctx.strokeStyle = "#2f3a40";
  ctx.lineWidth = 2;
  ctx.beginPath();
  ctx.arc(cx, cy, r, 0, Math.PI * 2);
  ctx.stroke();
  ctx.strokeRect(cx - r, cy - r, r * 2, r * 2);

  ctx.strokeStyle = "#263037";
  ctx.beginPath();
  ctx.moveTo(cx - r - 18, cy);
  ctx.lineTo(cx + r + 18, cy);
  ctx.moveTo(cx, cy - r - 18);
  ctx.lineTo(cx, cy + r + 18);
  ctx.stroke();

  drawDeadzone(ctx, cx, cy, r, stick);
  drawPoint(ctx, cx, cy, r, rawX, rawY, "#6aa9ff", 7);
  drawPoint(ctx, cx, cy, r, outX, outY, "#64c9b8", 10);

  ctx.fillStyle = "#e8edf0";
  ctx.font = "600 16px system-ui, sans-serif";
  ctx.fillText(title, 18, 28);
  ctx.font = "12px ui-monospace, monospace";
  ctx.fillStyle = "#dce4e7";
  ctx.fillText(`${stick.deadzone_shape}  dz ${stick.deadzone.toFixed(3)}  adz ${stick.anti_deadzone.toFixed(3)}`, 18, 48);
  ctx.fillText(`square ${stick.square ? "on" : "off"}`, 18, 66);
  ctx.fillStyle = "#6aa9ff";
  ctx.fillText("input", w - 92, 28);
  ctx.fillStyle = "#64c9b8";
  ctx.fillText("output", w - 92, 46);
}

function fmtStickValues(raw, output, stick) {
  return [
    `input   x ${raw.x.toString().padStart(6)}  y ${raw.y.toString().padStart(6)}`,
    `output  x ${output.x.toString().padStart(6)}  y ${output.y.toString().padStart(6)}`,
    `type ${stick.deadzone_shape}   dz ${stick.deadzone.toFixed(3)}   adz ${stick.anti_deadzone.toFixed(3)}`,
    `square ${stick.square ? "on" : "off"}`
  ].join("\n");
}

function render(status) {
  currentStatus = status;
  updateDeviceSelect(status);

  const profile = normalizeProfile(status.profile);
  if (!dirty) {
    draftProfile = structuredClone(profile);
    syncFormFromProfile(draftProfile);
  } else {
    readDraftFromForm();
  }

  const metrics = status.metrics;
  el.physicalPath.textContent = metrics.physical_path || "-";
  el.virtualPath.textContent = metrics.virtual_path || "-";
  el.hidrawPath.textContent = metrics.hidraw_path || "-";
  el.usbOcValue.textContent = fmtUsbOc(metrics);
  el.audioState.textContent = metrics.controller_audio_disabled ? "disabled" : "enabled";
  el.configuredRate.textContent = fmtPolling(metrics.configured_polling_hz, metrics.configured_polling_ms);
  el.hidrawRate.textContent = fmtHz(metrics.hidraw_hz);
  el.outputRate.textContent = fmtHz(metrics.output_hz);
  el.hiddenState.textContent = metrics.physical_hidden ? "yes" : "no";
  el.runningState.textContent = metrics.running ? "running" : "idle";
  el.lastError.textContent = metrics.last_error || "";

  drawStick(el.leftCanvas, "Left Stick", status.raw.left, status.output.left, draftProfile.left_stick);
  drawStick(el.rightCanvas, "Right Stick", status.raw.right, status.output.right, draftProfile.right_stick);
  el.leftValues.textContent = fmtStickValues(status.raw.left, status.output.left, draftProfile.left_stick);
  el.rightValues.textContent = fmtStickValues(status.raw.right, status.output.right, draftProfile.right_stick);
}

async function refresh() {
  if (refreshInFlight) return;
  refreshInFlight = true;
  try {
    const status = await invoke("daemon_status");
    el.daemonState.textContent = `Daemon: ${lastServiceStatus || "unknown"}`;
    render(status);
  } catch (error) {
    el.daemonState.textContent = "Daemon: unavailable";
    el.lastError.textContent = String(error);
  } finally {
    refreshInFlight = false;
  }
}

async function refreshServiceStatus() {
  if (serviceRefreshInFlight) return;
  serviceRefreshInFlight = true;
  try {
    lastServiceStatus = await invoke("service_status").catch(() => "unknown");
    el.daemonState.textContent = `Daemon: ${lastServiceStatus || "unknown"}`;
  } finally {
    serviceRefreshInFlight = false;
  }
}

async function applyDraft() {
  readDraftFromForm();
  el.applyProfile.disabled = true;
  try {
    await invoke("set_profile", { profile: draftProfile });
    setDirty(false);
    await refresh();
  } catch (error) {
    el.applyProfile.disabled = false;
    el.lastError.textContent = String(error);
  }
}

function bindDraftInput(input) {
  input.addEventListener("change", () => {
    readDraftFromForm();
    setDirty(true);
    if (currentStatus) render(currentStatus);
  });
}

function bindNumberPair(prefix, name) {
  const range = el[`${prefix}${name}`];
  const number = el[`${prefix}${name}Number`];

  range.addEventListener("input", () => {
    number.value = Number(range.value).toFixed(3);
    readDraftFromForm();
    setDirty(true);
    if (currentStatus) render(currentStatus);
  });

  number.addEventListener("input", () => {
    const value = clampNumber(number.value, Number(number.min), Number(number.max));
    range.value = Math.min(value, Number(range.max));
    readDraftFromForm();
    setDirty(true);
    if (currentStatus) render(currentStatus);
  });
}

function bindControls() {
  [
    el.deviceSelect,
    el.enabled,
    el.hidePhysical,
    el.disableControllerAudio,
    el.polling,
    el.leftShape,
    el.leftSquare,
    el.rightShape,
    el.rightSquare
  ].forEach(bindDraftInput);

  bindNumberPair("left", "Deadzone");
  bindNumberPair("left", "AntiDeadzone");
  bindNumberPair("right", "Deadzone");
  bindNumberPair("right", "AntiDeadzone");

  el.applyProfile.addEventListener("click", applyDraft);
  el.restartService.addEventListener("click", async () => {
    await invoke("service_action", { action: "restart" }).catch((error) => {
      el.lastError.textContent = String(error);
    });
    await refreshServiceStatus();
    await refresh();
  });
  el.enableService.addEventListener("click", async () => {
    await invoke("service_action", { action: "enable" }).catch((error) => {
      el.lastError.textContent = String(error);
    });
    await refreshServiceStatus();
    await refresh();
  });
  el.stopService.addEventListener("click", async () => {
    await invoke("service_action", { action: "stop" }).catch((error) => {
      el.lastError.textContent = String(error);
    });
    await refreshServiceStatus();
    await refresh();
  });
}

bindControls();
setDirty(false);
refreshServiceStatus();
refresh();
setInterval(refresh, 33);
setInterval(refreshServiceStatus, 5000);
