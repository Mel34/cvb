use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};

#[repr(C)]
struct LibinputInterface {
    open_restricted: Option<unsafe extern "C" fn(*const c_char, c_int, *mut c_void) -> c_int>,
    close_restricted: Option<unsafe extern "C" fn(c_int, *mut c_void)>,
}

#[repr(C)]
struct LibinputRaw {
    _private: [u8; 0],
}

#[repr(C)]
struct LibinputEvent {
    _private: [u8; 0],
}

#[repr(C)]
struct LibinputEventKeyboard {
    _private: [u8; 0],
}

#[repr(C)]
struct Udev {
    _private: [u8; 0],
}

#[repr(C)]
struct LibinputEventType(c_int);

#[repr(C)]
struct LibinputKeyState(c_int);

const LIBINPUT_EVENT_KEYBOARD_KEY: c_int = 300;

const LIBINPUT_KEY_STATE_RELEASED: c_int = 0;
const LIBINPUT_KEY_STATE_PRESSED: c_int = 1;
const LIBINPUT_KEY_STATE_KEY_REPEAT: c_int = 2;

const KEY_A: u32 = 30;
const KEY_LEFTCTRL: u32 = 29;
const KEY_RIGHTCTRL: u32 = 97;
const KEY_LEFTSHIFT: u32 = 42;
const KEY_RIGHTSHIFT: u32 = 54;

unsafe extern "C" {
    fn udev_new() -> *mut Udev;
    fn udev_unref(udev: *mut Udev) -> *mut Udev;

    fn libinput_udev_create_context(
        interface: *const LibinputInterface,
        user_data: *mut c_void,
        udev: *mut Udev,
    ) -> *mut LibinputRaw;

    fn libinput_udev_assign_seat(libinput: *mut LibinputRaw, seat_id: *const c_char) -> c_int;

    fn libinput_get_fd(libinput: *mut LibinputRaw) -> c_int;
    fn libinput_dispatch(libinput: *mut LibinputRaw) -> c_int;
    fn libinput_get_event(libinput: *mut LibinputRaw) -> *mut LibinputEvent;

    fn libinput_event_get_type(event: *mut LibinputEvent) -> LibinputEventType;
    fn libinput_event_destroy(event: *mut LibinputEvent);

    fn libinput_event_get_keyboard_event(event: *mut LibinputEvent) -> *mut LibinputEventKeyboard;

    fn libinput_event_keyboard_get_key(event: *mut LibinputEventKeyboard) -> u32;

    fn libinput_event_keyboard_get_key_state(event: *mut LibinputEventKeyboard)
    -> LibinputKeyState;

    fn libinput_unref(libinput: *mut LibinputRaw) -> *mut LibinputRaw;
}

unsafe extern "C" fn open_restricted(
    path: *const c_char,
    flags: c_int,
    _user_data: *mut c_void,
) -> c_int {
    unsafe { libc::open(path, flags) }
}

unsafe extern "C" fn close_restricted(fd: c_int, _user_data: *mut c_void) {
    unsafe {
        libc::close(fd);
    }
}

struct Libinput {
    context: *mut LibinputRaw,
    #[allow(dead_code)]
    interface: Box<LibinputInterface>,
}

impl Libinput {
    fn new() -> Result<Self, String> {
        let interface = Box::new(LibinputInterface {
            open_restricted: Some(open_restricted),
            close_restricted: Some(close_restricted),
        });

        let seat = CString::new("seat0")
            .map_err(|error| format!("CVB: unable to create seat name: {error}"))?;

        let udev = unsafe { udev_new() };

        if udev.is_null() {
            return Err("CVB: unable to create udev context".to_string());
        }

        let context =
            unsafe { libinput_udev_create_context(&*interface, std::ptr::null_mut(), udev) };

        unsafe {
            udev_unref(udev);
        }

        if context.is_null() {
            return Err("CVB: unable to create libinput context".to_string());
        }

        let assigned = unsafe { libinput_udev_assign_seat(context, seat.as_ptr()) };

        if assigned != 0 {
            unsafe {
                libinput_unref(context);
            }

            return Err("CVB: unable to assign libinput seat0".to_string());
        }

        Ok(Self { context, interface })
    }

    fn fd(&self) -> c_int {
        unsafe { libinput_get_fd(self.context) }
    }

    fn dispatch(&mut self) -> Result<(), String> {
        let result = unsafe { libinput_dispatch(self.context) };

        if result != 0 {
            return Err(format!("CVB: unable to dispatch libinput events: {result}"));
        }

        Ok(())
    }

    fn next_event(&mut self) -> Option<*mut LibinputEvent> {
        let event = unsafe { libinput_get_event(self.context) };

        if event.is_null() { None } else { Some(event) }
    }
}

impl Drop for Libinput {
    fn drop(&mut self) {
        unsafe {
            libinput_unref(self.context);
        }
    }
}

pub struct HotkeyMonitor {
    libinput: Libinput,
    ctrl: bool,
    shift: bool,
}

impl HotkeyMonitor {
    pub fn new() -> Result<Self, String> {
        let libinput = Libinput::new()?;

        Ok(Self {
            libinput,
            ctrl: false,
            shift: false,
        })
    }

    pub fn fd(&self) -> c_int {
        self.libinput.fd()
    }

    pub fn poll(&mut self) -> Result<bool, String> {
        self.libinput.dispatch()?;

        let mut detected = false;

        while let Some(event) = self.libinput.next_event() {
            let event_type = unsafe { libinput_event_get_type(event).0 };

            if event_type == LIBINPUT_EVENT_KEYBOARD_KEY {
                let keyboard_event = unsafe { libinput_event_get_keyboard_event(event) };

                if !keyboard_event.is_null() {
                    let key = unsafe { libinput_event_keyboard_get_key(keyboard_event) };

                    let state = unsafe { libinput_event_keyboard_get_key_state(keyboard_event).0 };

                    match state {
                        LIBINPUT_KEY_STATE_PRESSED => match key {
                            KEY_LEFTCTRL | KEY_RIGHTCTRL => {
                                self.ctrl = true;
                            }

                            KEY_LEFTSHIFT | KEY_RIGHTSHIFT => {
                                self.shift = true;
                            }

                            KEY_A if self.ctrl && self.shift => {
                                detected = true;
                            }

                            _ => {}
                        },

                        LIBINPUT_KEY_STATE_RELEASED => match key {
                            KEY_LEFTCTRL | KEY_RIGHTCTRL => {
                                self.ctrl = false;
                            }

                            KEY_LEFTSHIFT | KEY_RIGHTSHIFT => {
                                self.shift = false;
                            }

                            _ => {}
                        },

                        LIBINPUT_KEY_STATE_KEY_REPEAT => {}

                        _ => {}
                    }
                }
            }

            unsafe {
                libinput_event_destroy(event);
            }
        }

        Ok(detected)
    }
}
