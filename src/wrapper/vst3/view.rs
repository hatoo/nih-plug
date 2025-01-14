use atomic_float::AtomicF32;
use parking_lot::RwLock;
use raw_window_handle::RawWindowHandle;
use std::any::Any;
use std::ffi::{c_void, CStr};
use std::mem;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use vst3_com::utils::SharedVstPtr;
use vst3_sys::base::{kInvalidArgument, kResultFalse, kResultOk, tresult, TBool};
use vst3_sys::gui::{IPlugFrame, IPlugView, IPlugViewContentScaleSupport};
use vst3_sys::VST3;

use super::inner::WrapperInner;
use super::util::{ObjectPtr, VstPtr};
use crate::plugin::{Editor, ParentWindowHandle, Vst3Plugin};

// Alias needed for the VST3 attribute macro
use vst3_sys as vst3_com;

// Window handle type constants missing from vst3-sys
#[allow(unused)]
const VST3_PLATFORM_HWND: &str = "HWND";
#[allow(unused)]
const VST3_PLATFORM_HIVIEW: &str = "HIView";
#[allow(unused)]
const VST3_PLATFORM_NSVIEW: &str = "NSView";
#[allow(unused)]
const VST3_PLATFORM_UIVIEW: &str = "UIView";
#[allow(unused)]
const VST3_PLATFORM_X11_WINDOW: &str = "X11EmbedWindowID";

/// The plugin's [`IPlugView`] instance created in [`IEditController::create_view()`] if `P` has an
/// editor. This is managed separately so the lifetime bounds match up.
#[VST3(implements(IPlugView, IPlugViewContentScaleSupport))]
pub(crate) struct WrapperView<P: Vst3Plugin> {
    inner: Arc<WrapperInner<P>>,
    editor: Arc<dyn Editor>,
    editor_handle: RwLock<Option<Box<dyn Any>>>,

    /// The `IPlugFrame` instance passed by the host during [IPlugView::set_frame()].
    pub plug_frame: RwLock<Option<VstPtr<dyn IPlugFrame>>>,
    /// The DPI scaling factor as passed to the [IPlugViewContentScaleSupport::set_scale_factor()]
    /// function. Defaults to 1.0, and will be kept there on macOS. When reporting and handling size
    /// the sizes communicated to and from the DAW should be scaled by this factor since NIH-plug's
    /// APIs only deal in logical pixels.
    scaling_factor: AtomicF32,
}

impl<P: Vst3Plugin> WrapperView<P> {
    pub fn new(inner: Arc<WrapperInner<P>>, editor: Arc<dyn Editor>) -> Box<Self> {
        Self::allocate(
            inner,
            editor,
            RwLock::new(None),
            RwLock::new(None),
            AtomicF32::new(1.0),
        )
    }
}

impl<P: Vst3Plugin> IPlugView for WrapperView<P> {
    #[cfg(all(target_family = "unix", not(target_os = "macos")))]
    unsafe fn is_platform_type_supported(&self, type_: vst3_sys::base::FIDString) -> tresult {
        let type_ = CStr::from_ptr(type_);
        match type_.to_str() {
            Ok(type_) if type_ == VST3_PLATFORM_X11_WINDOW => kResultOk,
            _ => {
                nih_debug_assert_failure!("Invalid window handle type: {:?}", type_);
                kResultFalse
            }
        }
    }

    #[cfg(all(target_os = "macos"))]
    unsafe fn is_platform_type_supported(&self, type_: vst3_sys::base::FIDString) -> tresult {
        let type_ = CStr::from_ptr(type_);
        match type_.to_str() {
            Ok(type_) if type_ == VST3_PLATFORM_NSVIEW => kResultOk,
            _ => {
                nih_debug_assert_failure!("Invalid window handle type: {:?}", type_);
                kResultFalse
            }
        }
    }

    #[cfg(all(target_os = "windows"))]
    unsafe fn is_platform_type_supported(&self, type_: vst3_sys::base::FIDString) -> tresult {
        let type_ = CStr::from_ptr(type_);
        match type_.to_str() {
            Ok(type_) if type_ == VST3_PLATFORM_HWND => kResultOk,
            _ => {
                nih_debug_assert_failure!("Invalid window handle type: {:?}", type_);
                kResultFalse
            }
        }
    }

    unsafe fn attached(&self, parent: *mut c_void, type_: vst3_sys::base::FIDString) -> tresult {
        let mut editor_handle = self.editor_handle.write();
        if editor_handle.is_none() {
            let type_ = CStr::from_ptr(type_);
            let handle = match type_.to_str() {
                #[cfg(all(target_family = "unix", not(target_os = "macos")))]
                Ok(type_) if type_ == VST3_PLATFORM_X11_WINDOW => {
                    let mut handle = raw_window_handle::XcbHandle::empty();
                    handle.window = parent as usize as u32;
                    RawWindowHandle::Xcb(handle)
                }
                #[cfg(all(target_os = "macos"))]
                Ok(type_) if type_ == VST3_PLATFORM_NSVIEW => {
                    let mut handle = raw_window_handle::AppKitHandle::empty();
                    handle.ns_view = parent;
                    RawWindowHandle::AppKit(handle)
                }
                #[cfg(all(target_os = "windows"))]
                Ok(type_) if type_ == VST3_PLATFORM_HWND => {
                    let mut handle = raw_window_handle::Win32Handle::empty();
                    handle.hwnd = parent;
                    RawWindowHandle::Win32(handle)
                }
                _ => {
                    nih_debug_assert_failure!("Unknown window handle type: {:?}", type_);
                    return kInvalidArgument;
                }
            };

            *editor_handle = Some(self.editor.spawn(
                ParentWindowHandle { handle },
                self.inner.clone().make_gui_context(),
            ));
            *self.inner.plug_view.write() = Some(ObjectPtr::from(self));

            kResultOk
        } else {
            nih_debug_assert_failure!(
                "Host tried to attach editor while the editor is already attached"
            );

            kResultFalse
        }
    }

    unsafe fn removed(&self) -> tresult {
        let mut editor_handle = self.editor_handle.write();
        if editor_handle.is_some() {
            *self.inner.plug_view.write() = None;
            *editor_handle = None;

            kResultOk
        } else {
            nih_debug_assert_failure!("Host tried to remove the editor without an active editor");

            kResultFalse
        }
    }

    unsafe fn on_wheel(&self, _distance: f32) -> tresult {
        // We'll let the plugin use the OS' input mechamisms because not all DAWs (or very few
        // actually) implement these functions
        kResultOk
    }

    unsafe fn on_key_down(
        &self,
        _key: vst3_sys::base::char16,
        _key_code: i16,
        _modifiers: i16,
    ) -> tresult {
        kResultOk
    }

    unsafe fn on_key_up(
        &self,
        _key: vst3_sys::base::char16,
        _key_code: i16,
        _modifiers: i16,
    ) -> tresult {
        kResultOk
    }

    unsafe fn get_size(&self, size: *mut vst3_sys::gui::ViewRect) -> tresult {
        check_null_ptr!(size);

        *size = mem::zeroed();

        let (unscaled_width, unscaled_height) = self.editor.size();
        let scaling_factor = self.scaling_factor.load(Ordering::Relaxed);
        let size = &mut *size;
        size.left = 0;
        size.right = (unscaled_width as f32 * scaling_factor).round() as i32;
        size.top = 0;
        size.bottom = (unscaled_height as f32 * scaling_factor).round() as i32;

        kResultOk
    }

    unsafe fn on_size(&self, new_size: *mut vst3_sys::gui::ViewRect) -> tresult {
        // TODO: Implement resizing
        check_null_ptr!(new_size);

        let (unscaled_width, unscaled_height) = self.editor.size();
        let scaling_factor = self.scaling_factor.load(Ordering::Relaxed);
        let (editor_width, editor_height) = (
            (unscaled_width as f32 * scaling_factor).round() as i32,
            (unscaled_height as f32 * scaling_factor).round() as i32,
        );

        let width = (*new_size).right - (*new_size).left;
        let height = (*new_size).bottom - (*new_size).top;
        if width == editor_width && height == editor_height {
            kResultOk
        } else {
            kResultFalse
        }
    }

    unsafe fn on_focus(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn set_frame(&self, frame: *mut c_void) -> tresult {
        // The correct argument type is missing from the bindings
        let frame: SharedVstPtr<dyn IPlugFrame> = mem::transmute(frame);
        match frame.upgrade() {
            Some(frame) => *self.plug_frame.write() = Some(frame.into()),
            None => *self.plug_frame.write() = None,
        }

        kResultOk
    }

    unsafe fn can_resize(&self) -> tresult {
        // TODO: Implement resizing
        kResultFalse
    }

    unsafe fn check_size_constraint(&self, rect: *mut vst3_sys::gui::ViewRect) -> tresult {
        check_null_ptr!(rect);

        // TODO: Add this with the resizing
        if (*rect).right - (*rect).left > 0 && (*rect).bottom - (*rect).top > 0 {
            kResultOk
        } else {
            kResultFalse
        }
    }
}

impl<P: Vst3Plugin> IPlugViewContentScaleSupport for WrapperView<P> {
    unsafe fn set_scale_factor(&self, factor: f32) -> tresult {
        // On macOS scaling is done by the OS, and all window sizes are in logical pixels
        if cfg!(target_os = "macos") {
            nih_debug_assert_failure!("Ignoring host request to set explicit DPI scaling factor");
            return kResultFalse;
        }

        if self.editor.set_scale_factor(factor) {
            self.scaling_factor.store(factor, Ordering::Relaxed);
            kResultOk
        } else {
            kResultFalse
        }
    }
}
