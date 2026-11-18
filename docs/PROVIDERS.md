# External temperature providers

Oxide Monitor can read temperatures that an already-running monitoring tool publishes.
By default it loads no kernel driver, requires no administrator rights, and opens no
network listener. An optional PawnIO helper (off by default) can be elevated only by an
explicit user action for CPU Tctl/Tdie. Three providers are supported:

| Provider | Transport | Default |
|---|---|---|
| **HWiNFO** | shared memory `Global\HWiNFO_SENS_SM2` | off |
| **LibreHardwareMonitor** | HTTP `http://127.0.0.1:8085/metrics` (loopback only) | off |
| **PawnIO** | optional elevated helper, invoking-user Named Pipe | off |

All three are configured in `config.toml` (`[providers]`) or in Settings:

```toml
[providers]
hwinfo = true              # read HWiNFO shared memory
lhm_http = true            # read the LibreHardwareMonitor web server
lhm_url = "http://127.0.0.1:8085/metrics"
lhm_auto_update = false    # stage the latest official portable LHM (off by default)
pawnio = false             # optional elevated CPU-only helper (off by default)
# helper = false           # reserved, unsupported, not shown in the UI
```

## Official versions and LHM auto-update

Oxide Monitor discovers the latest official versions and can stage a portable LHM copy.
It **never executes** a downloaded binary, never stops/restarts a running LHM, and never
installs a driver. All network access is HTTPS to an allowlist of official hosts:
`api.github.com`, `github.com`, `objects.githubusercontent.com`,
`release-assets.githubusercontent.com`.

| Provider | Official source | Auto-update |
|---|---|---|
| **LibreHardwareMonitor** | `github.com/LibreHardwareMonitor/LibreHardwareMonitor` releases | **yes**, portable copy only, gated |
| **PawnIO** | `pawnio.eu` / `github.com/namazso/PawnIO.Setup` releases | **no** — discovery/link only |

### LHM update flow

1. **Check** - `GET /repos/LibreHardwareMonitor/LibreHardwareMonitor/releases/latest`
   (metadata only; cached for 24 h in `%LOCALAPPDATA%\OxideMonitor\providers\cache`).
   The check runs at startup and every 6 hours even while the providers are disabled, so
   the Sources tab can show current/latest versions; no binary is downloaded without the
   two toggles below.
2. **Gating** — a download happens only when **both** `providers.lhm_http = true` and
   `providers.lhm_auto_update = true`. Enabling the provider alone never downloads.
3. **Download** — the classic portable `LibreHardwareMonitor.zip` asset (bounded to 32 MiB).
4. **Verify** — SHA-256 from the release metadata (when published) and Authenticode.
   A present-but-invalid signature is always discarded. The official LHM executable is
   currently **unsigned**; it is accepted only when the published SHA-256 matched, and
   the status says so explicitly.
5. **Stage** — extracted to
   `%LOCALAPPDATA%\OxideMonitor\providers\LHM\<version>\LibreHardwareMonitor.exe`
   only. If LHM is already running from another path it is left untouched and the status
   says “update available; update it manually”. Start the staged copy yourself when
   ready; the monitor does not launch it.

Managed versions are detected by scanning that folder; a running
`LibreHardwareMonitor.exe` is detected by its process image. The Sources tab shows the
installed version, latest official version, state (`up_to_date`, `update_available`,
`update_ready`, `error`) and the staged path.

### PawnIO (optional elevated CPU helper - Phase 1)

PawnIO is a kernel driver. The monitor itself stays unelevated, never installs or updates
the driver, and only reads the installed version for display. It is available manually
from [`pawnio.eu`](https://pawnio.eu) (UAC required).

Phase 1 is **CPU Tctl/Tdie only** for AMD family 17h–1Ah (Zen 1–Zen 5); Intel and
unsupported platforms show an error instead of loading an unrelated module. It is
strictly opt-in:

1. `providers.pawnio = false` by default. Enabling it in Settings is an explicit action.
2. The elevated helper `oxide-pawnio-host.exe` is **never launched at monitor startup**.
   It starts only after the user presses **Enable CPU temperature (requires admin)** (or
   enables the provider after startup) and accepts the Windows UAC prompt.
3. The helper has a `requireAdministrator` manifest, binds a Named Pipe restricted to the
   invoking user SID (plus the helper's elevated token for UAC over-the-shoulder), and
   accepts only `oxide-monitor.exe` from its own directory.
4. The pipe is a fixed binary protocol (`Hello`, `ReadCpuTemp`, `Shutdown`), max 64 bytes
   per message, bounded timeouts. There is no arbitrary module path, IOCTL name, offset,
   or function call from the UI.
5. The helper dynamically loads the installed signed `PawnIOLib.dll`, verifies its
   Authenticode signature, and loads **one pinned official module** (`AMDFamily17.bin`
   from `PawnIO.Modules` 0.2.3, SHA-256 `099DC01D...2ADCF50`, byte-identical to the blob
   shipped by LHM 0.9.6). The module's RSA signature is also enforced by the official
   driver at load. It reads SMN `0x00059800` and applies the 49 C offset flag exactly like
   LHM/k10temp.
6. It performs **no writes** to MSRs, I/O ports, fan/voltage/clock controls, or persistent
   PCI configuration. The SMN read uses AMD's standard PCI index/data transaction, the
   same mechanism LHM/k10temp uses. No SuperIO/motherboard or SMBus module is loaded in
   Phase 1; SATA SMART is separate.
7. The monitor samples the helper at 1 Hz and publishes the CPU temperature with
   `TempSource::PawnIo`. While running, this explicit direct sensor takes precedence over
   ACPI and the HWiNFO/LHM provider values.
8. Stop conditions: the user presses Stop, `providers.pawnio` is disabled, Game Mode
   activates (manual or detected fullscreen/game), the helper disconnects, or the monitor
   exits. The helper also exits on pipe disconnect or 60 s idle.
9. If UAC is declined, the state is `denied` and the monitor **does not retry silently**.
   Press Enable again to retry. A UAC prompt is never shown at startup.

> **Kernel-driver warning:** PawnIO is a signed kernel driver with hardware access. Some
> anti-cheat products may flag or block it even when only official signed modules are
> loaded. Use it only on systems where that risk is acceptable. Motherboard/SuperIO and
> SATA-via-PawnIO are out of scope for Phase 1; Zen 4 remains source-level support until
> tested on a Raphael machine.

#### Module licensing and distribution

- The `AMDFamily17.bin` blob is from the official LGPL-2.1 `namazso/PawnIO.Modules`
  release 0.2.3. It is embedded unmodified in the helper with the LGPL text and a source
  link (`crates/oxide-pawnio-host/assets/`). Oxide does not modify or relicense it.
- The PawnIO driver/installer is **not redistributed**; users install/update it from
  `pawnio.eu`. The helper requires the official signed edition; an unsigned
  "unrestricted" PawnIOLib is rejected by the Authenticode check.
- No network access in the helper. No driver/service installation, no elevation
  persistence, no scheduled task.

#### PawnIO status fields

`Snapshot.pawnio`, `SnapshotDto.pawnio`, and `HardwareInventory.pawnio` expose:
`installed`, `installed_version`, `enabled`, `state`
(`disabled`/`not_installed`/`stopped`/`starting`/`running`/`denied`/`error`),
`temperature_c`, `helper_version`, `official_url`, `detail`, `last_error`,
`updated_at_unix_ms`.

Settings should also keep using the existing `provider_updates` row for the installed
PawnIO version / official link. Do not add an install or update button for the driver.

## HWiNFO

1. Install and start HWiNFO (Sensors window).
2. Enable **Settings → Safety → Shared Memory Support** (HWiNFO must keep running).
3. Enable the HWiNFO toggle in Oxide Monitor.

The HWiNFO free edition limits shared-memory support (it stops after roughly 12 hours of
runtime); the Pro version is required for permanent use. Oxide Monitor only maps the
section read-only and does not interact with HWiNFO otherwise.

## LibreHardwareMonitor

1. Start LibreHardwareMonitor.
2. Enable **Options → Remote Web Server → Run** (default port `8085`).
3. Keep the URL on loopback (`localhost`, `127.0.0.1` or `::1`) — non-loopback hosts are
   refused by design because LHM's web server has no authentication.

`/metrics` is preferred; older builds without it fall back to `/data.json` automatically.

## Live toggles and status

- Provider toggles take effect **on Apply** (or when `config.toml` is hot-reloaded).
  The Live Preview draft does not start or stop polling.
- The sensor thread recreates the provider reader on change, so disabling a provider
  stops its polling immediately — no restart.
- Every snapshot carries provider health, exposed in the Settings UI and the IPC
  telemetry (`SnapshotDto.providers`, `HardwareInventory.providers`):

| Field | Meaning |
|---|---|
| `id` | `hwinfo` / `lhm_http` |
| `display_name` | `HWiNFO` / `LibreHardwareMonitor` |
| `enabled` | toggle value from the committed config |
| `health` | `disabled`, `connected`, `not_running`, `stale`, `error` |
| `sensor_count` | temperature series with data (CPU/GPU/disk/mainboard, 0–4) |
| `detail` | human-readable reason for the current state |
| `last_update_age_ms` | age of the last successful poll (`null` if never) |

HWiNFO states: `not_running` (shared memory absent), `stale` (section not refreshed),
`error` (access denied, layout change). LHM states: `not_running` (nothing is listening on
the loopback port, or `/metrics`/`/data.json` answered 404), `stale` (the server accepted
the connection but did not answer within 500 ms), `error` (invalid/non-loopback URL, HTTP
error, malformed payload).

A cheap TCP preflight (≈120 ms) runs before every LHM poll, so a missing server is
reported immediately instead of consuming the 500 ms HTTP budget. The detail names the
endpoint and the fix, e.g. *“LHM web server is not running at 127.0.0.1:8085 (start LHM →
Options → Remote Web Server → Run)”*. Older builds showed `Помилка: timeout: global` for
this case; it is now `not_running`.

### Settings UI contract

- `providers.lhm_auto_update` - checkbox in the Sources tab (default off). It only takes
  effect together with the LHM provider toggle, and only after **Apply** / hot reload.
- `providers.pawnio` - opt-in toggle, default off. Turning it on is an explicit user
  action and may start the helper after Apply; it never starts at monitor startup.
  The Settings client can also send `SettingsToMonitor::PawnioControl { start: true }`
  (IPC helper: `IpcClient::pawnio_control(true)`) for a dedicated **Enable CPU
  temperature (requires admin)** button, and `start: false` for **Stop**. The command is
  honored only when `providers.pawnio` is true and PawnIO is installed.
- PawnIO status comes from `Snapshot.pawnio` (also `SnapshotDto.pawnio` and
  `HardwareInventory.pawnio`): `installed`, `installed_version`, `enabled`, `state`,
  `temperature_c`, `helper_version`, `official_url`, `detail`, `last_error`,
  `updated_at_unix_ms`. Show the warning "kernel driver / anti-cheat risk"; do not add
  an install/update control (link to `pawnio.eu` only).
- Update rows come from `Snapshot.provider_updates` (IPC `SnapshotDto.provider_updates`,
  also cloned into `HardwareInventory.provider_updates`):
  `id`, `display_name`, `installed_version`, `latest_version`, `install_kind`
  (`none`/`managed`/`external`/`driver`), `state`
  (`unknown`/`up_to_date`/`update_available`/`update_ready`/`updating`/`error`),
  `auto_update`, `official_url`, `staged_path`, `detail`, `last_error`,
  `checked_at_unix_ms`.
- LHM row: show installed/latest, state, and `staged_path` when `update_ready` ("start
  this copy yourself"). Do not add an auto-install control for PawnIO; show its
  detection result and an official-link button (`official_url`).

## Security notes

- HWiNFO shared memory is opened read-only; no writes, no injection.
- LHM is restricted to loopback plain HTTP. `http://10.0.0.5:8085` (or any non-loopback
  host) is rejected before any socket is opened and reported as an error status.
- Update checks/downloads use HTTPS only, with an explicit official-host allowlist and a
  24 h metadata cache. Rate-limit responses are reported and fall back to the cached
  version.
- Downloads are verified (published SHA-256 and Authenticode; invalid signatures are
  discarded) and only extracted under the per-user provider folder. Nothing is executed
  and no running LHM is touched.
- The monitor itself needs no admin, service, driver, or inbound listener. External
  providers and auto-update are disabled by default.
- The optional PawnIO CPU helper is the only elevated component. It is launched by an
  explicit user action through UAC, binds an invoking-user-only pipe, accepts a fixed
  read-only message set, loads one hash-pinned official module, and is stopped on user
  request, Game Mode, disconnect, or app exit. It never starts at monitor startup and
  never retries a declined UAC prompt silently.
- Game Mode only hides/pauses the widget; it is independent of provider polling and
  updates.

## Troubleshooting

| Symptom | Fix |
|---|---|
| HWiNFO `not_running` | Start HWiNFO and enable Shared Memory Support. |
| HWiNFO `stale` | HWiNFO stopped refreshing; restart it or its sensors. |
| LHM `not_running` (previously `timeout: global`) | Nothing is listening on the configured port. Start LHM → **Options → Remote Web Server → Run**, then verify `http://127.0.0.1:8085/metrics` opens in a browser (`Invoke-WebRequest http://127.0.0.1:8085/metrics`). |
| LHM `stale` | The port accepts connections but LHM did not answer within 500 ms (starting/hung). Restart LHM and check the port matches `providers.lhm_url`. |
| LHM `error` “non-loopback” | Use `localhost`/`127.0.0.1`/`::1`. |
| Temperatures still `—` | The provider may not publish that sensor; NVMe/NVML data takes precedence for GPU/disk. |
