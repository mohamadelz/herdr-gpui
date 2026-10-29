use super::*;
use crate::terminal_painter::bidi::{
    cluster_columns, fits_grid, joins_previous, kashida_fills, reorder, reorder_frame,
};
use anyhow::Context as _;
use herdr_client::protocol::SurfaceRect;

/// One cell per character, padded with spaces to `width`.
fn cells(text: &str, width: usize) -> Vec<CellData> {
    let mut cells: Vec<CellData> = text.chars().map(|c| cell(&c.to_string())).collect();
    cells.resize_with(width.max(cells.len()), || cell(" "));
    cells
}

/// The row as it reads on screen, trailing spaces dropped.
fn shown(cells: &[CellData], visual: &[usize]) -> String {
    let mut row = vec![String::new(); cells.len()];
    for (logical, cell) in cells.iter().enumerate() {
        row[visual[logical]] = cell.symbol.clone();
    }
    row.concat().trim_end().to_owned()
}

fn pane(x: u16, width: u16) -> PaneSurfacePane {
    let rect = SurfaceRect {
        x,
        y: 0,
        width,
        height: 1,
    };
    PaneSurfacePane {
        pane_id: format!("pane-{x}"),
        content_revision: 1,
        rect,
        inner_rect: rect,
        scrollbar_rect: None,
        scroll: None,
        focused: false,
        mouse_reporting: false,
        sgr_pixel_mouse: false,
        alternate_screen_active: false,
        pixel_width: 0,
        pixel_height: 0,
    }
}

#[test]
fn an_arabic_sentence_reads_right_to_left() -> Result<()> {
    // What `echo "مرحبا بالعالم"` leaves in the grid: logical order.
    let row = cells("مرحبا بالعالم", 20);
    let layout = reorder(&row).context("no right-to-left text")?;
    // The first word is rightmost; the trailing blanks keep their columns.
    assert_eq!(
        &layout.visual[..13],
        &[12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0]
    );
    assert_eq!(&layout.visual[13..], &[13, 14, 15, 16, 17, 18, 19]);
    assert_eq!(shown(&row, &layout.visual), "ملاعلاب ابحرم");
    // One run, shaped whole in logical order, every letter starting a cell.
    assert_eq!(layout.runs.len(), 1);
    let (text, run_cells) = &layout.runs[0];
    assert_eq!(text, "مرحبا بالعالم");
    assert_eq!(
        run_cells.iter().map(|&(_, cell)| cell).collect::<Vec<_>>(),
        (0..13).collect::<Vec<_>>()
    );
    // Each cell's byte is where its letter starts in the run's text.
    for &(byte, cell) in run_cells {
        assert!(text[byte..].starts_with(&row[cell].symbol));
    }
    Ok(())
}

#[test]
fn hebrew_after_a_prompt_keeps_the_prompt_in_place() -> Result<()> {
    let row = cells("$ שלום", 8);
    let layout = reorder(&row).context("no right-to-left text")?;
    assert_eq!(layout.visual, [0, 1, 5, 4, 3, 2, 6, 7]);
    assert_eq!(shown(&row, &layout.visual), "$ םולש");
    assert_eq!(layout.runs.len(), 1);
    assert_eq!(layout.runs[0].0, "שלום");
    Ok(())
}

#[test]
fn digits_inside_arabic_keep_their_left_to_right_order() -> Result<()> {
    let row = cells("عدد 123 هنا", 11);
    let layout = reorder(&row).context("no right-to-left text")?;
    assert_eq!(layout.visual, [10, 9, 8, 7, 4, 5, 6, 3, 2, 1, 0]);
    assert_eq!(shown(&row, &layout.visual), "انه 123 ددع");
    // The number is shaped with the words around it, as one run.
    assert_eq!(layout.runs.len(), 1);
    assert_eq!(layout.runs[0].0, "عدد 123 هنا");
    Ok(())
}

#[test]
fn latin_around_arabic_keeps_its_columns() -> Result<()> {
    let row = cells("hello مرحبا world", 17);
    let layout = reorder(&row).context("no right-to-left text")?;
    for column in (0..6).chain(11..17) {
        assert_eq!(layout.visual[column], column);
    }
    assert_eq!(&layout.visual[6..11], &[10, 9, 8, 7, 6]);
    assert_eq!(shown(&row, &layout.visual), "hello ابحرم world");
    // The spaces between scripts stay left to right, outside the run.
    assert_eq!(layout.runs.len(), 1);
    assert_eq!(layout.runs[0].0, "مرحبا");
    Ok(())
}

#[test]
fn left_to_right_rows_are_left_alone() {
    for text in ["", "ls -la", "héllo wörld", "日本語", "1 + 2 = 3", "→ ✓"] {
        assert_eq!(reorder(&cells(text, 12)), None, "{text:?}");
    }
    let frame = FrameData {
        cells: cells("cargo build", 16),
        width: 16,
        height: 1,
        cursor: None,
        hyperlinks: Vec::new(),
        graphics: Vec::new(),
    };
    assert!(reorder_frame(&frame, &[]).is_none());
}

#[test]
fn a_wide_cell_moves_with_its_continuation() -> Result<()> {
    // Alef, a wide emoji and its continuation cell, then beh.
    let mut row = vec![cell("ا"), cell("😀"), cell(""), cell("ب")];
    row[2].skip = true;
    let layout = reorder(&row).context("no right-to-left text")?;
    // The emoji keeps its continuation cell on its right.
    assert_eq!(layout.visual, [3, 1, 2, 0]);
    let (text, run_cells) = &layout.runs[0];
    assert_eq!(text, "ا😀ب");
    // The continuation cell starts no glyph.
    assert_eq!(
        run_cells.iter().map(|&(_, cell)| cell).collect::<Vec<_>>(),
        [0, 1, 3]
    );
    Ok(())
}

#[test]
fn each_pane_reorders_on_its_own() -> Result<()> {
    // Two panes side by side, five columns each, in one frame row.
    let mut row = cells("ابت", 5);
    row.extend(cells("ab", 5));
    let frame = FrameData {
        cells: row,
        width: 10,
        height: 1,
        cursor: None,
        hyperlinks: Vec::new(),
        graphics: Vec::new(),
    };
    let reordered =
        reorder_frame(&frame, &[pane(0, 5), pane(5, 5)]).context("no right-to-left text")?;
    let symbols: Vec<&str> = reordered
        .frame
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect();
    // The Arabic reverses inside the first pane; the second is untouched.
    assert_eq!(symbols, ["ت", "ب", "ا", " ", " ", "a", "b", " ", " ", " "]);
    assert_eq!(
        (0..10)
            .map(|index| reordered.in_run(index))
            .collect::<Vec<_>>(),
        [
            true, true, true, false, false, false, false, false, false, false
        ]
    );
    Ok(())
}

#[gpui::test]
fn a_right_to_left_run_is_shaped_once_and_cached(cx: &mut TestAppContext) {
    let (_, cx) = cx.add_window_view(|_, _| Empty);
    let frame = FrameData {
        cells: cells("$ مرحبا", 10),
        width: 10,
        height: 1,
        cursor: None,
        hyperlinks: Vec::new(),
        graphics: Vec::new(),
    };
    let mut cached = |bidi: bool| {
        let painter = std::rc::Rc::new(std::cell::RefCell::new(TerminalPainter::default()));
        painter.borrow_mut().set_bidi(bidi);
        for _ in 0..2 {
            let (painter, frame) = (painter.clone(), frame.clone());
            cx.draw(Point::default(), size(px(800.), px(600.)), |_, _| {
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, cx| {
                        let font = font("Menlo");
                        let cell_width = painter.borrow_mut().cell_width(&font, window, cx);
                        painter.borrow_mut().paint_frame(
                            &frame,
                            bounds.origin,
                            None,
                            Ground::Opaque,
                            cell_width,
                            &font,
                            &[],
                            &[],
                            None,
                            None,
                            window,
                            cx,
                        );
                    },
                )
                .size_full()
            });
        }
        painter.borrow().glyphs.len()
    };
    // Off, each letter is its own cached cell; on, the word is one run
    // beside the prompt's `$`, and a second paint reuses both.
    assert_eq!(cached(false), 6);
    assert_eq!(cached(true), 2);
}

/// A run's text, its (byte, visual column) cells, and which start a glyph.
type LigatureCells = (String, Vec<(usize, u16)>, Vec<bool>);

/// A run's (byte, visual column) cells, and which start a glyph, for
/// `text` shaped with lam-alef as one glyph: every cell starts a glyph
/// except an alef right after a lam.
fn ligature_cells(text: &str) -> Result<LigatureCells> {
    let row = cells(text, text.chars().count());
    let layout = reorder(&row).context("no right-to-left text")?;
    let (run_text, run_cells) = &layout.runs[0];
    let cells: Vec<(usize, u16)> = run_cells
        .iter()
        .map(|&(byte, cell)| (byte, layout.visual[cell] as u16))
        .collect();
    let starts: Vec<bool> = run_cells
        .iter()
        .map(|&(byte, _)| !(run_text[byte..].starts_with('ا') && run_text[..byte].ends_with('ل')))
        .collect();
    Ok((run_text.clone(), cells, starts))
}

#[test]
fn a_snapped_ligature_joined_on_its_right_fills_its_empty_cell() -> Result<()> {
    // السلام: lam-alef draws in alef's column 1, the leftmost of its cells,
    // and seen joins it from column 3, so lam's empty column 2 takes the
    // kashida.
    let (text, cells, starts) = ligature_cells("السلام")?;
    assert_eq!(cluster_columns(&cells, &starts)[3..5], [1, 1]);
    assert_eq!(
        kashida_fills(&text, &cells, &starts)
            .into_iter()
            .map(|(_, column)| column)
            .collect::<Vec<_>>(),
        [2]
    );
    Ok(())
}

#[test]
fn a_snapped_ligature_with_nothing_joining_its_right_stays_apart() -> Result<()> {
    // Word start, after a space, and after waw and alef, which join only on
    // their right and so never reach the ligature.
    for text in ["لا", "لام", "قال لا", "ولا", "الان"] {
        let (text, cells, starts) = ligature_cells(text)?;
        assert_eq!(kashida_fills(&text, &cells, &starts), [], "{text}");
    }
    // A cluster of one cell never needs a kashida.
    let text = "سل";
    assert!(joins_previous(text, 'س'.len_utf8()));
    assert_eq!(kashida_fills(text, &[(0, 1), (2, 0)], &[true, true]), []);
    Ok(())
}

#[test]
fn joining_skips_marks_and_needs_both_sides() {
    // Lam after seen with a shadda between them still joins.
    assert!(joins_previous("سّل", "سّ".len()));
    // Alef joins right only, so a letter after it does not join it.
    assert!(!joins_previous("ال", "ا".len()));
    // Nothing before the first letter.
    assert!(!joins_previous("ل", 0));
}

#[test]
fn only_a_face_as_wide_as_the_grid_keeps_its_shaped_positions() {
    // CoreText at 14 px: JetBrains Mono's cell is 8.4 px, and Kawkab Mono
    // advances 9.8 px a glyph, lam-alef being one glyph.
    let cell = 8.4;
    // مرحبا, five glyphs over five cells, and لام, two over three.
    assert!(!fits_grid(49., 5, cell));
    assert!(!fits_grid(19.6, 3, cell));
    // السلام is five glyphs over six cells: 49 against 50.4 px, which half a
    // pixel a cell would pass but 2% does not.
    assert!(!fits_grid(49., 6, cell));
    // A face that advances one cell a glyph fits, rounding included.
    assert!(fits_grid(42., 5, cell));
    assert!(fits_grid(42.3, 5, cell));
}
