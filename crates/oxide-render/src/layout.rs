//! Platform-independent `LayoutEngine v2` in logical DIP coordinates.
//!
//! Supports:
//! * `FlowRows` (1–4 rows, column-major / row-major, fit-content / equal columns, auto / fixed width)
//! * `Grid` (arbitrary `row`, `col`, `row_span`, `col_span` matrix)
//! * Vertical taskbar auto-adaptation (`vertical_taskbar = true`)
//! * Visual `GroupBox` pills and separators around contiguous group columns

use oxide_core::{
    ColumnSizing, Config, DisplayItemModel, FillOrder, GroupStyle, ItemVisual, LayoutGroupConfig,
    LayoutMode, Rgba, ThemePalette, WidthMode,
};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RectF {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl RectF {
    pub const fn new(left: f32, top: f32, right: f32, bottom: f32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub fn width(self) -> f32 {
        (self.right - self.left).max(0.0)
    }

    pub fn height(self) -> f32 {
        (self.bottom - self.top).max(0.0)
    }

    pub fn inset(self, x: f32, y: f32) -> Self {
        let left = self.left + x;
        let top = self.top + y;
        let right = (self.right - x).max(left);
        let bottom = (self.bottom - y).max(top);
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub fn union(self, other: Self) -> Self {
        Self {
            left: self.left.min(other.left),
            top: self.top.min(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.max(other.bottom),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellPlacement {
    pub item_index: usize,
    pub rect: RectF,
    pub row: usize,
    pub col: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GroupBox {
    pub group_id: String,
    pub rect: RectF,
    pub style: GroupStyle,
    pub pill_bg: Rgba,
    pub pill_radius: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct WidgetLayout {
    pub width_dip: f32,
    pub height_dip: f32,
    pub cells: Vec<CellPlacement>,
    pub groups: Vec<GroupBox>,
}

pub struct LayoutEngine;

impl LayoutEngine {
    /// Compute the full widget geometry in DIPs (96 DPI baseline).
    ///
    /// `measure_item` returns the desired content width (in DIPs) for a single item,
    /// including its label, stable value slot, unit, and any inline sparkline/progress.
    pub fn compute<F>(
        config: &Config,
        items: &[DisplayItemModel],
        palette: &ThemePalette,
        vertical_taskbar: bool,
        mut measure_item: F,
    ) -> WidgetLayout
    where
        F: FnMut(&DisplayItemModel) -> f32,
    {
        if items.is_empty() {
            let w = match config.layout.width_mode {
                WidthMode::Fixed(px) => px.max(80) as f32,
                WidthMode::Auto => 120.0,
            };
            let h = config.layout.height.max(24) as f32;
            return WidgetLayout {
                width_dip: w,
                height_dip: h,
                cells: Vec::new(),
                groups: Vec::new(),
            };
        }

        let desired_widths: Vec<f32> = items
            .iter()
            .map(|item| {
                if item.is_separator {
                    6.0
                } else {
                    measure_item(item).max(18.0)
                }
            })
            .collect();

        if vertical_taskbar && config.layout.vertical_auto_adapt {
            return compute_vertical(config, items, &desired_widths, palette);
        }

        match config.layout.mode {
            LayoutMode::Grid => compute_grid(config, items, &desired_widths, palette),
            LayoutMode::FlowRows | LayoutMode::AbsoluteSkin => {
                compute_flow_rows(config, items, &desired_widths, palette)
            }
        }
    }
}

fn compute_vertical(
    config: &Config,
    items: &[DisplayItemModel],
    desired_widths: &[f32],
    palette: &ThemePalette,
) -> WidgetLayout {
    let pad = config.surface.padding;
    let cell_pad_x = config.surface.cell_padding.x as f32;
    let cell_pad_y = config.surface.cell_padding.y as f32;
    let gap_y = config.layout.gap_y.max(1) as f32;

    let max_item_w = desired_widths.iter().copied().fold(48.0_f32, f32::max) + cell_pad_x * 2.0;
    // Vertical taskbars are narrow (typically 62–100 DIP), so ignore wide horizontal fixed widths.
    let width_dip = (max_item_w + (pad.left + pad.right) as f32).clamp(60.0, 110.0);

    let row_h = (config.typography.value.size_pt * 1.85).max(16.0);
    let inner_w = (width_dip - (pad.left + pad.right) as f32).max(20.0);

    let mut cells = Vec::with_capacity(items.len());
    let mut y = pad.top as f32;
    for (idx, item) in items.iter().enumerate() {
        let h = if item.is_separator {
            4.0
        } else if matches!(item.visual, ItemVisual::GraphOnly { .. }) {
            row_h * 1.8
        } else {
            row_h
        };
        let cell = RectF::new(pad.left as f32, y, pad.left as f32 + inner_w, y + h)
            .inset(cell_pad_x, cell_pad_y);
        cells.push(CellPlacement {
            item_index: idx,
            rect: cell,
            row: idx,
            col: 0,
        });
        y += h + gap_y;
    }

    let height_dip = (y - gap_y + pad.bottom as f32).max(24.0);
    let groups = build_group_boxes(config, items, &cells, palette);

    WidgetLayout {
        width_dip,
        height_dip,
        cells,
        groups,
    }
}

fn compute_flow_rows(
    config: &Config,
    items: &[DisplayItemModel],
    desired_widths: &[f32],
    palette: &ThemePalette,
) -> WidgetLayout {
    let count = items.len();
    let rows = config.layout.rows.max(1).min(count);
    let cols = count.div_ceil(rows);
    let pad = config.surface.padding;
    let cell_pad_x = config.surface.cell_padding.x as f32;
    let cell_pad_y = config.surface.cell_padding.y as f32;
    let gap_x = config.layout.gap_x.max(0) as f32;
    let gap_y = config.layout.gap_y.max(0) as f32;

    // Assign each item to `(row, col)`.
    let mut coords: Vec<(usize, usize)> = Vec::with_capacity(count);
    for idx in 0..count {
        let (row, col) = match config.layout.fill_order {
            FillOrder::ColumnMajor => (idx % rows, idx / rows),
            FillOrder::RowMajor => (idx / cols, idx % cols),
        };
        coords.push((row, col));
    }

    // Extra padding when adjacent columns belong to different pill groups.
    let col_group: Vec<Option<&str>> = (0..cols)
        .map(|c| {
            coords
                .iter()
                .enumerate()
                .find(|(_, (_, col))| *col == c)
                .and_then(|(idx, _)| items[idx].group.as_deref())
        })
        .collect();

    let mut col_widths = vec![0.0_f32; cols];
    for (idx, &(_, col)) in coords.iter().enumerate() {
        let needed = desired_widths[idx] + cell_pad_x * 2.0;
        col_widths[col] = col_widths[col].max(needed);
    }

    // Account for pill padding around group boundaries.
    let mut col_extra_left = vec![0.0_f32; cols];
    let mut col_extra_right = vec![0.0_f32; cols];
    for c in 0..cols {
        if let Some(gid) = col_group[c]
            && let Some(gcfg) = find_group(config, gid)
            && gcfg.style == GroupStyle::Pill
        {
            let prev_same = c > 0 && col_group[c - 1] == Some(gid);
            let next_same = c + 1 < cols && col_group[c + 1] == Some(gid);
            if !prev_same {
                col_extra_left[c] = gcfg.pill_padding_x as f32;
            }
            if !next_same {
                col_extra_right[c] = gcfg.pill_padding_x as f32;
            }
        }
    }

    let total_gaps_x = gap_x * (cols.saturating_sub(1) as f32);
    let total_pill_extras: f32 = col_extra_left.iter().chain(col_extra_right.iter()).sum();
    let horiz_pad = (pad.left + pad.right) as f32;

    let (width_dip, final_col_widths) = match config.layout.width_mode {
        WidthMode::Fixed(fixed_w) => {
            let width_dip = fixed_w.max(80) as f32;
            let available =
                (width_dip - horiz_pad - total_gaps_x - total_pill_extras).max(cols as f32);
            let widths = match config.layout.column_sizing {
                ColumnSizing::Equal => vec![available / cols as f32; cols],
                ColumnSizing::FitContent => {
                    let sum: f32 = col_widths.iter().sum::<f32>().max(1.0);
                    col_widths.iter().map(|w| w * (available / sum)).collect()
                }
            };
            (width_dip, widths)
        }
        WidthMode::Auto => {
            let widths = match config.layout.column_sizing {
                ColumnSizing::Equal => {
                    let max_col = col_widths.iter().copied().fold(24.0_f32, f32::max);
                    vec![max_col; cols]
                }
                ColumnSizing::FitContent => col_widths,
            };
            let raw_content: f32 = widths.iter().sum();
            let desired_total = horiz_pad + total_gaps_x + total_pill_extras + raw_content;
            let max_w = config.layout.max_width.max(120) as f32;
            if desired_total <= max_w {
                (desired_total.ceil(), widths)
            } else {
                let available =
                    (max_w - horiz_pad - total_gaps_x - total_pill_extras).max(cols as f32);
                let scale = available / raw_content.max(1.0);
                (max_w, widths.into_iter().map(|w| w * scale).collect())
            }
        }
    };

    let height_dip = if config.layout.height > 0 {
        config.layout.height as f32
    } else {
        let row_h = (config.typography.value.size_pt * 1.75).max(15.0);
        (pad.top + pad.bottom) as f32
            + row_h * rows as f32
            + gap_y * (rows.saturating_sub(1) as f32)
    };

    let inner_h =
        (height_dip - (pad.top + pad.bottom) as f32 - gap_y * (rows.saturating_sub(1) as f32))
            .max(rows as f32);
    let row_height = inner_h / rows as f32;

    let mut col_x = Vec::with_capacity(cols);
    let mut cursor_x = pad.left as f32;
    for c in 0..cols {
        cursor_x += col_extra_left[c];
        col_x.push(cursor_x);
        cursor_x += final_col_widths[c] + col_extra_right[c] + gap_x;
    }

    let mut cells = Vec::with_capacity(count);
    for (idx, &(row, col)) in coords.iter().enumerate() {
        let left = col_x[col];
        let right = left + final_col_widths[col];
        let top = pad.top as f32 + row as f32 * (row_height + gap_y);
        let bottom = top + row_height;
        let rect = RectF::new(left, top, right, bottom).inset(cell_pad_x, cell_pad_y);
        cells.push(CellPlacement {
            item_index: idx,
            rect,
            row,
            col,
        });
    }

    let groups = build_group_boxes(config, items, &cells, palette);

    WidgetLayout {
        width_dip,
        height_dip,
        cells,
        groups,
    }
}

fn compute_grid(
    config: &Config,
    items: &[DisplayItemModel],
    desired_widths: &[f32],
    palette: &ThemePalette,
) -> WidgetLayout {
    let pad = config.surface.padding;
    let cell_pad_x = config.surface.cell_padding.x as f32;
    let cell_pad_y = config.surface.cell_padding.y as f32;
    let gap_x = config.layout.gap_x.max(0) as f32;
    let gap_y = config.layout.gap_y.max(0) as f32;

    // Derive grid slots (items without explicit `grid` auto-flow after explicit slots).
    let default_rows = config.layout.rows.max(1);
    let mut slots: Vec<(usize, usize, usize, usize)> = Vec::with_capacity(items.len());
    let mut max_row = default_rows;
    let mut max_col = 1usize;

    for (idx, item) in items.iter().enumerate() {
        let (r, c, rs, cs) = if let Some(g) = item.grid {
            (
                g.row as usize,
                g.col as usize,
                g.row_span.max(1) as usize,
                g.col_span.max(1) as usize,
            )
        } else {
            (idx % default_rows, idx / default_rows, 1, 1)
        };
        max_row = max_row.max(r + rs);
        max_col = max_col.max(c + cs);
        slots.push((r, c, rs, cs));
    }

    // Measure column widths from single-column items first, then expand for multi-column spans.
    let mut col_widths = vec![24.0_f32; max_col];
    for (idx, &(_, col, _, cs)) in slots.iter().enumerate() {
        let needed = desired_widths[idx] + cell_pad_x * 2.0;
        if cs == 1 {
            col_widths[col] = col_widths[col].max(needed);
        } else {
            let current_span: f32 = col_widths[col..col + cs].iter().sum::<f32>()
                + gap_x * (cs.saturating_sub(1) as f32);
            if needed > current_span {
                let extra = (needed - current_span) / cs as f32;
                for w in &mut col_widths[col..col + cs] {
                    *w += extra;
                }
            }
        }
    }

    let horiz_pad = (pad.left + pad.right) as f32;
    let total_gaps_x = gap_x * (max_col.saturating_sub(1) as f32);
    let raw_sum: f32 = col_widths.iter().sum();

    let (width_dip, final_cols) = match config.layout.width_mode {
        WidthMode::Fixed(fixed_w) => {
            let w = fixed_w.max(80) as f32;
            let available = (w - horiz_pad - total_gaps_x).max(max_col as f32);
            let scale = available / raw_sum.max(1.0);
            (w, col_widths.into_iter().map(|c| c * scale).collect())
        }
        WidthMode::Auto => {
            let desired = horiz_pad + total_gaps_x + raw_sum;
            let max_w = config.layout.max_width.max(120) as f32;
            if desired <= max_w {
                (desired.ceil(), col_widths)
            } else {
                let available = (max_w - horiz_pad - total_gaps_x).max(max_col as f32);
                let scale = available / raw_sum.max(1.0);
                (max_w, col_widths.into_iter().map(|c| c * scale).collect())
            }
        }
    };

    let height_dip = if config.layout.height > 0 {
        config.layout.height as f32
    } else {
        let row_h = (config.typography.value.size_pt * 1.8).max(16.0);
        (pad.top + pad.bottom) as f32
            + row_h * max_row as f32
            + gap_y * (max_row.saturating_sub(1) as f32)
    };

    let inner_h =
        (height_dip - (pad.top + pad.bottom) as f32 - gap_y * (max_row.saturating_sub(1) as f32))
            .max(max_row as f32);
    let row_h = inner_h / max_row as f32;

    let mut col_x = Vec::with_capacity(max_col);
    let mut x = pad.left as f32;
    for &w in &final_cols {
        col_x.push(x);
        x += w + gap_x;
    }

    let mut cells = Vec::with_capacity(items.len());
    for (idx, &(r, c, rs, cs)) in slots.iter().enumerate() {
        let left = col_x[c];
        let right = col_x[c + cs - 1] + final_cols[c + cs - 1];
        let top = pad.top as f32 + r as f32 * (row_h + gap_y);
        let bottom = top + rs as f32 * row_h + (rs.saturating_sub(1) as f32) * gap_y;
        let rect = RectF::new(left, top, right, bottom).inset(cell_pad_x, cell_pad_y);
        cells.push(CellPlacement {
            item_index: idx,
            rect,
            row: r,
            col: c,
        });
    }

    let groups = build_group_boxes(config, items, &cells, palette);

    WidgetLayout {
        width_dip,
        height_dip,
        cells,
        groups,
    }
}

fn find_group<'a>(config: &'a Config, id: &str) -> Option<&'a LayoutGroupConfig> {
    config.layout.groups.iter().find(|g| g.id == id)
}

fn build_group_boxes(
    config: &Config,
    items: &[DisplayItemModel],
    cells: &[CellPlacement],
    palette: &ThemePalette,
) -> Vec<GroupBox> {
    if config.layout.groups.is_empty() {
        return Vec::new();
    }
    let mut result = Vec::new();
    for gcfg in &config.layout.groups {
        if gcfg.style == GroupStyle::None {
            continue;
        }
        let mut bounds: Option<RectF> = None;
        for cell in cells {
            if items[cell.item_index].group.as_deref() == Some(gcfg.id.as_str()) {
                bounds = Some(match bounds {
                    Some(existing) => existing.union(cell.rect),
                    None => cell.rect,
                });
            }
        }
        if let Some(rect) = bounds {
            let pad_x = if gcfg.style == GroupStyle::Pill {
                gcfg.pill_padding_x as f32
            } else {
                2.0
            };
            let padded = RectF::new(
                (rect.left - pad_x).max(1.0),
                (rect.top - 1.0).max(1.0),
                rect.right + pad_x,
                rect.bottom + 1.0,
            );
            result.push(GroupBox {
                group_id: gcfg.id.clone(),
                rect: padded,
                style: gcfg.style,
                pill_bg: gcfg.pill_bg.resolve(palette),
                pill_radius: gcfg.pill_radius,
            });
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxide_core::{
        DisplayModelBuilder, ExtraTelemetry, HistoryStore, Snapshot, find_builtin_preset,
    };

    fn mock_measure(item: &DisplayItemModel) -> f32 {
        let chars =
            item.label.chars().count() + item.value.chars().count() + item.unit.chars().count() + 2;
        match &item.visual {
            ItemVisual::GraphOnly { width, .. } => *width,
            ItemVisual::TextSparkline {
                placement:
                    oxide_core::SparklinePlacement::InlineLeft
                    | oxide_core::SparklinePlacement::InlineRight,
                graph_width,
                ..
            } => chars as f32 * 6.0 + *graph_width + 4.0,
            ItemVisual::TextProgress {
                style: oxide_core::ProgressStyle::BarInline | oxide_core::ProgressStyle::RadialRing,
                bar_width,
                ..
            } => chars as f32 * 6.0 + *bar_width + 4.0,
            _ => chars as f32 * 6.0,
        }
    }

    #[test]
    fn classic_preset_places_eight_cells_in_two_rows_column_major() {
        let cfg = Config::default();
        let snap = Snapshot::default();
        let history = HistoryStore::new();
        let palette = ThemePalette::dark();
        let model =
            DisplayModelBuilder::build(&cfg, &snap, &history, &palette, &ExtraTelemetry::default());
        let layout = LayoutEngine::compute(&cfg, &model.items, &palette, false, mock_measure);

        assert_eq!(layout.width_dip, 294.0);
        assert_eq!(layout.height_dip, 35.0);
        assert_eq!(layout.cells.len(), 8);
        // Column-major: item 0 at (row 0, col 0), item 1 at (row 1, col 0), item 2 at (row 0, col 1).
        assert_eq!((layout.cells[0].row, layout.cells[0].col), (0, 0));
        assert_eq!((layout.cells[1].row, layout.cells[1].col), (1, 0));
        assert_eq!((layout.cells[2].row, layout.cells[2].col), (0, 1));
    }

    #[test]
    fn gamer_grid_preset_spans_two_rows_for_net_graph() {
        let mut cfg = Config::default();
        find_builtin_preset("gamer-sparklines")
            .unwrap()
            .apply_to(&mut cfg);
        let snap = Snapshot::default();
        let history = HistoryStore::new();
        let palette = ThemePalette::dark();
        let model =
            DisplayModelBuilder::build(&cfg, &snap, &history, &palette, &ExtraTelemetry::default());
        let layout = LayoutEngine::compute(&cfg, &model.items, &palette, false, mock_measure);

        assert_eq!(layout.cells.len(), 7);
        // Cell 0 is the 2-row spanning net graph; it must be taller than single-row cell 1.
        assert!(layout.cells[0].rect.height() > layout.cells[1].rect.height() * 1.5);
    }

    #[test]
    fn fluent_preset_creates_three_pill_group_boxes() {
        let mut cfg = Config::default();
        find_builtin_preset("win11-fluent")
            .unwrap()
            .apply_to(&mut cfg);
        let snap = Snapshot::default();
        let history = HistoryStore::new();
        let palette = ThemePalette::dark();
        let model =
            DisplayModelBuilder::build(&cfg, &snap, &history, &palette, &ExtraTelemetry::default());
        let layout = LayoutEngine::compute(&cfg, &model.items, &palette, false, mock_measure);

        assert_eq!(layout.groups.len(), 3);
        assert!(layout.groups.iter().all(|g| g.style == GroupStyle::Pill));
    }

    #[test]
    fn vertical_taskbar_auto_adapts_into_single_column_stack() {
        let cfg = Config::default();
        let snap = Snapshot::default();
        let history = HistoryStore::new();
        let palette = ThemePalette::dark();
        let model =
            DisplayModelBuilder::build(&cfg, &snap, &history, &palette, &ExtraTelemetry::default());
        let layout = LayoutEngine::compute(&cfg, &model.items, &palette, true, mock_measure);

        assert!(layout.height_dip > layout.width_dip);
        assert!(layout.cells.iter().all(|c| c.col == 0));
    }
}
