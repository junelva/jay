use crate::drm::dma::DmaBuf;
use crate::drm::drm::Drm;
use crate::format::{Format, XRGB8888};
use crate::render::egl::context::EglContext;
use crate::render::egl::find_drm_device;
use crate::render::gl::program::GlProgram;
use crate::render::gl::render_buffer::GlRenderBuffer;
use crate::render::gl::sys::GLint;
use crate::render::gl::texture::GlTexture;
use crate::render::renderer::framebuffer::Framebuffer;
use crate::render::renderer::RENDERDOC;
use crate::render::{RenderError, Texture};
use renderdoc::{RenderDoc, V100};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use uapi::ustr;

pub struct RenderContext {
    pub(super) ctx: Rc<EglContext>,

    pub(super) renderdoc: Option<RefCell<RenderDoc<V100>>>,

    pub(super) tex_prog: GlProgram,
    pub(super) tex_prog_pos: GLint,
    pub(super) tex_prog_texcoord: GLint,
    pub(super) tex_prog_tex: GLint,

    pub(super) fill_prog: GlProgram,
    pub(super) fill_prog_pos: GLint,
    pub(super) fill_prog_color: GLint,
}

impl RenderContext {
    pub fn from_drm_device(drm: &Drm) -> Result<Self, RenderError> {
        let egl_dev = match find_drm_device(&drm)? {
            Some(d) => d,
            None => return Err(RenderError::UnknownDrmDevice),
        };
        let dpy = egl_dev.create_display()?;
        if !dpy.formats.contains_key(&XRGB8888.drm) {
            return Err(RenderError::XRGB888);
        }
        let ctx = dpy.create_context()?;
        ctx.with_current(|| unsafe { Self::new(&ctx) })
    }

    unsafe fn new(ctx: &Rc<EglContext>) -> Result<Self, RenderError> {
        let tex_prog = GlProgram::from_shaders(
            ctx,
            include_str!("../shaders/tex.vert.glsl"),
            include_str!("../shaders/tex.frag.glsl"),
        )?;
        let fill_prog = GlProgram::from_shaders(
            ctx,
            include_str!("../shaders/fill.vert.glsl"),
            include_str!("../shaders/fill.frag.glsl"),
        )?;
        Ok(Self {
            ctx: ctx.clone(),

            tex_prog_pos: tex_prog.get_attrib_location(ustr!("pos")),
            tex_prog_texcoord: tex_prog.get_attrib_location(ustr!("texcoord")),
            tex_prog_tex: tex_prog.get_uniform_location(ustr!("tex")),
            tex_prog,

            fill_prog_pos: fill_prog.get_attrib_location(ustr!("pos")),
            fill_prog_color: fill_prog.get_uniform_location(ustr!("color")),
            fill_prog,

            renderdoc: if RENDERDOC {
                Some(RefCell::new(RenderDoc::new().unwrap()))
            } else {
                None
            },
        })
    }

    pub fn dmabuf_fb(self: &Rc<Self>, buf: &DmaBuf) -> Result<Rc<Framebuffer>, RenderError> {
        self.ctx.with_current(|| unsafe {
            let img = self.ctx.dpy.import_dmabuf(buf)?;
            let rb = GlRenderBuffer::from_image(&img, &self.ctx)?;
            let fb = rb.create_framebuffer()?;
            Ok(Rc::new(Framebuffer {
                ctx: self.clone(),
                gl: fb,
            }))
        })
    }

    pub fn shmem_texture(
        self: &Rc<Self>,
        data: &[Cell<u8>],
        format: &'static Format,
        width: i32,
        height: i32,
        stride: i32,
    ) -> Result<Rc<Texture>, RenderError> {
        let gl = GlTexture::import_texture(&self.ctx, data, format, width, height, stride)?;
        Ok(Rc::new(Texture {
            ctx: self.clone(),
            gl,
        }))
    }
}