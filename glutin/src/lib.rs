//! The purpose of this library is to provide an OpenGL [`Context`] on as many
//! platforms as possible.
//!
//! # Building a [`WindowedContext<T>`]
//!
//! A [`WindowedContext<T>`] is composed of a [`Window`] and an OpenGL
//! [`Context`].
//!
//! Due to some operating-system-specific quirks, glutin prefers control over
//! the order of creation of the [`Context`] and [`Window`]. Here is an example
//! of building a [`WindowedContext<T>`]:
//!
//! ```no_run
//! # fn main() {
//! let el = glutin::event_loop::EventLoop::new();
//! let wb = glutin::window::WindowBuilder::new()
//!     .with_title("Hello world!")
//!     .with_inner_size(glutin::dpi::LogicalSize::new(1024.0, 768.0));
//! let windowed_context = glutin::ContextBuilder::new()
//!     .build_windowed(wb, &el)
//!     .unwrap();
//! # }
//! ```
//!
//! You can, of course, create a [`RawContext<T>`] separately from an existing
//! window, however that may result in an suboptimal configuration of the window
//! on some platforms. In that case use the unsafe platform-specific
//! [`RawContextExt`] available on unix operating systems and Windows.
//!
//! You can also produce headless [`Context`]s via the
//! [`ContextBuilder::build_headless()`] function.
//!
//! [`Window`]: crate::window::Window
#![cfg_attr(
    target_os = "windows",
    doc = "\
[`RawContextExt`]: crate::platform::windows::RawContextExt
"
)]
#![cfg_attr(
    not(any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "windows",
        target_os = "openbsd",
    )),
    doc = "\
[`RawContextExt`]: crate::platform
"
)]
#![cfg_attr(
    any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
    ),
    doc = "\
[`RawContextExt`]: crate::platform::unix::RawContextExt
"
)]
#![deny(missing_debug_implementations)]
#![allow(clippy::missing_safety_doc, clippy::too_many_arguments)]
#![cfg_attr(feature = "cargo-clippy", deny(warnings))]

#[cfg(any(
    target_os = "windows",
    target_os = "linux",
    target_os = "android",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
))]
#[macro_use]
extern crate lazy_static;
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[macro_use]
extern crate objc;

pub mod platform;

mod api;
mod context;
mod platform_impl;
mod windowed;

pub use crate::context::*;
pub use crate::windowed::*;
pub use winit::*;

use winit::error::OsError;

use std::io;

/// An object that allows you to build [`Context`]s, [`RawContext<T>`]s and
/// [`WindowedContext<T>`]s.
///
/// One notable limitation of the Wayland backend when it comes to shared
/// [`Context`]s is that both contexts must use the same events loop.
#[derive(Debug, Clone)]
pub struct ContextBuilder<'a, T: ContextCurrentState> {
    /// The attributes to use to create the context.
    pub gl_attr: GlAttributes<&'a Context<T>>,
    /// The pixel format requirements
    pub pf_reqs: PixelFormatRequirements,
}

impl Default for ContextBuilder<'_, NotCurrent> {
    fn default() -> Self {
        Self { gl_attr: Default::default(), pf_reqs: Default::default() }
    }
}

impl<'a> ContextBuilder<'a, NotCurrent> {
    /// Initializes a new `ContextBuilder` with default values.
    pub fn new() -> Self {
        Default::default()
    }
}

impl<'a, T: ContextCurrentState> ContextBuilder<'a, T> {
    /// Sets how the backend should choose the OpenGL API and version.
    #[inline]
    pub fn with_gl(mut self, request: GlRequest) -> Self {
        self.gl_attr.version = request;
        self
    }

    /// Sets the desired OpenGL [`Context`] profile.
    #[inline]
    pub fn with_gl_profile(mut self, profile: GlProfile) -> Self {
        self.gl_attr.profile = Some(profile);
        self
    }

    /// Sets the *debug* flag for the OpenGL [`Context`].
    ///
    /// The default value for this flag is `cfg!(debug_assertions)`, which means
    /// that it's enabled when you run `cargo build` and disabled when you run
    /// `cargo build --release`.
    #[inline]
    pub fn with_gl_debug_flag(mut self, flag: bool) -> Self {
        self.gl_attr.debug = flag;
        self
    }

    /// Sets the robustness of the OpenGL [`Context`]. See the docs of
    /// [`Robustness`].
    #[inline]
    pub fn with_gl_robustness(mut self, robustness: Robustness) -> Self {
        self.gl_attr.robustness = robustness;
        self
    }

    /// Requests that the window has vsync enabled.
    ///
    /// By default, vsync is not enabled.
    #[inline]
    pub fn with_vsync(mut self, vsync: VSyncMode) -> Self {
        self.gl_attr.vsync = vsync;
        self
    }

    /// Share the display lists with the given [`Context`].
    #[inline]
    pub fn with_shared_lists<T2: ContextCurrentState>(
        self,
        other: &'a Context<T2>,
    ) -> ContextBuilder<'a, T2> {
        ContextBuilder { gl_attr: self.gl_attr.set_sharing(Some(other)), pf_reqs: self.pf_reqs }
    }

    /// Sets the multisampling level to request. A value of `0` indicates that
    /// multisampling must not be enabled.
    ///
    /// # Panic
    ///
    /// Will panic if `samples` is not a power of two.
    #[inline]
    pub fn with_multisampling(mut self, samples: u16) -> Self {
        self.pf_reqs.multisampling = match samples {
            0 => None,
            _ => {
                assert!(samples.is_power_of_two());
                Some(samples)
            }
        };
        self
    }

    /// Sets the number of bits in the depth buffer.
    #[inline]
    pub fn with_depth_buffer(mut self, bits: u8) -> Self {
        self.pf_reqs.depth_bits = Some(bits);
        self
    }

    /// Sets the number of bits in the stencil buffer.
    #[inline]
    pub fn with_stencil_buffer(mut self, bits: u8) -> Self {
        self.pf_reqs.stencil_bits = Some(bits);
        self
    }

    /// Sets the number of bits in the color buffer.
    #[inline]
    pub fn with_pixel_format(mut self, color_bits: u8, alpha_bits: u8) -> Self {
        self.pf_reqs.color_bits = Some(color_bits);
        self.pf_reqs.alpha_bits = Some(alpha_bits);
        self
    }

    /// Request the backend to be stereoscopic.
    #[inline]
    pub fn with_stereoscopy(mut self) -> Self {
        self.pf_reqs.stereoscopy = true;
        self
    }

    /// Sets whether sRGB should be enabled on the window.
    ///
    /// The default value is [`true`].
    #[inline]
    pub fn with_srgb(mut self, srgb_enabled: bool) -> Self {
        self.pf_reqs.srgb = srgb_enabled;
        self
    }

    /// Sets whether double buffering should be enabled.
    ///
    /// The default value is [`None`].
    ///
    /// ## Platform-specific
    ///
    /// This option will be taken into account on the following platforms:
    ///
    ///   * MacOS
    ///   * Unix operating systems using GLX with X
    ///   * Windows using WGL
    #[inline]
    pub fn with_double_buffer(mut self, double_buffer: Option<bool>) -> Self {
        self.pf_reqs.double_buffer = double_buffer;
        self
    }

    /// Sets whether hardware acceleration is required.
    ///
    /// The default value is `Some(true)`
    ///
    /// ## Platform-specific
    ///
    /// This option will be taken into account on the following platforms:
    ///
    ///   * MacOS
    ///   * Unix operating systems using EGL with either X or Wayland
    ///   * Windows using EGL or WGL
    ///   * Android using EGL
    #[inline]
    pub fn with_hardware_acceleration(mut self, acceleration: Option<bool>) -> Self {
        self.pf_reqs.hardware_accelerated = acceleration;
        self
    }
}

/// Error that can happen while creating a window or a headless renderer.
#[derive(Debug)]
pub enum CreationError {
    OsError(String),
    NotSupported(String),
    NoBackendAvailable(Box<dyn std::error::Error + Send + Sync>),
    RobustnessNotSupported,
    OpenGlVersionNotSupported,
    NoAvailablePixelFormat,
    PlatformSpecific(String),
    Window(OsError),
    /// We received multiple errors, instead of one.
    CreationErrors(Vec<Box<CreationError>>),
}

impl CreationError {
    #[cfg(any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
    ))]
    #[cfg(feature = "x11")]
    pub(crate) fn append(self, err: CreationError) -> Self {
        match self {
            CreationError::CreationErrors(mut errs) => {
                errs.push(Box::new(err));
                CreationError::CreationErrors(errs)
            }
            _ => CreationError::CreationErrors(vec![Box::new(err), Box::new(self)]),
        }
    }
}

impl std::fmt::Display for CreationError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error> {
        f.write_str(match self {
            CreationError::OsError(text)
            | CreationError::NotSupported(text)
            | CreationError::PlatformSpecific(text) => text,
            CreationError::NoBackendAvailable(err) => {
                return write!(f, "No backend is available: {}", err);
            }
            CreationError::RobustnessNotSupported => {
                "You requested robustness, but it is not supported."
            }
            CreationError::OpenGlVersionNotSupported => {
                "The requested OpenGL version is not supported."
            }
            CreationError::NoAvailablePixelFormat => {
                "Couldn't find any pixel format that matches the criteria."
            }
            CreationError::Window(err) => {
                return write!(f, "{}", err);
            }
            CreationError::CreationErrors(ref es) => {
                writeln!(f, "Received multiple errors:")?;
                for e in es {
                    writeln!(f, "\t{}", e)?;
                }
                return Ok(());
            }
        })
    }
}

impl std::error::Error for CreationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CreationError::NoBackendAvailable(err) => Some(&**err),
            CreationError::Window(err) => Some(err),
            _ => None,
        }
    }
}

impl From<OsError> for CreationError {
    fn from(err: OsError) -> Self {
        CreationError::Window(err)
    }
}

/// Error that can happen when manipulating an OpenGL [`Context`].
#[derive(Debug)]
pub enum ContextError {
    /// General platform error.
    OsError(String),
    IoError(io::Error),
    ContextLost,
    FunctionUnavailable,
}

impl std::fmt::Display for ContextError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error> {
        match self {
            ContextError::OsError(string) => write!(formatter, "{}", string),
            ContextError::IoError(err) => write!(formatter, "{}", err),
            ContextError::ContextLost => write!(formatter, "Context lost"),
            ContextError::FunctionUnavailable => write!(formatter, "Function unavailable"),
        }
    }
}

impl std::error::Error for ContextError {}

/// All APIs related to OpenGL that you can possibly get while using glutin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Api {
    /// The classical OpenGL. Available on Windows, Unix operating systems,
    /// OS/X.
    OpenGl,
    /// OpenGL embedded system. Available on Unix operating systems, Android.
    OpenGlEs,
    /// OpenGL for the web. Very similar to OpenGL ES.
    WebGl,
}

/// Describes the requested OpenGL [`Context`] profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlProfile {
    /// Include all the immediate more functions and definitions.
    Compatibility,
    /// Include all the future-compatible functions and definitions.
    Core,
}

/// Describes the OpenGL API and version that are being requested when a context
/// is created.
#[derive(Debug, Copy, Clone)]
pub enum GlRequest {
    /// Request the latest version of the "best" API of this platform.
    ///
    /// On desktop, will try OpenGL.
    Latest,

    /// Request a specific version of a specific API.
    ///
    /// Example: `GlRequest::Specific(Api::OpenGl, (3, 3))`.
    Specific(Api, (u8, u8)),

    /// If OpenGL is available, create an OpenGL [`Context`] with the specified
    /// `opengl_version`. Else if OpenGL ES or WebGL is available, create a
    /// context with the specified `opengles_version`.
    GlThenGles {
        /// The version to use for OpenGL.
        opengl_version: (u8, u8),
        /// The version to use for OpenGL ES.
        opengles_version: (u8, u8),
    },
}

impl GlRequest {
    /// Extract the desktop GL version, if any.
    pub fn to_gl_version(self) -> Option<(u8, u8)> {
        match self {
            GlRequest::Specific(Api::OpenGl, opengl_version) => Some(opengl_version),
            GlRequest::GlThenGles { opengl_version, .. } => Some(opengl_version),
            _ => None,
        }
    }
}

/// The minimum core profile GL context. Useful for getting the minimum
/// required GL version while still running on OSX, which often forbids
/// the compatibility profile features.
pub static GL_CORE: GlRequest = GlRequest::Specific(Api::OpenGl, (3, 2));

/// Specifies the tolerance of the OpenGL [`Context`] to faults. If you accept
/// raw OpenGL commands and/or raw shader code from an untrusted source, you
/// should definitely care about this.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Robustness {
    /// Not everything is checked. Your application can crash if you do
    /// something wrong with your shaders.
    NotRobust,

    /// The driver doesn't check anything. This option is very dangerous.
    /// Please know what you're doing before using it. See the
    /// `GL_KHR_no_error` extension.
    ///
    /// Since this option is purely an optimization, no error will be returned
    /// if the backend doesn't support it. Instead it will automatically
    /// fall back to [`NotRobust`][Self::NotRobust].
    NoError,

    /// Everything is checked to avoid any crash. The driver will attempt to
    /// avoid any problem, but if a problem occurs the behavior is
    /// implementation-defined. You are just guaranteed not to get a crash.
    RobustNoResetNotification,

    /// Same as [`RobustNoResetNotification`][Self::RobustNoResetNotification]
    /// but the context creation doesn't fail if it's not supported.
    TryRobustNoResetNotification,

    /// Everything is checked to avoid any crash. If a problem occurs, the
    /// context will enter a "context lost" state. It must then be
    /// recreated. For the moment, glutin doesn't provide a way to recreate
    /// a context with the same window :-/
    RobustLoseContextOnReset,

    /// Same as [`RobustLoseContextOnReset`][Self::RobustLoseContextOnReset]
    /// but the context creation doesn't fail if it's not supported.
    TryRobustLoseContextOnReset,
}

/// The behavior of the driver when you change the current context.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ReleaseBehavior {
    /// Doesn't do anything. Most notably doesn't flush.
    None,

    /// Flushes the context that was previously current as if `glFlush` was
    /// called.
    Flush,
}

/// Describes a possible format.
#[allow(missing_docs)]
#[derive(Debug, Clone)]
pub struct PixelFormat {
    pub hardware_accelerated: bool,
    /// The number of color bits. Does not include alpha bits.
    pub color_bits: u8,
    pub alpha_bits: u8,
    pub depth_bits: u8,
    pub stencil_bits: u8,
    pub stereoscopy: bool,
    pub double_buffer: bool,
    /// [`None`] if multisampling is disabled, otherwise `Some(N)` where `N` is
    /// the multisampling level.
    pub multisampling: Option<u16>,
    pub srgb: bool,
}

/// Describes how the backend should choose a pixel format.
// TODO: swap method? (swap, copy)
#[derive(Clone, Debug)]
pub struct PixelFormatRequirements {
    /// If true, only hardware-accelerated formats will be considered. If
    /// false, only software renderers. [`None`] means "don't care". Default
    /// is `Some(true)`.
    pub hardware_accelerated: Option<bool>,

    /// Minimum number of bits for the color buffer, excluding alpha. [`None`]
    /// means "don't care". The default is `Some(24)`.
    pub color_bits: Option<u8>,

    /// If true, the color buffer must be in a floating point format. Default
    /// is [`false`].
    ///
    /// Using floating points allows you to write values outside of the `[0.0,
    /// 1.0]` range.
    pub float_color_buffer: bool,

    /// Minimum number of bits for the alpha in the color buffer. [`None`] means
    /// "don't care". The default is `Some(8)`.
    pub alpha_bits: Option<u8>,

    /// Minimum number of bits for the depth buffer. [`None`] means "don't care".
    /// The default value is `Some(24)`.
    pub depth_bits: Option<u8>,

    /// Minimum number of stencil bits. [`None`] means "don't care".
    /// The default value is `Some(8)`.
    pub stencil_bits: Option<u8>,

    /// If true, only double-buffered formats will be considered. If false,
    /// only single-buffer formats. [`None`] means "don't care". The default
    /// is `Some(true)`.
    pub double_buffer: Option<bool>,

    /// Contains the minimum number of samples per pixel in the color, depth
    /// and stencil buffers. [`None`] means "don't care". Default is [`None`].
    /// A value of `Some(0)` indicates that multisampling must not be enabled.
    pub multisampling: Option<u16>,

    /// If true, only stereoscopic formats will be considered. If false, only
    /// non-stereoscopic formats. The default is [`false`].
    pub stereoscopy: bool,

    /// If true, only sRGB-capable formats will be considered. If false, don't
    /// care. The default is [`true`].
    pub srgb: bool,

    /// The behavior when changing the current context. Default is `Flush`.
    pub release_behavior: ReleaseBehavior,

    /// X11 only: set internally to ensure a certain visual xid is used when
    /// choosing the fbconfig.
    #[allow(dead_code)]
    pub(crate) x11_visual_xid: Option<std::os::raw::c_ulong>,
}

impl Default for PixelFormatRequirements {
    #[inline]
    fn default() -> PixelFormatRequirements {
        PixelFormatRequirements {
            hardware_accelerated: Some(true),
            color_bits: Some(24),
            float_color_buffer: false,
            alpha_bits: Some(8),
            depth_bits: Some(24),
            stencil_bits: Some(8),
            double_buffer: None,
            multisampling: None,
            stereoscopy: false,
            srgb: true,
            release_behavior: ReleaseBehavior::Flush,
            x11_visual_xid: None,
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub enum VSyncMode {
    Adaptive,
    On,
    Off,
    SwapInterval(i8),
}

impl VSyncMode {
    pub fn get_swap_interval(&self) -> i32 {
        match &self {
            VSyncMode::Adaptive => -1,
            VSyncMode::Off => 0,
            VSyncMode::On => 1,
            VSyncMode::SwapInterval(interval) => *interval as i32,
        }
    }
}

/// Attributes to use when creating an OpenGL [`Context`].
#[derive(Clone, Debug)]
pub struct GlAttributes<S> {
    /// An existing context with which some OpenGL objects get shared.
    ///
    /// The default is [`None`].
    pub sharing: Option<S>,

    /// Version to try create. See [`GlRequest`] for more infos.
    ///
    /// The default is [`GlRequest::Latest`].
    pub version: GlRequest,

    /// OpenGL profile to use.
    ///
    /// The default is [`None`].
    pub profile: Option<GlProfile>,

    /// Whether to enable the `debug` flag of the context.
    ///
    /// Debug contexts are usually slower but give better error reporting.
    ///
    /// The default is [`true`] in debug mode and [`false`] in release mode.
    pub debug: bool,

    /// How the OpenGL [`Context`] should detect errors.
    ///
    /// The default is `NotRobust` because this is what is typically expected
    /// when you create an OpenGL [`Context`]. However for safety you should
    /// consider [`Robustness::TryRobustLoseContextOnReset`].
    pub robustness: Robustness,

    /// Whether to use vsync. If vsync is enabled, calling
    /// [`ContextWrapper::swap_buffers()`] will block until the screen refreshes.
    /// This is typically used to prevent screen tearing.
    ///
    /// The default is [`VSyncMode::Off`].
    pub vsync: VSyncMode,
}

impl<S> GlAttributes<S> {
    /// Turns the `sharing` parameter into another type by calling a closure.
    #[inline]
    pub fn map_sharing<F, T>(self, f: F) -> GlAttributes<T>
    where
        F: FnOnce(S) -> T,
    {
        GlAttributes {
            sharing: self.sharing.map(f),
            version: self.version,
            profile: self.profile,
            debug: self.debug,
            robustness: self.robustness,
            vsync: self.vsync,
        }
    }

    /// Turns the `sharing` parameter into another type.
    #[inline]
    fn set_sharing<T>(self, sharing: Option<T>) -> GlAttributes<T> {
        GlAttributes {
            sharing,
            version: self.version,
            profile: self.profile,
            debug: self.debug,
            robustness: self.robustness,
            vsync: self.vsync,
        }
    }
}

impl<S> Default for GlAttributes<S> {
    #[inline]
    fn default() -> GlAttributes<S> {
        GlAttributes {
            sharing: None,
            version: GlRequest::Latest,
            profile: None,
            debug: cfg!(debug_assertions),
            robustness: Robustness::NotRobust,
            vsync: VSyncMode::Off,
        }
    }
}

// Rectangles to submit as buffer damage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
