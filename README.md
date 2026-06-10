# ds4linux

Rust/Tauri controller remapper inspired by the local Python DualSense prototype.

Current scope:

- root daemon: `ds4linuxd`
- Tauri UI: `ds4linux`
- selectable physical controller
- virtual Xbox 360-style controller output
- per-stick deadzone and anti-deadzone
- per-stick radial or axial deadzone type
- per-stick optional square output, applied separately after deadzone processing
- optional physical-controller hide through udev/ACL
- optional controller microphone/speaker disable through udev/ALSA permissions
- polling override through `usb_oc`
- combined input/output stick scope with deadzone overlay
- configured polling, HID raw rate, and output rate display
- manual Apply button for profile changes
- remap hot path isolated from UI, socket IPC, hidraw metrics, sysfs reads, and systemd calls

Build/check:

```sh
cargo check -p ds4linux-core -p ds4linuxd
cargo test -p ds4linux-core
```

Install the daemon:

```sh
./scripts/install-daemon.sh
pkexec systemctl enable --now ds4linuxd.service
```

The install script writes `/etc/ds4linux/config.json` for the user that owns the
checkout. Override that when needed:

```sh
DS4LINUX_TARGET_USER=alice DS4LINUX_SOCKET_GROUP=users ./scripts/install-daemon.sh
```

Optional USB polling override:

ds4linux can ask Linux to change the USB polling interval for the selected
controller, but this is not something a user-space remapper can do alone. It
requires kernel-side support. The supported path is the optional
[`usb_oc-dkms`](https://github.com/p0358/usb_oc-dkms) module, which is the
Linux equivalent of the Windows `hidusbf` approach.

This is not required to use ds4linux. Without `usb_oc`, the remapper, deadzones,
anti-deadzones, square stick output, hiding, and virtual controller output still
work. Only the USB polling override depends on it.

Install `usb_oc-dkms` through your distro package manager first. Then ds4linux
can configure and persist the module settings:

```sh
./scripts/setup-usb-oc.sh
```

By default this configures a DualSense `054c:0ce6` at `bInterval=4`, which is
1000 Hz on high-speed USB. You can override the device and interval:

```sh
./scripts/setup-usb-oc.sh 054c 0ce6 4
DS4LINUX_USB_OC_VID=054c DS4LINUX_USB_OC_PID=0ce6 DS4LINUX_USB_OC_BINTERVAL=4 ./scripts/setup-usb-oc.sh
```

The script writes:

```text
/etc/modules-load.d/usb_oc.conf
/etc/modprobe.d/usb_oc.conf
```

The UI shows whether `usb_oc` is loaded, whether the persistent config is
present, and the current `interrupt_interval_override` value.

Controller audio disable:

Some PlayStation controllers expose an USB audio card for the built-in
microphone and speaker/headset path. ds4linux can disable that audio device at
the system level for the selected controller. This is separate from hiding the
physical controller input.

When enabled, ds4linux writes a udev rule under:

```text
/etc/udev/rules.d/99-ds4linux-disable-controller-audio.rules
```

The rule disables the matching USB audio interface (`bInterfaceClass=01`), marks
the matching ALSA card with `ACP_IGNORE=1` for PipeWire, and removes user access
from the matching `/dev/snd/controlC*` and `/dev/snd/pcmC*` nodes as a fallback.
Existing audio sessions may need the controller to be replugged, or the audio
service/game restarted, before already-created audio routes disappear.

Run the UI:

```sh
npm install
npm run dev
```

Release UI executable after `npm run build`:

```sh
./target/release/ds4linux-ui
```

On Wayland, the UI applies GTK/WebKit workarounds automatically:

- `WEBKIT_DISABLE_DMABUF_RENDERER=1`
- `GDK_BACKEND=x11` when XWayland is available

The daemon stores its persistent config at `/etc/ds4linux/config.json`.

The UI refreshes controller data at about 30 Hz and systemd status at 0.2 Hz. The daemon keeps the input-to-output remap path local to the remap thread and publishes UI state with non-blocking `try_lock` only.
