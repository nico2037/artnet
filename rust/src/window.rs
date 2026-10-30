use std::num::NonZeroU32;

use anyhow::{anyhow, Result};
use glutin::config::ConfigTemplateBuilder;
use glutin::context::{ContextApi, ContextAttributesBuilder, GlProfile, NotCurrentGlContext, PossiblyCurrentContext, Version};
use glutin::display::{GetGlDisplay, GlDisplay};
use glutin::surface::{GlSurface, Surface, SwapInterval, WindowSurface};
use glutin_winit::{DisplayBuilder, GlWindow};
use raw_window_handle::HasWindowHandle;
use winit::dpi::PhysicalSize;
use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

pub struct GlWin {
    pub window: Window,
    pub surface: Surface<WindowSurface>,
    pub context: PossiblyCurrentContext,
}

impl GlWin {
    pub fn swap(&self) -> Result<()> {
        self.surface.swap_buffers(&self.context)?;
        Ok(())
    }

    pub fn resize(&self, width: NonZeroU32, height: NonZeroU32) {
        self.surface.resize(&self.context, width, height);
    }
}

/// Window with an OpenGL 3.3 core context (forward-compatible on macOS), vsync off.
pub fn create(event_loop: &ActiveEventLoop, (w, h): (u32, u32), title: &str) -> Result<(GlWin, glow::Context)> {
    let attrs = Window::default_attributes()
        .with_title(title)
        .with_inner_size(PhysicalSize::new(w, h));
    let (window, config) = DisplayBuilder::new()
        .with_window_attributes(Some(attrs))
        .build(event_loop, ConfigTemplateBuilder::new(), |mut configs| configs.next().unwrap())
        .map_err(|e| anyhow!("creating window: {e}"))?;
    let window = window.ok_or_else(|| anyhow!("no window"))?;
    let display = config.display();

    let context_attrs = ContextAttributesBuilder::new()
        .with_context_api(ContextApi::OpenGl(Some(Version::new(3, 3))))
        .with_profile(GlProfile::Core)
        .build(Some(window.window_handle()?.as_raw()));
    let not_current = unsafe { display.create_context(&config, &context_attrs)? };
    let surface_attrs = window.build_surface_attributes(Default::default())?;
    let surface = unsafe { display.create_window_surface(&config, &surface_attrs)? };
    let context = not_current.make_current(&surface)?;
    surface.set_swap_interval(&context, SwapInterval::DontWait).ok();
    let size = window.inner_size();
    surface.resize(&context, NonZeroU32::new(size.width.max(1)).unwrap(), NonZeroU32::new(size.height.max(1)).unwrap());

    let gl = unsafe { glow::Context::from_loader_function_cstr(|s| display.get_proc_address(s)) };
    Ok((GlWin { window, surface, context }, gl))
}
