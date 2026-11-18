//! AMD ADLX backend — dynamic probe only.
//!
//! ADLX (`amdadlx64.dll`) is the only non-admin source of AMD GPU temperature.
//! It is a COM-like C++ API: initialization and every metric read go through
//! versioned vtables. A mismatch crashes the host process, and the installed
//! runtime on the reference machine (driver-side ADLX 1.x) is older than the
//! current public SDK (2.0). Attempting to call those vtable slots from Rust
//! without the matching SDK headers was not reliable enough for a 1 Hz monitor.
//!
//! This module therefore performs the safe part — resolve the DLL from
//! `System32` and verify that the expected entry points exist — and reports
//! nothing. AMD adapters are still fully enumerated through DXGI and receive
//! utilization from PDH; their temperature stays `None` (`—` in the widget) and
//! `temp_source` is `TempSource::None`.
//!
//! ### Why not the legacy ADL2 path?
//!
//! `atiadlxx.dll` (`ADL2_Main_Control_Create` + `ADL2_Adapter_Temperature_Get`)
//! initially looks simpler than ADLX, but it is not safe without the matching
//! SDK:
//!
//! * `ADL2_Main_Control_Create` requires a caller-provided allocator callback.
//!   ADL allocates internal buffers through it, and the Rust side cannot prove
//!   that the runtime releases them with the same allocator.
//! * adapter identity comes from the versioned `ADLAdapterInfo` array, whose
//!   layout changes between ADL releases; a wrong layout means writing past the
//!   caller's buffer.
//! * temperature is addressed by ADL adapter index, which does not map to the
//!   DXGI LUID and differs on hybrid systems.
//!
//! A wrong ADLX/ADL call crashes the whole monitor, so this crate deliberately
//! stops at a readable probe. The AMD hardware still shows load and VRAM; only
//! temperature is missing.
//!
//! Replacing this probe with a real ADLX reader is a local change: add a
//! `readings() -> Vec<VendorGpuReading>` implementation to `AdlxBackend` and
//! feed it to the merge step. No other module needs to change.

use crate::dynlib::Library;

/// Symbols that a usable ADLX runtime must export. `ADLXInitialize2` is the
/// newer entry point; runtimes that only expose `ADLXInitialize` are accepted
/// as well. `ADLXTerminate` is required in both cases.
const INIT_EXPORTS: [&[u8]; 2] = [b"ADLXInitialize\0", b"ADLXInitialize2\0"];
const TERMINATE_EXPORT: &[u8] = b"ADLXTerminate\0";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    Pending,
    Available,
    Unavailable,
}

/// One-shot ADLX discovery. Never calls into ADLX.
pub(crate) struct AdlxBackend {
    state: State,
}

impl AdlxBackend {
    pub(crate) fn new() -> Self {
        Self {
            state: State::Pending,
        }
    }

    /// Probe `System32\amdadlx64.dll` once. The library is released immediately;
    /// nothing is kept loaded because no ADLX interface is ever called.
    pub(crate) fn probe(&mut self) {
        if self.state != State::Pending {
            return;
        }

        self.state = match Library::load_system32("amdadlx64.dll") {
            Some(library)
                if INIT_EXPORTS.iter().any(|symbol| library.has_symbol(symbol))
                    && library.has_symbol(TERMINATE_EXPORT) =>
            {
                tracing::debug!(
                    "ADLX detected; AMD temperature stays unavailable (probe-only backend)"
                );
                State::Available
            }
            _ => {
                tracing::debug!("ADLX not usable; AMD temperature unavailable");
                State::Unavailable
            }
        };
    }

    /// ADLX readings for the merge step. Always empty while this backend is
    /// probe-only; replace with real `VendorGpuReading`s when an ADLX reader is
    /// added. Keeping the method makes that a local change.
    pub(crate) fn readings(&self) -> Vec<crate::merge::VendorGpuReading> {
        Vec::new()
    }
}
