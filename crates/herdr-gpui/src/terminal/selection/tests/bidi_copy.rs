use super::*;
use crate::terminal_painter::TerminalPainter;

/// A drag over a right-to-left row as it is drawn: the window reads each
/// pointer position in logical columns before the selection sees it.
#[test]
fn a_drag_over_reordered_hebrew_copies_it_in_logical_order() {
    let s = surface("$ שלום");
    let mut painter = TerminalPainter::default();
    painter.set_bidi(true);
    // Drawn as "$ םולש": the drag covers the visual columns 2 and 3, the
    // mem and vav, which are the logical cells 5 and 4.
    let mut logical =
        |x: f32| painter.logical_position(&s.frame, &s.panes, 0, x / CELL_WIDTH) * CELL_WIDTH;
    let (from, to) = (logical(20.5), logical(39.5));
    let selection = drag(&s, (from, 1.), (to, 1.));
    assert_eq!(
        selection
            .rows(&s, CELL_WIDTH, CELL_HEIGHT)
            .collect::<Vec<_>>(),
        vec![(0, 4..6)]
    );
    // The copy keeps the program's order: vav, then final mem.
    assert_eq!(text(&s, &selection), "ום");
    // Off, the same pixels are the logical cells they cover.
    painter.set_bidi(false);
    let mut logical =
        |x: f32| painter.logical_position(&s.frame, &s.panes, 0, x / CELL_WIDTH) * CELL_WIDTH;
    assert_eq!((logical(25.), logical(35.)), (25., 35.));
}
