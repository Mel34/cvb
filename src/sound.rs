use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::process::Command;

const CA_PROP_EVENT_ID: &str = "event.id";
const CA_PROP_CANBERRA_XDG_THEME_NAME: &str = "canberra.xdg-theme.name";

type CaContext = c_void;
type CaProplist = c_void;

#[link(name = "canberra")]
unsafe extern "C" {
    fn ca_context_create(context: *mut *mut CaContext) -> c_int;
    fn ca_context_destroy(context: *mut CaContext) -> c_int;
    fn ca_context_change_props_full(context: *mut CaContext, proplist: *mut CaProplist) -> c_int;

    fn ca_proplist_create(proplist: *mut *mut CaProplist) -> c_int;
    fn ca_proplist_destroy(proplist: *mut CaProplist) -> c_int;
    fn ca_proplist_sets(
        proplist: *mut CaProplist,
        key: *const c_char,
        value: *const c_char,
    ) -> c_int;

    fn ca_context_play_full(
        context: *mut CaContext,
        id: c_uint,
        proplist: *mut CaProplist,
        cb: Option<unsafe extern "C" fn(*mut CaContext, c_uint, c_int, *mut c_void)>,
        userdata: *mut c_void,
    ) -> c_int;
}

pub struct SoundPlayer {
    context: *mut CaContext,
}

impl SoundPlayer {
    pub fn new() -> Option<Self> {
        let mut context = std::ptr::null_mut();

        let result = unsafe { ca_context_create(&mut context) };

        if result < 0 || context.is_null() {
            return None;
        }

        if let Some(theme) = configured_theme() {
            if !set_theme(context, &theme) {
                unsafe {
                    ca_context_destroy(context);
                }

                return None;
            }
        }

        Some(Self { context })
    }

    pub fn play(&self, event: &str) {
        let key = match CString::new(CA_PROP_EVENT_ID) {
            Ok(value) => value,
            Err(_) => return,
        };

        let value = match CString::new(event) {
            Ok(value) => value,
            Err(_) => return,
        };

        let mut proplist = std::ptr::null_mut();

        let result = unsafe { ca_proplist_create(&mut proplist) };

        if result < 0 || proplist.is_null() {
            return;
        }

        let result = unsafe { ca_proplist_sets(proplist, key.as_ptr(), value.as_ptr()) };

        if result >= 0 {
            unsafe {
                ca_context_play_full(self.context, 0, proplist, None, std::ptr::null_mut());
            }
        }

        unsafe {
            ca_proplist_destroy(proplist);
        }
    }
}

impl Drop for SoundPlayer {
    fn drop(&mut self) {
        unsafe {
            ca_context_destroy(self.context);
        }
    }
}

fn configured_theme() -> Option<String> {
    let output = Command::new("gsettings")
        .args(["get", "org.gnome.desktop.sound", "theme-name"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.trim();

    let value = value.strip_prefix('\'')?;
    let value = value.strip_suffix('\'')?;

    if value.is_empty() {
        return None;
    }

    Some(value.to_owned())
}

fn set_theme(context: *mut CaContext, theme: &str) -> bool {
    let key = match CString::new(CA_PROP_CANBERRA_XDG_THEME_NAME) {
        Ok(value) => value,
        Err(_) => return false,
    };

    let value = match CString::new(theme) {
        Ok(value) => value,
        Err(_) => return false,
    };

    let mut proplist = std::ptr::null_mut();

    let result = unsafe { ca_proplist_create(&mut proplist) };

    if result < 0 || proplist.is_null() {
        return false;
    }

    let result = unsafe { ca_proplist_sets(proplist, key.as_ptr(), value.as_ptr()) };

    if result < 0 {
        unsafe {
            ca_proplist_destroy(proplist);
        }

        return false;
    }

    let result = unsafe { ca_context_change_props_full(context, proplist) };

    unsafe {
        ca_proplist_destroy(proplist);
    }

    result >= 0
}
