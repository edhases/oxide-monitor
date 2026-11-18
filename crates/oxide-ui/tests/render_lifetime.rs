//! GDI / handle lifetime guards for the shared Direct2D + DIB renderer.
//!
//! These live in a dedicated integration-test binary so `GetProcessHandleCount` is not
//! polluted by other unit tests running in parallel in the same process.

use oxide_core::{Config, ExtraTelemetry, HistoryStore, Snapshot, builtin_presets};
use oxide_render::{DibSurface, SystemThemeContext, WidgetRenderer, present_layered_window};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    GR_GDIOBJECTS, GR_USEROBJECTS, GetCurrentProcess, GetGuiResources, GetProcessHandleCount,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows::core::{PCWSTR, w};

/// Serializes the two measurement tests: process handle counts must not race.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn gdi_objects() -> u32 {
    unsafe { GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS) }
}

fn user_objects() -> u32 {
    unsafe { GetGuiResources(GetCurrentProcess(), GR_USEROBJECTS) }
}

fn handle_count() -> u32 {
    let mut count = 0u32;
    unsafe {
        let _ = GetProcessHandleCount(GetCurrentProcess(), &mut count);
    }
    count
}

fn mock_snapshot() -> Snapshot {
    let mut snap = Snapshot::default();
    snap.seq = 1;
    snap.cpu.usage_pct = 37.0;
    snap.memory.used_bytes = 9 << 30;
    snap.memory.total_bytes = 16 << 30;
    snap.memory.usage_pct = 56.0;
    snap.network.up_bps = 850_000.0;
    snap.network.down_bps = 4_200_000.0;
    snap
}

#[test]
fn dib_and_preset_rendering_are_gdi_and_handle_neutral() {
    let _serial = serial();

    // ---- Phase 1: DIB resize churn (the shared overlay/settings surface path) ----
    let mut dib = DibSurface::new(294, 35).unwrap();
    // Warm up so lazily created GDI bookkeeping is not counted as growth.
    dib.ensure_size(312, 41).unwrap();
    dib.ensure_size(294, 35).unwrap();

    let gdi_before = gdi_objects();
    let handles_before = handle_count();
    for i in 0..500 {
        // The warm-up left the surface at 294x35; start with the other size so every
        // iteration performs a real reallocation.
        let (w, h) = if i % 2 == 0 { (312, 41) } else { (294, 35) };
        assert!(dib.ensure_size(w, h).unwrap());
        dib.clear();
    }
    assert_eq!(dib.delete_failures(), 0, "a DIB bitmap was leaked");
    let gdi_after = gdi_objects();
    assert!(
        gdi_after.saturating_sub(gdi_before) <= 1,
        "GDI objects grew from {gdi_before} to {gdi_after} across 500 DIB resizes"
    );
    assert!(
        handle_count().saturating_sub(handles_before) <= 2,
        "handles grew across 500 DIB resizes"
    );

    // ---- Phase 2: preset render churn (sparklines, radial rings, pills, grids) ----
    let mut renderer = WidgetRenderer::new(294, 35).unwrap();
    let snapshot = mock_snapshot();
    let history = HistoryStore::new();
    let theme = SystemThemeContext::default();
    let extra = ExtraTelemetry::default();
    let presets = builtin_presets();

    let render_all = |renderer: &mut WidgetRenderer| {
        for preset in &presets {
            let mut cfg = Config::default();
            preset.apply_to(&mut cfg);
            renderer.invalidate();
            let frame = renderer
                .render_if_dirty(&cfg, &snapshot, &history, &theme, &extra, 106, false, false)
                .unwrap()
                .expect("a forced render must produce a frame");
            assert!(frame.width > 0 && frame.height > 0);
        }
    };

    // Warm up: DWrite loads the per-preset font families on first use.
    render_all(&mut renderer);

    let gdi_before = gdi_objects();
    let user_before = user_objects();
    let handles_before = handle_count();
    for _ in 0..20 {
        render_all(&mut renderer);
    }
    // Leave the renderer on the smallest preset, as a Cancel/revert would.
    let mut cfg = Config::default();
    presets[1].apply_to(&mut cfg);
    renderer.invalidate();
    let compressed = renderer
        .render_if_dirty(&cfg, &snapshot, &history, &theme, &extra, 106, false, true)
        .unwrap()
        .expect("the revert render must produce a frame");
    assert!(compressed.width > 0 && compressed.height > 0);

    let gdi_after = gdi_objects();
    let user_after = user_objects();
    assert!(
        gdi_after.saturating_sub(gdi_before) <= 1,
        "GDI objects grew from {gdi_before} to {gdi_after} across preset cycles"
    );
    assert!(
        user_after.saturating_sub(user_before) <= 1,
        "USER objects grew from {user_before} to {user_after} across preset cycles"
    );
    assert!(
        handle_count().saturating_sub(handles_before) <= 4,
        "handles grew across preset cycles"
    );
}
unsafe extern "system" fn test_wndproc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

/// The live monitor presents every dirty frame through `UpdateLayeredWindowIndirect`.
/// This covers the interactive path with real preset sizes (gamer sparklines, vertical
/// stack, pills) and proves that ULW + DIB presentation does not grow GDI/USER/handles.
#[test]
fn layered_presentation_of_preset_sized_dibs_is_handle_neutral() {
    let _serial = serial();

    let Ok(module) = (unsafe { GetModuleHandleW(PCWSTR::null()) }) else {
        eprintln!("skipping ULW test: GetModuleHandleW failed");
        return;
    };
    let instance = HINSTANCE(module.0);
    let class = w!("OxideLeakAuditLayer");
    let wc = WNDCLASSW {
        lpfnWndProc: Some(test_wndproc),
        hInstance: instance,
        lpszClassName: class,
        ..Default::default()
    };
    if unsafe { RegisterClassW(&wc) } == 0 {
        eprintln!("skipping ULW test: RegisterClassW failed");
        return;
    }
    let Ok(hwnd) = (unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW,
            class,
            w!("oxide-leak-audit"),
            WS_POPUP,
            0,
            0,
            294,
            35,
            None,
            None,
            Some(instance),
            None,
        )
    }) else {
        eprintln!("skipping ULW test: CreateWindowExW failed");
        return;
    };

    let mut renderer = WidgetRenderer::new(294, 35).unwrap();
    let snapshot = mock_snapshot();
    let history = HistoryStore::new();
    let theme = SystemThemeContext::default();
    let extra = ExtraTelemetry::default();
    let presets = builtin_presets();
    // gamer-sparklines and vertical-stack are the two shapes implicated in the report.
    let gamer = presets
        .iter()
        .find(|p| p.preset.id == "gamer-sparklines")
        .expect("gamer preset");
    let vertical = presets
        .iter()
        .find(|p| p.preset.id == "vertical-stack")
        .expect("vertical-stack preset");

    let present = |renderer: &mut WidgetRenderer, preset: &oxide_core::PresetDefinition| {
        let mut cfg = Config::default();
        preset.apply_to(&mut cfg);
        renderer.invalidate();
        let frame = renderer
            .render_if_dirty(&cfg, &snapshot, &history, &theme, &extra, 106, false, false)
            .unwrap()
            .expect("forced render");
        assert!(frame.width > 0 && frame.height > 0);
        present_layered_window(hwnd, renderer.dib(), Some((120, 120)), 255)
    };

    // Warm up D2D/DWrite and the first ULW surface allocation.
    if present(&mut renderer, gamer).is_err() {
        eprintln!("skipping ULW test: UpdateLayeredWindowIndirect unavailable in this session");
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        return;
    }
    let _ = present(&mut renderer, vertical);

    let gdi_before = gdi_objects();
    let user_before = user_objects();
    let handles_before = handle_count();
    for i in 0..50 {
        let preset = if i % 2 == 0 { gamer } else { vertical };
        present(&mut renderer, preset).unwrap();
    }
    let gdi_after = gdi_objects();
    let user_after = user_objects();
    unsafe {
        let _ = DestroyWindow(hwnd);
    }

    assert!(
        gdi_after.saturating_sub(gdi_before) <= 1,
        "GDI objects grew from {gdi_before} to {gdi_after} across ULW presentations"
    );
    assert!(
        user_after.saturating_sub(user_before) <= 1,
        "USER objects grew from {user_before} to {user_after} across ULW presentations"
    );
    assert!(
        handle_count().saturating_sub(handles_before) <= 4,
        "handles grew across ULW presentations"
    );
}
fn thread_count() -> u32 {
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }) else {
        return 0;
    };
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let pid = std::process::id();
    let mut count = 0u32;
    if unsafe { Thread32First(snapshot, &mut entry) }.is_ok() {
        loop {
            if entry.th32OwnerProcessID == pid {
                count = count.saturating_add(1);
            }
            entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
            if unsafe { Thread32Next(snapshot, &mut entry) }.is_err() {
                break;
            }
        }
    }
    unsafe {
        let _ = windows::Win32::Foundation::CloseHandle(snapshot);
    }
    count
}

/// Diagnostic probe (not a gate): isolates which layer grows the OS thread pool.
/// Run with: `cargo test -p oxide-ui --test render_lifetime -- --ignored --nocapture`
#[test]
#[ignore = "diagnostic probe; run manually with --ignored --nocapture"]
fn thread_pool_churn_probe() {
    let _serial = serial();
    println!("threads start              = {}", thread_count());

    let mut dib = DibSurface::new(294, 35).unwrap();
    for i in 0..300 {
        let (w, h) = if i % 2 == 0 { (312, 41) } else { (294, 35) };
        dib.ensure_size(w, h).unwrap();
    }
    println!("after 300 DIB resizes      = {}", thread_count());

    let mut renderer = WidgetRenderer::new(294, 35).unwrap();
    println!("after WidgetRenderer::new  = {}", thread_count());
    let snapshot = mock_snapshot();
    let history = HistoryStore::new();
    let theme = SystemThemeContext::default();
    let extra = ExtraTelemetry::default();
    for preset in builtin_presets() {
        let mut cfg = Config::default();
        preset.apply_to(&mut cfg);
        renderer.invalidate();
        let _ = renderer
            .render_if_dirty(&cfg, &snapshot, &history, &theme, &extra, 106, false, false)
            .unwrap();
        println!("after render {:<18} = {}", preset.preset.id, thread_count());
    }
    std::thread::sleep(std::time::Duration::from_secs(2));
    println!("after 2 s idle             = {}", thread_count());
}
