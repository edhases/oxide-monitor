# Third-party notices

Oxide Monitor application code is licensed under **MIT OR Apache-2.0** (see
`LICENSE-MIT` and `LICENSE-APACHE`).

## PawnIO

The optional `oxide-pawnio-host.exe` helper dynamically loads the official
**PawnIOLib.dll** (LGPL-2.1-or-later) from the user's PawnIO installation. The PawnIO
driver and installer are **not** redistributed by this repository; users install and
update PawnIO manually from <https://pawnio.eu>.

The helper embeds one unmodified official module used for the read-only AMD CPU
temperature path:

- `AMDFamily17.bin` from `namazso/PawnIO.Modules` **0.2.3**
- SHA-256: `099dc01d6db97ea997fec4a461e191cc64b9d7ce47c9d2153c451c56c2adcf50`
- License: **LGPL-2.1-or-later**
- Upstream: <https://github.com/namazso/PawnIO.Modules>
- Full license text: `crates/oxide-pawnio-host/assets/COPYING-PawnIO-Modules.txt`
- Additional notice: `crates/oxide-pawnio-host/assets/NOTICE.md`

The module is loaded by the signed PawnIO driver only after hash and signature checks.
The helper exposes a fixed read-only protocol; it does not accept module paths, IOCTLs,
offsets or arbitrary code from the UI.

## LibreHardwareMonitor

LibreHardwareMonitor is **not bundled**. Oxide Monitor can read a loopback
LibreHardwareMonitor web server and can download official LHM releases into a managed
portable directory when the user enables `providers.lhm_auto_update`. LibreHardwareMonitor
is distributed under the **Mozilla Public License 2.0** by its upstream project:
<https://github.com/LibreHardwareMonitor/LibreHardwareMonitor>.

The official LHM executable is not Authenticode-signed; Oxide verifies the published
SHA-256 of the downloaded release and never executes a staged copy automatically.

## HWiNFO

HWiNFO is **not bundled**. Oxide Monitor only reads HWiNFO's shared-memory section when
the user has installed and started HWiNFO with Shared Memory Support enabled. HWiNFO is
proprietary software; its license is available from <https://www.hwinfo.com/>.

## Rust and Windows dependencies

Third-party Rust crates retain their own licenses (MIT, Apache-2.0, BSD, ISC, MPL, etc.);
the resolved dependency graph and versions are recorded in `Cargo.lock`. Windows API
bindings use the `windows`/`windows-sys` crates (MIT OR Apache-2.0). `egui`/`eframe`, `wgpu`
and `ureq` are used under their respective upstream licenses.

## Windows Task Scheduler and other system components

Oxide Monitor uses documented Windows APIs (Task Scheduler COM, named pipes, DWM,
Direct2D/DirectWrite, DXGI, PDH). It does not redistribute Windows system binaries.
