//! The underline of a hovered link, painted over a frame after the frame.

use super::TerminalPainter;
use super::decorations::underline_offset;
use super::grid::grid_rect;
use crate::terminal::cell_colors;
use gpui::*;
use herdr_client::protocol::{FrameData, PaneSurfacePane, SurfaceRect};
use std::ops::Range;

/// The cell rectangle covering every in-frame cell of `rows`, if any.
pub(super) fn link_bounds(frame: &FrameData, rows: &[(u16, Range<u16>)]) -> Option<SurfaceRect> {
    let mut cells = rows.iter().filter_map(|(row, columns)| {
        let end = columns.end.min(frame.width);
        (*row < frame.height && columns.start < end).then_some((*row, columns.start, end))
    });
    let (row, start, end) = cells.next()?;
    let (top, bottom, left, right) = cells.fold(
        (row, row, start, end),
        |(top, bottom, left, right), (row, start, end)| {
            (
                top.min(row),
                bottom.max(row),
                left.min(start),
                right.max(end),
            )
        },
    );
    Some(SurfaceRect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top + 1,
    })
}

impl TerminalPainter {
    /// Underlines the cells of a hovered link, painted after `paint_frame`
    /// for the same frame, panes, and origin. `rows` are in the program's
    /// logical columns; each cell is underlined where it is drawn. Each cell
    /// takes its own text color, as an SGR underline would, and spaces along
    /// the link are underlined too.
    pub fn paint_link(
        &mut self,
        frame: &FrameData,
        panes: &[PaneSurfacePane],
        origin: Point<Pixels>,
        cell_width: f32,
        rows: &[(u16, Range<u16>)],
        window: &mut Window,
    ) {
        let reordered = self
            .bidi
            .then(|| self.bidi_cache.frame(frame, panes))
            .flatten();
        let visual = |row: u16, column: u16| {
            reordered
                .as_ref()
                .map_or(column, |reordered| reordered.map.visual(row, column))
        };
        let drawn: Vec<(u16, Range<u16>)> = match &reordered {
            Some(reordered) => rows
                .iter()
                .flat_map(|(row, columns)| {
                    let columns = columns.start..columns.end.min(frame.width);
                    reordered
                        .map
                        .visual_ranges(*row, columns)
                        .into_iter()
                        .map(|columns| (*row, columns))
                })
                .collect(),
            None => rows.to_vec(),
        };
        let Some(bounds) = link_bounds(frame, &drawn) else {
            return;
        };
        let layer = Bounds::new(
            origin
                + point(
                    px(f32::from(bounds.x) * cell_width),
                    px(f32::from(bounds.y) * self.cell_height),
                ),
            size(
                px(f32::from(bounds.width) * cell_width),
                px(f32::from(bounds.height) * self.cell_height),
            ),
        );
        window.paint_layer(layer, |window| {
            for (row, columns) in rows {
                if *row >= frame.height {
                    continue;
                }
                for column in columns.start..columns.end.min(frame.width) {
                    let index = usize::from(*row) * usize::from(frame.width) + usize::from(column);
                    let Some(data) = frame.cells.get(index) else {
                        continue;
                    };
                    let shown = visual(*row, column);
                    // The same snapped corners as the dashes this line replaces.
                    let top =
                        f32::from(*row) * self.cell_height + underline_offset(self.cell_height);
                    window.paint_quad(fill(
                        grid_rect(
                            window,
                            origin,
                            point(px(f32::from(shown) * cell_width), px(top)),
                            point(px(f32::from(shown + 1) * cell_width), px(top + 1.)),
                        ),
                        rgb(cell_colors(data, &self.theme).0),
                    ));
                }
            }
        });
    }
}
