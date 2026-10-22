extern crate cgmath;
#[macro_use]
extern crate glium;
extern crate image;
extern crate libc;
#[macro_use]
extern crate log;
extern crate mint;

pub mod config;
mod fxaa;
mod mesh;

use cgmath::EuclideanSpace;
use config::{AAMethod, Config};
use glium::backend::Facade;
use glium::glutin::dpi::PhysicalSize;
use glium::glutin::event_loop::{ControlFlow, EventLoop, EventLoopBuilder};
use glium::{glutin, CapabilitiesSource, Surface};
use image::{ImageEncoder, ImageFormat};
use libc::c_char;
use mesh::Mesh;
use std::error::Error;
use std::ffi::CStr;
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};
use std::{io, panic, ptr, thread, time};

#[cfg(target_os = "linux")]
use std::env;

// TODO: Move this stuff to config module
const CAM_FOV_DEG: f32 = 30.0;
const CAM_POSITION: cgmath::Point3<f32> = cgmath::Point3 {
    x: 2.0,
    y: -4.0,
    z: 2.0,
};

fn print_matrix(m: [[f32; 4]; 4]) {
    for row in &m {
        debug!("{:.3}\t{:.3}\t{:.3}\t{:.3}", row[0], row[1], row[2], row[3]);
    }
    debug!("");
}

fn print_context_info(display: &glium::backend::Context) {
    // Print context information
    info!("GL Version:   {:?}", display.get_opengl_version());
    info!("GL Version:   {}", display.get_opengl_version_string());
    info!("GLSL Version: {:?}", display.get_supported_glsl_version());
    info!("Vendor:       {}", display.get_opengl_vendor_string());
    info!("Renderer      {}", display.get_opengl_renderer_string());
    info!("Free GPU Mem: {:?}", display.get_free_video_memory());
    info!(
        "Depth Bits:   {:?}\n",
        display.get_capabilities().depth_bits
    );
}

fn create_normal_display(
    config: &Config,
    event_loop: &EventLoop<()>,
) -> Result<glium::Display, Box<dyn Error>> {
    let window_dim = PhysicalSize::new(config.width, config.height);
    let window = glutin::window::WindowBuilder::new()
        .with_title("stl-thumb")
        .with_inner_size(window_dim)
        .with_min_inner_size(window_dim)
        .with_max_inner_size(window_dim)
        .with_visible(config.visible);
    let cb = glutin::ContextBuilder::new().with_depth_buffer(24);
    //.with_multisampling(8);
    //.with_gl(glutin::GlRequest::Specific(glutin::Api::OpenGlEs, (2, 0)));
    let display = glium::Display::new(window, cb, event_loop)?;
    print_context_info(&display);
    Ok(display)
}

// winit allows only one event loop per process, so it can't be made per render.
// Instead one long-lived thread owns it and does all the offscreen rendering.
type RenderJob = Box<dyn FnOnce(Option<&EventLoop<()>>) + Send>;

fn render_thread() -> &'static Mutex<Sender<RenderJob>> {
    static RENDER_THREAD: OnceLock<Mutex<Sender<RenderJob>>> = OnceLock::new();
    RENDER_THREAD.get_or_init(|| {
        let (sender, receiver) = mpsc::channel::<RenderJob>();
        thread::Builder::new()
            .name("stl-thumb-render".to_string())
            .spawn(move || {
                // Workaround for issues with OpenGL 3.1 on Mesa 18.3.
                // Done once, before any GL, and never over a value the user set.
                #[cfg(target_os = "linux")]
                if env::var_os("MESA_GL_VERSION_OVERRIDE").is_none() {
                    env::set_var("MESA_GL_VERSION_OVERRIDE", "2.1");
                }
                let event_loop = create_event_loop();
                for job in receiver {
                    job(event_loop.as_ref());
                }
            })
            .expect("Unable to start render thread");
        Mutex::new(sender)
    })
}

#[cfg(target_os = "windows")]
fn create_event_loop() -> Option<EventLoop<()>> {
    use glium::glutin::platform::windows::EventLoopBuilderExtWindows;

    Some(EventLoopBuilder::new().with_any_thread(true).build())
}

#[cfg(target_os = "linux")]
fn create_event_loop() -> Option<EventLoop<()>> {
    use glium::glutin::platform::unix::EventLoopBuilderExtUnix;

    // If there is no X server or Wayland, creating the event loop panics.
    // Without one only osmesa software rendering is possible.
    // TODO: Submit PR upstream to stop panicing
    match panic::catch_unwind(|| EventLoopBuilder::new().with_any_thread(true).build()) {
        Ok(event_loop) => Some(event_loop),
        Err(e) => {
            warn!(
                "No Wayland or X server. Falling back to osmesa software rendering. Reason {:?}",
                panic_message(&e)
            );
            None
        }
    }
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

#[cfg(target_os = "windows")]
fn create_headless_display(
    config: &Config,
    event_loop: Option<&EventLoop<()>>,
) -> Result<glium::HeadlessRenderer, Box<dyn Error>> {
    let event_loop = event_loop.ok_or("No event loop")?;
    let size = PhysicalSize::new(config.width, config.height);
    let cb = glutin::ContextBuilder::new();
    let context = cb.build_headless(event_loop, size)?;

    let context = unsafe { context.treat_as_current() };
    let display = glium::backend::glutin::headless::Headless::new(context)?;
    print_context_info(&display);
    Ok(display)
}

#[cfg(target_os = "linux")]
fn create_headless_display(
    config: &Config,
    event_loop: Option<&EventLoop<()>>,
) -> Result<glium::HeadlessRenderer, Box<dyn Error>> {
    use glium::glutin::platform::unix::HeadlessContextExt;

    let size = PhysicalSize::new(config.width, config.height);
    let cb = glutin::ContextBuilder::new();

    // Linux requires an elaborate chain of attempts and fallbacks to find the ideal type of opengl context.
    let context: glium::glutin::Context<glium::glutin::NotCurrent> = match event_loop {
        Some(event_loop) => {
            // Try surfaceless, headless, and osmesa in that order
            // This is the procedure recommended in
            // https://github.com/rust-windowing/glutin/blob/bab33a84dfb094ff65c059400bed7993434638e2/glutin_examples/examples/headless.rs
            match cb.clone().build_surfaceless(event_loop) {
                Ok(c) => c,
                Err(e) => {
                    warn!("Unable to create surfaceless GL context. Trying headless instead. Reason: {:?}", e);
                    match cb.clone().build_headless(event_loop, size) {
                        Ok(c) => c,
                        Err(e) => {
                            warn!("Unable to create headless GL context. Trying osmesa software renderer instead. Reason: {:?}", e);
                            cb.build_osmesa(size)?
                        }
                    }
                }
            }
        }
        // No X server or Wayland, which doesn't stop osmesa
        None => cb.build_osmesa(size)?,
    };

    let context = unsafe { context.treat_as_current() };
    let display = glium::backend::glutin::headless::Headless::new(context)?;
    print_context_info(&display);
    Ok(display)
}

fn render_pipeline<F>(
    display: &F,
    config: &Config,
    mesh: &Mesh,
    framebuffer: &mut glium::framebuffer::SimpleFrameBuffer,
    texture: &glium::Texture2d,
) -> Result<image::DynamicImage, Box<dyn Error>>
where
    F: Facade,
{
    // Graphics Stuff
    // ==============

    let params = glium::DrawParameters {
        depth: glium::Depth {
            test: glium::draw_parameters::DepthTest::IfLess,
            write: true,
            ..Default::default()
        },
        backface_culling: glium::draw_parameters::BackfaceCullingMode::CullClockwise,
        ..Default::default()
    };

    // Load and compile shaders
    // ------------------------

    let vertex_shader_src = include_str!("shaders/model.vert");
    let pixel_shader_src = include_str!("shaders/model.frag");

    // TODO: Cache program binary
    let program = glium::Program::from_source(display, vertex_shader_src, pixel_shader_src, None);
    let program = match program {
        Ok(p) => p,
        Err(glium::CompilationError(err, _)) => {
            return Err(format!("Compiling shaders: {}", err).into());
        }
        Err(err) => return Err(err.into()),
    };

    // Send mesh data to GPU
    // ---------------------

    let vertex_buf = glium::VertexBuffer::new(display, &mesh.vertices)?;
    let normal_buf = glium::VertexBuffer::new(display, &mesh.normals)?;
    // Can use NoIndices here because STLs are dumb
    let indices = glium::index::NoIndices(glium::index::PrimitiveType::TrianglesList);

    // Setup uniforms
    // --------------

    // Transformation matrix (positions, scales and rotates model)
    let transform_matrix = mesh.scale_and_center();

    // View matrix (convert to positions relative to camera)
    let view_matrix = cgmath::Matrix4::look_at_rh(
        CAM_POSITION,
        cgmath::Point3::origin(),
        cgmath::Vector3::unit_z(),
    );
    debug!("View:");
    print_matrix(view_matrix.into());

    // Perspective matrix (give illusion of depth)
    let perspective_matrix = cgmath::perspective(
        cgmath::Deg(CAM_FOV_DEG),
        config.width as f32 / config.height as f32,
        0.1,
        1024.0,
    );
    debug!("Perspective:");
    print_matrix(perspective_matrix.into());

    // Direction of light source
    //let light_dir = [-1.4, 0.4, -0.7f32];
    let light_dir = [-1.1, 0.4, 1.0f32];

    let uniforms = uniform! {
        //model: Into::<[[f32; 4]; 4]>::into(transform_matrix),
        //view: Into::<[[f32; 4]; 4]>::into(view_matrix),
        modelview: Into::<[[f32; 4]; 4]>::into(view_matrix * transform_matrix),
        perspective: Into::<[[f32; 4]; 4]>::into(perspective_matrix),
        u_light: light_dir,
        ambient_color: config.material.ambient,
        diffuse_color: config.material.diffuse,
        specular_color: config.material.specular,
    };

    // Draw
    // ----

    // Create FXAA system
    let fxaa = fxaa::FxaaSystem::new(display);
    let fxaa_enable = matches!(config.aamethod, AAMethod::FXAA);

    let mut draw_result = Ok(());
    fxaa::draw(&fxaa, framebuffer, fxaa_enable, |target| {
        // Fills background color and clears depth buffer
        target.clear_color_and_depth(config.background, 1.0);
        draw_result = target.draw(
            (&vertex_buf, &normal_buf),
            indices,
            &program,
            &uniforms,
            &params,
        );
        // TODO: Shadows
    });
    draw_result?;

    // Convert Image
    // =============

    let pixels: glium::texture::RawImage2d<u8> = texture.read();
    let img = image::ImageBuffer::from_raw(config.width, config.height, pixels.data.into_owned())
        .ok_or("Rendered image has the wrong size")?;

    Ok(image::DynamicImage::ImageRgba8(img).flipv())
}

pub fn render_to_window(config: Config) -> Result<(), Box<dyn Error>> {
    // Get geometry from model file
    // ==========================
    let mesh = Mesh::load(&config.model_filename, config.recalc_normals)?;

    // Create GL context
    // =================
    let event_loop = EventLoop::new();
    let display = create_normal_display(&config, &event_loop)?;

    let sleep_time = time::Duration::from_millis(10);

    let texture = glium::Texture2d::empty(&display, config.width, config.height)?;
    let depthtexture =
        glium::texture::DepthTexture2d::empty(&display, config.width, config.height)?;

    event_loop.run(move |ev, _, control_flow| {
        *control_flow =
            glutin::event_loop::ControlFlow::WaitUntil(std::time::Instant::now() + sleep_time);
        let mut framebuffer = glium::framebuffer::SimpleFrameBuffer::with_depth_buffer(
            &display,
            &texture,
            &depthtexture,
        )
        .unwrap();

        match ev {
            glutin::event::Event::WindowEvent {
                event: glutin::event::WindowEvent::CloseRequested,
                ..
            } => {
                *control_flow = ControlFlow::Exit;
                return;
            }
            glutin::event::Event::NewEvents(glutin::event::StartCause::Init) => {
                if let Err(e) =
                    render_pipeline(&display, &config, &mesh, &mut framebuffer, &texture)
                {
                    error!("Application error: {}", e);
                    *control_flow = ControlFlow::Exit;
                    return;
                }
            }
            _ => (),
        }

        let mut target = display.draw();
        // Clearing with color_srgb false turns off sRGB conversion, which the model's
        // program left on. Otherwise the blit would brighten the image, unlike saved ones.
        target.clear(None, Some((0.0, 0.0, 0.0, 0.0)), false, None, None);
        target.blit_from_simple_framebuffer(
            &framebuffer,
            &glium::Rect {
                left: 0,
                bottom: 0,
                width: config.width,
                height: config.height,
            },
            &glium::BlitTarget {
                left: 0,
                bottom: 0,
                width: config.width as i32,
                height: config.height as i32,
            },
            glium::uniforms::MagnifySamplerFilter::Nearest,
        );
        target.finish().unwrap();
    });
}

pub fn render_to_image(config: &Config) -> Result<image::DynamicImage, Box<dyn Error>> {
    // Get geometry from model file
    // =========================
    let mesh = Mesh::load(&config.model_filename, config.recalc_normals)?;

    // Render on the render thread. A panic there (OpenGL problems) becomes an error here.
    let config = config.clone();
    let (result_sender, result_receiver) = mpsc::channel();
    let job: RenderJob = Box::new(move |event_loop| {
        let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            render_offscreen(&config, &mesh, event_loop).map_err(|e| e.to_string())
        }))
        .unwrap_or_else(|e| Err(format!("Renderer panicked: {}", panic_message(&e))));
        let _ = result_sender.send(result);
    });
    render_thread()
        .lock()
        .map_err(|_| "Render thread is unavailable")?
        .send(job)
        .map_err(|_| "Render thread has stopped")?;
    let img = result_receiver
        .recv()
        .map_err(|_| "Render thread has stopped")??;

    Ok(img)
}

fn render_offscreen(
    config: &Config,
    mesh: &Mesh,
    event_loop: Option<&EventLoop<()>>,
) -> Result<image::DynamicImage, Box<dyn Error>> {
    // Create GL context
    // =================
    // 1. If not visible create a headless context.
    // 2. If headless context creation fails, create a normal context with a hidden window.
    match create_headless_display(config, event_loop) {
        Ok(display) => {
            let texture = glium::Texture2d::empty(&display, config.width, config.height)?;
            let depthtexture =
                glium::texture::DepthTexture2d::empty(&display, config.width, config.height)?;
            let mut framebuffer = glium::framebuffer::SimpleFrameBuffer::with_depth_buffer(
                &display,
                &texture,
                &depthtexture,
            )?;
            render_pipeline(&display, config, mesh, &mut framebuffer, &texture)
        }
        Err(e) => {
            warn!(
                "Unable to create headless GL context. Trying hidden window instead. Reason: {:?}",
                e
            );
            let event_loop = event_loop.ok_or("No GL context and no display for a window")?;
            let display = create_normal_display(config, event_loop)?;
            let texture = glium::Texture2d::empty(&display, config.width, config.height)?;
            let depthtexture =
                glium::texture::DepthTexture2d::empty(&display, config.width, config.height)?;
            let mut framebuffer = glium::framebuffer::SimpleFrameBuffer::with_depth_buffer(
                &display,
                &texture,
                &depthtexture,
            )?;
            render_pipeline(&display, config, mesh, &mut framebuffer, &texture)
        }
    }
}

// JPEG has no transparency: blend transparent parts onto white
fn remove_alpha(img: &image::DynamicImage) -> image::DynamicImage {
    let rgba = img.to_rgba8();
    let rgb = image::RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let p = rgba.get_pixel(x, y).0;
        let a = p[3] as u32;
        image::Rgb([0, 1, 2].map(|i| ((p[i] as u32 * a + 255 * (255 - a) + 127) / 255) as u8))
    });
    image::DynamicImage::ImageRgb8(rgb)
}

pub fn render_to_file(config: &Config) -> Result<(), Box<dyn Error>> {
    let mut img = render_to_image(config)?;
    if config.format == ImageFormat::Jpeg {
        img = remove_alpha(&img);
    }

    // write_to() requires a seekable writer for performance reasons.
    // So we create an in-memory buffer and then dump that to the output.
    // I wonder if it would be better to use std::io::BufWriter for writing files instead.
    let mut buff: Vec<u8> = Vec::new();
    let mut cursor = io::Cursor::new(&mut buff);

    // Encode image with specified format
    // If encoding a PNG image, use fastest compression method
    // Not sure if this is really necessary. Fast is the default anyways.
    match config.format {
        ImageFormat::Png => {
            let encoder = image::codecs::png::PngEncoder::new_with_quality(
                &mut cursor,
                image::codecs::png::CompressionType::Fast,
                //image::codecs::png::CompressionType::Default,
                image::codecs::png::FilterType::Adaptive,
            );
            encoder.write_image(
                img.as_bytes(),
                img.width(),
                img.height(),
                img.color().into(),
            )?;
        }
        _ => img.write_to(&mut cursor, config.format.to_owned())?,
    }
    //img.write_to(&mut cursor, config.format.to_owned())?;

    // Choose output
    // Write to stdout if user did not specify a file
    let mut output: Box<dyn io::Write> = match config.img_filename.as_str() {
        "-" => Box::new(io::stdout()),
        _ => Box::new(std::fs::File::create(&config.img_filename)?),
    };
    output.write_all(&buff)?;
    output.flush()?;

    Ok(())
}

/// Allows utilizing `stl-thumb` from C-like languages
///
/// This function renders an image of the file `model_filename_c` and stores it into the buffer `buf_ptr`.
///
/// You must provide a memory buffer large enough to store the image. Images are written in 8-bit RGBA format,
/// so the buffer must be at least `width`*`height`*4 bytes in size. `model_filename_c` is a pointer to a C string with
/// the file path.
///
/// Returns `true` if succesful and `false` if unsuccesful.
///
/// # Example in C
/// ```c
/// const char* model_filename_c = "3DBenchy.stl";
/// int width = 256;
/// int height = 256;
///
/// size_t img_size = (size_t)width * height * 4;
/// uint8_t *buf_ptr = malloc(img_size);
///
/// render_to_buffer(buf_ptr, width, height, model_filename_c);
/// ```
///
/// # Safety
///
/// * `buf_ptr` _must_ point to a buffer that is valid for writes of `width * height * 4` bytes.
/// * `model_filename_c` must point to a valid null-terminated string.
#[no_mangle]
pub unsafe extern "C" fn render_to_buffer(
    buf_ptr: *mut u8,
    width: u32,
    height: u32,
    model_filename_c: *const c_char,
) -> bool {
    // Never let a panic unwind into the caller
    panic::catch_unwind(panic::AssertUnwindSafe(|| unsafe {
        render_to_buffer_inner(buf_ptr, width, height, model_filename_c)
    }))
    .unwrap_or_else(|e| {
        error!("Application error: {}", panic_message(&e));
        false
    })
}

unsafe fn render_to_buffer_inner(
    buf_ptr: *mut u8,
    width: u32,
    height: u32,
    model_filename_c: *const c_char,
) -> bool {
    // Check that the buffer pointer is valid
    if buf_ptr.is_null() {
        error!("Image buffer pointer is null");
        return false;
    };
    if width == 0 || height == 0 || width > config::MAX_SIZE || height > config::MAX_SIZE {
        error!(
            "Invalid image size {}x{} (each side must be 1 to {})",
            width,
            height,
            config::MAX_SIZE
        );
        return false;
    }
    let buf_size = width as usize * height as usize * 4;

    // Check validity of provided file path string
    let model_filename_cstr = unsafe {
        if model_filename_c.is_null() {
            error!("model file path pointer is null");
            return false;
        }
        CStr::from_ptr(model_filename_c)
    };
    let model_filename_str = match model_filename_cstr.to_str() {
        Ok(s) => s,
        Err(_) => {
            error!("Invalid model file path {:?}", model_filename_cstr);
            return false;
        }
    };

    // Setup configuration for the renderer
    let config = Config {
        model_filename: model_filename_str.to_string(),
        width,
        height,
        ..Default::default()
    };

    // Render
    let img = match render_to_image(&config) {
        Ok(img) => img,
        Err(e) => {
            error!("Application error: {}", e);
            return false;
        }
    };

    // Copy image to output buffer
    match img.as_rgba8() {
        Some(s) if s.len() == buf_size => unsafe {
            ptr::copy_nonoverlapping(s.as_ptr(), buf_ptr, buf_size)
        },
        _ => {
            error!("Unable to get image");
            return false;
        }
    }

    true
}

// TODO: Move tests to their own file
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::ErrorKind;

    #[test]
    fn cube_stl() {
        let img_filename = "cube-stl.png".to_string();
        let config = Config {
            model_filename: "test_data/cube.stl".to_string(),
            img_filename: img_filename.clone(),
            format: image::ImageFormat::Png,
            ..Default::default()
        };

        match fs::remove_file(&img_filename) {
            Ok(_) => (),
            Err(ref error) if error.kind() == ErrorKind::NotFound => (),
            Err(_) => {
                panic!("Couldn't clean files before testing");
            }
        }

        render_to_file(&config).expect("Error in render function");

        let size = fs::metadata(img_filename).expect("No file created").len();

        assert_ne!(0, size);
    }

    #[test]
    fn cube_obj() {
        let img_filename = "cube-obj.png".to_string();
        let config = Config {
            model_filename: "test_data/cube.obj".to_string(),
            img_filename: img_filename.clone(),
            format: image::ImageFormat::Png,
            ..Default::default()
        };

        match fs::remove_file(&img_filename) {
            Ok(_) => (),
            Err(ref error) if error.kind() == ErrorKind::NotFound => (),
            Err(_) => {
                panic!("Couldn't clean files before testing");
            }
        }

        render_to_file(&config).expect("Error in render function");

        let size = fs::metadata(img_filename).expect("No file created").len();

        assert_ne!(0, size);
    }

    #[test]
    fn cube_3mf() {
        let img_filename = "cube-3mf.png".to_string();
        let config = Config {
            model_filename: "test_data/cube.3mf".to_string(),
            img_filename: img_filename.clone(),
            format: image::ImageFormat::Png,
            ..Default::default()
        };

        match fs::remove_file(&img_filename) {
            Ok(_) => (),
            Err(ref error) if error.kind() == ErrorKind::NotFound => (),
            Err(_) => {
                panic!("Couldn't clean files before testing");
            }
        }

        render_to_file(&config).expect("Error in render function");

        let size = fs::metadata(img_filename).expect("No file created").len();

        assert_ne!(0, size);
    }
}
