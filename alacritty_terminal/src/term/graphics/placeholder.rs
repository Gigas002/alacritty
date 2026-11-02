//! Kitty graphics protocol unicode placeholders.
//!
//! Instead of placing images directly, clients can create a virtual placement and print
//! placeholder characters into the grid. Each placeholder cell displays one cell sized part of
//! the image. The image ID is encoded in the foreground color, the placement ID in the underline
//! color and the row, column and most significant byte of the image ID are encoded as combining
//! diacritics.
//!
//! See <https://sw.kovidgoyal.net/kitty/graphics-protocol/#unicode-placeholders>.

use crate::grid::{Dimensions, Grid};
use crate::index::{Column, Line};
use crate::term::cell::Cell;
use crate::vte::ansi::Color;

/// Character reserved for image placeholders.
pub const PLACEHOLDER: char = '\u{10EEEE}';

/// Diacritics encoding the numbers `0..297`, sorted by codepoint.
///
/// See <https://sw.kovidgoyal.net/kitty/_downloads/f0a0de9ec8d9ff4456206db8e0814937/rowcolumn-diacritics.txt>.
static DIACRITICS: [char; 297] = [
    '\u{305}',
    '\u{30D}',
    '\u{30E}',
    '\u{310}',
    '\u{312}',
    '\u{33D}',
    '\u{33E}',
    '\u{33F}',
    '\u{346}',
    '\u{34A}',
    '\u{34B}',
    '\u{34C}',
    '\u{350}',
    '\u{351}',
    '\u{352}',
    '\u{357}',
    '\u{35B}',
    '\u{363}',
    '\u{364}',
    '\u{365}',
    '\u{366}',
    '\u{367}',
    '\u{368}',
    '\u{369}',
    '\u{36A}',
    '\u{36B}',
    '\u{36C}',
    '\u{36D}',
    '\u{36E}',
    '\u{36F}',
    '\u{483}',
    '\u{484}',
    '\u{485}',
    '\u{486}',
    '\u{487}',
    '\u{592}',
    '\u{593}',
    '\u{594}',
    '\u{595}',
    '\u{597}',
    '\u{598}',
    '\u{599}',
    '\u{59C}',
    '\u{59D}',
    '\u{59E}',
    '\u{59F}',
    '\u{5A0}',
    '\u{5A1}',
    '\u{5A8}',
    '\u{5A9}',
    '\u{5AB}',
    '\u{5AC}',
    '\u{5AF}',
    '\u{5C4}',
    '\u{610}',
    '\u{611}',
    '\u{612}',
    '\u{613}',
    '\u{614}',
    '\u{615}',
    '\u{616}',
    '\u{617}',
    '\u{657}',
    '\u{658}',
    '\u{659}',
    '\u{65A}',
    '\u{65B}',
    '\u{65D}',
    '\u{65E}',
    '\u{6D6}',
    '\u{6D7}',
    '\u{6D8}',
    '\u{6D9}',
    '\u{6DA}',
    '\u{6DB}',
    '\u{6DC}',
    '\u{6DF}',
    '\u{6E0}',
    '\u{6E1}',
    '\u{6E2}',
    '\u{6E4}',
    '\u{6E7}',
    '\u{6E8}',
    '\u{6EB}',
    '\u{6EC}',
    '\u{730}',
    '\u{732}',
    '\u{733}',
    '\u{735}',
    '\u{736}',
    '\u{73A}',
    '\u{73D}',
    '\u{73F}',
    '\u{740}',
    '\u{741}',
    '\u{743}',
    '\u{745}',
    '\u{747}',
    '\u{749}',
    '\u{74A}',
    '\u{7EB}',
    '\u{7EC}',
    '\u{7ED}',
    '\u{7EE}',
    '\u{7EF}',
    '\u{7F0}',
    '\u{7F1}',
    '\u{7F3}',
    '\u{816}',
    '\u{817}',
    '\u{818}',
    '\u{819}',
    '\u{81B}',
    '\u{81C}',
    '\u{81D}',
    '\u{81E}',
    '\u{81F}',
    '\u{820}',
    '\u{821}',
    '\u{822}',
    '\u{823}',
    '\u{825}',
    '\u{826}',
    '\u{827}',
    '\u{829}',
    '\u{82A}',
    '\u{82B}',
    '\u{82C}',
    '\u{82D}',
    '\u{951}',
    '\u{953}',
    '\u{954}',
    '\u{F82}',
    '\u{F83}',
    '\u{F86}',
    '\u{F87}',
    '\u{135D}',
    '\u{135E}',
    '\u{135F}',
    '\u{17DD}',
    '\u{193A}',
    '\u{1A17}',
    '\u{1A75}',
    '\u{1A76}',
    '\u{1A77}',
    '\u{1A78}',
    '\u{1A79}',
    '\u{1A7A}',
    '\u{1A7B}',
    '\u{1A7C}',
    '\u{1B6B}',
    '\u{1B6D}',
    '\u{1B6E}',
    '\u{1B6F}',
    '\u{1B70}',
    '\u{1B71}',
    '\u{1B72}',
    '\u{1B73}',
    '\u{1CD0}',
    '\u{1CD1}',
    '\u{1CD2}',
    '\u{1CDA}',
    '\u{1CDB}',
    '\u{1CE0}',
    '\u{1DC0}',
    '\u{1DC1}',
    '\u{1DC3}',
    '\u{1DC4}',
    '\u{1DC5}',
    '\u{1DC6}',
    '\u{1DC7}',
    '\u{1DC8}',
    '\u{1DC9}',
    '\u{1DCB}',
    '\u{1DCC}',
    '\u{1DD1}',
    '\u{1DD2}',
    '\u{1DD3}',
    '\u{1DD4}',
    '\u{1DD5}',
    '\u{1DD6}',
    '\u{1DD7}',
    '\u{1DD8}',
    '\u{1DD9}',
    '\u{1DDA}',
    '\u{1DDB}',
    '\u{1DDC}',
    '\u{1DDD}',
    '\u{1DDE}',
    '\u{1DDF}',
    '\u{1DE0}',
    '\u{1DE1}',
    '\u{1DE2}',
    '\u{1DE3}',
    '\u{1DE4}',
    '\u{1DE5}',
    '\u{1DE6}',
    '\u{1DFE}',
    '\u{20D0}',
    '\u{20D1}',
    '\u{20D4}',
    '\u{20D5}',
    '\u{20D6}',
    '\u{20D7}',
    '\u{20DB}',
    '\u{20DC}',
    '\u{20E1}',
    '\u{20E7}',
    '\u{20E9}',
    '\u{20F0}',
    '\u{2CEF}',
    '\u{2CF0}',
    '\u{2CF1}',
    '\u{2DE0}',
    '\u{2DE1}',
    '\u{2DE2}',
    '\u{2DE3}',
    '\u{2DE4}',
    '\u{2DE5}',
    '\u{2DE6}',
    '\u{2DE7}',
    '\u{2DE8}',
    '\u{2DE9}',
    '\u{2DEA}',
    '\u{2DEB}',
    '\u{2DEC}',
    '\u{2DED}',
    '\u{2DEE}',
    '\u{2DEF}',
    '\u{2DF0}',
    '\u{2DF1}',
    '\u{2DF2}',
    '\u{2DF3}',
    '\u{2DF4}',
    '\u{2DF5}',
    '\u{2DF6}',
    '\u{2DF7}',
    '\u{2DF8}',
    '\u{2DF9}',
    '\u{2DFA}',
    '\u{2DFB}',
    '\u{2DFC}',
    '\u{2DFD}',
    '\u{2DFE}',
    '\u{2DFF}',
    '\u{A66F}',
    '\u{A67C}',
    '\u{A67D}',
    '\u{A6F0}',
    '\u{A6F1}',
    '\u{A8E0}',
    '\u{A8E1}',
    '\u{A8E2}',
    '\u{A8E3}',
    '\u{A8E4}',
    '\u{A8E5}',
    '\u{A8E6}',
    '\u{A8E7}',
    '\u{A8E8}',
    '\u{A8E9}',
    '\u{A8EA}',
    '\u{A8EB}',
    '\u{A8EC}',
    '\u{A8ED}',
    '\u{A8EE}',
    '\u{A8EF}',
    '\u{A8F0}',
    '\u{A8F1}',
    '\u{AAB0}',
    '\u{AAB2}',
    '\u{AAB3}',
    '\u{AAB7}',
    '\u{AAB8}',
    '\u{AABE}',
    '\u{AABF}',
    '\u{AAC1}',
    '\u{FE20}',
    '\u{FE21}',
    '\u{FE22}',
    '\u{FE23}',
    '\u{FE24}',
    '\u{FE25}',
    '\u{FE26}',
    '\u{10A0F}',
    '\u{10A38}',
    '\u{1D185}',
    '\u{1D186}',
    '\u{1D187}',
    '\u{1D188}',
    '\u{1D189}',
    '\u{1D1AA}',
    '\u{1D1AB}',
    '\u{1D1AC}',
    '\u{1D1AD}',
    '\u{1D242}',
    '\u{1D243}',
    '\u{1D244}',
];

/// Decoded placeholder cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaceholderCell {
    /// Viewport line of the cell.
    pub line: usize,
    /// Grid column of the cell.
    pub column: usize,
    /// Image ID.
    pub image_id: u32,
    /// Placement ID, zero if unspecified.
    pub placement_id: u32,
    /// Image row displayed by this cell.
    pub row: u32,
    /// Image column displayed by this cell.
    pub col: u32,
}

/// Fields of a placeholder cell before the most significant image ID byte is applied.
#[derive(Debug, Clone, Copy)]
struct Decoded {
    id_low: u32,
    placement_id: u32,
    row: u32,
    col: u32,
    msb: u32,
}

/// Number encoded by a diacritic.
fn diacritic_value(c: char) -> Option<u32> {
    DIACRITICS.binary_search(&c).ok().map(|index| index as u32)
}

/// Number encoded by a color, used for image and placement IDs.
fn color_value(color: Color) -> Option<u32> {
    match color {
        Color::Spec(rgb) => Some(u32::from(rgb.r) << 16 | u32::from(rgb.g) << 8 | u32::from(rgb.b)),
        Color::Indexed(index) => Some(u32::from(index)),
        Color::Named(_) => None,
    }
}

/// Decode a placeholder cell, inheriting omitted values from the placeholder to its left.
fn decode(cell: &Cell, left: Option<Decoded>) -> Option<Decoded> {
    if cell.c != PLACEHOLDER {
        return None;
    }

    let id_low = color_value(cell.fg)?;
    let placement_id = cell.underline_color().and_then(color_value).unwrap_or(0);

    let mut diacritics = cell.zerowidth().unwrap_or(&[]).iter().filter_map(|&c| diacritic_value(c));
    let row = diacritics.next();
    let col = diacritics.next();
    let msb = diacritics.next();

    let left = left.filter(|left| left.id_low == id_low && left.placement_id == placement_id);
    let row = row.unwrap_or_else(|| left.map_or(0, |left| left.row));
    let left = left.filter(|left| left.row == row);
    let col = col.unwrap_or_else(|| left.map_or(0, |left| left.col + 1));
    let msb = msb.unwrap_or_else(|| left.map_or(0, |left| left.msb));

    Some(Decoded { id_low, placement_id, row, col, msb })
}

/// Find all placeholder cells in the viewport.
pub fn visible_placeholders(grid: &Grid<Cell>) -> Vec<PlaceholderCell> {
    let display_offset = grid.display_offset() as i32;
    let mut placeholders = Vec::new();

    for line in 0..grid.screen_lines() {
        let row = &grid[Line(line as i32 - display_offset)];
        let mut left = None;

        for column in 0..grid.columns() {
            left = decode(&row[Column(column)], left);

            if let Some(decoded) = left {
                let image_id = decoded.msb << 24 | decoded.id_low;
                if image_id == 0 {
                    continue;
                }

                placeholders.push(PlaceholderCell {
                    line,
                    column,
                    image_id,
                    placement_id: decoded.placement_id,
                    row: decoded.row,
                    col: decoded.col,
                });
            }
        }
    }

    placeholders
}

/// Encode a number as a diacritic.
#[cfg(test)]
pub fn diacritic(value: u32) -> char {
    DIACRITICS[value as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diacritics_sorted() {
        assert!(DIACRITICS.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(diacritic_value('\u{305}'), Some(0));
        assert_eq!(diacritic_value('\u{30D}'), Some(1));
        assert_eq!(diacritic_value('a'), None);
    }
}
