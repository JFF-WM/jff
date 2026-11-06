//! Dual Kawase blur on the GPU.
//!
//! Instead of one wide kernel, shrink the picture by half a few times and blow it back up.
//! Every shrink averages neighbouring pixels (blurrier image), the smaller images have fewer
//! pixels, so the whole thing costs far less than a wide Gaussian.
//!
//! ```text
//!   textures[0]  full size        <---------------------------+  (result)
//!   textures[1]  1/2 size   --down-->            --up--------+
//!   textures[2]  1/4 size   --down-->     --up---+
//!   textures[3]  1/8 size   (deepest, passes = 3)
//! ```
//!
//! `source` is blurred into `textures[1]`, that into `textures[2]`, and so on down to the
//! deepest, the upsample passes walk back up into `textures[0]`, which matches `source` in
//! size and holds the blur.
//!
//! More passes mean a bigger radius (doubles per pass). `offset` stretches the sample pattern
//! of each pass - stronger blur without extra passes, at the cost of artifacts when pushed.

use std::cmp::max;
use std::rc::Rc;

use anyhow::{ensure, Context as _};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::{ffi, link_program, GlesError, GlesRenderer, GlesTexture};
use smithay::backend::renderer::{ContextId, Renderer as _, Texture as _};
use smithay::gpu_span_location;
use smithay::utils::{Buffer, Size};

use crate::config;

/// Blur settings for one blur. Cheap to copy and compare, so effects can detect changes with
/// `==`.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct BlurOptions {
    /// Down/upsample rounds, clamped to `1..=31` when used.
    pub passes: u8,
    /// Stretch of the sample pattern, `1.0` is tightest.
    pub offset: f64,
}

impl BlurOptions {
    fn passes(self) -> usize {
        usize::from(self.passes.clamp(1, 31))
    }
}

impl From<config::Blur> for BlurOptions {
    fn from(config: config::Blur) -> Self {
        Self {
            passes: config.passes,
            offset: config.offset,
        }
    }
}

/// A linked GL program and its input locations.
#[derive(Debug)]
struct Program {
    id: ffi::types::GLuint,
    uniform_tex: ffi::types::GLint,
    uniform_half_pixel: ffi::types::GLint,
    uniform_offset: ffi::types::GLint,
    attrib_vert: ffi::types::GLint,
}

/// The two blur programs: one shrinks, one grows.
#[derive(Debug)]
struct Programs {
    down: Program,
    up: Program,
}

/// Compiled blur programs. Cloning is cheap, shared across all [`Blur`]s of a GL context.
#[derive(Debug, Clone)]
pub struct BlurProgram(Rc<Programs>);

unsafe fn compile_program(gl: &ffi::Gles2, fragment: &str) -> Result<Program, GlesError> {
    let id = unsafe { link_program(gl, include_str!("shaders/blur.vert"), fragment)? };

    let uniform = |name: &std::ffi::CStr| unsafe { gl.GetUniformLocation(id, name.as_ptr()) };
    Ok(Program {
        id,
        uniform_tex: uniform(c"tex"),
        uniform_half_pixel: uniform(c"half_pixel"),
        uniform_offset: uniform(c"offset"),
        attrib_vert: unsafe { gl.GetAttribLocation(id, c"vert".as_ptr()) },
    })
}

/// EGL-context user data: the programs, or `None` if compiling failed (so a broken driver
/// isn't asked again every frame).
struct CachedPrograms(Option<BlurProgram>);

impl BlurProgram {
    /// The programs of this renderer's GL context, compiled on first use and shared after.
    /// `None` if the driver rejected them.
    fn get_or_compile(renderer: &mut GlesRenderer) -> Option<Self> {
        if let Some(cached) = renderer.egl_context().user_data().get::<CachedPrograms>() {
            return cached.0.clone();
        }
        let compiled = Self::compile(renderer)
            .map_err(|err| tracing::warn!("error compiling the blur shaders: {err:#}"))
            .ok();
        let stored = compiled.clone();
        renderer
            .egl_context()
            .user_data()
            .insert_if_missing(|| CachedPrograms(stored));
        compiled
    }

    /// Compile both programs. Fails when the GL driver rejects a shader.
    pub fn compile(renderer: &mut GlesRenderer) -> anyhow::Result<Self> {
        renderer
            .with_context(|gl| unsafe {
                let down = compile_program(gl, include_str!("shaders/blur_down.frag"))
                    .context("error compiling the blur downsample shader")?;
                let up = compile_program(gl, include_str!("shaders/blur_up.frag"))
                    .context("error compiling the blur upsample shader")?;
                Ok(Self(Rc::new(Programs { down, up })))
            })
            .context("error making the GL context current")?
    }
}

/// A blur and its intermediate textures. One per blurred thing: textures are reused between
/// frames as long as the size stays the same.
#[derive(Debug)]
pub struct Blur {
    program: BlurProgram,
    /// Renderer whose GL context made the program and textures. Using them with another
    /// would draw garbage, so [`render`](Self::render) checks.
    context_id: ContextId<GlesTexture>,
    /// Result texture first, then intermediates, large to small.
    textures: Vec<GlesTexture>,
}

impl Blur {
    /// A blur for `renderer`, or `None` if its blur programs did not compile (driver too old
    /// or too limited - callers should skip blurring).
    pub fn new(renderer: &mut GlesRenderer) -> Option<Self> {
        let program = BlurProgram::get_or_compile(renderer)?;
        Some(Self {
            program,
            context_id: renderer.context_id(),
            textures: Vec::new(),
        })
    }

    /// Ensure the texture chain exists and matches `source` and `options`.
    ///
    /// Only allocates when something is missing, so calling every frame is cheap.
    /// `create_texture` allocates one texture of the given format and size.
    pub fn prepare_textures(
        &mut self,
        mut create_texture: impl FnMut(Fourcc, Size<i32, Buffer>) -> Result<GlesTexture, GlesError>,
        source: &GlesTexture,
        options: BlurOptions,
    ) -> anyhow::Result<()> {
        let passes = options.passes();
        let size = source.size();

        if let Some(result) = self.textures.first_mut() {
            let old_size = result.size();
            if old_size != size {
                tracing::trace!(
                    "blur: source size changed from {}x{} to {}x{}, recreating textures",
                    old_size.w,
                    old_size.h,
                    size.w,
                    size.h
                );
                self.textures.clear();
            } else if !result.is_unique_reference() {
                // Last frame's result is still held (drawn from a clone). Drawing into it
                // now would change a picture that is still in use.
                tracing::debug!("blur: result texture still referenced, recreating textures");
                self.textures.clear();
            }
        }

        // Texture `i` is the source size halved `i` times.
        let (mut w, mut h) = (size.w, size.h);
        for i in 0..=passes {
            let this_size = Size::new(w, h);
            w = max(1, w / 2);
            h = max(1, h / 2);

            if i < self.textures.len() {
                continue;
            }
            let texture = create_texture(Fourcc::Abgr8888, this_size)
                .context("error creating a blur texture")?;
            self.textures.push(texture);
        }

        // Fewer passes than before: drop the deepest textures.
        self.textures.truncate(passes + 1);
        Ok(())
    }

    /// Blur `source` and return the result (a texture the size of `source`).
    ///
    /// [`prepare_textures`](Self::prepare_textures) must have been called first with the same
    /// `source` size and `options`.
    pub fn render(
        &mut self,
        renderer: &mut GlesRenderer,
        source: &GlesTexture,
        options: BlurOptions,
    ) -> anyhow::Result<GlesTexture> {
        ensure!(
            renderer.context_id() == self.context_id,
            "blur used with a different renderer than the one that made it"
        );

        let passes = options.passes();
        let size = source.size();
        ensure!(
            self.textures.len() == passes + 1,
            "expected {} blur textures, have {}",
            passes + 1,
            self.textures.len()
        );
        ensure!(
            self.textures[0].size() == size,
            "blur result texture is {:?}, source is {size:?}",
            self.textures[0].size()
        );
        ensure!(
            self.textures[0].is_unique_reference(),
            "blur result texture is still referenced elsewhere"
        );

        let programs = &self.program.0;
        let textures = &self.textures;
        let offset = options.offset as f32;

        let gl_error =
            renderer.with_profiled_context(gpu_span_location!("Blur::render"), |gl| unsafe {
                // Clear stale errors so the check below reflects only our work.
                while gl.GetError() != ffi::NO_ERROR {}

                let saved = SavedGlState::capture(gl);

                gl.Disable(ffi::BLEND);
                gl.Disable(ffi::SCISSOR_TEST);
                gl.ActiveTexture(ffi::TEXTURE0);

                // One FBO, each pass reattaches a different texture.
                let mut fbo = 0;
                gl.GenFramebuffers(1, &mut fbo);
                gl.BindFramebuffer(ffi::FRAMEBUFFER, fbo);

                // Down: source -> textures[1] -> ... -> textures[passes].
                begin_program(gl, &programs.down, offset);
                for i in 0..passes {
                    let from = if i == 0 { source } else { &textures[i] };
                    let to = &textures[i + 1];
                    let to_size = to.size();
                    // Shrinking: footprint follows the *destination* pixel grid.
                    let half_pixel = [0.5 / to_size.w as f32, 0.5 / to_size.h as f32];
                    run_pass(gl, &programs.down, from, to, half_pixel);
                }
                end_program(gl, &programs.down);

                // Up: textures[passes] -> ... -> textures[1] -> textures[0].
                begin_program(gl, &programs.up, offset);
                for i in (0..passes).rev() {
                    let from = &textures[i + 1];
                    let to = &textures[i];
                    let from_size = from.size();
                    // Growing: footprint follows the *source* pixel grid.
                    let half_pixel = [0.5 / from_size.w as f32, 0.5 / from_size.h as f32];
                    run_pass(gl, &programs.up, from, to, half_pixel);
                }
                end_program(gl, &programs.up);

                gl.DeleteFramebuffers(1, &fbo);

                let error = gl.GetError();
                saved.restore(gl);
                error
            })?;

        ensure!(
            gl_error == ffi::NO_ERROR,
            "GL error 0x{gl_error:x} while blurring"
        );
        Ok(self.textures[0].clone())
    }
}

/// Full-screen quad as a triangle strip, in unit coordinates (see `blur.vert`).
static QUAD: [f32; 8] = [0., 0., 1., 0., 0., 1., 1., 1.];

/// Select `program` and set what stays the same across its passes.
unsafe fn begin_program(gl: &ffi::Gles2, program: &Program, offset: f32) {
    unsafe {
        gl.UseProgram(program.id);
        gl.Uniform1i(program.uniform_tex, 0);
        gl.Uniform1f(program.uniform_offset, offset);

        // Plain client-side vertex data, no buffer object.
        gl.BindBuffer(ffi::ARRAY_BUFFER, 0);
        gl.EnableVertexAttribArray(program.attrib_vert as u32);
        gl.VertexAttribPointer(
            program.attrib_vert as u32,
            2,
            ffi::FLOAT,
            ffi::FALSE,
            0,
            QUAD.as_ptr().cast(),
        );
    }
}

unsafe fn end_program(gl: &ffi::Gles2, program: &Program) {
    unsafe { gl.DisableVertexAttribArray(program.attrib_vert as u32) };
}

/// Draw `from` into `to` with the selected program, covering all of `to`.
unsafe fn run_pass(
    gl: &ffi::Gles2,
    program: &Program,
    from: &GlesTexture,
    to: &GlesTexture,
    half_pixel: [f32; 2],
) {
    let to_size = to.size();
    unsafe {
        gl.Viewport(0, 0, to_size.w, to_size.h);
        gl.Uniform2f(program.uniform_half_pixel, half_pixel[0], half_pixel[1]);

        gl.FramebufferTexture2D(
            ffi::FRAMEBUFFER,
            ffi::COLOR_ATTACHMENT0,
            ffi::TEXTURE_2D,
            to.tex_id(),
            0,
        );

        // Bilinear filtering is part of the algorithm: a sample between texel centres is
        // already an average. Clamping keeps the edge from bleeding in black.
        gl.BindTexture(ffi::TEXTURE_2D, from.tex_id());
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
        gl.TexParameteri(
            ffi::TEXTURE_2D,
            ffi::TEXTURE_WRAP_S,
            ffi::CLAMP_TO_EDGE as i32,
        );
        gl.TexParameteri(
            ffi::TEXTURE_2D,
            ffi::TEXTURE_WRAP_T,
            ffi::CLAMP_TO_EDGE as i32,
        );

        gl.DrawArrays(ffi::TRIANGLE_STRIP, 0, 4);
    }
}

/// The GL state the blur changes, so it can be restored afterwards.
///
/// The compositor is mid-frame when the blur runs, its drawing code expects its own
/// framebuffer, viewport, etc. to still be set up afterwards.
struct SavedGlState {
    framebuffer: i32,
    viewport: [i32; 4],
    program: i32,
    active_texture: i32,
    texture_unit0: i32,
    array_buffer: i32,
    blend: bool,
    scissor: bool,
}

impl SavedGlState {
    unsafe fn capture(gl: &ffi::Gles2) -> Self {
        unsafe {
            let get = |pname| {
                let mut value = 0;
                gl.GetIntegerv(pname, &mut value);
                value
            };

            let mut viewport = [0; 4];
            gl.GetIntegerv(ffi::VIEWPORT, viewport.as_mut_ptr());

            let active_texture = get(ffi::ACTIVE_TEXTURE);
            // Blur works on unit 0, remember what is bound there.
            gl.ActiveTexture(ffi::TEXTURE0);
            let texture_unit0 = get(ffi::TEXTURE_BINDING_2D);

            Self {
                framebuffer: get(ffi::FRAMEBUFFER_BINDING),
                viewport,
                program: get(ffi::CURRENT_PROGRAM),
                active_texture,
                texture_unit0,
                array_buffer: get(ffi::ARRAY_BUFFER_BINDING),
                blend: gl.IsEnabled(ffi::BLEND) == ffi::TRUE,
                scissor: gl.IsEnabled(ffi::SCISSOR_TEST) == ffi::TRUE,
            }
        }
    }

    unsafe fn restore(&self, gl: &ffi::Gles2) {
        unsafe {
            gl.BindFramebuffer(ffi::FRAMEBUFFER, self.framebuffer as u32);
            gl.Viewport(
                self.viewport[0],
                self.viewport[1],
                self.viewport[2],
                self.viewport[3],
            );
            gl.UseProgram(self.program as u32);
            gl.BindBuffer(ffi::ARRAY_BUFFER, self.array_buffer as u32);

            gl.ActiveTexture(ffi::TEXTURE0);
            gl.BindTexture(ffi::TEXTURE_2D, self.texture_unit0 as u32);
            gl.ActiveTexture(self.active_texture as u32);

            let set = |cap, on| {
                if on {
                    gl.Enable(cap)
                } else {
                    gl.Disable(cap)
                }
            };
            set(ffi::BLEND, self.blend);
            set(ffi::SCISSOR_TEST, self.scissor);
        }
    }
}
