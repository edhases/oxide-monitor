//! Taskbar geometry discovery, 4 anchor modes, magnetic Alt+Drag snapping,
//! auto-hide visibility detection, and Flyout placement math.
//!
//! All core positioning functions operate on plain `RectI` / `TaskbarGeometry` structs
//! so they can be exhaustively verified with deterministic mock tests.

use std::mem::size_of;

use oxide_core::{Config, MonitorTarget, TaskbarAnchor};
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTOPRIMARY, MONITORINFOEXW,
    MonitorFromWindow,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Shell::{ABM_GETSTATE, ABS_AUTOHIDE, APPBARDATA, SHAppBarMessage};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowExW, FindWindowW, GetClassNameW, GetSystemMetrics, GetWindowRect,
    SM_CXSCREEN, SM_CYSCREEN,
};
use windows::core::{BOOL, PCWSTR, w};

/// Pixel rectangle in virtual desktop screen coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RectI {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl RectI {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub const fn width(self) -> i32 {
        let w = self.right - self.left;
        if w < 0 { 0 } else { w }
    }

    pub const fn height(self) -> i32 {
        let h = self.bottom - self.top;
        if h < 0 { 0 } else { h }
    }

    pub const fn is_valid(self) -> bool {
        self.right > self.left && self.bottom > self.top
    }

    pub const fn intersect(self, other: Self) -> Self {
        let left = if self.left > other.left {
            self.left
        } else {
            other.left
        };
        let top = if self.top > other.top {
            self.top
        } else {
            other.top
        };
        let right = if self.right < other.right {
            self.right
        } else {
            other.right
        };
        let bottom = if self.bottom < other.bottom {
            self.bottom
        } else {
            other.bottom
        };
        Self {
            left,
            top,
            right,
            bottom,
        }
    }
}

impl From<RECT> for RectI {
    fn from(r: RECT) -> Self {
        Self {
            left: r.left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TaskbarEdge {
    #[default]
    Bottom,
    Top,
    Left,
    Right,
}

impl TaskbarEdge {
    pub const fn is_vertical(self) -> bool {
        matches!(self, Self::Left | Self::Right)
    }
}

/// Complete physical-pixel geometry snapshot of the target monitor and its taskbar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskbarGeometry {
    pub monitor_rect: RectI,
    pub work_rect: RectI,
    pub taskbar_rect: RectI,
    pub tray_rect: Option<RectI>,
    pub edge: TaskbarEdge,
    pub dpi: u32,
    pub is_autohide: bool,
}

impl TaskbarGeometry {
    pub fn scale(&self) -> f32 {
        self.dpi.max(48) as f32 / 96.0
    }

    pub fn dip_to_px(&self, dip: i32) -> i32 {
        (dip as f32 * self.scale()).round() as i32
    }

    pub fn px_to_dip(&self, px: i32) -> i32 {
        (px as f32 / self.scale()).round() as i32
    }
}

/// Infer which edge of `monitor` the `taskbar` sits on.
pub fn detect_taskbar_edge(monitor: RectI, taskbar: RectI) -> TaskbarEdge {
    if taskbar.height() > taskbar.width() {
        let mon_mid_x = (monitor.left + monitor.right) / 2;
        let tb_mid_x = (taskbar.left + taskbar.right) / 2;
        if tb_mid_x < mon_mid_x {
            TaskbarEdge::Left
        } else {
            TaskbarEdge::Right
        }
    } else {
        let mon_mid_y = (monitor.top + monitor.bottom) / 2;
        let tb_mid_y = (taskbar.top + taskbar.bottom) / 2;
        if tb_mid_y < mon_mid_y {
            TaskbarEdge::Top
        } else {
            TaskbarEdge::Bottom
        }
    }
}

/// Determine whether an auto-hide taskbar is currently slid off-screen (`<= 4 px` visible).
pub fn is_taskbar_hidden_autohide(geom: &TaskbarGeometry) -> bool {
    if !geom.is_autohide || !geom.taskbar_rect.is_valid() {
        return false;
    }
    let visible = geom.taskbar_rect.intersect(geom.monitor_rect);
    let depth = match geom.edge {
        TaskbarEdge::Bottom | TaskbarEdge::Top => visible.height(),
        TaskbarEdge::Left | TaskbarEdge::Right => visible.width(),
    };
    depth <= 4
}

/// Compute the overlay `(x, y)` in physical pixels for any of the 4 `TaskbarAnchor` modes.
pub fn compute_overlay_position(
    config: &Config,
    geom: &TaskbarGeometry,
    width_px: i32,
    height_px: i32,
) -> (i32, i32) {
    let tb = geom.taskbar_rect;
    let x_off = geom.dip_to_px(config.taskbar.x_offset);
    let y_off = geom.dip_to_px(config.taskbar.y_offset);
    let margin = geom.dip_to_px(4);
    let left_space = geom.dip_to_px(config.taskbar.left_space_win11);
    let right_space = geom.dip_to_px(config.taskbar.right_space_fallback);

    if !tb.is_valid() {
        let mon = geom.monitor_rect;
        return (
            mon.right - width_px - margin * 2 + x_off,
            mon.bottom - height_px - margin * 2 + y_off,
        );
    }

    if geom.edge.is_vertical() {
        let center_x = tb.left + (tb.width() - width_px) / 2;
        let (base_x, base_y) = match config.taskbar.anchor {
            TaskbarAnchor::TrayLeft => {
                if let Some(tray) = geom.tray_rect {
                    (center_x, tray.top - height_px - margin)
                } else {
                    (center_x, tb.bottom - height_px - margin * 2)
                }
            }
            TaskbarAnchor::TaskbarLeft => (center_x, tb.top + margin * 2),
            TaskbarAnchor::TaskbarRight => (center_x, tb.bottom - height_px - margin * 2),
            TaskbarAnchor::FreeDrag => (tb.left, tb.top),
        };
        return clamp_if_free_drag(
            config.taskbar.anchor,
            base_x + x_off,
            base_y + y_off,
            width_px,
            height_px,
            geom.monitor_rect,
        );
    }

    let center_y = tb.top + (tb.height() - height_px) / 2;
    let (base_x, base_y) = match config.taskbar.anchor {
        TaskbarAnchor::TrayLeft => {
            if let Some(tray) = geom.tray_rect {
                (tray.left - width_px - margin, center_y)
            } else {
                (tb.right - right_space - width_px, center_y)
            }
        }
        TaskbarAnchor::TaskbarLeft => (tb.left + left_space, center_y),
        TaskbarAnchor::TaskbarRight => {
            let right_edge = geom
                .tray_rect
                .map(|t| t.left - margin)
                .unwrap_or(tb.right - margin * 2);
            (right_edge - width_px, center_y)
        }
        TaskbarAnchor::FreeDrag => (tb.left, tb.top),
    };

    clamp_if_free_drag(
        config.taskbar.anchor,
        base_x + x_off,
        base_y + y_off,
        width_px,
        height_px,
        geom.monitor_rect,
    )
}

fn clamp_if_free_drag(
    anchor: TaskbarAnchor,
    x: i32,
    y: i32,
    width_px: i32,
    height_px: i32,
    monitor: RectI,
) -> (i32, i32) {
    if anchor != TaskbarAnchor::FreeDrag || !monitor.is_valid() {
        return (x, y);
    }
    let max_x = (monitor.right - width_px).max(monitor.left);
    let max_y = (monitor.bottom - height_px).max(monitor.top);
    (x.clamp(monitor.left, max_x), y.clamp(monitor.top, max_y))
}

/// Apply magnetic snapping while the user drags the widget (`Alt + Drag` or unlocked drag).
pub fn snap_drag_position(
    raw_x: i32,
    raw_y: i32,
    width_px: i32,
    height_px: i32,
    geom: &TaskbarGeometry,
    config: &Config,
) -> (i32, i32) {
    let snap = geom.dip_to_px(config.taskbar.snap_distance);
    let tb = geom.taskbar_rect;
    let mon = geom.monitor_rect;
    let margin = geom.dip_to_px(4);
    let left_space = geom.dip_to_px(config.taskbar.left_space_win11);

    let mut x = raw_x;
    let mut y = raw_y;

    if snap > 0 && tb.is_valid() {
        let mut x_targets = vec![
            tb.left + margin,
            tb.left + left_space,
            tb.right - width_px - margin,
            mon.left + (mon.width() - width_px) / 2,
        ];
        if let Some(tray) = geom.tray_rect {
            x_targets.push(tray.left - width_px - margin);
        }
        for target in x_targets {
            if (x - target).abs() <= snap {
                x = target;
                break;
            }
        }

        let center_y = tb.top + (tb.height() - height_px) / 2;
        if (y - center_y).abs() <= snap {
            y = center_y;
        }
    }

    if mon.is_valid() {
        let max_x = (mon.right - width_px).max(mon.left);
        let max_y = (mon.bottom - height_px).max(mon.top);
        x = x.clamp(mon.left, max_x);
        y = y.clamp(mon.top, max_y);
    }
    (x, y)
}

/// Convert a dragged physical position back into `(x_offset_dip, y_offset_dip)`
/// relative to `TaskbarAnchor::FreeDrag` (anchored at `taskbar_rect.left/top`).
pub fn free_drag_offsets_dip(x: i32, y: i32, geom: &TaskbarGeometry) -> (i32, i32) {
    let base_x = if geom.taskbar_rect.is_valid() {
        geom.taskbar_rect.left
    } else {
        geom.monitor_rect.left
    };
    let base_y = if geom.taskbar_rect.is_valid() {
        geom.taskbar_rect.top
    } else {
        geom.monitor_rect.top
    };
    (
        geom.px_to_dip(x - base_x).clamp(-2000, 2000),
        geom.px_to_dip(y - base_y).clamp(-2000, 2000),
    )
}

/// Compute Flyout `(x, y)` anchored cleanly next to `overlay_rect` and clamped to `work_rect`.
pub fn compute_flyout_position(
    overlay_rect: RectI,
    flyout_w: i32,
    flyout_h: i32,
    geom: &TaskbarGeometry,
) -> (i32, i32) {
    let gap = geom.dip_to_px(8);
    let edge_pad = geom.dip_to_px(8);
    let work = if geom.work_rect.is_valid() {
        geom.work_rect
    } else {
        geom.monitor_rect
    };

    let (raw_x, raw_y) = match geom.edge {
        TaskbarEdge::Bottom => (
            overlay_rect.left + (overlay_rect.width() - flyout_w) / 2,
            overlay_rect.top - flyout_h - gap,
        ),
        TaskbarEdge::Top => (
            overlay_rect.left + (overlay_rect.width() - flyout_w) / 2,
            overlay_rect.bottom + gap,
        ),
        TaskbarEdge::Left => (
            overlay_rect.right + gap,
            overlay_rect.top + (overlay_rect.height() - flyout_h) / 2,
        ),
        TaskbarEdge::Right => (
            overlay_rect.left - flyout_w - gap,
            overlay_rect.top + (overlay_rect.height() - flyout_h) / 2,
        ),
    };

    let min_x = work.left + edge_pad;
    let max_x = (work.right - flyout_w - edge_pad).max(min_x);
    let min_y = work.top + edge_pad;
    let max_y = (work.bottom - flyout_h - edge_pad).max(min_y);

    (raw_x.clamp(min_x, max_x), raw_y.clamp(min_y, max_y))
}

/// Physical fit bounds `(max_width_px, max_height_px)` for the widget.
///
/// Horizontal taskbar: height is the taskbar thickness (the widget is vertically
/// centered on it) and width is the work-area width. Vertical taskbar: the axes swap.
/// `(None, None)` disables fitting when the taskbar geometry is unknown.
pub fn render_bounds(geom: &TaskbarGeometry) -> (Option<u32>, Option<u32>) {
    let taskbar = geom.taskbar_rect.is_valid().then_some(geom.taskbar_rect);
    let work = if geom.work_rect.is_valid() {
        geom.work_rect
    } else {
        geom.monitor_rect
    };
    let positive = |value: i32| (value > 0).then_some(value as u32);

    if geom.edge.is_vertical() {
        (
            taskbar.and_then(|tb| positive(tb.width())),
            positive(work.height()),
        )
    } else {
        (
            positive(work.width()),
            taskbar.and_then(|tb| positive(tb.height())),
        )
    }
}

/// Query live Windows taskbar & monitor geometry for the configured `MonitorTarget`.
pub fn query_taskbar_geometry(target: &MonitorTarget) -> (TaskbarGeometry, Option<HWND>) {
    let is_autohide = query_autohide_flag();

    let (taskbar_hwnd, monitor_rect, work_rect, dpi) = match target {
        MonitorTarget::Device { device, .. } if !device.is_empty() => {
            if let Some((tb, mon, work, dpi)) = find_taskbar_for_device(device) {
                (tb, mon, work, dpi)
            } else {
                find_primary_taskbar()
            }
        }
        _ => find_primary_taskbar(),
    };

    let taskbar_rect = taskbar_hwnd
        .and_then(window_rect)
        .unwrap_or(RectI::default());
    let tray_rect = taskbar_hwnd.and_then(find_tray_rect);
    let edge = if taskbar_rect.is_valid() {
        detect_taskbar_edge(monitor_rect, taskbar_rect)
    } else {
        TaskbarEdge::Bottom
    };

    (
        TaskbarGeometry {
            monitor_rect,
            work_rect,
            taskbar_rect,
            tray_rect,
            edge,
            dpi,
            is_autohide,
        },
        taskbar_hwnd,
    )
}

fn query_autohide_flag() -> bool {
    let mut abd = APPBARDATA {
        cbSize: size_of::<APPBARDATA>() as u32,
        ..Default::default()
    };
    let state = unsafe { SHAppBarMessage(ABM_GETSTATE, &mut abd) };
    (state & (ABS_AUTOHIDE as usize)) != 0
}

fn find_primary_taskbar() -> (Option<HWND>, RectI, RectI, u32) {
    let taskbar = unsafe { FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) }
        .ok()
        .filter(|h| !h.is_invalid());

    if let Some(hwnd) = taskbar {
        let hmon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY) };
        if let Some((mon, work, dpi)) = monitor_metrics(hmon) {
            return (Some(hwnd), mon, work, dpi);
        }
    }

    let sw = unsafe { GetSystemMetrics(SM_CXSCREEN) }.max(640);
    let sh = unsafe { GetSystemMetrics(SM_CYSCREEN) }.max(480);
    let full = RectI::new(0, 0, sw, sh);
    (taskbar, full, full, 96)
}

fn find_taskbar_for_device(device_name: &str) -> Option<(Option<HWND>, RectI, RectI, u32)> {
    let hmon = find_monitor_by_device(device_name)?;
    let (mon, work, dpi) = monitor_metrics(hmon)?;
    let tb = find_taskbar_on_monitor(hmon);
    Some((tb, mon, work, dpi))
}

fn find_monitor_by_device(target_device: &str) -> Option<HMONITOR> {
    struct Search<'a> {
        target: &'a str,
        found: Option<HMONITOR>,
    }

    unsafe extern "system" fn callback(
        hmon: HMONITOR,
        _: HDC,
        _: *mut RECT,
        lparam: LPARAM,
    ) -> BOOL {
        let state = unsafe { &mut *(lparam.0 as *mut Search<'_>) };
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        if unsafe { GetMonitorInfoW(hmon, &mut info.monitorInfo) }.as_bool() {
            let end = info
                .szDevice
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(info.szDevice.len());
            let name = String::from_utf16_lossy(&info.szDevice[..end]);
            if name.eq_ignore_ascii_case(state.target) {
                state.found = Some(hmon);
                return BOOL(0);
            }
        }
        BOOL(1)
    }

    let mut search = Search {
        target: target_device,
        found: None,
    };
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(callback),
            LPARAM(&mut search as *mut _ as isize),
        );
    }
    search.found
}

fn find_taskbar_on_monitor(target_mon: HMONITOR) -> Option<HWND> {
    struct Search {
        target_mon: HMONITOR,
        found: Option<HWND>,
    }

    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let state = unsafe { &mut *(lparam.0 as *mut Search) };
        let mut buf = [0u16; 64];
        let len = unsafe { GetClassNameW(hwnd, &mut buf) };
        if len > 0 {
            let cls = String::from_utf16_lossy(&buf[..len as usize]);
            if cls == "Shell_TrayWnd" || cls == "Shell_SecondaryTrayWnd" {
                let hmon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY) };
                if hmon == state.target_mon {
                    state.found = Some(hwnd);
                    return BOOL(0);
                }
            }
        }
        BOOL(1)
    }

    let mut search = Search {
        target_mon,
        found: None,
    };
    unsafe {
        let _ = EnumWindows(Some(callback), LPARAM(&mut search as *mut _ as isize));
    }
    search.found
}

fn monitor_metrics(hmon: HMONITOR) -> Option<(RectI, RectI, u32)> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    if !unsafe { GetMonitorInfoW(hmon, &mut info.monitorInfo) }.as_bool() {
        return None;
    }
    let mut dpi_x = 96u32;
    let mut dpi_y = 96u32;
    let dpi = if unsafe { GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }
        .is_ok()
        && dpi_x >= 48
    {
        dpi_x
    } else {
        96
    };
    Some((
        RectI::from(info.monitorInfo.rcMonitor),
        RectI::from(info.monitorInfo.rcWork),
        dpi,
    ))
}

fn window_rect(hwnd: HWND) -> Option<RectI> {
    let mut r = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut r) }.ok()?;
    let ri = RectI::from(r);
    ri.is_valid().then_some(ri)
}

fn find_tray_rect(taskbar: HWND) -> Option<RectI> {
    let tray =
        unsafe { FindWindowExW(Some(taskbar), None, w!("TrayNotifyWnd"), PCWSTR::null()) }.ok()?;
    if tray.is_invalid() {
        return None;
    }
    window_rect(tray)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bottom_win11_geom() -> TaskbarGeometry {
        TaskbarGeometry {
            monitor_rect: RectI::new(0, 0, 1920, 1080),
            work_rect: RectI::new(0, 0, 1920, 1032),
            taskbar_rect: RectI::new(0, 1032, 1920, 1080),
            tray_rect: Some(RectI::new(1540, 1032, 1920, 1080)),
            edge: TaskbarEdge::Bottom,
            dpi: 96,
            is_autohide: false,
        }
    }

    #[test]
    fn four_anchor_modes_place_overlay_accurately() {
        let geom = bottom_win11_geom();
        let mut cfg = Config::default();
        let (w, h) = (300, 36);

        // 1. TrayLeft: 4px left of TrayNotifyWnd (1540 - 300 - 4 = 1236), vertically centered (1032 + 6 = 1038)
        cfg.taskbar.anchor = TaskbarAnchor::TrayLeft;
        assert_eq!(compute_overlay_position(&cfg, &geom, w, h), (1236, 1038));

        // 2. TaskbarLeft: left_space_win11 (160) from left edge
        cfg.taskbar.anchor = TaskbarAnchor::TaskbarLeft;
        assert_eq!(compute_overlay_position(&cfg, &geom, w, h), (160, 1038));

        // 3. TaskbarRight (without TrayNotifyWnd on secondary monitor)
        let mut secondary = geom;
        secondary.tray_rect = None;
        cfg.taskbar.anchor = TaskbarAnchor::TaskbarRight;
        assert_eq!(
            compute_overlay_position(&cfg, &secondary, w, h),
            (1920 - 8 - 300, 1038)
        );

        // 4. FreeDrag: relative to taskbar top-left, clamped to monitor bounds
        cfg.taskbar.anchor = TaskbarAnchor::FreeDrag;
        cfg.taskbar.x_offset = 500;
        cfg.taskbar.y_offset = 6;
        assert_eq!(compute_overlay_position(&cfg, &geom, w, h), (500, 1038));

        cfg.taskbar.x_offset = 1900;
        assert_eq!(
            compute_overlay_position(&cfg, &geom, w, h),
            (1920 - 300, 1038)
        );
    }

    #[test]
    fn magnetic_snap_pulls_toward_tray_and_vertical_center() {
        let geom = bottom_win11_geom();
        let cfg = Config::default(); // snap_distance = 8 DIP
        // 5px away from tray target (1236) and 4px away from center_y (1038)
        let snapped = snap_drag_position(1231, 1042, 300, 36, &geom, &cfg);
        assert_eq!(snapped, (1236, 1038));

        // Far from snap targets stays unchanged
        let free = snap_drag_position(800, 1042, 300, 36, &geom, &cfg);
        assert_eq!(free, (800, 1038));
    }

    #[test]
    fn autohide_detects_hidden_vs_visible_on_all_edges() {
        let mut bottom_hidden = bottom_win11_geom();
        bottom_hidden.is_autohide = true;
        bottom_hidden.taskbar_rect = RectI::new(0, 1078, 1920, 1126); // 2px sliver visible
        assert!(is_taskbar_hidden_autohide(&bottom_hidden));

        let mut bottom_shown = bottom_hidden;
        bottom_shown.taskbar_rect = RectI::new(0, 1032, 1920, 1080); // 48px visible
        assert!(!is_taskbar_hidden_autohide(&bottom_shown));

        let left_hidden = TaskbarGeometry {
            monitor_rect: RectI::new(0, 0, 1920, 1080),
            work_rect: RectI::new(0, 0, 1920, 1080),
            taskbar_rect: RectI::new(-60, 0, 2, 1080), // 2px sliver on left
            tray_rect: None,
            edge: TaskbarEdge::Left,
            dpi: 96,
            is_autohide: true,
        };
        assert!(is_taskbar_hidden_autohide(&left_hidden));

        let mut left_shown = left_hidden;
        left_shown.taskbar_rect = RectI::new(0, 0, 62, 1080);
        assert!(!is_taskbar_hidden_autohide(&left_shown));
    }

    #[test]
    fn flyout_stays_clamped_inside_work_area() {
        let geom = bottom_win11_geom();
        // Overlay near right edge of screen
        let overlay = RectI::new(1600, 1038, 1900, 1074);
        let (fx, fy) = compute_flyout_position(overlay, 380, 420, &geom);
        assert_eq!(fx, 1920 - 380 - 8); // clamped 8px from right edge
        assert_eq!(fy, 1032 - 420 - 8); // clamped 8px above taskbar work_rect bottom
        assert!(fy >= geom.work_rect.top);
    }

    #[test]
    fn render_bounds_follow_the_taskbar_edge() {
        let geom = bottom_win11_geom();
        assert_eq!(render_bounds(&geom), (Some(1920), Some(48)));

        let top = TaskbarGeometry {
            taskbar_rect: RectI::new(0, 0, 1920, 48),
            work_rect: RectI::new(0, 48, 1920, 1080),
            edge: TaskbarEdge::Top,
            ..geom
        };
        assert_eq!(render_bounds(&top), (Some(1920), Some(48)));

        let left = TaskbarGeometry {
            taskbar_rect: RectI::new(0, 0, 62, 1080),
            work_rect: RectI::new(62, 0, 1920, 1080),
            edge: TaskbarEdge::Left,
            ..geom
        };
        assert_eq!(render_bounds(&left), (Some(62), Some(1080)));

        let unknown = TaskbarGeometry {
            taskbar_rect: RectI::default(),
            work_rect: RectI::default(),
            monitor_rect: RectI::default(),
            ..geom
        };
        assert_eq!(render_bounds(&unknown), (None, None));
    }

    #[test]
    fn scaled_sizes_stay_inside_the_taskbar_and_work_area() {
        use oxide_core::{Config, SizeMode, WidthMode};

        let mut cfg = Config::default();
        cfg.layout.size_mode = Some(SizeMode::ExactPhysical);
        cfg.layout.width_mode = WidthMode::Fixed(294);
        cfg.layout.height = 35;

        for taskbar_height in [40, 48, 56, 64, 72] {
            for monitor_height in [768, 1080, 1440, 2160] {
                let geom = TaskbarGeometry {
                    monitor_rect: RectI::new(0, 0, 1920, monitor_height),
                    work_rect: RectI::new(0, 0, 1920, monitor_height - taskbar_height),
                    taskbar_rect: RectI::new(
                        0,
                        monitor_height - taskbar_height,
                        1920,
                        monitor_height,
                    ),
                    tray_rect: Some(RectI::new(
                        1540,
                        monitor_height - taskbar_height,
                        1920,
                        monitor_height,
                    )),
                    edge: TaskbarEdge::Bottom,
                    dpi: 96,
                    is_autohide: false,
                };
                let (max_w, max_h) = render_bounds(&geom);
                for scale_pct in [100u16, 150, 200] {
                    cfg.layout.scale_pct = scale_pct;
                    let size = cfg.layout.render_size(294.0, 35.0, 96, false, max_w, max_h);
                    let (x, y) = compute_overlay_position(
                        &cfg,
                        &geom,
                        size.width_px as i32,
                        size.height_px as i32,
                    );
                    assert!(
                        size.height_px <= taskbar_height as u32,
                        "scale {scale_pct}: height {} > taskbar {taskbar_height}",
                        size.height_px
                    );
                    assert!(
                        y >= geom.taskbar_rect.top
                            && y + size.height_px as i32 <= geom.taskbar_rect.bottom,
                        "scale {scale_pct}: widget {y}..{} outside taskbar {:?}",
                        y + size.height_px as i32,
                        geom.taskbar_rect
                    );
                    assert!(x >= geom.monitor_rect.left);
                    assert!(x + size.width_px as i32 <= geom.monitor_rect.right);
                }
            }
        }
    }
    #[test]
    fn fixed_dip_sizes_follow_dpi_and_stay_anchored() {
        use oxide_core::{Config, SizeMode, WidthMode};

        let mut cfg = Config::default();
        cfg.layout.size_mode = Some(SizeMode::FixedDip);
        cfg.layout.width_mode = WidthMode::Fixed(294);
        cfg.layout.height = 35;
        cfg.layout.normalize();

        for dpi in [96u32, 106, 144, 192] {
            for taskbar_height in [40, 48, 56, 64, 72] {
                for (screen_w, screen_h) in [(1366, 768), (1920, 1080), (2560, 1440), (3840, 2160)]
                {
                    let geom = TaskbarGeometry {
                        monitor_rect: RectI::new(0, 0, screen_w, screen_h),
                        work_rect: RectI::new(0, 0, screen_w, screen_h - taskbar_height),
                        taskbar_rect: RectI::new(0, screen_h - taskbar_height, screen_w, screen_h),
                        tray_rect: Some(RectI::new(
                            screen_w - 380,
                            screen_h - taskbar_height,
                            screen_w,
                            screen_h,
                        )),
                        edge: TaskbarEdge::Bottom,
                        dpi,
                        is_autohide: false,
                    };
                    let (max_w, max_h) = render_bounds(&geom);
                    let size = cfg
                        .layout
                        .render_size(294.0, 35.0, dpi, false, max_w, max_h);

                    let natural_w = (294.0 * dpi as f32 / 96.0).ceil() as u32;
                    if natural_w <= screen_w as u32 {
                        // Fitting only shrinks, never enlarges.
                        assert!(size.width_px <= natural_w + 1);
                    }
                    assert!(size.height_px <= taskbar_height as u32);

                    let (x, y) = compute_overlay_position(
                        &cfg,
                        &geom,
                        size.width_px as i32,
                        size.height_px as i32,
                    );
                    assert!(
                        y >= geom.taskbar_rect.top
                            && y + size.height_px as i32 <= geom.taskbar_rect.bottom,
                        "dpi {dpi} / taskbar {taskbar_height}: widget outside the taskbar"
                    );
                    assert!(x >= geom.monitor_rect.left);
                    assert!(x + size.width_px as i32 <= geom.monitor_rect.right);
                }
            }
        }
    }
}
