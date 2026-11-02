//! Rendering of Kitty graphics protocol images.

use std::collections::HashMap;
use std::mem;
use std::sync::{Arc, Weak};

use ahash::RandomState;
use log::debug;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::graphics::{ImageData, PLACEHOLDER, RenderableGraphic};

use crate::display::SizeInfo;
use crate::display::content::RenderableCell;
use crate::gl;
use crate::gl::types::*;
use crate::renderer::shader::{ShaderProgram, ShaderVersion};
use crate::renderer::{self, Renderer};

const GRAPHICS_SHADER_V: &str = include_str!("../../../res/graphics.v.glsl");
const GRAPHICS_SHADER_F: &str = include_str!("../../../res/graphics.f.glsl");

/// Texture unit used for images.
const TEXTURE_UNIT: GLenum = 1;

#[derive(Debug, Copy, Clone)]
#[repr(C)]
struct Vertex {
    // Normalized screen coordinates.
    x: f32,
    y: f32,

    // Texture coordinates.
    u: f32,
    v: f32,
}

/// Image uploaded to the GPU.
#[derive(Debug)]
struct Texture {
    id: GLuint,

    /// Image data the texture was created from, used to detect when it is no longer needed.
    image: Weak<ImageData>,
}

impl Drop for Texture {
    fn drop(&mut self) {
        unsafe { gl::DeleteTextures(1, &self.id) };
    }
}

#[derive(Debug)]
pub struct GraphicsRenderer {
    vao: GLuint,
    vbo: GLuint,
    program: ShaderProgram,

    /// Uploaded textures by image UID.
    textures: HashMap<u64, Texture, RandomState>,

    /// Maximum supported texture width and height.
    max_texture_size: u32,
}

impl GraphicsRenderer {
    pub fn new(use_glsl3: bool) -> Result<Self, renderer::Error> {
        let shader_version = if use_glsl3 { ShaderVersion::Glsl3 } else { ShaderVersion::Gles2 };
        let program =
            ShaderProgram::new(shader_version, None, GRAPHICS_SHADER_V, GRAPHICS_SHADER_F)?;

        let mut vao: GLuint = 0;
        let mut vbo: GLuint = 0;
        let mut max_texture_size: GLint = 0;

        unsafe {
            gl::GetIntegerv(gl::MAX_TEXTURE_SIZE, &mut max_texture_size);

            gl::GenVertexArrays(1, &mut vao);
            gl::GenBuffers(1, &mut vbo);

            gl::BindVertexArray(vao);
            gl::BindBuffer(gl::ARRAY_BUFFER, vbo);

            let stride = mem::size_of::<Vertex>() as i32;

            // Position.
            gl::VertexAttribPointer(0, 2, gl::FLOAT, gl::FALSE, stride, std::ptr::null());
            gl::EnableVertexAttribArray(0);

            // Texture coordinates.
            let offset = 2 * mem::size_of::<f32>();
            gl::VertexAttribPointer(1, 2, gl::FLOAT, gl::FALSE, stride, offset as *const _);
            gl::EnableVertexAttribArray(1);

            gl::BindVertexArray(0);
            gl::BindBuffer(gl::ARRAY_BUFFER, 0);

            // Bind attribute locations explicitly for GLES2 shaders.
            gl::BindAttribLocation(program.id(), 0, c"aPos".as_ptr());
            gl::BindAttribLocation(program.id(), 1, c"aTexCoords".as_ptr());
            gl::LinkProgram(program.id());

            // Sample images from a dedicated texture unit, since the text renderer caches the
            // texture bound to unit 0.
            gl::UseProgram(program.id());
            if let Ok(location) = program.get_uniform_location(c"image") {
                gl::Uniform1i(location, TEXTURE_UNIT as GLint);
            }
            gl::UseProgram(0);
        }

        Ok(Self {
            vao,
            vbo,
            program,
            textures: Default::default(),
            max_texture_size: max_texture_size.max(1) as u32,
        })
    }

    /// Release textures of images which no longer exist.
    pub fn collect_garbage(&mut self) {
        self.textures.retain(|_, texture| texture.image.strong_count() > 0);
    }

    /// Draw images on top of the current framebuffer content.
    pub fn draw(&mut self, size_info: &SizeInfo, graphics: &[RenderableGraphic]) {
        if graphics.is_empty() {
            return;
        }

        let half_width = size_info.width() / 2.;
        let half_height = size_info.height() / 2.;

        // Images are clipped to the grid area.
        let clip_left = size_info.padding_x();
        let clip_top = size_info.padding_y();
        let clip_right = clip_left + size_info.columns() as f32 * size_info.cell_width();
        let clip_bottom = clip_top + size_info.screen_lines() as f32 * size_info.cell_height();

        unsafe {
            gl::Viewport(0, 0, size_info.width() as i32, size_info.height() as i32);
            gl::BlendFunc(gl::ONE, gl::ONE_MINUS_SRC_ALPHA);

            gl::UseProgram(self.program.id());
            gl::BindVertexArray(self.vao);
            gl::BindBuffer(gl::ARRAY_BUFFER, self.vbo);
            gl::ActiveTexture(gl::TEXTURE0 + TEXTURE_UNIT);
        }

        for graphic in graphics {
            let left = clip_left + graphic.x;
            let top = clip_top + graphic.y;
            let right = left + graphic.width;
            let bottom = top + graphic.height;

            // Compute visible area.
            let visible_left = left.max(clip_left);
            let visible_top = top.max(clip_top);
            let visible_right = right.min(clip_right);
            let visible_bottom = bottom.min(clip_bottom);
            if visible_left >= visible_right || visible_top >= visible_bottom {
                continue;
            }

            let Some(texture) = self.texture(&graphic.image) else { continue };

            // Map visible area to texture coordinates.
            let image_width = graphic.image.width() as f32;
            let image_height = graphic.image.height() as f32;
            let (source_x, source_y, source_width, source_height) = graphic.source;
            let u = |x: f32| {
                let fraction = (x - left) / graphic.width;
                (source_x + fraction * source_width) / image_width
            };
            let v = |y: f32| {
                let fraction = (y - top) / graphic.height;
                (source_y + fraction * source_height) / image_height
            };

            // Convert to normalized device coordinates.
            let x0 = visible_left / half_width - 1.;
            let x1 = visible_right / half_width - 1.;
            let y0 = -visible_top / half_height + 1.;
            let y1 = -visible_bottom / half_height + 1.;
            let (u0, u1) = (u(visible_left), u(visible_right));
            let (v0, v1) = (v(visible_top), v(visible_bottom));

            let vertices = [
                Vertex { x: x0, y: y0, u: u0, v: v0 },
                Vertex { x: x0, y: y1, u: u0, v: v1 },
                Vertex { x: x1, y: y0, u: u1, v: v0 },
                Vertex { x: x1, y: y0, u: u1, v: v0 },
                Vertex { x: x1, y: y1, u: u1, v: v1 },
                Vertex { x: x0, y: y1, u: u0, v: v1 },
            ];

            unsafe {
                gl::BindTexture(gl::TEXTURE_2D, texture);
                gl::BufferData(
                    gl::ARRAY_BUFFER,
                    mem::size_of_val(&vertices) as isize,
                    vertices.as_ptr() as *const _,
                    gl::STREAM_DRAW,
                );
                gl::DrawArrays(gl::TRIANGLES, 0, vertices.len() as i32);
            }
        }

        unsafe {
            gl::BindTexture(gl::TEXTURE_2D, 0);
            gl::ActiveTexture(gl::TEXTURE0);
            gl::BindBuffer(gl::ARRAY_BUFFER, 0);
            gl::BindVertexArray(0);
            gl::UseProgram(0);
        }
    }

    /// Get the texture for an image, uploading it if necessary.
    fn texture(&mut self, image: &Arc<ImageData>) -> Option<GLuint> {
        if let Some(texture) = self.textures.get(&image.uid()) {
            return Some(texture.id);
        }

        // Downscale images exceeding the GPU's texture size limit.
        let scaled;
        let (width, height, pixels) =
            if image.width() > self.max_texture_size || image.height() > self.max_texture_size {
                debug!(
                    "Downscaling {}x{} image to fit texture size limit of {}",
                    image.width(),
                    image.height(),
                    self.max_texture_size
                );
                scaled = downscale(image, self.max_texture_size);
                (scaled.0, scaled.1, &scaled.2[..])
            } else {
                (image.width(), image.height(), image.pixels())
            };

        let mut id: GLuint = 0;
        unsafe {
            gl::ActiveTexture(gl::TEXTURE0 + TEXTURE_UNIT);
            gl::GenTextures(1, &mut id);
            gl::BindTexture(gl::TEXTURE_2D, id);

            gl::PixelStorei(gl::UNPACK_ALIGNMENT, 1);
            gl::TexImage2D(
                gl::TEXTURE_2D,
                0,
                gl::RGBA as i32,
                width as i32,
                height as i32,
                0,
                gl::RGBA,
                gl::UNSIGNED_BYTE,
                pixels.as_ptr() as *const _,
            );

            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
        }

        self.textures.insert(image.uid(), Texture { id, image: Arc::downgrade(image) });

        Some(id)
    }
}

impl Renderer {
    /// Hide unicode placeholder characters, since the image is drawn in their place.
    pub fn hide_graphics_placeholders(cells: &mut [RenderableCell]) {
        for cell in cells.iter_mut().filter(|cell| cell.character == PLACEHOLDER) {
            cell.character = ' ';
            if let Some(extra) = &mut cell.extra {
                extra.zerowidth = None;
            }
        }
    }

    /// Draw images which are stacked below the text.
    pub fn draw_graphics_below(&mut self, size_info: &SizeInfo, graphics: &[RenderableGraphic]) {
        let split = graphics.partition_point(|graphic| graphic.z_index < 0);
        self.draw_graphics(size_info, &graphics[..split]);
    }

    /// Draw images which are stacked above the text.
    ///
    /// This also releases resources of images which no longer exist, so it must be called once
    /// per frame.
    pub fn draw_graphics_above(&mut self, size_info: &SizeInfo, graphics: &[RenderableGraphic]) {
        let split = graphics.partition_point(|graphic| graphic.z_index < 0);
        self.draw_graphics(size_info, &graphics[split..]);
        self.graphics_renderer.collect_garbage();
    }

    fn draw_graphics(&mut self, size_info: &SizeInfo, graphics: &[RenderableGraphic]) {
        if graphics.is_empty() {
            return;
        }

        self.graphics_renderer.draw(size_info, graphics);

        // Activate regular state again.
        unsafe {
            // Reset blending strategy.
            gl::BlendFunc(gl::SRC1_COLOR, gl::ONE_MINUS_SRC1_COLOR);

            // Restore viewport with padding.
            self.set_viewport(size_info);
        }
    }
}

impl Drop for GraphicsRenderer {
    fn drop(&mut self) {
        self.textures.clear();
        unsafe {
            gl::DeleteBuffers(1, &self.vbo);
            gl::DeleteVertexArrays(1, &self.vao);
        }
    }
}

/// Downscale an image with nearest neighbor sampling to fit within `max_size`.
fn downscale(image: &ImageData, max_size: u32) -> (u32, u32, Vec<u8>) {
    let scale = max_size as f64 / image.width().max(image.height()) as f64;
    let width = ((image.width() as f64 * scale) as u32).clamp(1, max_size);
    let height = ((image.height() as f64 * scale) as u32).clamp(1, max_size);

    let source = image.pixels();
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        let source_y = (y as u64 * image.height() as u64 / height as u64) as usize;
        for x in 0..width {
            let source_x = (x as u64 * image.width() as u64 / width as u64) as usize;
            let index = (source_y * image.width() as usize + source_x) * 4;
            pixels.extend_from_slice(&source[index..index + 4]);
        }
    }

    (width, height, pixels)
}
