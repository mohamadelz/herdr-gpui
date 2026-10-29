//! Shaping and painting right-to-left runs. A run is shaped as one line so
//! the font joins its letters. Each style segment of it is then placed in
//! one of two ways:
//!
//! - When its shaped width is its cells' width, the face draws on the grid
//!   (a monospace face that fits the cell), so its glyphs keep their shaped
//!   positions from the segment's left edge, ligatures and joins included.
//! - Otherwise, as with a proportional fallback, each glyph cluster is
//!   snapped into the leftmost visual column of its cells. A cluster over
//!   several cells, such as lam-alef, leaves the others empty; they take a
//!   kashida when the cluster joins the letter on its right, whose stroke
//!   must then cross them.

use super::super::glyphs::{self, CACHE_LIMIT};
use super::super::{CellSeparator, Graphic, Span, TerminalPainter, covers};
use super::{Reordered, Run};
use crate::terminal::cell_colors;
use gpui::*;
use herdr_client::protocol::CellData;
use icu_properties::{CodePointMapData, props::JoiningType};

/// Shaped runs share the glyph cache's bound, but take at most this share of
/// it: scrolling right-to-left output makes a new run per line, and a cache
/// full of them would leave every other symbol shaped on each paint.
pub(in super::super) const RUN_CACHE_LIMIT: usize = CACHE_LIMIT / 4;

/// The tatweel, which draws only the joining stroke. It fills a cell that a
/// ligature leaves empty, so the stroke reaches the letter on its right.
const KASHIDA: &str = "\u{0640}";

fn joining(c: char) -> JoiningType {
    CodePointMapData::<JoiningType>::new().get(c)
}

/// Whether a shaped `width` is the width of `cells` grid cells: within half
/// a pixel a cell and 2% of the whole, whichever is tighter. A lam-alef that
/// draws as one glyph can bring a face wider than the cell close to the
/// whole width by chance; the 2% keeps that from passing.
pub(in super::super) fn fits_grid(width: f32, cells: usize, cell_width: f32) -> bool {
    let grid = cells as f32 * cell_width;
    (width - grid).abs() <= (0.5 * cells as f32).min(0.02 * grid)
}

/// Whether the letter at `byte` of `text` joins the letter before it, which
/// in right-to-left text is its right-hand neighbour. Marks between them are
/// transparent to joining.
pub(in super::super) fn joins_previous(text: &str, byte: usize) -> bool {
    let Some(first) = text.get(byte..).and_then(|rest| rest.chars().next()) else {
        return false;
    };
    let previous = text[..byte]
        .chars()
        .rev()
        .find(|&c| joining(c) != JoiningType::Transparent);
    matches!(
        joining(first),
        JoiningType::DualJoining | JoiningType::RightJoining | JoiningType::JoinCausing
    ) && previous.is_some_and(|c| {
        matches!(
            joining(c),
            JoiningType::DualJoining | JoiningType::LeftJoining | JoiningType::JoinCausing
        )
    })
}

/// The cells of each glyph cluster: a cell a glyph starts in and the cells
/// after it in logical order that no glyph starts in.
fn clusters(starts: &[bool]) -> impl Iterator<Item = std::ops::Range<usize>> + '_ {
    let mut cell = 0;
    std::iter::from_fn(move || {
        (cell < starts.len()).then(|| {
            let end = (cell + 1..starts.len())
                .find(|&next| starts[next])
                .unwrap_or(starts.len());
            let cluster = cell..end;
            cell = end;
            cluster
        })
    })
}

/// The visual column each cell's cluster is snapped into: the leftmost of
/// the cluster's columns. `cells` are a run's (byte, visual column) in
/// logical order, and `starts` marks the cells a glyph starts in.
pub(in super::super) fn cluster_columns(cells: &[(usize, u16)], starts: &[bool]) -> Vec<u16> {
    let mut columns = vec![0; cells.len()];
    for cluster in clusters(starts) {
        let leftmost = cells[cluster.clone()]
            .iter()
            .map(|&(_, column)| column)
            .min()
            .unwrap_or(0);
        columns[cluster].fill(leftmost);
    }
    columns
}

/// The cells a kashida fills when clusters are snapped, as (cluster's first
/// cell, visual column): the cells right of a cluster's own column when its
/// first letter joins the letter on its right.
pub(in super::super) fn kashida_fills(
    text: &str,
    cells: &[(usize, u16)],
    starts: &[bool],
) -> Vec<(usize, u16)> {
    let columns = cluster_columns(cells, starts);
    let mut fills = Vec::new();
    for cluster in clusters(starts) {
        if cluster.len() < 2
            || !starts[cluster.start]
            || !joins_previous(text, cells[cluster.start].0)
        {
            continue;
        }
        let placed = columns[cluster.start];
        fills.extend(
            cells[cluster.clone()]
                .iter()
                .filter(|&&(_, column)| column != placed)
                .map(|&(_, column)| (cluster.start, column)),
        );
    }
    fills
}

impl TerminalPainter {
    fn shape_run(&self, font: &Font, run: &Run, window: &Window) -> ShapedLine {
        let runs: Vec<TextRun> = run
            .styles
            .iter()
            .map(|&(len, style)| TextRun {
                len,
                font: glyphs::styled_font(font, style),
                color: Hsla::default(),
                background_color: None,
                underline: None,
                strikethrough: None,
            })
            .collect();
        window.text_system().shape_line(
            SharedString::from(run.text.clone()),
            px(self.font_size),
            &runs,
            None,
        )
    }

    /// Shapes each right-to-left run `area` covers as one line and paints its
    /// glyph clusters at their cells' visual columns, returning failures.
    #[allow(clippy::too_many_arguments)]
    pub(in super::super) fn paint_runs(
        &mut self,
        reordered: &Reordered,
        area: &[Span],
        origin: Point<Pixels>,
        cell_width: f32,
        font: &Font,
        cached: bool,
        in_bar: &impl Fn(usize) -> bool,
        shows_through: &impl Fn(&CellData) -> bool,
        window: &mut Window,
        #[cfg(feature = "integration-test")] counts: &mut crate::performance::Counts,
    ) -> u64 {
        let frame = &reordered.frame;
        let width = usize::from(frame.width);
        let mut paint_errors = 0;
        for run in &reordered.runs {
            if !run
                .cells
                .iter()
                .any(|&(_, column)| covers(area, column, run.row))
            {
                continue;
            }
            let newly_shaped;
            let line = match cached
                .then(|| self.glyphs.get_run(&run.styles, &run.text))
                .flatten()
            {
                Some(line) => line,
                None => {
                    #[cfg(feature = "integration-test")]
                    {
                        counts.shapes += 1;
                    }
                    let line = self.shape_run(font, run, window);
                    if cached && self.glyphs.has_room_for_run() {
                        self.glyphs.insert_run(&run.styles, &run.text, line)
                    } else {
                        newly_shaped = line;
                        &newly_shaped
                    }
                }
            };
            // A glyph belongs to the last cell starting at or before its
            // byte. A cell no glyph starts in joined the one before it, as a
            // ligature does, into one cluster over all their columns.
            let cell_of = |byte: usize| {
                run.cells
                    .partition_point(|&(start, _)| start <= byte)
                    .saturating_sub(1)
            };
            let mut starts = vec![false; run.cells.len()];
            let mut left = vec![f32::INFINITY; run.cells.len()];
            for glyph in line.runs.iter().flat_map(|shaped| &shaped.glyphs) {
                let cell = cell_of(glyph.index);
                starts[cell] = true;
                left[cell] = left[cell].min(f32::from(glyph.position.x));
            }
            // Clusters sit side by side in the shaped line, so one ends where
            // the next one to its right begins.
            let mut edges: Vec<f32> = left.iter().copied().filter(|x| x.is_finite()).collect();
            edges.sort_by(f32::total_cmp);
            let right_of = |x: f32| {
                edges
                    .iter()
                    .copied()
                    .find(|&edge| edge > x)
                    .unwrap_or(f32::from(line.width))
            };
            let snapped = cluster_columns(&run.cells, &starts);
            // Each glyph's column and its shift from that column's left edge:
            // per style segment, at its shaped place when the segment fits
            // the grid, else snapped into its cluster's leftmost cell.
            let mut offsets = vec![(0_u16, 0_f32); run.cells.len()];
            let mut fits = vec![false; run.cells.len()];
            let mut byte = 0;
            for &(length, _) in &run.styles {
                let segment = run.cells.partition_point(|&(start, _)| start < byte)
                    ..run
                        .cells
                        .partition_point(|&(start, _)| start < byte + length);
                byte += length;
                let shaped = segment.clone().filter(|&cell| starts[cell]);
                let lefts = shaped.clone().map(|cell| left[cell]);
                let start = lefts.clone().fold(f32::INFINITY, f32::min);
                let end = lefts.map(right_of).fold(f32::NEG_INFINITY, f32::max);
                let column = run.cells[segment.clone()]
                    .iter()
                    .map(|&(_, column)| column)
                    .min();
                let fit = column.is_some() && fits_grid(end - start, segment.len(), cell_width);
                for cell in segment {
                    fits[cell] = fit;
                    offsets[cell] = match column.filter(|_| fit) {
                        Some(column) => (column, -start),
                        None => (snapped[cell], -left[cell]),
                    };
                }
            }
            let baseline = (px(self.cell_height) - line.ascent - line.descent) / 2. + line.ascent;
            for shaped in &line.runs {
                for glyph in &shaped.glyphs {
                    let cell = cell_of(glyph.index);
                    let own = usize::from(run.row) * width + usize::from(run.cells[cell].1);
                    let Some(data) = frame.cells.get(own) else {
                        continue;
                    };
                    let (column, shift) = offsets[cell];
                    if !covers(area, snapped[cell], run.row)
                        || data.symbol.trim().is_empty()
                        || shows_through(data)
                        || in_bar(own)
                        || Graphic::from_symbol(&data.symbol).is_some()
                        || CellSeparator::from_symbol(&data.symbol).is_some()
                    {
                        continue;
                    }
                    let position = origin
                        + point(
                            px(f32::from(column) * cell_width) + glyph.position.x + px(shift),
                            px(f32::from(run.row) * self.cell_height) + baseline,
                        );
                    let result = if glyph.is_emoji {
                        window.paint_emoji(position, shaped.font_id, glyph.id, line.font_size)
                    } else {
                        let color = rgb(cell_colors(data, &self.theme).0);
                        window.paint_glyph(
                            position,
                            shaped.font_id,
                            glyph.id,
                            line.font_size,
                            color.into(),
                        )
                    };
                    paint_errors += u64::from(result.is_err());
                    #[cfg(feature = "integration-test")]
                    {
                        counts.glyphs += 1;
                        counts.paint_errors += usize::from(result.is_err());
                    }
                }
            }
            for (cell, column) in kashida_fills(&run.text, &run.cells, &starts)
                .into_iter()
                .filter(|&(cell, _)| !fits[cell])
            {
                let own = usize::from(run.row) * width + usize::from(run.cells[cell].1);
                let Some(data) = frame.cells.get(own) else {
                    continue;
                };
                if !covers(area, column, run.row) || shows_through(data) || in_bar(own) {
                    continue;
                }
                // Shaped alone, the kashida is one more cached symbol in the
                // cluster's face, and takes the cluster's color.
                let style = glyphs::style(data.modifier);
                let newly_shaped;
                let kashida = match cached.then(|| self.glyphs.get(style, KASHIDA)).flatten() {
                    Some(line) => line,
                    None => {
                        #[cfg(feature = "integration-test")]
                        {
                            counts.shapes += 1;
                        }
                        let line = self.shape(font, style, KASHIDA, window);
                        if cached && self.glyphs.has_room() {
                            self.glyphs.insert(style, KASHIDA, line)
                        } else {
                            newly_shaped = line;
                            &newly_shaped
                        }
                    }
                };
                let position = origin
                    + point(
                        px(f32::from(column) * cell_width),
                        px(f32::from(run.row) * self.cell_height),
                    );
                let color = rgb(cell_colors(data, &self.theme).0);
                let result =
                    glyphs::paint_glyphs(kashida, position, px(self.cell_height), color, window);
                paint_errors += u64::from(result.is_err());
                #[cfg(feature = "integration-test")]
                {
                    counts.glyphs += kashida.runs.iter().map(|r| r.glyphs.len()).sum::<usize>();
                    counts.paint_errors += usize::from(result.is_err());
                }
            }
        }
        paint_errors
    }
}
