//! Renders all 5 built-in presets through `oxide-render` (Direct2D + DirectWrite)
//! to uncompressed 32-bit BGRA BMP files and prints per-frame render latency.

use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use oxide_core::{
    Config, DiskSample, ExtraTelemetry, GpuSample, GpuVendor, HistoryStore, Localizer, Snapshot,
    TempSource, builtin_presets,
};
use oxide_render::{FlyoutInput, FlyoutRenderer, SystemThemeContext, WidgetRenderer};

fn main() {
    let out_dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("oxide-render-previews"));
    fs::create_dir_all(&out_dir).expect("create output dir");

    let (snap, history) = sample_telemetry();
    let theme_ctx = SystemThemeContext::default();
    let extra = ExtraTelemetry::default();
    let mut renderer = WidgetRenderer::new(320, 40).expect("init D2D renderer");

    for preset in builtin_presets() {
        let mut cfg = Config::default();
        preset.apply_to(&mut cfg);
        cfg.aliases
            .disks
            .insert("Apacer AS2280P4U 1TB".into(), "NVMe".into());

        // Warmup + measure 100 forced D2D frames.
        let _ = renderer
            .render_if_dirty(&cfg, &snap, &history, &theme_ctx, &extra, 144, false, true)
            .expect("warmup render");

        let start = Instant::now();
        for _ in 0..100 {
            let _ = renderer
                .render_if_dirty(&cfg, &snap, &history, &theme_ctx, &extra, 144, false, true)
                .expect("bench render");
        }
        let avg_us = start.elapsed().as_micros() as f64 / 100.0;

        let frame = renderer
            .render_if_dirty(&cfg, &snap, &history, &theme_ctx, &extra, 144, false, true)
            .expect("final render")
            .expect("frame");
        let path = out_dir.join(format!("{}.bmp", preset.preset.id));
        write_bgra_bmp(&path, frame.width, frame.height, frame.bgra_premultiplied)
            .expect("write bmp");
        println!(
            "{:<18} {:>3}x{:<3} px (150% DPI) | avg D2D frame: {:>6.1} us -> {}",
            preset.preset.id,
            frame.width,
            frame.height,
            avg_us,
            path.display()
        );
    }

    // Also render the Win11 Flyout card at 150% DPI for visual verification.
    let mut flyout = FlyoutRenderer::new().expect("init Flyout D2D renderer");
    let mut cfg = Config::default();
    cfg.aliases
        .disks
        .insert("Apacer AS2280P4U 1TB".into(), "NVMe".into());
    let localizer = Localizer::new("uk");
    let per_core = [
        14.0, 52.0, 88.0, 22.0, 61.0, 18.0, 35.0, 74.0, 12.0, 41.0, 29.0, 16.0,
    ];
    let fin = FlyoutInput {
        config: &cfg,
        snapshot: &snap,
        history: &history,
        per_core_pct: &per_core,
        theme_ctx: &theme_ctx,
        localizer: &localizer,
        game_mode_active: false,
        dpi: 144,
    };
    let fframe = flyout.render(&fin).expect("render flyout");
    let fpath = out_dir.join("flyout_dark.bmp");
    write_bgra_bmp(
        &fpath,
        fframe.width_px,
        fframe.height_px,
        flyout.dib().pixels(),
    )
    .expect("write flyout bmp");
    println!(
        "{:<18} {:>3}x{:<3} px (150% DPI) -> {}",
        "flyout_dark",
        fframe.width_px,
        fframe.height_px,
        fpath.display()
    );
}

fn sample_telemetry() -> (Snapshot, HistoryStore) {
    let mut snap = Snapshot::default();
    snap.seq = 1;
    snap.network.adapter = "Wi-Fi".into();
    snap.network.connected = true;
    snap.network.up_bps = 340_000.0;
    snap.network.down_bps = 2_450_000.0;
    snap.cpu.usage_pct = 24.0;
    snap.cpu.freq_mhz = Some(3480.0);
    snap.cpu.temp_c = Some(54.0);
    snap.cpu.temp_source = TempSource::Hwinfo;
    snap.memory.used_bytes = 10_400_000_000;
    snap.memory.total_bytes = 16_000_000_000;
    snap.memory.usage_pct = 65.0;
    snap.gpus = vec![
        GpuSample {
            name: "AMD Radeon(TM) Graphics".into(),
            vendor: GpuVendor::Amd,
            luid: Some(1),
            usage_pct: Some(5.0),
            temp_c: Some(47.0),
            temp_source: TempSource::Adlx,
            dedicated_vram_bytes: Some(512 * 1024 * 1024),
        },
        GpuSample {
            name: "NVIDIA GeForce GTX 1650".into(),
            vendor: GpuVendor::Nvidia,
            luid: Some(2),
            usage_pct: Some(48.0),
            temp_c: Some(68.0),
            temp_source: TempSource::Nvml,
            dedicated_vram_bytes: Some(4 * 1024 * 1024 * 1024),
        },
    ];
    snap.gpu = snap.gpus[1].clone();
    snap.disks = vec![DiskSample {
        name: "Apacer AS2280P4U 1TB".into(),
        index: Some(0),
        is_nvme: true,
        activity_pct: Some(4.0),
        temp_c: Some(41.0),
        temp_source: TempSource::Nvme,
    }];
    snap.disk = snap.disks[0].clone();

    let mut history = HistoryStore::new();
    for i in 0..45 {
        let mut s = snap.clone();
        s.cpu.usage_pct = (15.0 + ((i as f32 * 0.35).sin() * 0.5 + 0.5) * 65.0).clamp(5.0, 95.0);
        s.gpus[1].usage_pct =
            Some((25.0 + ((i as f32 * 0.22).cos() * 0.5 + 0.5) * 55.0).clamp(5.0, 95.0));
        s.gpu = s.gpus[1].clone();
        s.network.down_bps = 350_000.0 + ((i as f64 * 0.3).sin() * 0.5 + 0.5) * 2_800_000.0;
        s.network.up_bps = 120_000.0 + ((i as f64 * 0.45).cos() * 0.5 + 0.5) * 900_000.0;
        history.record_snapshot(&s);
    }
    (snap, history)
}

fn write_bgra_bmp(path: &PathBuf, width: u32, height: u32, bgra: &[u8]) -> std::io::Result<()> {
    let pixel_bytes = (width * height * 4) as u32;
    let file_size = 54 + pixel_bytes;
    let mut buf = Vec::with_capacity(file_size as usize);
    // BITMAPFILEHEADER (14 bytes)
    buf.extend_from_slice(b"BM");
    buf.extend_from_slice(&file_size.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(&54u32.to_le_bytes());
    // BITMAPINFOHEADER (40 bytes) with negative height (top-down BGRA)
    buf.extend_from_slice(&40u32.to_le_bytes());
    buf.extend_from_slice(&(width as i32).to_le_bytes());
    buf.extend_from_slice(&(-(height as i32)).to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&32u16.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(&pixel_bytes.to_le_bytes());
    buf.extend_from_slice(&0i32.to_le_bytes());
    buf.extend_from_slice(&0i32.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(bgra);
    fs::write(path, buf)
}
