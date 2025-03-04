#![cfg(any(
    target_os = "windows",
    target_os = "linux",
    target_os = "android",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
))]

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::ops::{Deref, DerefMut};
use std::os::raw;
use std::sync::{Arc, Mutex};

use glutin_egl_sys as ffi;
use libloading;
#[cfg(unix)]
use libloading::os::unix as libloading_os;
#[cfg(windows)]
use libloading::os::windows as libloading_os;
#[cfg(any(
    target_os = "android",
    target_os = "windows",
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
))]
use winit::dpi;

use self::make_current_guard::MakeCurrentGuard;
use crate::api::dlloader::{SymTrait, SymWrapper};
#[cfg(not(target_os = "windows"))]
use crate::Rect;
use crate::{
    Api, ContextError, CreationError, GlAttributes, GlRequest, PixelFormat,
    PixelFormatRequirements, ReleaseBehavior, Robustness, VSyncError, VSyncMode,
};

#[derive(Clone)]
pub struct Egl(pub SymWrapper<ffi::egl::Egl>);

/// Because `*const raw::c_void` doesn't implement `Sync`.
unsafe impl Sync for Egl {}

type EglGetProcAddressType = libloading_os::Symbol<
    unsafe extern "C" fn(*const std::os::raw::c_void) -> *const std::os::raw::c_void,
>;

lazy_static! {
    static ref EGL_GET_PROC_ADDRESS: Arc<Mutex<Option<EglGetProcAddressType>>> =
        Arc::new(Mutex::new(None));
}

impl SymTrait for ffi::egl::Egl {
    fn load_with(lib: &libloading::Library) -> Self {
        let f = move |s: &'static str| -> *const std::os::raw::c_void {
            // Check if the symbol is available in the library directly. If
            // it is, just return it.
            if let Ok(sym) = unsafe {
                lib.get(std::ffi::CString::new(s.as_bytes()).unwrap().as_bytes_with_nul())
            } {
                return *sym;
            }

            let mut egl_get_proc_address = (*EGL_GET_PROC_ADDRESS).lock().unwrap();
            if egl_get_proc_address.is_none() {
                unsafe {
                    let sym: libloading::Symbol<
                        unsafe extern "C" fn(
                            *const std::os::raw::c_void,
                        )
                            -> *const std::os::raw::c_void,
                    > = lib.get(b"eglGetProcAddress").unwrap();
                    *egl_get_proc_address = Some(sym.into_raw());
                }
            }

            // The symbol was not available in the library, so ask
            // eglGetProcAddress for it. Note that eglGetProcAddress was
            // only able to look up extension functions prior to EGL 1.5,
            // hence this two-part dance.
            unsafe {
                (egl_get_proc_address.as_ref().unwrap())(
                    std::ffi::CString::new(s.as_bytes()).unwrap().as_bytes_with_nul().as_ptr()
                        as *const std::os::raw::c_void,
                )
            }
        };

        Self::load_with(f)
    }
}

impl Egl {
    pub fn new() -> Result<Self, ()> {
        #[cfg(target_os = "windows")]
        let paths = vec!["libEGL.dll", "atioglxx.dll"];

        #[cfg(not(target_os = "windows"))]
        let paths = vec!["libEGL.so.1", "libEGL.so"];

        SymWrapper::new(paths).map(Egl)
    }
}

mod make_current_guard;

impl Deref for Egl {
    type Target = ffi::egl::Egl;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Egl {
    fn deref_mut(&mut self) -> &mut ffi::egl::Egl {
        &mut self.0
    }
}

lazy_static! {
    pub static ref EGL: Option<Egl> = Egl::new().ok();
}

/// Specifies the type of display passed as `native_display`.
#[derive(Debug)]
#[allow(dead_code)]
pub enum NativeDisplay {
    /// [`None`] means `EGL_DEFAULT_DISPLAY`.
    X11(Option<ffi::EGLNativeDisplayType>),
    /// [`None`] means `EGL_DEFAULT_DISPLAY`.
    Gbm(Option<ffi::EGLNativeDisplayType>),
    /// [`None`] means `EGL_DEFAULT_DISPLAY`.
    Wayland(Option<ffi::EGLNativeDisplayType>),
    /// `EGL_DEFAULT_DISPLAY` is mandatory for Android.
    Android,
    // TODO: should be `EGLDeviceEXT`
    Device(ffi::EGLNativeDisplayType),
    /// Don't specify any display type. Useful on windows. [`None`] means
    /// `EGL_DEFAULT_DISPLAY`.
    Other(Option<ffi::EGLNativeDisplayType>),
}

#[derive(Debug)]
pub struct Context {
    display: ffi::egl::types::EGLDisplay,
    context: ffi::egl::types::EGLContext,
    surface: Option<parking_lot::Mutex<ffi::egl::types::EGLSurface>>,
    api: Api,
    pixel_format: PixelFormat,
    swap_interval_range: SwapIntervalRange,
}

#[derive(Debug)]
struct SwapIntervalRange(i32, i32);

fn get_egl_version(
    display: ffi::egl::types::EGLDisplay,
) -> Result<(ffi::egl::types::EGLint, ffi::egl::types::EGLint), CreationError> {
    unsafe {
        let egl = EGL.as_ref().unwrap();
        let mut major: ffi::egl::types::EGLint = std::mem::zeroed();
        let mut minor: ffi::egl::types::EGLint = std::mem::zeroed();

        if egl.Initialize(display, &mut major, &mut minor) == 0 {
            return Err(CreationError::OsError("eglInitialize failed".to_string()));
        }

        Ok((major, minor))
    }
}

unsafe fn bind_and_get_api<'a>(
    opengl: &'a GlAttributes<&'a Context>,
    egl_version: (ffi::egl::types::EGLint, ffi::egl::types::EGLint),
) -> Result<(Option<(u8, u8)>, Api), CreationError> {
    let egl = EGL.as_ref().unwrap();
    match opengl.version {
        GlRequest::Latest => {
            if egl_version >= (1, 4) {
                if egl.BindAPI(ffi::egl::OPENGL_API) != 0 {
                    Ok((None, Api::OpenGl))
                } else if egl.BindAPI(ffi::egl::OPENGL_ES_API) != 0 {
                    Ok((None, Api::OpenGlEs))
                } else {
                    Err(CreationError::OpenGlVersionNotSupported)
                }
            } else {
                Ok((None, Api::OpenGlEs))
            }
        }
        GlRequest::Specific(Api::OpenGlEs, version) => {
            if egl_version >= (1, 2) && egl.BindAPI(ffi::egl::OPENGL_ES_API) == 0 {
                return Err(CreationError::OpenGlVersionNotSupported);
            }
            Ok((Some(version), Api::OpenGlEs))
        }
        GlRequest::Specific(Api::OpenGl, version) => {
            if egl_version < (1, 4) {
                return Err(CreationError::OpenGlVersionNotSupported);
            }
            if egl.BindAPI(ffi::egl::OPENGL_API) == 0 {
                return Err(CreationError::OpenGlVersionNotSupported);
            }
            Ok((Some(version), Api::OpenGl))
        }
        GlRequest::Specific(_, _) => Err(CreationError::OpenGlVersionNotSupported),
        GlRequest::GlThenGles { opengles_version, opengl_version } => {
            if egl_version >= (1, 4) {
                if egl.BindAPI(ffi::egl::OPENGL_API) != 0 {
                    Ok((Some(opengl_version), Api::OpenGl))
                } else if egl.BindAPI(ffi::egl::OPENGL_ES_API) != 0 {
                    Ok((Some(opengles_version), Api::OpenGlEs))
                } else {
                    Err(CreationError::OpenGlVersionNotSupported)
                }
            } else {
                Ok((Some(opengles_version), Api::OpenGlEs))
            }
        }
    }
}

fn get_native_display(native_display: &NativeDisplay) -> *const raw::c_void {
    let egl = EGL.as_ref().unwrap();
    // the first step is to query the list of extensions without any display, if
    // supported
    let dp_extensions = unsafe {
        let p = egl.QueryString(ffi::egl::NO_DISPLAY, ffi::egl::EXTENSIONS as i32);

        // this possibility is available only with EGL 1.5 or
        // EGL_EXT_platform_base, otherwise `eglQueryString` returns an
        // error
        if p.is_null() {
            vec![]
        } else {
            let p = CStr::from_ptr(p);
            let list = String::from_utf8(p.to_bytes().to_vec()).unwrap_or_default();
            list.split(' ').map(|e| e.to_string()).collect::<Vec<_>>()
        }
    };

    let has_dp_extension = |e: &str| dp_extensions.iter().any(|s| s == e);

    match *native_display {
        // Note: Some EGL implementations are missing the
        // `eglGetPlatformDisplay(EXT)` symbol       despite reporting
        // `EGL_EXT_platform_base`. I'm pretty sure this is a bug.
        //       Therefore we detect whether the symbol is loaded in addition to
        // checking for       extensions.
        NativeDisplay::X11(display)
            if has_dp_extension("EGL_KHR_platform_x11") && egl.GetPlatformDisplay.is_loaded() =>
        {
            let d = display.unwrap_or(ffi::egl::DEFAULT_DISPLAY as *const _);
            // TODO: `PLATFORM_X11_SCREEN_KHR`
            unsafe {
                egl.GetPlatformDisplay(ffi::egl::PLATFORM_X11_KHR, d as *mut _, std::ptr::null())
            }
        }

        NativeDisplay::X11(display)
            if has_dp_extension("EGL_EXT_platform_x11")
                && egl.GetPlatformDisplayEXT.is_loaded() =>
        {
            let d = display.unwrap_or(ffi::egl::DEFAULT_DISPLAY as *const _);
            // TODO: `PLATFORM_X11_SCREEN_EXT`
            unsafe {
                egl.GetPlatformDisplayEXT(ffi::egl::PLATFORM_X11_EXT, d as *mut _, std::ptr::null())
            }
        }

        NativeDisplay::Gbm(display)
            if has_dp_extension("EGL_KHR_platform_gbm") && egl.GetPlatformDisplay.is_loaded() =>
        {
            let d = display.unwrap_or(ffi::egl::DEFAULT_DISPLAY as *const _);
            unsafe {
                egl.GetPlatformDisplay(ffi::egl::PLATFORM_GBM_KHR, d as *mut _, std::ptr::null())
            }
        }

        NativeDisplay::Gbm(display)
            if has_dp_extension("EGL_MESA_platform_gbm")
                && egl.GetPlatformDisplayEXT.is_loaded() =>
        {
            let d = display.unwrap_or(ffi::egl::DEFAULT_DISPLAY as *const _);
            unsafe {
                egl.GetPlatformDisplayEXT(ffi::egl::PLATFORM_GBM_KHR, d as *mut _, std::ptr::null())
            }
        }

        NativeDisplay::Wayland(display)
            if has_dp_extension("EGL_KHR_platform_wayland")
                && egl.GetPlatformDisplay.is_loaded() =>
        {
            let d = display.unwrap_or(ffi::egl::DEFAULT_DISPLAY as *const _);
            unsafe {
                egl.GetPlatformDisplay(
                    ffi::egl::PLATFORM_WAYLAND_KHR,
                    d as *mut _,
                    std::ptr::null(),
                )
            }
        }

        NativeDisplay::Wayland(display)
            if has_dp_extension("EGL_EXT_platform_wayland")
                && egl.GetPlatformDisplayEXT.is_loaded() =>
        {
            let d = display.unwrap_or(ffi::egl::DEFAULT_DISPLAY as *const _);
            unsafe {
                egl.GetPlatformDisplayEXT(
                    ffi::egl::PLATFORM_WAYLAND_EXT,
                    d as *mut _,
                    std::ptr::null(),
                )
            }
        }

        NativeDisplay::Android
            if has_dp_extension("EGL_KHR_platform_android")
                && egl.GetPlatformDisplay.is_loaded() =>
        unsafe {
            egl.GetPlatformDisplay(
                ffi::egl::PLATFORM_ANDROID_KHR,
                ffi::egl::DEFAULT_DISPLAY as *mut _,
                std::ptr::null(),
            )
        },

        NativeDisplay::Device(display)
            if has_dp_extension("EGL_EXT_platform_device")
                && egl.GetPlatformDisplay.is_loaded() =>
        unsafe {
            egl.GetPlatformDisplay(
                ffi::egl::PLATFORM_DEVICE_EXT,
                display as *mut _,
                std::ptr::null(),
            )
        },

        NativeDisplay::X11(Some(display))
        | NativeDisplay::Gbm(Some(display))
        | NativeDisplay::Wayland(Some(display))
        | NativeDisplay::Device(display)
        | NativeDisplay::Other(Some(display)) => unsafe { egl.GetDisplay(display as *mut _) },

        NativeDisplay::X11(None)
        | NativeDisplay::Gbm(None)
        | NativeDisplay::Wayland(None)
        | NativeDisplay::Android
        | NativeDisplay::Other(None) => unsafe {
            egl.GetDisplay(ffi::egl::DEFAULT_DISPLAY as *mut _)
        },
    }
}

#[allow(dead_code)] // Not all platforms use all
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceType {
    PBuffer,
    Window,
    Surfaceless,
}

impl Context {
    /// Start building an EGL context.
    ///
    /// This function initializes some things and chooses the pixel format.
    ///
    /// To finish the process, you must call [`ContextPrototype::finish()`].
    #[allow(clippy::new_ret_no_self)]
    pub fn new<'a, F>(
        pf_reqs: &PixelFormatRequirements,
        opengl: &'a GlAttributes<&'a Context>,
        native_display: NativeDisplay,
        surface_type: SurfaceType,
        config_selector: F,
    ) -> Result<ContextPrototype<'a>, CreationError>
    where
        F: FnMut(
            Vec<ffi::egl::types::EGLConfig>,
            ffi::egl::types::EGLDisplay,
        ) -> Result<ffi::egl::types::EGLConfig, ()>,
    {
        let egl = EGL.as_ref().unwrap();
        // calling `eglGetDisplay` or equivalent
        let display = get_native_display(&native_display);

        if display.is_null() {
            return Err(CreationError::OsError("Could not create EGL display object".to_string()));
        }

        let egl_version = get_egl_version(display)?;

        // the list of extensions supported by the client once initialized is
        // different from the list of extensions obtained earlier
        let extensions = if egl_version >= (1, 2) {
            let p =
                unsafe { CStr::from_ptr(egl.QueryString(display, ffi::egl::EXTENSIONS as i32)) };
            let list = String::from_utf8(p.to_bytes().to_vec()).unwrap_or_default();
            list.split(' ').map(|e| e.to_string()).collect::<Vec<_>>()
        } else {
            vec![]
        };

        // binding the right API and choosing the version
        let (version, api) = unsafe { bind_and_get_api(opengl, egl_version)? };

        let (config_id, pixel_format, swap_interval_range) = unsafe {
            choose_fbconfig(
                display,
                &egl_version,
                api,
                version,
                pf_reqs,
                surface_type,
                opengl,
                config_selector,
            )?
        };

        Ok(ContextPrototype {
            opengl,
            display,
            egl_version,
            extensions,
            api,
            version,
            config_id,
            pixel_format,
            swap_interval_range,
        })
    }

    unsafe fn check_make_current(&self, ret: Option<u32>) -> Result<(), ContextError> {
        let egl = EGL.as_ref().unwrap();
        if ret == Some(0) {
            match egl.GetError() as u32 {
                ffi::egl::CONTEXT_LOST => Err(ContextError::ContextLost),
                err => {
                    panic!("make_current: eglMakeCurrent failed (eglGetError returned 0x{:x})", err)
                }
            }
        } else {
            Ok(())
        }
    }

    pub unsafe fn make_current(&self) -> Result<(), ContextError> {
        let egl = EGL.as_ref().unwrap();
        let surface = self.surface.as_ref().map(|s| *s.lock()).unwrap_or(ffi::egl::NO_SURFACE);
        let ret = egl.MakeCurrent(self.display, surface, surface, self.context);

        self.check_make_current(Some(ret))
    }

    pub unsafe fn make_not_current(&self) -> Result<(), ContextError> {
        let egl = EGL.as_ref().unwrap();

        let surface_eq = if let Some(surface) = self.surface.as_ref().map(|s| *s.lock()) {
            egl.GetCurrentSurface(ffi::egl::DRAW as i32) == surface
                || egl.GetCurrentSurface(ffi::egl::READ as i32) == surface
        } else {
            false
        };

        if surface_eq || egl.GetCurrentContext() == self.context {
            let ret = egl.MakeCurrent(
                self.display,
                ffi::egl::NO_SURFACE,
                ffi::egl::NO_SURFACE,
                ffi::egl::NO_CONTEXT,
            );

            self.check_make_current(Some(ret))
        } else {
            self.check_make_current(None)
        }
    }

    #[inline]
    pub fn is_current(&self) -> bool {
        let egl = EGL.as_ref().unwrap();
        unsafe { egl.GetCurrentContext() == self.context }
    }

    #[inline]
    pub fn get_api(&self) -> Api {
        self.api
    }

    pub fn supports_vsync_mode(&self, mode: VSyncMode) -> bool {
        let swap_interval = mode.get_swap_interval();
        let SwapIntervalRange(min, max) = self.swap_interval_range;
        swap_interval >= min && swap_interval <= max
    }

    pub fn set_vsync_mode(&self, mode: VSyncMode) -> Result<(), VSyncError> {
        unsafe {
            let surface = self.surface.as_ref().map(|s| *s.lock()).unwrap_or(ffi::egl::NO_SURFACE);
            let _guard = MakeCurrentGuard::new(self.display, surface, surface, self.context)
                .map_err(|e| VSyncError::ContextError(ContextError::OsError(e)))?;

            let egl = EGL.as_ref().unwrap();
            if egl.SwapInterval(self.display, mode.get_swap_interval())
                == ffi::egl::FALSE
            {
                panic!("finish_impl: eglSwapInterval failed: 0x{:x}", egl.GetError());
            }

            Ok(())
        }
    }

    #[inline]
    pub unsafe fn raw_handle(&self) -> ffi::egl::types::EGLContext {
        self.context
    }

    #[inline]
    pub unsafe fn get_egl_display(&self) -> ffi::egl::types::EGLDisplay {
        self.display
    }

    #[inline]
    pub fn get_proc_address(&self, addr: &str) -> *const core::ffi::c_void {
        let egl = EGL.as_ref().unwrap();
        let addr = CString::new(addr.as_bytes()).unwrap();
        let addr = addr.as_ptr();
        unsafe { egl.GetProcAddress(addr) as *const _ }
    }

    #[inline]
    pub fn swap_buffers(&self) -> Result<(), ContextError> {
        let egl = EGL.as_ref().unwrap();
        let surface = self.surface.as_ref().unwrap().lock();
        if *surface == ffi::egl::NO_SURFACE {
            return Err(ContextError::ContextLost);
        }

        let ret = unsafe { egl.SwapBuffers(self.display, *surface) };

        if ret == 0 {
            match unsafe { egl.GetError() } as u32 {
                ffi::egl::CONTEXT_LOST => Err(ContextError::ContextLost),
                err => {
                    panic!("swap_buffers: eglSwapBuffers failed (eglGetError returned 0x{:x})", err)
                }
            }
        } else {
            Ok(())
        }
    }

    #[inline]
    #[cfg(not(target_os = "windows"))]
    pub fn swap_buffers_with_damage(&self, rects: &[Rect]) -> Result<(), ContextError> {
        let egl = EGL.as_ref().unwrap();

        if !egl.SwapBuffersWithDamageKHR.is_loaded() {
            return Err(ContextError::FunctionUnavailable);
        }

        let surface = self.surface.as_ref().unwrap().lock();
        if *surface == ffi::egl::NO_SURFACE {
            return Err(ContextError::ContextLost);
        }

        let mut ffirects: Vec<ffi::egl::types::EGLint> = Vec::with_capacity(rects.len() * 4);

        for rect in rects {
            ffirects.push(rect.x as ffi::egl::types::EGLint);
            ffirects.push(rect.y as ffi::egl::types::EGLint);
            ffirects.push(rect.width as ffi::egl::types::EGLint);
            ffirects.push(rect.height as ffi::egl::types::EGLint);
        }

        let ret = unsafe {
            egl.SwapBuffersWithDamageKHR(
                self.display,
                *surface,
                ffirects.as_mut_ptr(),
                rects.len() as ffi::egl::types::EGLint,
            )
        };

        if ret == ffi::egl::FALSE {
            match unsafe { egl.GetError() } as u32 {
                ffi::egl::CONTEXT_LOST => Err(ContextError::ContextLost),
                err => {
                    panic!("swap_buffers: eglSwapBuffers failed (eglGetError returned 0x{:x})", err)
                }
            }
        } else {
            Ok(())
        }
    }

    #[inline]
    #[cfg(not(target_os = "windows"))]
    pub fn swap_buffers_with_damage_supported(&self) -> bool {
        let egl = EGL.as_ref().unwrap();
        egl.SwapBuffersWithDamageKHR.is_loaded()
    }

    #[inline]
    pub fn get_pixel_format(&self) -> PixelFormat {
        self.pixel_format.clone()
    }

    #[inline]
    pub fn buffer_age(&self) -> u32 {
        let egl = EGL.as_ref().unwrap();
        let surface = self.surface.as_ref().unwrap().lock();

        let mut buffer_age = 0;
        let result = unsafe {
            egl.QuerySurface(
                self.display,
                *surface as *const _,
                ffi::egl::BUFFER_AGE_EXT as i32,
                &mut buffer_age,
            )
        };

        if result == ffi::egl::FALSE {
            0
        } else {
            buffer_age as u32
        }
    }
}

unsafe impl Send for Context {}
unsafe impl Sync for Context {}

impl Drop for Context {
    fn drop(&mut self) {
        unsafe {
            // https://stackoverflow.com/questions/54402688/recreate-eglcreatewindowsurface-with-same-native-window
            let egl = EGL.as_ref().unwrap();
            let surface = self.surface.as_ref().map(|s| *s.lock()).unwrap_or(ffi::egl::NO_SURFACE);
            // Ok, so we got to call `glFinish` before destroying the context
            // to ensure it actually gets destroyed. This requires making the
            // this context current.
            let mut guard = MakeCurrentGuard::new(self.display, surface, surface, self.context)
                .map_err(ContextError::OsError)
                .unwrap();

            guard.if_any_same_then_invalidate(surface, surface, self.context);

            let gl_finish_fn = self.get_proc_address("glFinish");
            assert!(!gl_finish_fn.is_null());
            let gl_finish_fn = std::mem::transmute::<_, extern "system" fn()>(gl_finish_fn);
            gl_finish_fn();

            egl.DestroyContext(self.display, self.context);
            self.context = ffi::egl::NO_CONTEXT;
            egl.DestroySurface(self.display, surface);
            if let Some(ref surface) = self.surface {
                let mut surface = surface.lock();
                *surface = ffi::egl::NO_SURFACE;
            }

            // In a reasonable world, we could uncomment the line bellow.
            //
            // This is no such world. Lets talk about something.
            //
            // You see, every call to `get_native_display` returns the exact
            // same display, just look at the docs:
            //
            // "Multiple calls made to eglGetDisplay with the same display_id
            // will return the same EGLDisplay handle."
            //
            // My EGL implementation does not do any ref counting, nor do the
            // EGL docs mention ref counting anywhere. In fact, the docs state
            // that there will be *no effect*, which, in a way, implies no ref
            // counting:
            //
            // "Initializing an already initialized EGL display connection has
            // no effect besides returning the version numbers."
            //
            // So, if we terminate the display, other people who are using it
            // won't be so happy.
            //
            // Well, how did I stumble on this issue you might ask...
            //
            // In this case, the "other people" was us, for it appears my EGL
            // implementation does not follow the docs,  or maybe I'm misreading
            // them. You see, according to the egl docs:
            //
            // "To release the current context without assigning a new one, set
            // context to EGL_NO_CONTEXT and set draw and read to
            // EGL_NO_SURFACE.  [...] ******This is the only case where an
            // uninitialized display may be passed to eglMakeCurrent.******"
            // (Emphasis mine).
            //
            // Well, my computer returns EGL_BAD_DISPLAY if the display passed
            // to eglMakeCurrent is uninitialized, which allowed to me to spot
            // this issue.
            //
            // I would have assumed that if EGL was going to provide us with
            // the same EGLDisplay that they'd at least do
            // some ref counting, but they don't.
            //
            // FIXME: Technically we are leaking resources, not much we can do.
            // Yeah, we could have a global static that does ref counting
            // ourselves, but what if some other library is using the display.
            //
            // On unix operating systems, we could preload a little lib that
            // does ref counting on that level, but:
            //      A) What about other platforms?
            //      B) Do you *really* want all glutin programs to preload a
            //      library?
            //      C) Who the hell is going to maintain that?
            //
            // egl.Terminate(self.display);
        }
    }
}

#[derive(Debug)]
pub struct ContextPrototype<'a> {
    opengl: &'a GlAttributes<&'a Context>,
    display: ffi::egl::types::EGLDisplay,
    egl_version: (ffi::egl::types::EGLint, ffi::egl::types::EGLint),
    extensions: Vec<String>,
    api: Api,
    version: Option<(u8, u8)>,
    config_id: ffi::egl::types::EGLConfig,
    pixel_format: PixelFormat,
    swap_interval_range: SwapIntervalRange,
}

#[cfg(any(
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
))]
#[cfg(feature = "x11")]
pub fn get_native_visual_id(
    display: ffi::egl::types::EGLDisplay,
    config_id: ffi::egl::types::EGLConfig,
) -> ffi::egl::types::EGLint {
    let egl = EGL.as_ref().unwrap();
    let mut value = unsafe { std::mem::zeroed() };
    let ret = unsafe {
        egl.GetConfigAttrib(
            display,
            config_id,
            ffi::egl::NATIVE_VISUAL_ID as ffi::egl::types::EGLint,
            &mut value,
        )
    };
    if ret == 0 {
        panic!("get_native_visual_id: eglGetConfigAttrib failed with 0x{:x}", unsafe {
            egl.GetError()
        })
    };
    value
}

impl<'a> ContextPrototype<'a> {
    #[cfg(any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
    ))]
    #[cfg(feature = "x11")]
    pub fn get_native_visual_id(&self) -> ffi::egl::types::EGLint {
        get_native_visual_id(self.display, self.config_id)
    }

    pub fn finish(self, nwin: ffi::EGLNativeWindowType) -> Result<Context, CreationError> {
        let egl = EGL.as_ref().unwrap();
        let surface = unsafe {
            let surface =
                egl.CreateWindowSurface(self.display, self.config_id, nwin, std::ptr::null());
            if surface.is_null() {
                return Err(CreationError::OsError("eglCreateWindowSurface failed".to_string()));
            }
            surface
        };

        self.finish_impl(Some(surface))
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
    ))]
    pub fn finish_surfaceless(self) -> Result<Context, CreationError> {
        // FIXME: Also check for the GL_OES_surfaceless_context *CONTEXT*
        // extension
        if !self.extensions.iter().any(|s| s == "EGL_KHR_surfaceless_context") {
            Err(CreationError::NotSupported("EGL surfaceless not supported".to_string()))
        } else {
            self.finish_impl(None)
        }
    }

    #[cfg(any(
        target_os = "android",
        target_os = "windows",
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
    ))]
    pub fn finish_pbuffer(self, size: dpi::PhysicalSize<u32>) -> Result<Context, CreationError> {
        let size: (u32, u32) = size.into();

        let egl = EGL.as_ref().unwrap();
        let attrs = &[
            ffi::egl::WIDTH as raw::c_int,
            size.0 as raw::c_int,
            ffi::egl::HEIGHT as raw::c_int,
            size.1 as raw::c_int,
            ffi::egl::NONE as raw::c_int,
        ];

        let surface = unsafe {
            let surface = egl.CreatePbufferSurface(self.display, self.config_id, attrs.as_ptr());
            if surface.is_null() || surface == ffi::egl::NO_SURFACE {
                return Err(CreationError::OsError("eglCreatePbufferSurface failed".to_string()));
            }
            surface
        };

        self.finish_impl(Some(surface))
    }

    fn finish_impl(
        self,
        surface: Option<ffi::egl::types::EGLSurface>,
    ) -> Result<Context, CreationError> {
        let share = match self.opengl.sharing {
            Some(ctx) => ctx.context,
            None => std::ptr::null(),
        };

        let context = unsafe {
            if let Some(version) = self.version {
                create_context(
                    self.display,
                    &self.egl_version,
                    &self.extensions,
                    self.api,
                    version,
                    self.config_id,
                    self.opengl.debug,
                    self.opengl.robustness,
                    share,
                )?
            } else if self.api == Api::OpenGlEs {
                if let Ok(ctx) = create_context(
                    self.display,
                    &self.egl_version,
                    &self.extensions,
                    self.api,
                    (2, 0),
                    self.config_id,
                    self.opengl.debug,
                    self.opengl.robustness,
                    share,
                ) {
                    ctx
                } else if let Ok(ctx) = create_context(
                    self.display,
                    &self.egl_version,
                    &self.extensions,
                    self.api,
                    (1, 0),
                    self.config_id,
                    self.opengl.debug,
                    self.opengl.robustness,
                    share,
                ) {
                    ctx
                } else {
                    return Err(CreationError::OpenGlVersionNotSupported);
                }
            } else if let Ok(ctx) = create_context(
                self.display,
                &self.egl_version,
                &self.extensions,
                self.api,
                (3, 2),
                self.config_id,
                self.opengl.debug,
                self.opengl.robustness,
                share,
            ) {
                ctx
            } else if let Ok(ctx) = create_context(
                self.display,
                &self.egl_version,
                &self.extensions,
                self.api,
                (3, 1),
                self.config_id,
                self.opengl.debug,
                self.opengl.robustness,
                share,
            ) {
                ctx
            } else if let Ok(ctx) = create_context(
                self.display,
                &self.egl_version,
                &self.extensions,
                self.api,
                (1, 0),
                self.config_id,
                self.opengl.debug,
                self.opengl.robustness,
                share,
            ) {
                ctx
            } else {
                return Err(CreationError::OpenGlVersionNotSupported);
            }
        };

        if let Some(surface) = surface {
            // VSync defaults to enabled; disable it if it was not requested.
            // if !self.opengl.vsync {
            let _guard = MakeCurrentGuard::new(self.display, surface, surface, context)
                .map_err(CreationError::OsError)?;

            let egl = EGL.as_ref().unwrap();
            unsafe {
                if egl.SwapInterval(self.display, self.opengl.vsync.get_swap_interval())
                    == ffi::egl::FALSE
                {
                    panic!("finish_impl: eglSwapInterval failed: 0x{:x}", egl.GetError());
                }
            }
            // }
        }

        Ok(Context {
            display: self.display,
            context,
            surface: surface.map(parking_lot::Mutex::new),
            api: self.api,
            pixel_format: self.pixel_format,
            swap_interval_range: self.swap_interval_range,
        })
    }
}

unsafe fn choose_fbconfig<F>(
    display: ffi::egl::types::EGLDisplay,
    egl_version: &(ffi::egl::types::EGLint, ffi::egl::types::EGLint),
    api: Api,
    version: Option<(u8, u8)>,
    pf_reqs: &PixelFormatRequirements,
    surface_type: SurfaceType,
    opengl: &GlAttributes<&Context>,
    mut config_selector: F,
) -> Result<(ffi::egl::types::EGLConfig, PixelFormat, SwapIntervalRange), CreationError>
where
    F: FnMut(
        Vec<ffi::egl::types::EGLConfig>,
        ffi::egl::types::EGLDisplay,
    ) -> Result<ffi::egl::types::EGLConfig, ()>,
{
    let egl = EGL.as_ref().unwrap();

    let descriptor = {
        let mut out: Vec<raw::c_int> = Vec::with_capacity(37);

        if egl_version >= &(1, 2) {
            out.push(ffi::egl::COLOR_BUFFER_TYPE as raw::c_int);
            out.push(ffi::egl::RGB_BUFFER as raw::c_int);
        }

        out.push(ffi::egl::SURFACE_TYPE as raw::c_int);
        let surface_type = match surface_type {
            SurfaceType::Window => ffi::egl::WINDOW_BIT,
            SurfaceType::PBuffer => ffi::egl::PBUFFER_BIT,
            SurfaceType::Surfaceless => 0,
        };
        out.push(surface_type as raw::c_int);

        match (api, version) {
            (Api::OpenGlEs, Some((3, _))) => {
                if egl_version < &(1, 3) {
                    return Err(CreationError::NoAvailablePixelFormat);
                }
                out.push(ffi::egl::RENDERABLE_TYPE as raw::c_int);
                out.push(ffi::egl::OPENGL_ES3_BIT as raw::c_int);
                out.push(ffi::egl::CONFORMANT as raw::c_int);
                out.push(ffi::egl::OPENGL_ES3_BIT as raw::c_int);
            }
            (Api::OpenGlEs, Some((2, _))) => {
                if egl_version < &(1, 3) {
                    return Err(CreationError::NoAvailablePixelFormat);
                }
                out.push(ffi::egl::RENDERABLE_TYPE as raw::c_int);
                out.push(ffi::egl::OPENGL_ES2_BIT as raw::c_int);
                out.push(ffi::egl::CONFORMANT as raw::c_int);
                out.push(ffi::egl::OPENGL_ES2_BIT as raw::c_int);
            }
            (Api::OpenGlEs, _) => {
                if egl_version >= &(1, 3) {
                    out.push(ffi::egl::RENDERABLE_TYPE as raw::c_int);
                    out.push(ffi::egl::OPENGL_ES_BIT as raw::c_int);
                    out.push(ffi::egl::CONFORMANT as raw::c_int);
                    out.push(ffi::egl::OPENGL_ES_BIT as raw::c_int);
                }
            }
            (Api::OpenGl, _) => {
                if egl_version < &(1, 3) {
                    return Err(CreationError::NoAvailablePixelFormat);
                }
                out.push(ffi::egl::RENDERABLE_TYPE as raw::c_int);
                out.push(ffi::egl::OPENGL_BIT as raw::c_int);
                out.push(ffi::egl::CONFORMANT as raw::c_int);
                out.push(ffi::egl::OPENGL_BIT as raw::c_int);
            }
            (_, _) => unimplemented!(),
        };

        if let Some(hardware_accelerated) = pf_reqs.hardware_accelerated {
            out.push(ffi::egl::CONFIG_CAVEAT as raw::c_int);
            out.push(if hardware_accelerated {
                ffi::egl::NONE as raw::c_int
            } else {
                ffi::egl::SLOW_CONFIG as raw::c_int
            });
        }

        if let Some(color) = pf_reqs.color_bits {
            out.push(ffi::egl::RED_SIZE as raw::c_int);
            out.push((color / 3) as raw::c_int);
            out.push(ffi::egl::GREEN_SIZE as raw::c_int);
            out.push((color / 3 + if color % 3 != 0 { 1 } else { 0 }) as raw::c_int);
            out.push(ffi::egl::BLUE_SIZE as raw::c_int);
            out.push((color / 3 + if color % 3 == 2 { 1 } else { 0 }) as raw::c_int);
        }

        if let Some(alpha) = pf_reqs.alpha_bits {
            out.push(ffi::egl::ALPHA_SIZE as raw::c_int);
            out.push(alpha as raw::c_int);
        }

        if let Some(depth) = pf_reqs.depth_bits {
            out.push(ffi::egl::DEPTH_SIZE as raw::c_int);
            out.push(depth as raw::c_int);
        }

        if let Some(stencil) = pf_reqs.stencil_bits {
            out.push(ffi::egl::STENCIL_SIZE as raw::c_int);
            out.push(stencil as raw::c_int);
        }

        if let Some(true) = pf_reqs.double_buffer {
            return Err(CreationError::NoAvailablePixelFormat);
        }

        if let Some(multisampling) = pf_reqs.multisampling {
            out.push(ffi::egl::SAMPLES as raw::c_int);
            out.push(multisampling as raw::c_int);
        }

        if pf_reqs.stereoscopy {
            return Err(CreationError::NoAvailablePixelFormat);
        }

        if let Some(xid) = pf_reqs.x11_visual_xid {
            out.push(ffi::egl::NATIVE_VISUAL_ID as raw::c_int);
            out.push(xid as raw::c_int);
        }

        // FIXME: srgb is not taken into account

        match pf_reqs.release_behavior {
            ReleaseBehavior::Flush => (),
            ReleaseBehavior::None => {
                // TODO: with EGL you need to manually set the behavior
                unimplemented!()
            }
        }

        out.push(ffi::egl::NONE as raw::c_int);
        out
    };

    // calling `eglChooseConfig`
    let mut num_configs = std::mem::zeroed();
    if egl.ChooseConfig(display, descriptor.as_ptr(), std::ptr::null_mut(), 0, &mut num_configs)
        == 0
    {
        return Err(CreationError::OsError("eglChooseConfig failed".to_string()));
    }

    if num_configs == 0 {
        return Err(CreationError::NoAvailablePixelFormat);
    }

    let mut config_ids = Vec::with_capacity(num_configs as usize);
    config_ids.resize_with(num_configs as usize, || std::mem::zeroed());
    if egl.ChooseConfig(
        display,
        descriptor.as_ptr(),
        config_ids.as_mut_ptr(),
        num_configs,
        &mut num_configs,
    ) == 0
    {
        return Err(CreationError::OsError("eglChooseConfig failed".to_string()));
    }

    // We're interested in those configs which allow our desired VSync.
    let desired_swap_interval = opengl.vsync.get_swap_interval();

    let mut config_ids_with_range = config_ids
        .into_iter()
        .filter_map(|config| {
            let mut min_swap_interval = 0;
            let _res = egl.GetConfigAttrib(
                display,
                config,
                ffi::egl::MIN_SWAP_INTERVAL as ffi::egl::types::EGLint,
                &mut min_swap_interval,
            );

            if desired_swap_interval < min_swap_interval {
                return None;
            }

            let mut max_swap_interval = 0;
            let _res = egl.GetConfigAttrib(
                display,
                config,
                ffi::egl::MAX_SWAP_INTERVAL as ffi::egl::types::EGLint,
                &mut max_swap_interval,
            );

            if desired_swap_interval > max_swap_interval {
                return None;
            }

            Some((config, SwapIntervalRange(min_swap_interval, max_swap_interval)))
        })
        .collect::<HashMap<_, _>>();
    let config_ids = config_ids_with_range.iter().map(|(i, _)| *i).collect::<Vec<_>>();

    if config_ids.is_empty() {
        return Err(CreationError::NoAvailablePixelFormat);
    }

    let config_id =
        config_selector(config_ids, display).map_err(|_| CreationError::NoAvailablePixelFormat)?;

    // analyzing each config
    macro_rules! attrib {
        ($egl:expr, $display:expr, $config:expr, $attr:expr) => {{
            let mut value = std::mem::zeroed();
            let res = $egl.GetConfigAttrib(
                $display,
                $config,
                $attr as ffi::egl::types::EGLint,
                &mut value,
            );
            if res == 0 {
                return Err(CreationError::OsError("eglGetConfigAttrib failed".to_string()));
            }
            value
        }};
    }

    let desc = PixelFormat {
        hardware_accelerated: attrib!(egl, display, config_id, ffi::egl::CONFIG_CAVEAT)
            != ffi::egl::SLOW_CONFIG as i32,
        color_bits: attrib!(egl, display, config_id, ffi::egl::RED_SIZE) as u8
            + attrib!(egl, display, config_id, ffi::egl::BLUE_SIZE) as u8
            + attrib!(egl, display, config_id, ffi::egl::GREEN_SIZE) as u8,
        alpha_bits: attrib!(egl, display, config_id, ffi::egl::ALPHA_SIZE) as u8,
        depth_bits: attrib!(egl, display, config_id, ffi::egl::DEPTH_SIZE) as u8,
        stencil_bits: attrib!(egl, display, config_id, ffi::egl::STENCIL_SIZE) as u8,
        stereoscopy: false,
        double_buffer: true,
        multisampling: match attrib!(egl, display, config_id, ffi::egl::SAMPLES) {
            0 | 1 => None,
            a => Some(a as u16),
        },
        srgb: false, // TODO: use EGL_KHR_gl_colorspace to know that
    };

    let swap_interval_range = config_ids_with_range.remove(&config_id).unwrap();
    Ok((config_id, desc, swap_interval_range))
}

unsafe fn create_context(
    display: ffi::egl::types::EGLDisplay,
    egl_version: &(ffi::egl::types::EGLint, ffi::egl::types::EGLint),
    extensions: &[String],
    api: Api,
    version: (u8, u8),
    config_id: ffi::egl::types::EGLConfig,
    gl_debug: bool,
    gl_robustness: Robustness,
    share: ffi::EGLContext,
) -> Result<ffi::egl::types::EGLContext, CreationError> {
    let egl = EGL.as_ref().unwrap();

    let mut context_attributes = Vec::with_capacity(10);
    let mut flags = 0;

    if egl_version >= &(1, 5) || extensions.iter().any(|s| s == "EGL_KHR_create_context") {
        context_attributes.push(ffi::egl::CONTEXT_MAJOR_VERSION as i32);
        context_attributes.push(version.0 as i32);
        context_attributes.push(ffi::egl::CONTEXT_MINOR_VERSION as i32);
        context_attributes.push(version.1 as i32);

        // handling robustness
        let supports_robustness = egl_version >= &(1, 5)
            || extensions.iter().any(|s| s == "EGL_EXT_create_context_robustness");

        match gl_robustness {
            Robustness::NotRobust => (),

            Robustness::NoError => {
                if extensions.iter().any(|s| s == "EGL_KHR_create_context_no_error") {
                    context_attributes.push(ffi::egl::CONTEXT_OPENGL_NO_ERROR_KHR as raw::c_int);
                    context_attributes.push(1);
                }
            }

            Robustness::RobustNoResetNotification => {
                if supports_robustness {
                    context_attributes
                        .push(ffi::egl::CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY as raw::c_int);
                    context_attributes.push(ffi::egl::NO_RESET_NOTIFICATION as raw::c_int);
                    flags |= ffi::egl::CONTEXT_OPENGL_ROBUST_ACCESS as raw::c_int;
                } else {
                    return Err(CreationError::RobustnessNotSupported);
                }
            }

            Robustness::TryRobustNoResetNotification => {
                if supports_robustness {
                    context_attributes
                        .push(ffi::egl::CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY as raw::c_int);
                    context_attributes.push(ffi::egl::NO_RESET_NOTIFICATION as raw::c_int);
                    flags |= ffi::egl::CONTEXT_OPENGL_ROBUST_ACCESS as raw::c_int;
                }
            }

            Robustness::RobustLoseContextOnReset => {
                if supports_robustness {
                    context_attributes
                        .push(ffi::egl::CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY as raw::c_int);
                    context_attributes.push(ffi::egl::LOSE_CONTEXT_ON_RESET as raw::c_int);
                    flags |= ffi::egl::CONTEXT_OPENGL_ROBUST_ACCESS as raw::c_int;
                } else {
                    return Err(CreationError::RobustnessNotSupported);
                }
            }

            Robustness::TryRobustLoseContextOnReset => {
                if supports_robustness {
                    context_attributes
                        .push(ffi::egl::CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY as raw::c_int);
                    context_attributes.push(ffi::egl::LOSE_CONTEXT_ON_RESET as raw::c_int);
                    flags |= ffi::egl::CONTEXT_OPENGL_ROBUST_ACCESS as raw::c_int;
                }
            }
        }

        if gl_debug && egl_version >= &(1, 5) {
            context_attributes.push(ffi::egl::CONTEXT_OPENGL_DEBUG as i32);
            context_attributes.push(ffi::egl::TRUE as i32);

            // TODO: using this flag sometimes generates an error
            //       there was a change in the specs that added this flag, so it
            // may not be       supported everywhere ; however it is
            // not possible to know whether it is       supported or
            // not flags = flags |
            // ffi::egl::CONTEXT_OPENGL_DEBUG_BIT_KHR as i32;
        }

        // In at least some configurations, the Android emulator’s GL
        // implementation advertises support for the
        // EGL_KHR_create_context extension but returns BAD_ATTRIBUTE
        // when CONTEXT_FLAGS_KHR is used.
        if flags != 0 {
            context_attributes.push(ffi::egl::CONTEXT_FLAGS_KHR as i32);
            context_attributes.push(flags);
        }
    } else if egl_version >= &(1, 3) && api == Api::OpenGlEs {
        // robustness is not supported
        match gl_robustness {
            Robustness::RobustNoResetNotification | Robustness::RobustLoseContextOnReset => {
                return Err(CreationError::RobustnessNotSupported);
            }
            _ => (),
        }

        context_attributes.push(ffi::egl::CONTEXT_CLIENT_VERSION as i32);
        context_attributes.push(version.0 as i32);
    }

    context_attributes.push(ffi::egl::NONE as i32);

    let context = egl.CreateContext(display, config_id, share, context_attributes.as_ptr());

    if context.is_null() {
        match egl.GetError() as u32 {
            ffi::egl::BAD_MATCH | ffi::egl::BAD_ATTRIBUTE => {
                return Err(CreationError::OpenGlVersionNotSupported);
            }
            e => panic!("create_context: eglCreateContext failed: 0x{:x}", e),
        }
    }

    Ok(context)
}
