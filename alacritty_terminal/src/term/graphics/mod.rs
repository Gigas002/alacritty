//! Terminal graphics support.
//!
//! This module is the only connection point between [`Term`] and the Kitty graphics protocol
//! implementation, the rest of the terminal only calls the hooks exposed here. When the
//! `kitty_graphics` feature is disabled, all hooks are no-ops.
//!
//! [`Term`]: crate::term::Term

#[cfg(feature = "kitty_graphics")]
mod kitty;
#[cfg(feature = "kitty_graphics")]
mod placeholder;
#[cfg(feature = "kitty_graphics")]
pub(crate) use kitty::Graphics;
#[cfg(feature = "kitty_graphics")]
pub use kitty::{ImageData, RenderableGraphic};
#[cfg(feature = "kitty_graphics")]
pub use placeholder::PLACEHOLDER;

#[cfg(not(feature = "kitty_graphics"))]
mod disabled;
#[cfg(not(feature = "kitty_graphics"))]
pub(crate) use disabled::Graphics;
#[cfg(not(feature = "kitty_graphics"))]
pub use disabled::{ImageData, RenderableGraphic};
