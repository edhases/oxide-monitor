//! Placement of the configured items inside the overlay.

use oxide_core::{Config, WidgetRenderSize};
use windows::Win32::Foundation::RECT;

const CELL_PAD_X: i32 = 2;
const CELL_PAD_Y: i32 = 1;

#[derive(Clone, Copy, Debug)]
pub struct Placement {
    /// Index into the list of visible items.
    pub visible_index: usize,
    /// Content rectangle in the overlay's client coordinates.
    pub cell: RECT,
}

/// Legacy 100 % overlay dimensions in physical pixels (no DPI/scale/fit applied).
pub fn overlay_size(config: &Config) -> (i32, i32) {
    (config.taskbar.width.max(1), config.taskbar.height.max(1))
}

/// Physical size + effective DPI for the GDI fallback, using the same sizing model as
/// the Direct2D renderer (`scale_pct`, `fit_to_taskbar`, taskbar work-area bounds).
pub fn render_metrics_for(
    config: &Config,
    dpi: u32,
    vertical_taskbar: bool,
    max_width_px: Option<u32>,
    max_height_px: Option<u32>,
) -> WidgetRenderSize {
    let (width, height) = overlay_size(config);
    config.layout.render_size(
        width as f32,
        height as f32,
        dpi,
        vertical_taskbar,
        max_width_px,
        max_height_px,
    )
}

pub fn overlay_size_for(
    config: &Config,
    dpi: u32,
    vertical_taskbar: bool,
    max_width_px: Option<u32>,
    max_height_px: Option<u32>,
) -> (i32, i32) {
    let size = render_metrics_for(config, dpi, vertical_taskbar, max_width_px, max_height_px);
    (size.width_px.max(1) as i32, size.height_px.max(1) as i32)
}

/// Compute item cells using the TrafficMonitor-compatible order.
///
/// With `rows = 2` the item index is `row + column * rows`, which gives
/// `0, 2, 4, 6` for the first row and `1, 3, 5, 7` for the second one.
pub fn placements(config: &Config, item_count: usize, width: i32, height: i32) -> Vec<Placement> {
    if item_count == 0 {
        return Vec::new();
    }

    let width = width.max(1);
    let height = height.max(1);
    let rows = config.taskbar.rows.max(1).min(item_count);
    let columns = item_count.div_ceil(rows);
    let gap = config.taskbar.item_space.max(0);
    let total_gap = gap.saturating_mul(columns.saturating_sub(1) as i32);
    let available = (width - total_gap).max(columns as i32);
    let column_width = (available / columns as i32).max(1);

    let mut result = Vec::with_capacity(item_count);
    for row in 0..rows {
        let top = (height * row as i32) / rows as i32;
        let bottom = (height * (row as i32 + 1)) / rows as i32;
        for column in 0..columns {
            let visible_index = row + column * rows;
            if visible_index >= item_count {
                continue;
            }
            let left = column as i32 * (column_width + gap);
            let right = if column + 1 == columns {
                width
            } else {
                (left + column_width).min(width)
            };
            if right <= left || bottom <= top {
                continue;
            }
            let cell = RECT {
                left: left + CELL_PAD_X,
                top: top + CELL_PAD_Y,
                right: right - CELL_PAD_X,
                bottom: bottom - CELL_PAD_Y,
            };
            if cell.right <= cell.left || cell.bottom <= cell.top {
                continue;
            }
            result.push(Placement {
                visible_index,
                cell,
            });
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_layout_is_column_major() {
        let config = Config::default();
        let (width, height) = overlay_size(&config);
        let order: Vec<usize> = placements(&config, 8, width, height)
            .iter()
            .map(|placement| placement.visible_index)
            .collect();
        assert_eq!(order, vec![0, 2, 4, 6, 1, 3, 5, 7]);
    }

    #[test]
    fn scaling_uses_the_shared_sizing_model() {
        let mut config = Config::default();
        config.layout.size_mode = Some(oxide_core::SizeMode::ExactPhysical);
        config.layout.scale_pct = 150;
        let (w, h) = overlay_size_for(&config, 144, false, None, None);
        assert_eq!((w, h), (441, 53));
    }

    #[test]
    fn fit_never_exceeds_the_taskbar_height() {
        let mut config = Config::default();
        config.layout.size_mode = Some(oxide_core::SizeMode::ExactPhysical);
        config.layout.scale_pct = 200;
        let (w, h) = overlay_size_for(&config, 96, false, None, Some(48));
        assert!(h <= 48, "height {h} exceeds the taskbar");
        assert!(w < 588, "width must shrink proportionally, got {w}");
        let metrics = render_metrics_for(&config, 96, false, None, Some(48));
        assert!(metrics.dpi < 192.0);
    }
}
