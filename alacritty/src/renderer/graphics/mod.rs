//! Rendering of terminal graphics.
//!
//! This module is the only connection point between the renderer and the Kitty graphics protocol
//! implementation. When the `kitty_graphics` feature is disabled, all hooks are no-ops.

#[cfg(feature = "kitty_graphics")]
mod kitty;
#[cfg(feature = "kitty_graphics")]
pub use kitty::GraphicsRenderer;

#[cfg(not(feature = "kitty_graphics"))]
pub use disabled::GraphicsRenderer;

#[cfg(not(feature = "kitty_graphics"))]
mod disabled {
    use alacritty_terminal::term::graphics::RenderableGraphic;

    use crate::display::SizeInfo;
    use crate::display::content::RenderableCell;
    use crate::renderer::{self, Renderer};

    #[derive(Debug, Copy, Clone)]
    pub struct GraphicsRenderer;

    impl GraphicsRenderer {
        pub fn new(_use_glsl3: bool) -> Result<Self, renderer::Error> {
            Ok(Self)
        }
    }

    impl Renderer {
        /// Hide unicode placeholder characters.
        #[inline]
        pub fn hide_graphics_placeholders(_: &mut [RenderableCell]) {}

        /// Draw images which are stacked below the text.
        #[inline]
        pub fn draw_graphics_below(&mut self, _: &SizeInfo, _: &[RenderableGraphic]) {}

        /// Draw images which are stacked above the text.
        #[inline]
        pub fn draw_graphics_above(&mut self, _: &SizeInfo, _: &[RenderableGraphic]) {
            let GraphicsRenderer = self.graphics_renderer;
        }
    }
}
