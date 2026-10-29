//! Paint cost of right-to-left rows, run by hand:
//! `cargo test --release -p herdr-gpui --bin herdr-gpui bidi_paint_cost -- --ignored --nocapture`.
//! It prints timings and asserts nothing about them; the headless text system
//! is not the native one, so the numbers compare paths, not machines.

use super::*;
use std::time::Duration;

const WIDTH: u16 = 160;
const HEIGHT: u16 = 50;
const PAINTS: u32 = 200;

/// A dense grid of `line` repeated, each row starting one character later.
fn grid(line: &str) -> FrameData {
    let mut cells = Vec::new();
    for row in 0..HEIGHT {
        let mut row: Vec<CellData> = line
            .chars()
            .cycle()
            .skip(usize::from(row))
            .take(usize::from(WIDTH))
            .map(|c| cell(&c.to_string()))
            .collect();
        row.resize_with(usize::from(WIDTH), || cell(" "));
        cells.extend(row);
    }
    FrameData {
        cells,
        width: WIDTH,
        height: HEIGHT,
        cursor: None,
        hyperlinks: Vec::new(),
        graphics: Vec::new(),
    }
}

/// The mean CPU time of one `paint_frame`, whole or as three layers, of
/// `frame` as it is or scrolled up a row before each paint.
fn mean_paint(
    cx: &mut TestAppContext,
    frame: &FrameData,
    bidi: bool,
    layered: bool,
    scroll: bool,
) -> Duration {
    let (_, cx) = cx.add_window_view(|_, _| Empty);
    let painter = std::rc::Rc::new(std::cell::RefCell::new(TerminalPainter::default()));
    painter.borrow_mut().set_bidi(bidi);
    let total = std::rc::Rc::new(std::cell::Cell::new(Duration::ZERO));
    let mut frame = frame.clone();
    // The first paints fill the caches and are not counted.
    for paint in 0..PAINTS + 10 {
        if scroll {
            frame.cells.rotate_left(usize::from(WIDTH));
        }
        let (painter, frame, total) = (painter.clone(), frame.clone(), total.clone());
        cx.draw(Point::default(), size(px(2400.), px(1400.)), |_, _| {
            canvas(
                |_, _, _| (),
                move |bounds, _, window, cx| {
                    let font = font("Menlo");
                    let cell_width = painter.borrow_mut().cell_width(&font, window, cx);
                    let area = whole(&frame);
                    let parts: Vec<Option<Part<'_>>> = if layered {
                        Layer::ALL
                            .map(|layer| Some(Part { area: &area, layer }))
                            .to_vec()
                    } else {
                        vec![None]
                    };
                    let started = Instant::now();
                    for part in parts {
                        painter.borrow_mut().paint_frame(
                            &frame,
                            bounds.origin,
                            None,
                            Ground::Opaque,
                            cell_width,
                            &font,
                            &[],
                            &[],
                            part,
                            None,
                            window,
                            cx,
                        );
                    }
                    if paint >= 10 {
                        total.set(total.get() + started.elapsed());
                    }
                },
            )
            .size_full()
        });
    }
    total.get() / PAINTS
}

#[gpui::test]
#[ignore = "timing report, run by hand"]
fn bidi_paint_cost(cx: &mut TestAppContext) {
    let arabic = grid("$ echo مرحبا بالعالم 123 and שלום עולם done ");
    let latin = grid("$ echo marhaba bilalam 123 and shalom olam done ");
    for (name, frame, bidi) in [
        ("latin, bidi off", &latin, false),
        ("latin, bidi on", &latin, true),
        ("arabic, bidi off", &arabic, false),
        ("arabic, bidi on", &arabic, true),
    ] {
        let whole = mean_paint(cx, frame, bidi, false, false);
        let layered = mean_paint(cx, frame, bidi, true, false);
        let scrolling = mean_paint(cx, frame, bidi, false, true);
        println!(
            "{name:18} whole {whole:>10.1?}  three layers {layered:>10.1?}  scrolling {scrolling:>10.1?}"
        );
    }
}
