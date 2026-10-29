//! Right-to-left text in the grid, behind `[terminal] bidi`.
//!
//! Herdr keeps every row in logical order, one code point per cell, so Arabic
//! and Hebrew would read backwards with every letter in its isolated form.
//! With bidi on, each pane row holding a strong right-to-left character is
//! reordered by the Unicode Bidirectional Algorithm at a left-to-right
//! paragraph level, as Terminal.app and WezTerm do. The painter then draws a
//! copy of the frame whose cells sit at their visual columns, so backgrounds,
//! box graphics, and decorations follow the text. Right-to-left runs are
//! shaped whole so the font joins their letters (see `runs`).
//!
//! The program, the daemon, and the selection all keep logical columns. The
//! [`ColumnMap`] of a frame is the one translation between the two: overlays
//! paint at a cell's visual column, and the pointer's visual column is read
//! back as a logical one before anything else sees it.

mod runs;

pub(super) use self::runs::RUN_CACHE_LIMIT;
#[cfg(test)]
pub(super) use self::runs::{cluster_columns, fits_grid, joins_previous, kashida_fills};
use super::{Highlight, TerminalPainter, glyphs};
use herdr_client::protocol::{CellData, FrameData, PaneSurfacePane};
use std::{collections::HashMap, ops::Range, rc::Rc};
use unicode_bidi::{BidiClass, BidiInfo, Level, bidi_class};

/// Reordered segments kept between paints. A screen of right-to-left output
/// is about a hundred; scrolling replaces them, so a full cache restarts.
const LAYOUT_CACHE_LIMIT: usize = 512;
/// Reordered frames kept between paints: the main grid, a popup, and the
/// frames of a group, each painted up to once per layer.
const FRAME_CACHE_LIMIT: usize = 4;

/// One segment of cells reordered: where each cell goes, and its
/// right-to-left runs.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Layout {
    /// The visual offset of each logical cell in the segment.
    pub(super) visual: Vec<usize>,
    /// The logical offset of each visual cell, the inverse of `visual`.
    pub(super) logical: Vec<usize>,
    /// Whether each logical cell is at a right-to-left level, where the
    /// columns run backwards.
    pub(super) rtl: Vec<bool>,
    /// Each run's text in logical order, and its cells that start a glyph:
    /// their first byte in that text and their logical offset.
    pub(super) runs: Vec<(String, Vec<(usize, usize)>)>,
}

/// The reordered segments of a frame, by row: logical to visual columns and
/// back. Columns outside them map to themselves.
#[derive(Debug, Default)]
pub(crate) struct ColumnMap {
    /// Row, first column, and layout, sorted by row and column.
    segments: Vec<(u16, u16, Rc<Layout>)>,
}

impl ColumnMap {
    /// The segment holding `column` of `row`, and the column's offset in it.
    fn find(&self, row: u16, column: u16) -> Option<(&Layout, usize)> {
        self.segments.iter().find_map(|(at, start, layout)| {
            let offset = usize::from(column.checked_sub(*start)?);
            (*at == row && offset < layout.visual.len()).then_some((&**layout, offset))
        })
    }

    /// Where the logical cell at `column` of `row` is drawn.
    pub(crate) fn visual(&self, row: u16, column: u16) -> u16 {
        self.find(row, column).map_or(column, |(layout, offset)| {
            column - offset as u16 + layout.visual[offset] as u16
        })
    }

    /// A pointer `column` (in cells, with its fraction) read in logical
    /// columns. Inside a right-to-left cell the fraction turns round too, so
    /// the half nearer the cell's logical start stays the left side.
    pub(crate) fn logical_position(&self, row: u16, column: f32) -> f32 {
        if !(0. ..f32::from(u16::MAX)).contains(&column) {
            return column;
        }
        let cell = column.floor() as u16;
        let Some((layout, offset)) = self.find(row, cell) else {
            return column;
        };
        let logical = layout.logical[offset];
        let fraction = column.fract();
        let fraction = if layout.rtl[logical] {
            1. - fraction
        } else {
            fraction
        };
        f32::from(cell - offset as u16 + logical as u16) + fraction
    }

    /// The visual column ranges that logical `columns` of `row` cover,
    /// sorted. One logical range across a direction change is several.
    pub(crate) fn visual_ranges(&self, row: u16, columns: Range<u16>) -> Vec<Range<u16>> {
        let reordered = self.segments.iter().any(|(at, start, layout)| {
            *at == row
                && columns.start < start.saturating_add(layout.visual.len() as u16)
                && *start < columns.end
        });
        if !reordered {
            return vec![columns];
        }
        let mut visual: Vec<u16> = columns.map(|column| self.visual(row, column)).collect();
        visual.sort_unstable();
        let mut ranges: Vec<Range<u16>> = Vec::new();
        for column in visual {
            match ranges.last_mut() {
                Some(range) if range.end == column => range.end += 1,
                _ => ranges.push(column..column + 1),
            }
        }
        ranges
    }

    /// `highlights` in visual columns, split where a range is.
    pub(crate) fn highlights(&self, highlights: &[Highlight]) -> Vec<Highlight> {
        highlights
            .iter()
            .flat_map(|highlight| {
                self.visual_ranges(highlight.row, highlight.columns.clone())
                    .into_iter()
                    .map(|columns| Highlight {
                        columns,
                        ..highlight.clone()
                    })
            })
            .collect()
    }
}

/// The exact input of [`reorder`]: each cell's length in `text`, or `None`
/// for a continuation cell.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct SegmentKey {
    text: String,
    cells: Vec<Option<u32>>,
}

impl SegmentKey {
    fn new(cells: &[CellData]) -> Self {
        let mut text = String::new();
        let lengths = cells
            .iter()
            .map(|cell| {
                (!cell.skip).then(|| {
                    let symbol = cell_text(&cell.symbol);
                    text.push_str(symbol);
                    u32::try_from(symbol.len()).unwrap_or(u32::MAX)
                })
            })
            .collect();
        Self {
            text,
            cells: lengths,
        }
    }
}

/// Bounded caches of reordering, so a frame that does not change is not
/// reordered again on every paint, or on each layer of a split region.
#[derive(Default)]
pub(super) struct Cache {
    layouts: HashMap<SegmentKey, Rc<Layout>>,
    /// Most recent first.
    frames: Vec<Rc<Reordered>>,
    /// Segments reordered rather than found, for tests and measurement.
    pub(super) computed: usize,
}

impl Cache {
    /// The layout of a segment holding right-to-left text.
    fn layout(&mut self, cells: &[CellData]) -> Option<Rc<Layout>> {
        let key = SegmentKey::new(cells);
        if let Some(layout) = self.layouts.get(&key) {
            return Some(layout.clone());
        }
        self.computed += 1;
        let layout = Rc::new(reorder(cells)?);
        if self.layouts.len() >= LAYOUT_CACHE_LIMIT {
            self.layouts.clear();
        }
        self.layouts.insert(key, layout.clone());
        Some(layout)
    }

    /// `frame` in visual order, or `None` when no row holds right-to-left
    /// text and it paints as it is.
    pub(super) fn frame(
        &mut self,
        frame: &FrameData,
        panes: &[PaneSurfacePane],
    ) -> Option<Rc<Reordered>> {
        if !frame.cells.iter().any(|cell| strong_rtl(&cell.symbol)) {
            return None;
        }
        let segments = frame_segments(frame, panes);
        if let Some(found) = self
            .frames
            .iter()
            .position(|reordered| reordered.is_of(frame, &segments))
        {
            let reordered = self.frames.remove(found);
            self.frames.insert(0, reordered.clone());
            return Some(reordered);
        }
        let reordered = Rc::new(self.reorder_frame(frame, segments));
        self.frames.truncate(FRAME_CACHE_LIMIT - 1);
        self.frames.insert(0, reordered.clone());
        Some(reordered)
    }

    /// The map of the one row `row` of `frame`, for the pointer and the input
    /// cursor, without reordering the rest of the frame.
    pub(super) fn row_map(
        &mut self,
        frame: &FrameData,
        panes: &[PaneSurfacePane],
        row: u16,
    ) -> ColumnMap {
        let mut map = ColumnMap::default();
        if row >= frame.height {
            return map;
        }
        for columns in segments(frame, panes, row) {
            let Some(cells) = segment_cells(frame, row, &columns) else {
                continue;
            };
            if !cells.iter().any(|cell| strong_rtl(&cell.symbol)) {
                continue;
            }
            if let Some(layout) = self.layout(cells) {
                map.segments.push((row, columns.start, layout));
            }
        }
        map
    }

    fn reorder_frame(&mut self, frame: &FrameData, segments: Vec<(u16, Range<u16>)>) -> Reordered {
        let mut reordered = Reordered {
            frame: frame.clone(),
            cursor: frame.cursor.clone(),
            segments: Vec::new(),
            map: ColumnMap::default(),
            runs: Vec::new(),
            in_run: vec![false; frame.cells.len()],
        };
        let width = usize::from(frame.width);
        for (row, columns) in &segments {
            let (row, columns) = (*row, columns.clone());
            let Some(cells) = segment_cells(frame, row, &columns) else {
                continue;
            };
            if !cells.iter().any(|cell| strong_rtl(&cell.symbol)) {
                continue;
            }
            let Some(layout) = self.layout(cells) else {
                continue;
            };
            let start = usize::from(row) * width + usize::from(columns.start);
            // The copy's cells move to their columns rather than being
            // cloned again; a blank's empty symbol does not allocate.
            let copy = &mut reordered.frame.cells[start..start + cells.len()];
            let moved: Vec<CellData> = copy
                .iter_mut()
                .map(|cell| {
                    std::mem::replace(
                        cell,
                        CellData {
                            symbol: String::new(),
                            ..*cell
                        },
                    )
                })
                .collect();
            for (offset, cell) in moved.into_iter().enumerate() {
                copy[layout.visual[offset]] = cell;
            }
            for (text, run_cells) in &layout.runs {
                reordered.runs.push(Run::new(
                    row,
                    columns.start,
                    text,
                    run_cells,
                    cells,
                    &layout,
                    |visual| reordered.in_run[start + visual] = true,
                ));
            }
            reordered.map.segments.push((row, columns.start, layout));
        }
        if let Some(cursor) = reordered.frame.cursor.as_mut() {
            cursor.x = reordered.map.visual(cursor.y, cursor.x);
        }
        reordered.segments = segments;
        reordered
    }
}

impl TerminalPainter {
    /// A pointer `column` (in cells, with its fraction) on `row` of a frame
    /// painted with `panes`, in the program's logical columns. With bidi off,
    /// or on a row without right-to-left text, it is `column` unchanged.
    pub(crate) fn logical_position(
        &mut self,
        frame: &FrameData,
        panes: &[PaneSurfacePane],
        row: u16,
        column: f32,
    ) -> f32 {
        if !self.bidi {
            return column;
        }
        self.bidi_cache
            .row_map(frame, panes, row)
            .logical_position(row, column)
    }

    /// Where the logical cell at `column` of `row` is drawn, as the cursor
    /// is: an IME composition starts there.
    pub(crate) fn visual_column(
        &mut self,
        frame: &FrameData,
        panes: &[PaneSurfacePane],
        row: u16,
        column: u16,
    ) -> u16 {
        if !self.bidi {
            return column;
        }
        self.bidi_cache
            .row_map(frame, panes, row)
            .visual(row, column)
    }
}

/// A right-to-left run of one row, ready to shape.
pub(super) struct Run {
    row: u16,
    text: String,
    /// Each cell that starts a glyph, in logical order: its first byte in
    /// `text` and its visual column.
    cells: Vec<(usize, u16)>,
    /// `text` as segments of (byte length, glyph style).
    styles: Vec<(usize, usize)>,
}

impl Run {
    fn new(
        row: u16,
        first: u16,
        text: &str,
        run_cells: &[(usize, usize)],
        cells: &[CellData],
        layout: &Layout,
        mut in_run: impl FnMut(usize),
    ) -> Self {
        let mut styles: Vec<(usize, usize)> = Vec::new();
        for (position, &(byte, offset)) in run_cells.iter().enumerate() {
            let end = run_cells
                .get(position + 1)
                .map_or(text.len(), |next| next.0);
            let style = glyphs::style(cells[offset].modifier);
            match styles.last_mut() {
                Some(last) if last.1 == style => last.0 += end - byte,
                _ => styles.push((end - byte, style)),
            }
        }
        let columns = run_cells
            .iter()
            .map(|&(byte, offset)| {
                let visual = layout.visual[offset];
                in_run(visual);
                // Columns fit in u16: the segment lies inside the frame.
                (byte, first + visual as u16)
            })
            .collect();
        Self {
            row,
            text: text.to_owned(),
            cells: columns,
            styles,
        }
    }
}

/// A frame with its right-to-left rows in visual order.
pub(super) struct Reordered {
    /// The cells in visual order, and the cursor at its visual column.
    pub(super) frame: FrameData,
    /// The source's own cursor, to recognise it again.
    cursor: Option<herdr_client::protocol::CursorState>,
    /// Every segment the frame's panes made, reordered or not.
    segments: Vec<(u16, Range<u16>)>,
    pub(super) map: ColumnMap,
    runs: Vec<Run>,
    /// Whether each cell of `frame` is drawn by a run rather than on its own.
    in_run: Vec<bool>,
}

impl Reordered {
    pub(super) fn in_run(&self, index: usize) -> bool {
        self.in_run.get(index).copied().unwrap_or(false)
    }

    /// Whether this is `frame` reordered over the same segments. A cell in a
    /// reordered segment is compared with its copy at its visual column, so
    /// no copy of the source is kept.
    fn is_of(&self, frame: &FrameData, segments: &[(u16, Range<u16>)]) -> bool {
        if self.frame.width != frame.width
            || self.frame.height != frame.height
            || self.frame.cells.len() != frame.cells.len()
            || self.cursor != frame.cursor
            || self.frame.hyperlinks != frame.hyperlinks
            || self.frame.graphics != frame.graphics
            || self.segments != segments
        {
            return false;
        }
        let width = usize::from(frame.width);
        let mut moved: Vec<(usize, &Layout)> = self
            .map
            .segments
            .iter()
            .map(|(row, start, layout)| {
                (usize::from(*row) * width + usize::from(*start), &**layout)
            })
            .collect();
        moved.sort_unstable_by_key(|(start, _)| *start);
        let mut next = 0;
        for (start, layout) in moved {
            let end = start + layout.visual.len();
            if frame.cells.get(next..start) != self.frame.cells.get(next..start)
                || !layout.visual.iter().enumerate().all(|(offset, visual)| {
                    frame.cells.get(start + offset) == self.frame.cells.get(start + visual)
                })
            {
                return false;
            }
            next = end;
        }
        frame.cells.get(next..) == self.frame.cells.get(next..)
    }
}

fn strong_rtl(symbol: &str) -> bool {
    !symbol.is_ascii()
        && symbol
            .chars()
            .any(|c| matches!(bidi_class(c), BidiClass::R | BidiClass::AL))
}

/// The text a cell contributes to its row. A blank cell is a space, and a
/// paragraph separator would split the row, so it reads as one too.
fn cell_text(symbol: &str) -> &str {
    // Most cells are printable ASCII, which holds no separator to look up.
    if !symbol.is_empty() && symbol.bytes().all(|byte| (b' '..=b'~').contains(&byte)) {
        return symbol;
    }
    if symbol.is_empty() || symbol.chars().any(|c| bidi_class(c) == BidiClass::B) {
        " "
    } else {
        symbol
    }
}

/// Reorders one segment of a row, or `None` when it holds no right-to-left
/// text. A wide cell moves with the continuation cells after it, which stay
/// on its right.
pub(super) fn reorder(cells: &[CellData]) -> Option<Layout> {
    if !cells.iter().any(|cell| strong_rtl(&cell.symbol)) {
        return None;
    }
    // Each unit is a cell with its continuation cells: first cell, cell
    // count, and first byte in `text`.
    let mut units: Vec<(usize, usize, usize)> = Vec::new();
    let mut text = String::new();
    for (index, cell) in cells.iter().enumerate() {
        match units.last_mut() {
            Some(unit) if cell.skip => unit.1 += 1,
            _ => {
                units.push((index, 1, text.len()));
                text.push_str(if cell.skip {
                    " "
                } else {
                    cell_text(&cell.symbol)
                });
            }
        }
    }
    let bidi = BidiInfo::new(&text, Some(Level::ltr()));
    let paragraph = bidi.paragraphs.first()?;
    let levels = bidi.reordered_levels(paragraph, paragraph.range.clone());
    let unit_levels: Vec<Level> = units.iter().map(|unit| levels[unit.2]).collect();
    let mut visual = vec![0; cells.len()];
    let mut logical = vec![0; cells.len()];
    let mut rtl = vec![false; cells.len()];
    let mut column = 0;
    for unit in BidiInfo::reorder_visual(&unit_levels) {
        let (first, count, _) = units[unit];
        for offset in 0..count {
            visual[first + offset] = column + offset;
            logical[column + offset] = first + offset;
            rtl[first + offset] = unit_levels[unit].is_rtl();
        }
        column += count;
    }
    let mut runs = Vec::new();
    let mut start = 0;
    while start < units.len() {
        if unit_levels[start] == Level::ltr() {
            start += 1;
            continue;
        }
        let end = (start..units.len())
            .find(|&unit| unit_levels[unit] == Level::ltr())
            .unwrap_or(units.len());
        let base = units[start].2;
        let stop = units.get(end).map_or(text.len(), |unit| unit.2);
        let cells = units[start..end]
            .iter()
            .map(|&(first, _, byte)| (byte - base, first))
            .collect();
        runs.push((text[base..stop].to_owned(), cells));
        start = end;
    }
    Some(Layout {
        visual,
        logical,
        rtl,
        runs,
    })
}

/// The column ranges of `row` that reorder on their own: each pane's content
/// left of its scrollbar, or the whole row of a frame without panes.
fn segments(frame: &FrameData, panes: &[PaneSurfacePane], row: u16) -> Vec<Range<u16>> {
    if panes.is_empty() {
        return std::iter::once(0..frame.width).collect();
    }
    let within = |rect: &herdr_client::protocol::SurfaceRect| {
        (rect.y..rect.y.saturating_add(rect.height)).contains(&row)
    };
    panes
        .iter()
        .filter(|pane| within(&pane.inner_rect))
        .map(|pane| {
            let rect = pane.inner_rect;
            let mut end = rect.x.saturating_add(rect.width).min(frame.width);
            if let Some(bar) = pane.scrollbar_rect.filter(|bar| within(bar)) {
                end = end.min(bar.x.max(rect.x));
            }
            rect.x..end
        })
        .filter(|columns| columns.start < columns.end)
        .collect()
}

fn frame_segments(frame: &FrameData, panes: &[PaneSurfacePane]) -> Vec<(u16, Range<u16>)> {
    (0..frame.height)
        .flat_map(|row| {
            segments(frame, panes, row)
                .into_iter()
                .map(move |columns| (row, columns))
        })
        .collect()
}

fn segment_cells<'a>(
    frame: &'a FrameData,
    row: u16,
    columns: &Range<u16>,
) -> Option<&'a [CellData]> {
    let start = usize::from(row) * usize::from(frame.width) + usize::from(columns.start);
    frame.cells.get(start..start + columns.len())
}

/// `frame` with every right-to-left pane row in visual order, uncached.
#[cfg(test)]
pub(super) fn reorder_frame(frame: &FrameData, panes: &[PaneSurfacePane]) -> Option<Reordered> {
    let mut cache = Cache::default();
    let reordered = cache.frame(frame, panes)?;
    drop(cache);
    Rc::try_unwrap(reordered).ok()
}
