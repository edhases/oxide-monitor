# oxide-pawnio-host

Optional, elevated, read-only PawnIO CPU-temperature helper for Oxide Monitor.

## Why it exists

PawnIO's device object denies access to normal-integrity processes
(`pawnio_open` returns `0x80070005`). The monitor therefore stays unelevated and asks
the user to approve a small helper via UAC. The helper is **never launched at startup**.

## Security model

- `requireAdministrator` manifest; explicit `ShellExecuteExW("runas")` from the monitor
  only after the user presses **Enable CPU temperature (requires admin)**.
- Named Pipe restricted to the invoking user SID (plus the helper's elevated token when
  UAC over-the-shoulder runs it as a different admin account), `FILE_FLAG_FIRST_PIPE_INSTANCE`,
  `PIPE_REJECT_REMOTE_CLIENTS`, and sibling-image verification on both ends. The pipe is
  created with a Medium mandatory label so the unelevated monitor can use it; the monitor
  passes its SID so UAC over-the-shoulder elevation (helper runs as a different admin
  account) still binds the correct per-user pipe/DACL.
- Fixed 64-byte-max binary protocol: `Hello`, `ReadCpuTemp`, `Shutdown`. No arbitrary
  module paths, IOCTLs, offsets, or function names from the client.
- One embedded official module (`AMDFamily17.bin`, SHA-256 pinned), loaded after a hash
  check; the official driver additionally verifies its RSA signature at load.
- Only `ioctl_read_smn(0x00059800)` is called. The SMN read uses AMD's standard PCI
  index/data transaction; there are no MSR/port/fan/voltage/clock writes, no SuperIO or
  SMBus modules, no network, and no driver installation.
- Stops on Shutdown, pipe disconnect, 60 s idle, or monitor/app exit (Game Mode is
  handled by the monitor).

## Licensing

- `assets/AMDFamily17.bin` is from `namazso/PawnIO.Modules` release **0.2.3**, licensed
  LGPL-2.1-or-later. It is embedded unmodified; the license text is in
  `assets/COPYING-PawnIO-Modules.txt` and the source is at
  <https://github.com/namazso/PawnIO.Modules/tree/0.2.3>.
- The PawnIO driver/installer is **not** redistributed. Users obtain it from
  <https://pawnio.eu>. The helper rejects an unsigned/untrusted `PawnIOLib.dll`.

## Module pin

```
release:  namazso/PawnIO.Modules 0.2.3 (2026-02-25)
file:     AMDFamily17.bin (9,300 bytes)
sha256:   099dc01d6db97ea997fec4a461e191cc64b9d7ce47c9d2153c451c56c2adcf50
```

This is byte-identical to the `AMDFamily17.bin` embedded by LibreHardwareMonitor 0.9.6
and is contemporary with the installed PawnIO 2.1.0 driver. The latest 0.2.12 pack also
passed the read-only spike, but 0.2.3 is the conservative pin.

## Build

```powershell
cargo build --release --bin oxide-pawnio-host
```

The manifest is embedded only into the binary (`cargo:rustc-link-arg-bins`), so
`cargo test -p oxide-pawnio-host` stays medium-integrity and never shows UAC.

## Manual real test

Start Settings, open Sources, enable `providers.pawnio`, press
**Enable CPU temperature (requires admin)**, accept UAC. The status should become
`running` with a plausible Tctl/Tdie value. Press Stop; the helper must exit. A declined
UAC prompt must show `denied` and must not retry automatically.
