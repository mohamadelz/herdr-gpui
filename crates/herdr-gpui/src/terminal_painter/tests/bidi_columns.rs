//! The logical and visual columns of reordered rows, which the cursor,
//! highlights, links, and the pointer translate through.

use super::*;
use crate::terminal_painter::bidi::{Cache, reorder};
use anyhow::Context as _;
use herdr_client::protocol::CursorState;

fn row(text: &str, width: u16) -> FrameData {
    let mut cells: Vec<CellData> = text.chars().map(|c| cell(&c.to_string())).collect();
    cells.resize_with(usize::from(width), || cell(" "));
    FrameData {
        cells,
        width,
        height: 1,
        cursor: None,
        hyperlinks: Vec::new(),
        graphics: Vec::new(),
    }
}

#[test]
fn the_logical_map_inverts_the_visual_one() -> Result<()> {
    for text in [
        "مرحبا بالعالم",
        "$ שלום",
        "عدد 123 هنا",
        "hello مرحبا world",
    ] {
        let frame = row(text, 20);
        let layout = reorder(&frame.cells).context("no right-to-left text")?;
        for logical in 0..20 {
            assert_eq!(layout.logical[layout.visual[logical]], logical, "{text}");
        }
        let map = Cache::default().row_map(&frame, &[], 0);
        for column in 0..20 {
            let visual = map.visual(0, column);
            // The cell's middle reads back as the same logical cell.
            let back = map.logical_position(0, f32::from(visual) + 0.5);
            assert_eq!(back.floor() as u16, column, "{text} {column}");
        }
    }
    Ok(())
}

#[test]
fn a_pointer_inside_a_right_to_left_cell_turns_round() {
    // "$ םולש" on screen: visual column 2 is the logical final mem at 5.
    let map = Cache::default().row_map(&row("$ שלום", 10), &[], 0);
    // Its visual left quarter is the logical right quarter of mem.
    assert_eq!(map.logical_position(0, 2.25), 5.75);
    // Left-to-right cells and columns past the text keep their place.
    assert_eq!(map.logical_position(0, 0.25), 0.25);
    assert_eq!(map.logical_position(0, 8.5), 8.5);
    // Off the row, or on a row the map does not hold, nothing changes.
    assert_eq!(map.logical_position(0, -1.), -1.);
    assert_eq!(map.logical_position(1, 2.25), 2.25);
}

#[test]
fn a_highlight_across_a_direction_change_splits_into_visual_segments() {
    // "hello ابحرم world": logical 6..11 is the Arabic, drawn at 6..11 reversed.
    let map = Cache::default().row_map(&row("hello مرحبا world", 17), &[], 0);
    // "o م ر": the "o " keeps 4..6, and the first two letters sit at 9..11.
    assert_eq!(map.visual_ranges(0, 4..8), [4..6, 9..11]);
    // The whole word stays one segment; plain text stays where it is.
    assert_eq!(map.visual_ranges(0, 6..11), vec![6..11]);
    assert_eq!(map.visual_ranges(0, 12..17), vec![12..17]);
    let highlights = map.highlights(&[
        Highlight {
            row: 0,
            columns: 4..8,
            tint: Tint::Selection,
        },
        Highlight {
            row: 1,
            columns: 4..8,
            tint: Tint::Match,
        },
    ]);
    assert_eq!(
        highlights,
        [
            Highlight {
                row: 0,
                columns: 4..6,
                tint: Tint::Selection,
            },
            Highlight {
                row: 0,
                columns: 9..11,
                tint: Tint::Selection,
            },
            // A row without right-to-left text is left as it was.
            Highlight {
                row: 1,
                columns: 4..8,
                tint: Tint::Match,
            },
        ]
    );
}

#[test]
fn the_cursor_moves_with_its_cell_and_stays_put_past_the_text() -> Result<()> {
    let mut frame = row("$ שלום", 10);
    let mut cache = Cache::default();
    for (logical, drawn) in [(2, 5), (5, 2), (6, 6), (9, 9)] {
        frame.cursor = Some(CursorState {
            x: logical,
            y: 0,
            visible: true,
            shape: 0,
        });
        let reordered = cache.frame(&frame, &[]).context("no right-to-left text")?;
        let cursor = reordered.frame.cursor.as_ref().context("no cursor")?;
        assert_eq!(cursor.x, drawn, "{logical}");
    }
    Ok(())
}

#[test]
fn reordering_is_cached_between_paints_and_layers() -> Result<()> {
    let mut frame = row("مرحبا", 10);
    frame.cells.extend(row("שלום", 10).cells);
    frame.cells.extend(row("plain", 10).cells);
    frame.height = 3;
    let mut cache = Cache::default();
    let first = cache.frame(&frame, &[]).context("no right-to-left text")?;
    assert_eq!(cache.computed, 2);
    // Each layer of a split region, and the next unchanged frame, reuse it.
    for _ in 0..3 {
        let again = cache.frame(&frame, &[]).context("no right-to-left text")?;
        assert!(std::rc::Rc::ptr_eq(&first, &again));
    }
    // A changed cell is a new frame; only its row is reordered again.
    frame.cells[1].symbol = "ب".into();
    let changed = cache.frame(&frame, &[]).context("no right-to-left text")?;
    assert!(!std::rc::Rc::ptr_eq(&first, &changed));
    assert_eq!(changed.frame.cells[3].symbol, "ب");
    assert_eq!(cache.computed, 3);
    // The pointer's row map reads the same cached layouts.
    cache.row_map(&frame, &[], 1);
    assert_eq!(cache.computed, 3);
    Ok(())
}

#[test]
fn with_bidi_off_the_pointer_and_cursor_keep_their_columns() {
    let frame = row("$ שלום", 10);
    let mut painter = TerminalPainter::default();
    assert_eq!(painter.logical_position(&frame, &[], 0, 2.25), 2.25);
    assert_eq!(painter.visual_column(&frame, &[], 0, 2), 2);
    painter.set_bidi(true);
    assert_eq!(painter.logical_position(&frame, &[], 0, 2.25), 5.75);
    assert_eq!(painter.visual_column(&frame, &[], 0, 2), 5);
}
