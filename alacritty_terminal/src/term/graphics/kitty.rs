//! Kitty graphics protocol.
//!
//! This implements the image transmission, placement and deletion parts of the protocol described
//! at <https://sw.kovidgoyal.net/kitty/graphics-protocol/>.
//!
//! Images are stored per screen buffer and are not part of the grid. Placements are anchored to
//! grid lines and are moved along with the grid content whenever it is scrolled.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::{cmp, fmt, mem, str};

use base64::Engine;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use log::debug;

use crate::event::{Event, EventListener};
use crate::grid::{Dimensions, Grid};
use crate::index::{Column, Line};
use crate::term::Term;
use crate::term::cell::Cell;
use crate::term::graphics::placeholder::{self, PlaceholderCell};
use crate::vte::ansi::Handler;

/// Base64 engine accepting payloads both with and without padding.
const BASE64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// Maximum width and height of an image in pixels.
const MAX_IMAGE_DIMENSION: u32 = 10_000;

/// Maximum number of bytes used by decoded images of a single screen buffer.
const STORAGE_QUOTA: usize = 320 * 1024 * 1024;

/// Maximum size of the transmitted data, before decompression and decoding.
const MAX_TRANSMISSION_SIZE: usize = 128 * 1024 * 1024;

/// Fallback cell size, used until the real cell size is known.
const DEFAULT_CELL_SIZE: (f32, f32) = (10., 20.);

/// Unique identifier generator for image data.
static NEXT_IMAGE_UID: AtomicU64 = AtomicU64::new(1);

/// Decoded RGBA image data.
#[derive(Debug)]
pub struct ImageData {
    uid: u64,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl ImageData {
    /// Create new image from RGBA pixels with straight alpha.
    ///
    /// # Panics
    ///
    /// Panics if `pixels` does not contain exactly `width * height * 4` bytes.
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Self {
        assert_eq!(pixels.len(), width as usize * height as usize * 4);
        let uid = NEXT_IMAGE_UID.fetch_add(1, Ordering::Relaxed);
        Self { uid, width, height, pixels }
    }

    /// Unique identifier of this image data.
    ///
    /// Unlike the image ID used by the protocol, this is never reused.
    #[inline]
    pub fn uid(&self) -> u64 {
        self.uid
    }

    #[inline]
    pub fn width(&self) -> u32 {
        self.width
    }

    #[inline]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// RGBA pixels with straight alpha, row by row starting at the top.
    #[inline]
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
}

/// Image placement ready for rendering.
#[derive(Debug, Clone)]
pub struct RenderableGraphic {
    /// Image data.
    pub image: Arc<ImageData>,

    /// Horizontal position relative to the left of the grid, in pixels.
    pub x: f32,

    /// Vertical position relative to the top of the viewport, in pixels.
    pub y: f32,

    /// Rendered width in pixels.
    pub width: f32,

    /// Rendered height in pixels.
    pub height: f32,

    /// Visible rectangle of the image in pixels, as `(x, y, width, height)`.
    pub source: (f32, f32, f32, f32),

    /// Stacking order; negative values are drawn below the text.
    pub z_index: i32,
}

/// Error returned to the client.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GraphicsError {
    code: &'static str,
    message: String,
}

impl GraphicsError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::new("EINVAL", message)
    }
}

impl fmt::Display for GraphicsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.code, self.message)
    }
}

impl From<io::Error> for GraphicsError {
    fn from(err: io::Error) -> Self {
        let code = match err.kind() {
            io::ErrorKind::NotFound => "ENOENT",
            io::ErrorKind::PermissionDenied => "EPERM",
            _ => "EBADF",
        };
        Self::new(code, err.to_string())
    }
}

type GraphicsResult<T> = Result<T, GraphicsError>;

/// A parsed graphics command.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Command {
    /// Action (`a`).
    action: u8,
    /// Response suppression level (`q`).
    quiet: u32,
    /// Pixel data format (`f`).
    format: u32,
    /// Transmission medium (`t`).
    medium: u8,
    /// Whether the data is zlib compressed (`o=z`).
    compressed: bool,
    /// Image width in pixels for raw data (`s`).
    width: u32,
    /// Image height in pixels for raw data (`v`).
    height: u32,
    /// Number of bytes to read from a file (`S`).
    data_size: u32,
    /// Offset to start reading a file at (`O`).
    data_offset: u32,
    /// Image ID (`i`).
    image_id: u32,
    /// Image number (`I`).
    image_number: u32,
    /// Placement ID (`p`).
    placement_id: u32,
    /// Whether more chunks will follow (`m`).
    more: bool,
    /// Source rectangle (`x`, `y`, `w`, `h`).
    source: (u32, u32, u32, u32),
    /// Pixel offset inside the first cell (`X`, `Y`).
    cell_offset: (u32, u32),
    /// Number of columns to display the image over (`c`).
    columns: u32,
    /// Number of rows to display the image over (`r`).
    rows: u32,
    /// Cursor movement policy (`C`).
    cursor_movement: u32,
    /// Whether to create a virtual placement for unicode placeholders (`U`).
    unicode_placeholder: bool,
    /// Stacking order (`z`).
    z_index: i32,
    /// Delete target (`d`).
    delete: u8,
}

impl Default for Command {
    fn default() -> Self {
        Self {
            action: b't',
            quiet: 0,
            format: 32,
            medium: b'd',
            compressed: false,
            width: 0,
            height: 0,
            data_size: 0,
            data_offset: 0,
            image_id: 0,
            image_number: 0,
            placement_id: 0,
            more: false,
            source: (0, 0, 0, 0),
            cell_offset: (0, 0),
            columns: 0,
            rows: 0,
            cursor_movement: 0,
            unicode_placeholder: false,
            z_index: 0,
            delete: b'a',
        }
    }
}

impl Command {
    /// Parse the control data and payload of an APC graphics command.
    ///
    /// The leading `G` must already be stripped.
    fn parse(bytes: &[u8]) -> GraphicsResult<(Self, &[u8])> {
        let (control, payload) = match bytes.iter().position(|&b| b == b';') {
            Some(index) => (&bytes[..index], &bytes[index + 1..]),
            None => (bytes, &[][..]),
        };

        let mut command = Self::default();
        for pair in control.split(|&b| b == b',').filter(|pair| !pair.is_empty()) {
            let (key, value) = match pair {
                [key, b'=', value @ ..] if !value.is_empty() => (*key, value),
                _ => return Err(GraphicsError::invalid("malformed control data")),
            };

            let char_value = || match value {
                [value] => Ok(*value),
                _ => Err(GraphicsError::invalid(format!("invalid value for key {}", key as char))),
            };
            let parse_number = || {
                str::from_utf8(value).ok().and_then(|value| value.parse::<u32>().ok()).ok_or_else(
                    || GraphicsError::invalid(format!("invalid value for key {}", key as char)),
                )
            };

            match key {
                b'a' => command.action = char_value()?,
                b'q' => command.quiet = parse_number()?,
                b'f' => command.format = parse_number()?,
                b't' => command.medium = char_value()?,
                b'o' => command.compressed = char_value()? == b'z',
                b's' => command.width = parse_number()?,
                b'v' => command.height = parse_number()?,
                b'S' => command.data_size = parse_number()?,
                b'O' => command.data_offset = parse_number()?,
                b'i' => command.image_id = parse_number()?,
                b'I' => command.image_number = parse_number()?,
                b'p' => command.placement_id = parse_number()?,
                b'm' => command.more = parse_number()? == 1,
                b'x' => command.source.0 = parse_number()?,
                b'y' => command.source.1 = parse_number()?,
                b'w' => command.source.2 = parse_number()?,
                b'h' => command.source.3 = parse_number()?,
                b'X' => command.cell_offset.0 = parse_number()?,
                b'Y' => command.cell_offset.1 = parse_number()?,
                b'c' => command.columns = parse_number()?,
                b'r' => command.rows = parse_number()?,
                b'C' => command.cursor_movement = parse_number()?,
                b'U' => command.unicode_placeholder = parse_number()? == 1,
                b'd' => command.delete = char_value()?,
                b'z' => {
                    command.z_index = str::from_utf8(value)
                        .ok()
                        .and_then(|value| value.parse::<i32>().ok())
                        .ok_or_else(|| GraphicsError::invalid("invalid value for key z"))?;
                },
                // Ignore keys for unsupported features, like animation and relative placements.
                _ => (),
            }
        }

        Ok((command, payload))
    }

    /// Whether the response for this command should be sent to the client.
    fn should_respond(&self, result: &GraphicsResult<()>) -> bool {
        // Clients opt into responses by specifying an image ID or number.
        if self.image_id == 0 && self.image_number == 0 {
            return false;
        }

        match result {
            Ok(()) => self.quiet == 0,
            Err(_) => self.quiet < 2,
        }
    }

    /// Format the response to this command.
    fn response(&self, result: &GraphicsResult<()>) -> String {
        let mut response = format!("\x1b_Gi={}", self.image_id);
        if self.image_number != 0 {
            response.push_str(&format!(",I={}", self.image_number));
        }
        if self.placement_id != 0 {
            response.push_str(&format!(",p={}", self.placement_id));
        }
        response.push(';');
        match result {
            Ok(()) => response.push_str("OK"),
            Err(err) => response.push_str(&err.to_string()),
        }
        response.push_str("\x1b\\");
        response
    }
}

/// Partially received chunked transmission.
#[derive(Debug)]
struct PendingTransmission {
    command: Command,
    /// Decoded payload received so far.
    data: Vec<u8>,
    /// Trailing base64 characters which could not be decoded yet.
    remainder: Vec<u8>,
    /// Error encountered while receiving the chunks.
    error: Option<GraphicsError>,
}

impl PendingTransmission {
    fn new(command: Command) -> Self {
        Self { command, data: Vec::new(), remainder: Vec::new(), error: None }
    }

    /// Add a base64 encoded chunk.
    fn push(&mut self, chunk: &[u8]) {
        if self.error.is_some() {
            return;
        }

        self.remainder.extend(chunk.iter().copied().filter(|b| !b.is_ascii_whitespace()));

        // Only decode complete groups of 4 characters, unless the chunk is padded.
        let len = if self.remainder.last() == Some(&b'=') {
            self.remainder.len()
        } else {
            self.remainder.len() / 4 * 4
        };

        if let Err(err) = BASE64.decode_vec(&self.remainder[..len], &mut self.data) {
            self.error = Some(GraphicsError::invalid(format!("invalid base64 data: {err}")));
            return;
        }
        self.remainder.drain(..len);

        if self.data.len() > MAX_TRANSMISSION_SIZE {
            self.error = Some(GraphicsError::new("EFBIG", "transmitted data is too large"));
            self.data = Vec::new();
        }
    }

    /// Get the complete decoded payload.
    fn finish(mut self) -> (Command, GraphicsResult<Vec<u8>>) {
        if self.error.is_none() && !self.remainder.is_empty() {
            let remainder = mem::take(&mut self.remainder);
            if let Err(err) = BASE64.decode_vec(&remainder, &mut self.data) {
                self.error = Some(GraphicsError::invalid(format!("invalid base64 data: {err}")));
            }
        }

        match self.error {
            Some(err) => (self.command, Err(err)),
            None => (self.command, Ok(self.data)),
        }
    }
}

/// Stored image.
#[derive(Debug)]
struct Image {
    data: Arc<ImageData>,
    /// Client specified image number.
    number: u32,
    /// Creation order, used to find the newest image with a number and for eviction.
    sequence: u64,
}

/// Image displayed in the grid.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Placement {
    image_id: u32,
    placement_id: u32,
    /// Grid line of the top left corner.
    line: i32,
    /// Grid column of the top left corner.
    column: usize,
    /// Source rectangle in image pixels.
    source: (u32, u32, u32, u32),
    /// Pixel offset inside the top left cell.
    cell_offset: (u32, u32),
    /// Requested number of columns, zero to derive it from the image size.
    columns: u32,
    /// Requested number of rows, zero to derive it from the image size.
    rows: u32,
    z_index: i32,
    /// Creation order, used to stack placements with identical z-index.
    sequence: u64,
}

impl Placement {
    /// Rendered size of the placement in pixels.
    fn pixel_size(&self, cell_size: (f32, f32)) -> (f32, f32) {
        let (width, height) = (self.source.2 as f32, self.source.3 as f32);
        match (self.columns, self.rows) {
            (0, 0) => (width, height),
            (columns, 0) => {
                let target_width = columns as f32 * cell_size.0;
                (target_width, height * target_width / width)
            },
            (0, rows) => {
                let target_height = rows as f32 * cell_size.1;
                (width * target_height / height, target_height)
            },
            (columns, rows) => (columns as f32 * cell_size.0, rows as f32 * cell_size.1),
        }
    }

    /// Number of columns and rows covered by the placement.
    fn cell_extent(&self, cell_size: (f32, f32)) -> (usize, usize) {
        let (width, height) = self.pixel_size(cell_size);
        let columns = ((width + self.cell_offset.0 as f32) / cell_size.0).ceil() as usize;
        let rows = ((height + self.cell_offset.1 as f32) / cell_size.1).ceil() as usize;
        (columns.max(1), rows.max(1))
    }

    /// Grid lines covered by the placement.
    fn lines(&self, cell_size: (f32, f32)) -> Range<i32> {
        let (_, rows) = self.cell_extent(cell_size);
        self.line..self.line + rows as i32
    }

    /// Check if the placement covers a cell.
    fn contains(&self, line: i32, column: usize, cell_size: (f32, f32)) -> bool {
        let (columns, _) = self.cell_extent(cell_size);
        self.lines(cell_size).contains(&line)
            && (self.column..self.column + columns).contains(&column)
    }
}

/// Images and placements of a single screen buffer.
#[derive(Debug, Default)]
struct ScreenGraphics {
    images: HashMap<u32, Image>,
    placements: Vec<Placement>,
    /// Placements displayed through unicode placeholders, their position is unused.
    virtual_placements: Vec<Placement>,
    used_bytes: usize,
    next_image_id: u32,
}

impl ScreenGraphics {
    /// Store an image, replacing any existing image with the same ID.
    fn insert_image(&mut self, id: u32, number: u32, data: ImageData, sequence: u64) {
        self.remove_image(id);

        self.used_bytes += data.pixels.len();
        self.images.insert(id, Image { data: Arc::new(data), number, sequence });

        self.enforce_quota(id);
    }

    /// Remove an image and all its placements.
    fn remove_image(&mut self, id: u32) {
        if let Some(image) = self.images.remove(&id) {
            self.used_bytes -= image.data.pixels.len();
            self.placements.retain(|placement| placement.image_id != id);
            self.virtual_placements.retain(|placement| placement.image_id != id);
        }
    }

    /// Evict images until the storage quota is satisfied.
    ///
    /// Images without placements are evicted first, the image with the ID `keep` is never evicted.
    fn enforce_quota(&mut self, keep: u32) {
        while self.used_bytes > STORAGE_QUOTA {
            let placed: HashSet<u32> =
                self.all_placements().map(|placement| placement.image_id).collect();
            let candidate = self
                .images
                .iter()
                .filter(|(id, _)| **id != keep)
                .min_by_key(|(id, image)| (placed.contains(id), image.sequence))
                .map(|(id, _)| *id);

            match candidate {
                Some(id) => self.remove_image(id),
                None => break,
            }
        }
    }

    /// Find an unused image ID.
    fn free_image_id(&mut self) -> u32 {
        loop {
            // Start high to make collisions with client chosen IDs unlikely.
            self.next_image_id = self.next_image_id.wrapping_add(1).max(1 << 31);
            if !self.images.contains_key(&self.next_image_id) {
                return self.next_image_id;
            }
        }
    }

    /// Get the ID of the newest image with the specified number.
    fn image_id_by_number(&self, number: u32) -> Option<u32> {
        self.images
            .iter()
            .filter(|(_, image)| image.number == number)
            .max_by_key(|(_, image)| image.sequence)
            .map(|(id, _)| *id)
    }

    /// Iterate over regular and virtual placements.
    fn all_placements(&self) -> impl Iterator<Item = &Placement> {
        self.placements.iter().chain(&self.virtual_placements)
    }

    /// Remove placements matching a predicate.
    ///
    /// Virtual placements are only considered if `include_virtual` is set.
    ///
    /// If `delete_images` is set, images which no longer have any placements and were either
    /// referenced by a removed placement or are part of `extra_images` are removed too.
    fn remove_placements<F>(
        &mut self,
        delete_images: bool,
        include_virtual: bool,
        extra_images: &[u32],
        mut predicate: F,
    ) where
        F: FnMut(&Placement) -> bool,
    {
        let mut affected: HashSet<u32> = extra_images.iter().copied().collect();
        let mut filter = |placement: &Placement| {
            let remove = predicate(placement);
            if remove {
                affected.insert(placement.image_id);
            }
            !remove
        };

        self.placements.retain(&mut filter);
        if include_virtual {
            self.virtual_placements.retain(&mut filter);
        }

        if delete_images {
            for id in affected {
                if !self.all_placements().any(|placement| placement.image_id == id) {
                    self.remove_image(id);
                }
            }
        }
    }

    /// Remove all placements touching the visible screen area.
    fn clear_screen(&mut self, screen_lines: usize, cell_size: (f32, f32)) {
        let screen = 0..screen_lines as i32;
        self.placements.retain(|placement| {
            let lines = placement.lines(cell_size);
            lines.end <= screen.start || lines.start >= screen.end
        });
    }

    /// Remove all placements which are completely inside the scrollback history.
    fn clear_history(&mut self, cell_size: (f32, f32)) {
        self.placements.retain(|placement| placement.lines(cell_size).end > 0);
    }

    /// Move placements in the scrolling region up.
    ///
    /// When the region starts at the top of the screen, placements are moved into the history
    /// instead of being removed, matching the grid's behavior.
    fn scroll_up(
        &mut self,
        region: &Range<Line>,
        lines: usize,
        history_size: usize,
        cell_size: (f32, f32),
    ) {
        let (start, end) = (region.start.0, region.end.0);
        let lines = lines as i32;

        self.placements.retain_mut(|placement| {
            if start == 0 {
                if placement.line >= end {
                    return true;
                }

                // Remove placements which have left the history completely.
                placement.line -= lines;
                placement.lines(cell_size).end > -(history_size as i32)
            } else if placement.line >= start && placement.line < end {
                // Remove placements scrolled out of a region with fixed lines at the top.
                placement.line -= lines;
                placement.line >= start
            } else {
                true
            }
        });
    }

    /// Move placements in the scrolling region down.
    fn scroll_down(&mut self, region: &Range<Line>, lines: usize) {
        let (start, end) = (region.start.0, region.end.0);
        let lines = lines as i32;

        self.placements.retain_mut(|placement| {
            if placement.line < start || placement.line >= end {
                return true;
            }

            placement.line += lines;
            placement.line < end
        });
    }

    /// Move all placements by a fixed number of lines.
    fn shift(&mut self, delta: i32, history_size: usize, cell_size: (f32, f32)) {
        self.placements.retain_mut(|placement| {
            placement.line += delta;
            placement.lines(cell_size).end > -(history_size as i32)
        });
    }

    /// Remove all images and placements.
    fn clear(&mut self) {
        self.images.clear();
        self.placements.clear();
        self.virtual_placements.clear();
        self.used_bytes = 0;
    }
}

/// Graphics protocol state of a terminal.
#[derive(Debug)]
pub(crate) struct Graphics {
    /// Graphics of the active screen buffer.
    active: ScreenGraphics,

    /// Graphics of the inactive screen buffer.
    inactive: ScreenGraphics,

    /// Chunked transmission in progress.
    pending: Option<PendingTransmission>,

    /// Cell dimensions in pixels.
    cell_size: (f32, f32),

    /// Counter used to order images and placements.
    sequence: u64,
}

impl Default for Graphics {
    fn default() -> Self {
        Self {
            active: Default::default(),
            inactive: Default::default(),
            pending: Default::default(),
            cell_size: DEFAULT_CELL_SIZE,
            sequence: Default::default(),
        }
    }
}

impl Graphics {
    /// Update the cell dimensions in pixels.
    pub fn set_cell_size(&mut self, width: f32, height: f32) {
        if width >= 1. && height >= 1. {
            self.cell_size = (width, height);
        }
    }

    /// Swap the active and inactive screen buffers.
    ///
    /// When switching to the alternate screen, its graphics are cleared.
    pub fn swap_alt(&mut self, entering_alt: bool) {
        if entering_alt {
            self.inactive.clear();
        }
        mem::swap(&mut self.active, &mut self.inactive);
    }

    /// Whether there are any placements on the active screen.
    pub fn has_placements(&self) -> bool {
        !self.active.placements.is_empty() || !self.active.virtual_placements.is_empty()
    }

    pub fn scroll_up(&mut self, region: &Range<Line>, lines: usize, history_size: usize) {
        self.active.scroll_up(region, lines, history_size, self.cell_size);
    }

    pub fn scroll_down(&mut self, region: &Range<Line>, lines: usize) {
        self.active.scroll_down(region, lines);
    }

    /// Adjust placements after the terminal was resized.
    pub fn resize(&mut self, delta: i32, active_history: usize, inactive_history: usize) {
        self.active.shift(delta, active_history, self.cell_size);
        self.inactive.shift(delta, inactive_history, self.cell_size);
    }

    pub fn clear_screen(&mut self, screen_lines: usize) {
        self.active.clear_screen(screen_lines, self.cell_size);
    }

    pub fn clear_history(&mut self) {
        self.active.clear_history(self.cell_size);
    }

    /// Remove all graphics from both screen buffers.
    pub fn reset(&mut self) {
        self.active.clear();
        self.inactive.clear();
        self.pending = None;
    }

    fn next_sequence(&mut self) -> u64 {
        self.sequence += 1;
        self.sequence
    }

    /// Placements visible in the viewport, ordered from bottom to top.
    pub fn renderable(&self, grid: &Grid<Cell>) -> Vec<RenderableGraphic> {
        let (cell_width, cell_height) = self.cell_size;
        let viewport_height = grid.screen_lines() as f32 * cell_height;
        let display_offset = grid.display_offset() as i32;

        // Collect graphics with their stacking order.
        let mut graphics: Vec<(i32, u64, RenderableGraphic)> = Vec::new();

        for placement in &self.active.placements {
            let Some(image) = self.active.images.get(&placement.image_id) else { continue };
            let (width, height) = placement.pixel_size(self.cell_size);
            let line = placement.line + display_offset;
            let x = placement.column as f32 * cell_width + placement.cell_offset.0 as f32;
            let y = line as f32 * cell_height + placement.cell_offset.1 as f32;

            if y >= viewport_height || y + height <= 0. {
                continue;
            }

            let (source_x, source_y, source_width, source_height) = placement.source;
            let graphic = RenderableGraphic {
                image: image.data.clone(),
                x,
                y,
                width,
                height,
                source: (
                    source_x as f32,
                    source_y as f32,
                    source_width as f32,
                    source_height as f32,
                ),
                z_index: placement.z_index,
            };
            graphics.push((placement.z_index, placement.sequence, graphic));
        }

        if !self.active.virtual_placements.is_empty() {
            self.placeholder_graphics(grid, &mut graphics);
        }

        graphics.sort_by_key(|(z_index, sequence, _)| (*z_index, *sequence));
        graphics.into_iter().map(|(_, _, graphic)| graphic).collect()
    }

    /// Collect the image slices displayed by unicode placeholder cells.
    fn placeholder_graphics(
        &self,
        grid: &Grid<Cell>,
        graphics: &mut Vec<(i32, u64, RenderableGraphic)>,
    ) {
        let mut last_placement: Option<(u32, u32, &Placement)> = None;
        // Index of the last graphic created from a placeholder, used to merge adjacent cells.
        let mut last_graphic: Option<usize> = None;

        for cell in placeholder::visible_placeholders(grid) {
            let Some(image) = self.active.images.get(&cell.image_id) else { continue };

            // Find the virtual placement, reusing the previous lookup when possible.
            let placement = match last_placement {
                Some((image_id, placement_id, placement))
                    if image_id == cell.image_id && placement_id == cell.placement_id =>
                {
                    placement
                },
                _ => {
                    let Some(placement) = self.virtual_placement(cell.image_id, cell.placement_id)
                    else {
                        continue;
                    };
                    last_placement = Some((cell.image_id, cell.placement_id, placement));
                    placement
                },
            };

            let Some(graphic) = self.placeholder_slice(&cell, placement, &image.data) else {
                continue;
            };

            // Merge with the slice of the cell to the left.
            if let Some(last) = last_graphic.and_then(|index| graphics.get_mut(index)) {
                let last = &mut last.2;
                let close = |a: f32, b: f32| (a - b).abs() < 0.01;
                if Arc::ptr_eq(&last.image, &graphic.image)
                    && close(last.y, graphic.y)
                    && close(last.height, graphic.height)
                    && close(last.source.1, graphic.source.1)
                    && close(last.source.3, graphic.source.3)
                    && close(last.x + last.width, graphic.x)
                    && close(last.source.0 + last.source.2, graphic.source.0)
                {
                    last.width += graphic.width;
                    last.source.2 += graphic.source.2;
                    continue;
                }
            }

            last_graphic = Some(graphics.len());
            graphics.push((placement.z_index, placement.sequence, graphic));
        }
    }

    /// Find the virtual placement for a placeholder.
    ///
    /// Without a placement ID, the newest virtual placement of the image is used.
    fn virtual_placement(&self, image_id: u32, placement_id: u32) -> Option<&Placement> {
        self.active
            .virtual_placements
            .iter()
            .filter(|placement| placement.image_id == image_id)
            .filter(|placement| placement_id == 0 || placement.placement_id == placement_id)
            .max_by_key(|placement| placement.sequence)
    }

    /// Compute the part of an image displayed by a single placeholder cell.
    fn placeholder_slice(
        &self,
        cell: &PlaceholderCell,
        placement: &Placement,
        image: &Arc<ImageData>,
    ) -> Option<RenderableGraphic> {
        let (cell_width, cell_height) = self.cell_size;
        let (source_x, source_y, source_width, source_height) = placement.source;
        let (source_width, source_height) = (source_width as f32, source_height as f32);

        // Without explicit size the image is displayed unscaled at the top left, otherwise it is
        // fit into the placement's cells and centered.
        let (scale, offset_x, offset_y) = if placement.columns == 0 && placement.rows == 0 {
            (1., 0., 0.)
        } else {
            let (columns, rows) = placement.cell_extent(self.cell_size);
            let box_width = columns as f32 * cell_width;
            let box_height = rows as f32 * cell_height;
            let scale = (box_width / source_width).min(box_height / source_height);
            let offset_x = (box_width - source_width * scale) / 2.;
            let offset_y = (box_height - source_height * scale) / 2.;
            (scale, offset_x, offset_y)
        };

        // Intersect the cell with the image, in placement pixel coordinates.
        let cell_left = cell.col as f32 * cell_width;
        let cell_top = cell.row as f32 * cell_height;
        let left = cell_left.max(offset_x);
        let top = cell_top.max(offset_y);
        let right = (cell_left + cell_width).min(offset_x + source_width * scale);
        let bottom = (cell_top + cell_height).min(offset_y + source_height * scale);
        if left >= right || top >= bottom {
            return None;
        }

        Some(RenderableGraphic {
            image: image.clone(),
            x: cell.column as f32 * cell_width + (left - cell_left),
            y: cell.line as f32 * cell_height + (top - cell_top),
            width: right - left,
            height: bottom - top,
            source: (
                source_x as f32 + (left - offset_x) / scale,
                source_y as f32 + (top - offset_y) / scale,
                (right - left) / scale,
                (bottom - top) / scale,
            ),
            z_index: placement.z_index,
        })
    }
}

impl<T: EventListener> Term<T> {
    /// Update the cell dimensions in pixels, used for image placement.
    pub fn set_cell_size(&mut self, width: f32, height: f32) {
        if (width, height) != self.graphics.cell_size {
            self.graphics.set_cell_size(width, height);
            if self.graphics.has_placements() {
                self.mark_fully_damaged();
            }
        }
    }

    /// Image placements visible in the viewport, ordered from bottom to top.
    pub fn renderable_graphics(&self) -> Vec<RenderableGraphic> {
        if !self.graphics.has_placements() {
            return Vec::new();
        }

        self.graphics.renderable(&self.grid)
    }

    /// Handle an APC string.
    pub(crate) fn graphics_apc(&mut self, payload: &[u8]) {
        match payload.split_first() {
            Some((b'G', command)) => self.graphics_command(command),
            _ => debug!("[unhandled apc] {} bytes", payload.len()),
        }
    }

    /// Handle a graphics protocol APC string, without the leading `G`.
    fn graphics_command(&mut self, bytes: &[u8]) {
        let (command, payload) = match Command::parse(bytes) {
            Ok(parsed) => parsed,
            Err(err) => {
                debug!("Invalid graphics command: {err}");
                return;
            },
        };

        // Handle continuation of a chunked transmission.
        if let Some(pending) = &mut self.graphics.pending {
            pending.push(payload);
            if command.more {
                return;
            }

            let (mut first_command, data) = self.graphics.pending.take().unwrap().finish();
            // Allow the last chunk to change the response verbosity.
            if command.quiet != 0 {
                first_command.quiet = command.quiet;
            }
            self.execute_graphics_command(first_command, data);
            return;
        }

        let transmits = matches!(command.action, b't' | b'T' | b'q');
        if transmits && command.more {
            let mut pending = PendingTransmission::new(command);
            pending.push(payload);
            self.graphics.pending = Some(pending);
            return;
        }

        let mut pending = PendingTransmission::new(command);
        if transmits {
            pending.push(payload);
        }
        let (command, data) = pending.finish();
        self.execute_graphics_command(command, data);
    }

    /// Execute a complete graphics command.
    fn execute_graphics_command(&mut self, mut command: Command, data: GraphicsResult<Vec<u8>>) {
        let result = match command.action {
            b't' | b'T' => data.and_then(|data| self.graphics_transmit(&mut command, data)),
            b'q' => data.and_then(|data| load_image(&command, data)).map(|_| ()),
            b'p' => self.graphics_place(&mut command),
            b'd' => {
                self.graphics_delete(&command);
                return;
            },
            b'f' | b'a' | b'c' => Err(GraphicsError::invalid("animation is not supported")),
            action => Err(GraphicsError::invalid(format!("unknown action {}", action as char))),
        };

        if let Err(err) = &result {
            debug!("Graphics command failed: {err}");
        }

        if command.should_respond(&result) {
            self.event_proxy.send_event(Event::PtyWrite(command.response(&result)));
        }
    }

    /// Transmit an image, displaying it for `a=T`.
    fn graphics_transmit(&mut self, command: &mut Command, data: Vec<u8>) -> GraphicsResult<()> {
        if command.image_id != 0 && command.image_number != 0 {
            return Err(GraphicsError::invalid("image ID and number are mutually exclusive"));
        }

        let image = load_image(command, data)?;

        if command.image_id == 0 {
            command.image_id = self.graphics.active.free_image_id();
        }

        let sequence = self.graphics.next_sequence();
        let screen = &mut self.graphics.active;
        screen.insert_image(command.image_id, command.image_number, image, sequence);

        if command.action == b'T' {
            self.graphics_place(command)?;
        }

        self.mark_fully_damaged();

        Ok(())
    }

    /// Display an image at the cursor position.
    fn graphics_place(&mut self, command: &mut Command) -> GraphicsResult<()> {
        if command.image_id == 0 && command.image_number != 0 {
            command.image_id = self
                .graphics
                .active
                .image_id_by_number(command.image_number)
                .ok_or_else(|| GraphicsError::new("ENOENT", "image not found"))?;
        }

        let image = match self.graphics.active.images.get(&command.image_id) {
            Some(image) => image.data.clone(),
            None => return Err(GraphicsError::new("ENOENT", "image not found")),
        };

        // Clamp source rectangle to the image.
        let (x, y, width, height) = command.source;
        let x = cmp::min(x, image.width - 1);
        let y = cmp::min(y, image.height - 1);
        let max_width = image.width - x;
        let max_height = image.height - y;
        let width = if width == 0 { max_width } else { cmp::min(width, max_width) };
        let height = if height == 0 { max_height } else { cmp::min(height, max_height) };

        let cursor = self.grid.cursor.point;
        let sequence = self.graphics.next_sequence();
        let placement = Placement {
            image_id: command.image_id,
            placement_id: command.placement_id,
            line: cursor.line.0,
            column: cursor.column.0,
            source: (x, y, width, height),
            cell_offset: command.cell_offset,
            columns: command.columns,
            rows: command.rows,
            z_index: command.z_index,
            sequence,
        };
        let (columns, rows) = placement.cell_extent(self.graphics.cell_size);

        // Replace existing placement with the same ID.
        let screen = &mut self.graphics.active;
        let placements = if command.unicode_placeholder {
            &mut screen.virtual_placements
        } else {
            &mut screen.placements
        };
        if command.placement_id != 0 {
            placements.retain(|p| {
                p.image_id != command.image_id || p.placement_id != command.placement_id
            });
        }
        placements.push(placement);

        // Virtual placements are only displayed through placeholders.
        if command.cursor_movement != 1 && !command.unicode_placeholder {
            self.graphics_move_cursor(columns, rows);
        }

        self.mark_fully_damaged();

        Ok(())
    }

    /// Move the cursor past a newly placed image.
    fn graphics_move_cursor(&mut self, columns: usize, rows: usize) {
        let cursor = self.grid.cursor.point;
        let mut column = cursor.column.0 + columns;
        let mut line = cursor.line.0 + rows as i32 - 1;

        if column >= self.columns() {
            column = 0;
            line += 1;
        }

        // Scroll the content, including the new placement, if the cursor leaves the region.
        let bottom = self.scroll_region.end.0 - 1;
        if cursor.line.0 <= bottom && line > bottom {
            self.scroll_up((line - bottom) as usize);
            line = bottom;
        }

        let line = cmp::min(line, self.screen_lines() as i32 - 1);
        self.grid.cursor.point.line = Line(line);
        self.grid.cursor.point.column = Column(column);
        self.grid.cursor.input_needs_wrap = false;
    }

    /// Delete images or placements.
    fn graphics_delete(&mut self, command: &Command) {
        let cell_size = self.graphics.cell_size;
        let screen_lines = self.screen_lines() as i32;
        let delete_images = command.delete.is_ascii_uppercase();
        let (x, y) = (command.source.0, command.source.1);
        // Cell coordinates in delete commands are one-based.
        let column = (x as usize).saturating_sub(1);
        let line = y as i32 - 1;
        let screen = &mut self.graphics.active;

        match command.delete.to_ascii_lowercase() {
            b'a' => screen.remove_placements(delete_images, true, &[], |placement| {
                let lines = placement.lines(cell_size);
                lines.start < screen_lines && lines.end > 0
            }),
            b'i' => {
                let id = command.image_id;
                let extra = if command.placement_id == 0 { vec![id] } else { Vec::new() };
                screen.remove_placements(delete_images, true, &extra, |placement| {
                    placement.image_id == id
                        && (command.placement_id == 0
                            || placement.placement_id == command.placement_id)
                });
            },
            b'n' => {
                let Some(id) = screen.image_id_by_number(command.image_number) else { return };
                let extra = if command.placement_id == 0 { vec![id] } else { Vec::new() };
                screen.remove_placements(delete_images, true, &extra, |placement| {
                    placement.image_id == id
                        && (command.placement_id == 0
                            || placement.placement_id == command.placement_id)
                });
            },
            b'c' => {
                let cursor = self.grid.cursor.point;
                screen.remove_placements(delete_images, false, &[], |placement| {
                    placement.contains(cursor.line.0, cursor.column.0, cell_size)
                });
            },
            b'p' => screen.remove_placements(delete_images, false, &[], |placement| {
                placement.contains(line, column, cell_size)
            }),
            b'q' => screen.remove_placements(delete_images, false, &[], |placement| {
                placement.z_index == command.z_index && placement.contains(line, column, cell_size)
            }),
            b'r' => {
                let range = x..=y;
                let ids: Vec<u32> =
                    screen.images.keys().copied().filter(|id| range.contains(id)).collect();
                screen.remove_placements(delete_images, true, &ids, |placement| {
                    range.contains(&placement.image_id)
                });
            },
            b'x' => screen.remove_placements(delete_images, false, &[], |placement| {
                let (columns, _) = placement.cell_extent(cell_size);
                (placement.column..placement.column + columns).contains(&column)
            }),
            b'y' => screen.remove_placements(delete_images, false, &[], |placement| {
                placement.lines(cell_size).contains(&line)
            }),
            b'z' => screen.remove_placements(delete_images, true, &[], |placement| {
                placement.z_index == command.z_index
            }),
            delete => {
                debug!("Unsupported graphics delete target {}", delete as char);
                return;
            },
        }

        self.mark_fully_damaged();
    }
}

/// Load and decode image data from the transmission medium.
fn load_image(command: &Command, data: Vec<u8>) -> GraphicsResult<ImageData> {
    let data = match command.medium {
        b'd' => data,
        b'f' => read_file(command, &data, false)?,
        b't' => read_file(command, &data, true)?,
        #[cfg(unix)]
        b's' => read_shared_memory(command, &data)?,
        medium => {
            let message = format!("unsupported transmission medium {}", medium as char);
            return Err(GraphicsError::invalid(message));
        },
    };

    let data = if command.compressed {
        miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&data, STORAGE_QUOTA)
            .map_err(|err| GraphicsError::invalid(format!("zlib decompression failed: {err}")))?
    } else {
        data
    };

    match command.format {
        24 | 32 => decode_raw(command, &data),
        100 => decode_png(&data),
        format => Err(GraphicsError::invalid(format!("unsupported format {format}"))),
    }
}

/// Validate image dimensions.
fn check_dimensions(width: u32, height: u32) -> GraphicsResult<()> {
    if width == 0 || height == 0 {
        Err(GraphicsError::invalid("image dimensions must not be zero"))
    } else if width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION {
        Err(GraphicsError::new("EFBIG", "image dimensions are too large"))
    } else {
        Ok(())
    }
}

/// Decode raw RGB or RGBA pixel data.
fn decode_raw(command: &Command, data: &[u8]) -> GraphicsResult<ImageData> {
    let (width, height) = (command.width, command.height);
    check_dimensions(width, height)?;

    let pixel_count = width as usize * height as usize;
    let bytes_per_pixel = if command.format == 24 { 3 } else { 4 };
    let Some(data) = data.get(..pixel_count * bytes_per_pixel) else {
        return Err(GraphicsError::new("ENODATA", "insufficient image data"));
    };

    let pixels = if bytes_per_pixel == 4 {
        data.to_vec()
    } else {
        let mut pixels = Vec::with_capacity(pixel_count * 4);
        for rgb in data.chunks_exact(3) {
            pixels.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
        pixels
    };

    Ok(ImageData::new(width, height, pixels))
}

/// Decode PNG image data.
fn decode_png(data: &[u8]) -> GraphicsResult<ImageData> {
    let png_error = |err: png::DecodingError| GraphicsError::invalid(format!("invalid PNG: {err}"));

    let limits = png::Limits { bytes: STORAGE_QUOTA };
    let mut decoder = png::Decoder::new_with_limits(data, limits);
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(png_error)?;

    let (width, height) = (reader.info().width, reader.info().height);
    check_dimensions(width, height)?;

    let mut buffer = vec![0; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut buffer).map_err(png_error)?;
    buffer.truncate(frame.buffer_size());

    let pixels = match frame.color_type {
        png::ColorType::Rgba => buffer,
        png::ColorType::Rgb => {
            buffer.chunks_exact(3).flat_map(|rgb| [rgb[0], rgb[1], rgb[2], 255]).collect()
        },
        png::ColorType::GrayscaleAlpha => {
            buffer.chunks_exact(2).flat_map(|ga| [ga[0], ga[0], ga[0], ga[1]]).collect()
        },
        png::ColorType::Grayscale => buffer.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => {
            return Err(GraphicsError::invalid("unexpected indexed PNG output"));
        },
    };

    if pixels.len() != width as usize * height as usize * 4 {
        return Err(GraphicsError::new("ENODATA", "insufficient image data"));
    }

    Ok(ImageData::new(width, height, pixels))
}

/// Read image data from a file.
///
/// Temporary files are deleted after reading.
fn read_file(command: &Command, path: &[u8], temporary: bool) -> GraphicsResult<Vec<u8>> {
    let path = str::from_utf8(path).map_err(|_| GraphicsError::invalid("invalid file path"))?;
    let path = Path::new(path);

    // Refuse to read from special filesystems, which could block or have side effects.
    let canonical = fs::canonicalize(path)?;
    if ["/proc", "/sys", "/dev"].iter().any(|dir| canonical.starts_with(dir)) {
        return Err(GraphicsError::new("EPERM", "reading from this path is not allowed"));
    }

    if temporary {
        // Only delete files which are clearly intended for this protocol.
        let in_temp_dir = canonical.starts_with(std::env::temp_dir())
            || canonical.starts_with("/tmp")
            || canonical.starts_with("/dev/shm");
        let name = canonical.to_string_lossy();
        if !in_temp_dir || !name.contains("tty-graphics-protocol") {
            return Err(GraphicsError::new("EPERM", "not a valid temporary file"));
        }
    }

    let result = read_file_range(&canonical, command.data_offset, command.data_size);

    if temporary {
        let _ = fs::remove_file(&canonical);
    }

    result
}

/// Read up to `size` bytes starting at `offset` from a regular file.
///
/// If `size` is zero, the file is read until the end.
fn read_file_range(path: &Path, offset: u32, size: u32) -> GraphicsResult<Vec<u8>> {
    let mut file = File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(GraphicsError::invalid("not a regular file"));
    }

    file.seek(SeekFrom::Start(offset.into()))?;
    let limit = if size == 0 { MAX_TRANSMISSION_SIZE as u64 + 1 } else { size.into() };

    let mut data = Vec::new();
    file.take(limit).read_to_end(&mut data)?;

    if data.len() > MAX_TRANSMISSION_SIZE {
        return Err(GraphicsError::new("EFBIG", "file is too large"));
    }

    Ok(data)
}

/// Read image data from a POSIX shared memory object, unlinking it afterwards.
#[cfg(unix)]
fn read_shared_memory(command: &Command, name: &[u8]) -> GraphicsResult<Vec<u8>> {
    use std::ffi::CString;
    use std::os::fd::FromRawFd;

    let name = CString::new(name).map_err(|_| GraphicsError::invalid("invalid shm name"))?;

    // SAFETY: The name is a valid nul-terminated string.
    let fd = unsafe { libc::shm_open(name.as_ptr(), libc::O_RDONLY, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error().into());
    }

    // SAFETY: The file descriptor was just opened and is exclusively owned by us.
    let mut file = unsafe { File::from_raw_fd(fd) };

    let result = (|| {
        file.seek(SeekFrom::Start(command.data_offset.into()))?;
        let size = file.metadata()?.len().saturating_sub(command.data_offset.into());
        let size = if command.data_size == 0 { size } else { size.min(command.data_size.into()) };
        if size > MAX_TRANSMISSION_SIZE as u64 {
            return Err(GraphicsError::new("EFBIG", "shared memory object is too large"));
        }

        let mut data = Vec::with_capacity(size as usize);
        file.take(size).read_to_end(&mut data)?;
        Ok(data)
    })();

    // SAFETY: The name is a valid nul-terminated string.
    unsafe { libc::shm_unlink(name.as_ptr()) };

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::event::VoidListener;
    use crate::index::Point;
    use crate::term::Config;
    use crate::term::test::TermSize;

    fn term() -> Term<VoidListener> {
        let size = TermSize::new(20, 10);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        term.set_cell_size(10., 20.);
        term
    }

    /// Transmit and display a `width`x`height` RGBA image.
    fn display(term: &mut Term<VoidListener>, control: &str, width: u32, height: u32) {
        let data = vec![255; (width * height * 4) as usize];
        let payload = BASE64.encode(data);
        let command = format!("a=T,f=32,s={width},v={height},{control};{payload}");
        term.graphics_command(command.as_bytes());
    }

    #[test]
    fn parse_command() {
        let (command, payload) =
            Command::parse(b"a=T,f=100,i=42,q=2,z=-5,c=3,r=2,m=1;AAAA").unwrap();
        assert_eq!(command.action, b'T');
        assert_eq!(command.format, 100);
        assert_eq!(command.image_id, 42);
        assert_eq!(command.quiet, 2);
        assert_eq!(command.z_index, -5);
        assert_eq!((command.columns, command.rows), (3, 2));
        assert!(command.more);
        assert_eq!(payload, b"AAAA");

        assert!(Command::parse(b"a=T,i=x").is_err());
        assert!(Command::parse(b"a").is_err());
    }

    #[test]
    fn response_format() {
        let command = Command { image_id: 7, image_number: 3, ..Default::default() };
        assert_eq!(command.response(&Ok(())), "\x1b_Gi=7,I=3;OK\x1b\\");

        let err = Err(GraphicsError::new("ENOENT", "image not found"));
        assert_eq!(command.response(&err), "\x1b_Gi=7,I=3;ENOENT:image not found\x1b\\");

        // No response without ID.
        assert!(!Command::default().should_respond(&Ok(())));

        let quiet = Command { image_id: 1, quiet: 1, ..Default::default() };
        assert!(!quiet.should_respond(&Ok(())));
        assert!(quiet.should_respond(&err));
    }

    #[test]
    fn transmit_and_display() {
        let mut term = term();
        display(&mut term, "i=1", 25, 30);

        let image = &term.graphics.active.images[&1];
        assert_eq!((image.data.width(), image.data.height()), (25, 30));

        // Image covers 3 columns and 2 lines, cursor moves past it.
        assert_eq!(term.grid.cursor.point, Point::new(Line(1), Column(3)));

        let graphics = term.renderable_graphics();
        assert_eq!(graphics.len(), 1);
        assert_eq!((graphics[0].x, graphics[0].y), (0., 0.));
        assert_eq!((graphics[0].width, graphics[0].height), (25., 30.));
    }

    #[test]
    fn chunked_transmission() {
        let mut term = term();
        let data = BASE64.encode(vec![128; 2 * 2 * 3]);
        let (first, second) = data.split_at(6);

        term.graphics_command(format!("a=t,f=24,s=2,v=2,i=5,m=1;{first}").as_bytes());
        assert!(term.graphics.active.images.is_empty());
        term.graphics_command(format!("m=0;{second}").as_bytes());

        let image = &term.graphics.active.images[&5];
        assert_eq!(image.data.pixels(), &[128, 128, 128, 255].repeat(4)[..]);
        assert!(term.graphics.active.placements.is_empty());
    }

    #[test]
    fn png_transmission() {
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 2, 1);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[255, 0, 0, 0, 255, 0]).unwrap();
        }

        let mut term = term();
        let command = format!("a=t,f=100,i=1;{}", BASE64.encode(png));
        term.graphics_command(command.as_bytes());

        let image = &term.graphics.active.images[&1];
        assert_eq!(image.data.pixels(), &[255, 0, 0, 255, 0, 255, 0, 255]);
    }

    #[test]
    fn compressed_transmission() {
        let raw = vec![7; 3 * 3 * 4];
        let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);

        let mut term = term();
        let command = format!("a=t,o=z,s=3,v=3,i=1;{}", BASE64.encode(compressed));
        term.graphics_command(command.as_bytes());

        assert_eq!(term.graphics.active.images[&1].data.pixels(), &raw[..]);
    }

    #[test]
    fn place_existing_image() {
        let mut term = term();
        term.graphics_command(format!("a=t,s=1,v=1,i=9;{}", BASE64.encode([0; 4])).as_bytes());
        assert!(term.graphics.active.placements.is_empty());

        term.grid.cursor.point = Point::new(Line(4), Column(2));
        term.graphics_command(b"a=p,i=9,p=1,c=4,r=2,C=1");
        term.graphics_command(b"a=p,i=9,p=1,c=4,r=3,C=1");

        // Placement with the same ID was replaced.
        assert_eq!(term.graphics.active.placements.len(), 1);
        let placement = &term.graphics.active.placements[0];
        assert_eq!((placement.line, placement.column), (4, 2));
        assert_eq!(placement.cell_extent(term.graphics.cell_size), (4, 3));

        // Cursor movement was suppressed.
        assert_eq!(term.grid.cursor.point, Point::new(Line(4), Column(2)));
    }

    #[test]
    fn scroll_into_history() {
        let mut term = term();
        display(&mut term, "i=1,C=1", 10, 20);

        term.scroll_up(3);
        assert_eq!(term.graphics.active.placements[0].line, -3);

        // Visible when scrolled back.
        assert!(term.renderable_graphics().is_empty());
        term.grid.scroll_display(crate::grid::Scroll::Delta(3));
        assert_eq!(term.renderable_graphics()[0].y, 0.);
    }

    #[test]
    fn scroll_region() {
        let mut term = term();
        term.grid.cursor.point = Point::new(Line(5), Column(0));
        display(&mut term, "i=1,C=1", 10, 20);

        // Region with fixed lines at the top drops placements leaving it.
        term.set_scrolling_region(3, Some(8));
        term.scroll_up(3);
        assert_eq!(term.graphics.active.placements[0].line, 2);
        term.scroll_up(1);
        assert!(term.graphics.active.placements.is_empty());
    }

    #[test]
    fn delete_placements() {
        let mut term = term();
        display(&mut term, "i=1,C=1", 10, 20);
        term.grid.cursor.point = Point::new(Line(5), Column(5));
        display(&mut term, "i=2,C=1", 10, 20);

        // Lowercase only removes the placement.
        term.graphics_command(b"a=d,d=i,i=1");
        assert_eq!(term.graphics.active.placements.len(), 1);
        assert!(term.graphics.active.images.contains_key(&1));

        // Uppercase also frees the image.
        term.graphics_command(b"a=d,d=P,x=6,y=6");
        assert!(term.graphics.active.placements.is_empty());
        assert!(!term.graphics.active.images.contains_key(&2));

        term.graphics_command(b"a=d,d=I,i=1");
        assert!(term.graphics.active.images.is_empty());
    }

    #[test]
    fn clear_screen_removes_placements() {
        let mut term = term();
        display(&mut term, "i=1,C=1", 10, 20);
        term.clear_screen(crate::vte::ansi::ClearMode::All);
        assert!(term.graphics.active.placements.is_empty());
        assert!(term.graphics.active.images.contains_key(&1));
    }

    #[test]
    fn alt_screen_graphics() {
        let mut term = term();
        display(&mut term, "i=1,C=1", 10, 20);

        term.swap_alt();
        assert!(term.renderable_graphics().is_empty());
        display(&mut term, "i=2,C=1", 10, 20);

        term.swap_alt();
        assert_eq!(term.graphics.active.placements[0].image_id, 1);
    }

    #[test]
    fn auto_assigned_id_by_number() {
        let mut term = term();
        display(&mut term, "I=13,C=1", 1, 1);
        let id = term.graphics.active.image_id_by_number(13).unwrap();
        assert_ne!(id, 0);

        term.graphics_command(b"a=d,d=N,I=13");
        assert!(term.graphics.active.images.is_empty());
    }

    /// Feed raw bytes through the parser, like a client would.
    fn feed(term: &mut Term<VoidListener>, bytes: &[u8]) {
        let mut parser: crate::vte::ansi::Processor = crate::vte::ansi::Processor::new();
        parser.advance(term, bytes);
    }

    /// Placeholder with row, column and ID byte diacritics.
    fn placeholder(row: u32, col: u32, msb: u32) -> String {
        use placeholder::{PLACEHOLDER, diacritic};
        format!("{PLACEHOLDER}{}{}{}", diacritic(row), diacritic(col), diacritic(msb))
    }

    #[test]
    fn unicode_placeholders() {
        let mut term = term();
        term.grid.cursor.point = Point::new(Line(1), Column(0));

        // Virtual placement of a 20x40 image, chunked like ratatui-image does it.
        let id = 5 << 24 | 7;
        let data = BASE64.encode(vec![255; 20 * 40 * 4]);
        let (first, second) = data.split_at(4096);
        let mut bytes = format!("\x1b_Gq=2,i={id},a=T,U=1,f=32,t=d,s=20,v=40,m=1;{first}\x1b\\");
        bytes.push_str(&format!("\x1b_Gq=2,m=0;{second}\x1b\\"));

        // Two rows, the second cell of each row inherits its position.
        let p = placeholder::PLACEHOLDER;
        bytes.push_str("\x1b[38;2;0;0;7m");
        bytes.push_str(&format!("{}{p}\r\n{}{p}", placeholder(0, 0, 5), placeholder(1, 0, 5)));
        feed(&mut term, bytes.as_bytes());

        // Placeholders are regular single width characters.
        assert_eq!(term.grid.cursor.point, Point::new(Line(2), Column(2)));
        assert!(term.graphics.active.placements.is_empty());

        // Each row is merged into a single slice.
        let graphics = term.renderable_graphics();
        assert_eq!(graphics.len(), 2);
        assert_eq!((graphics[0].x, graphics[0].y), (0., 20.));
        assert_eq!((graphics[0].width, graphics[0].height), (20., 20.));
        assert_eq!(graphics[0].source, (0., 0., 20., 20.));
        assert_eq!((graphics[1].x, graphics[1].y), (0., 40.));
        assert_eq!(graphics[1].source, (0., 20., 20., 20.));

        // Deleting the image removes the virtual placement.
        feed(&mut term, format!("\x1b_Ga=d,d=I,i={id}\x1b\\").as_bytes());
        assert!(term.renderable_graphics().is_empty());
        assert!(term.graphics.active.images.is_empty());
    }

    #[test]
    fn unicode_placeholder_fit() {
        let mut term = term();

        // A 10x40 image fit into 2x2 cells is centered horizontally.
        let data = BASE64.encode(vec![255; 10 * 40 * 4]);
        let mut bytes = format!("\x1b_Gq=2,i=3,a=T,U=1,s=10,v=40,c=2,r=2;{data}\x1b\\");
        bytes.push_str(&format!("\x1b[38;5;3m{}", placeholder(0, 0, 0)));
        feed(&mut term, bytes.as_bytes());

        let graphics = term.renderable_graphics();
        assert_eq!(graphics.len(), 1);
        assert_eq!((graphics[0].x, graphics[0].width), (5., 5.));
        assert_eq!(graphics[0].source, (0., 0., 5., 20.));
    }

    #[test]
    fn unicode_placeholder_without_placement() {
        let mut term = term();
        feed(&mut term, format!("\x1b[38;5;9m{}", placeholder(0, 0, 0)).as_bytes());
        assert!(term.renderable_graphics().is_empty());
    }
}
