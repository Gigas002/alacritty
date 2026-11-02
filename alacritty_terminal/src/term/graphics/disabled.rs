//! No-op graphics hooks, used when the `kitty_graphics` feature is disabled.

use std::ops::Range;

use log::debug;

use crate::index::Line;
use crate::term::Term;

/// Decoded image data.
///
/// This type can't be constructed without the `kitty_graphics` feature.
#[derive(Debug)]
pub enum ImageData {}

/// Image placement ready for rendering.
///
/// This type can't be constructed without the `kitty_graphics` feature.
#[derive(Debug, Clone)]
pub enum RenderableGraphic {}

#[derive(Debug, Default)]
pub(crate) struct Graphics;

impl Graphics {
    #[inline]
    pub fn swap_alt(&mut self, _entering_alt: bool) {}

    #[inline]
    pub fn scroll_up(&mut self, _region: &Range<Line>, _lines: usize, _history_size: usize) {}

    #[inline]
    pub fn scroll_down(&mut self, _region: &Range<Line>, _lines: usize) {}

    #[inline]
    pub fn resize(&mut self, _delta: i32, _active_history: usize, _inactive_history: usize) {}

    #[inline]
    pub fn clear_screen(&mut self, _screen_lines: usize) {}

    #[inline]
    pub fn clear_history(&mut self) {}

    #[inline]
    pub fn reset(&mut self) {}
}

impl<T> Term<T> {
    /// Update the cell dimensions in pixels, used for image placement.
    #[inline]
    pub fn set_cell_size(&mut self, _width: f32, _height: f32) {}

    /// Image placements visible in the viewport, ordered from bottom to top.
    #[inline]
    pub fn renderable_graphics(&self) -> Vec<RenderableGraphic> {
        Vec::new()
    }

    /// Handle an APC string.
    #[inline]
    pub(crate) fn graphics_apc(&mut self, payload: &[u8]) {
        debug!("[unhandled apc] {} bytes", payload.len());
    }
}
