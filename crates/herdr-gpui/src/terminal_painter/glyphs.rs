//! Shaped cell symbols, reused across frames.
//!
//! Only the font, its size and bold/italic change a symbol's glyphs; color is
//! applied when painting. Keying by color would shape one symbol once per
//! color, so truecolor output would exhaust the bounded cache and reshape
//! every frame. ASCII, most of any grid, is found by index without hashing.

use crate::terminal::{BOLD, ITALIC};
use gpui::{Font, FontStyle, Pixels, Point, Result, Rgba, ShapedLine, Window, point, px};
use std::collections::HashMap;

pub(super) const CACHE_LIMIT: usize = 4096;
const STYLES: usize = 4;

/// A run's style segments and the index of its shaped line.
type RunEntry = (Vec<(usize, usize)>, u32);

pub(super) struct GlyphCache {
    /// A shaped line is kilobytes, so the tables below hold indexes into this.
    lines: Vec<ShapedLine>,
    /// Right-to-left runs shaped whole, by text, then by their style segments.
    runs: HashMap<String, Vec<RunEntry>>,
    run_count: usize,
    /// `ascii[style * 128 + byte]`.
    ascii: Vec<Option<u32>>,
    other: [HashMap<String, u32>; STYLES],
}

impl Default for GlyphCache {
    fn default() -> Self {
        Self {
            lines: Vec::new(),
            runs: HashMap::new(),
            run_count: 0,
            ascii: vec![None; STYLES * 128],
            other: Default::default(),
        }
    }
}

/// The modifier bits that change shaping, as an index.
pub(super) fn style(modifier: u16) -> usize {
    usize::from(modifier & BOLD != 0) | usize::from(modifier & ITALIC != 0) << 1
}

pub(super) fn style_modifier(style: usize) -> u16 {
    (if style & 1 != 0 { BOLD } else { 0 }) | if style & 2 != 0 { ITALIC } else { 0 }
}

/// The face a cached glyph `style` shapes in: bold cells draw a step heavier
/// than the configured weight rather than at a fixed bold.
pub(super) fn styled_font(font: &Font, style: usize) -> Font {
    let mut font = font.clone();
    let modifier = style_modifier(style);
    if modifier & BOLD != 0 {
        font.weight = crate::config::bold_font_weight(font.weight);
    }
    if modifier & ITALIC != 0 {
        font.style = FontStyle::Italic;
    }
    font
}

/// `ShapedLine::paint` without its per-call layer, whose BoundsTree insert
/// would otherwise run once per cell. Glyph placement matches GPUI's.
pub(super) fn paint_glyphs(
    line: &ShapedLine,
    origin: Point<Pixels>,
    line_height: Pixels,
    color: Rgba,
    window: &mut Window,
) -> Result<()> {
    let baseline = origin
        + point(
            px(0.),
            (line_height - line.ascent - line.descent) / 2. + line.ascent,
        );
    for run in &line.runs {
        for glyph in &run.glyphs {
            let position = baseline + point(glyph.position.x, px(0.));
            if glyph.is_emoji {
                window.paint_emoji(position, run.font_id, glyph.id, line.font_size)?;
            } else {
                window.paint_glyph(
                    position,
                    run.font_id,
                    glyph.id,
                    line.font_size,
                    color.into(),
                )?;
            }
        }
    }
    Ok(())
}

fn ascii_index(style: usize, symbol: &str) -> Option<usize> {
    match symbol.as_bytes() {
        [byte] if byte.is_ascii() => Some(style * 128 + usize::from(*byte)),
        _ => None,
    }
}

impl GlyphCache {
    pub(super) fn get(&self, style: usize, symbol: &str) -> Option<&ShapedLine> {
        let id = match ascii_index(style, symbol) {
            Some(index) => self.ascii[index]?,
            None => *self.other[style].get(symbol)?,
        };
        self.lines.get(usize::try_from(id).ok()?)
    }

    /// Whether `insert` keeps another line; past the limit, lines are shaped
    /// for one paint alone.
    pub(super) fn has_room(&self) -> bool {
        self.lines.len() < CACHE_LIMIT
    }

    pub(super) fn insert(&mut self, style: usize, symbol: &str, line: ShapedLine) -> &ShapedLine {
        // CACHE_LIMIT fits in u32; `has_room` keeps the count below it.
        let id = u32::try_from(self.lines.len()).unwrap_or(u32::MAX);
        match ascii_index(style, symbol) {
            Some(index) => self.ascii[index] = Some(id),
            None => {
                self.other[style].insert(symbol.to_owned(), id);
            }
        }
        self.lines.push(line);
        &self.lines[self.lines.len() - 1]
    }

    pub(super) fn get_run(&self, styles: &[(usize, usize)], text: &str) -> Option<&ShapedLine> {
        let (_, id) = self.runs.get(text)?.iter().find(|(key, _)| key == styles)?;
        self.lines.get(usize::try_from(*id).ok()?)
    }

    /// Whether `insert_run` keeps another run: within the shared bound and
    /// the runs' own share of it.
    pub(super) fn has_room_for_run(&self) -> bool {
        self.has_room() && self.run_count < super::bidi::RUN_CACHE_LIMIT
    }

    pub(super) fn insert_run(
        &mut self,
        styles: &[(usize, usize)],
        text: &str,
        line: ShapedLine,
    ) -> &ShapedLine {
        let id = u32::try_from(self.lines.len()).unwrap_or(u32::MAX);
        self.runs
            .entry(text.to_owned())
            .or_default()
            .push((styles.to_vec(), id));
        self.run_count += 1;
        self.lines.push(line);
        &self.lines[self.lines.len() - 1]
    }

    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }

    #[cfg(any(test, feature = "integration-test"))]
    pub(super) fn len(&self) -> usize {
        self.lines.len()
    }

    /// Every cached symbol with its style index.
    #[cfg(any(test, feature = "integration-test"))]
    pub(super) fn iter(&self) -> impl Iterator<Item = (usize, String, &ShapedLine)> {
        let ascii = self.ascii.iter().enumerate().filter_map(|(index, id)| {
            let byte = u8::try_from(index % 128).ok()?;
            let symbol = char::from(byte).to_string();
            Some((
                index / 128,
                symbol,
                self.lines.get(usize::try_from((*id)?).ok()?)?,
            ))
        });
        let other = self.other.iter().enumerate().flat_map(move |(style, ids)| {
            ids.iter().filter_map(move |(symbol, id)| {
                Some((
                    style,
                    symbol.clone(),
                    self.lines.get(usize::try_from(*id).ok()?)?,
                ))
            })
        });
        ascii.chain(other)
    }
}
