use crate::keybind::{Key, KeyBinding};
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

const KEY_B: u32 = 48;
const KEY_LEFTCTRL: u32 = 29;
const KEY_RIGHTCTRL: u32 = 97;
const KEY_LEFTSHIFT: u32 = 42;
const KEY_RIGHTSHIFT: u32 = 54;

unsafe extern "C" {
    fn cvb_libinput_silence(libinput: *mut LibinputRaw);
    
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

        unsafe {
            cvb_libinput_silence(context);
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
    bindings: Vec<KeyBinding>,
    ctrl: bool,
    shift: bool,
    alt: bool,
    super_key: bool,
}

impl HotkeyMonitor {
    pub fn new(bindings: Vec<KeyBinding>) -> Result<Self, String> {
        let libinput = Libinput::new()?;

        Ok(Self {
            libinput,
            bindings,
            ctrl: false,
            shift: false,
            alt: false,
            super_key: false,
        })
    }

    pub fn fd(&self) -> c_int {
        self.libinput.fd()
    }
    
    pub fn set_bindings(&mut self, bindings: Vec<KeyBinding>) {
        self.bindings = bindings;
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

                    let state =
                        unsafe { libinput_event_keyboard_get_key_state(keyboard_event).0 };

                    match state {
                        LIBINPUT_KEY_STATE_PRESSED => {
                            match key {
                                KEY_LEFTCTRL | KEY_RIGHTCTRL => {
                                    self.ctrl = true;
                                }

                                KEY_LEFTSHIFT | KEY_RIGHTSHIFT => {
                                    self.shift = true;
                                }

                                _ => {}
                            }

                            if self.matches_binding(key) {
                                detected = true;
                            }
                        }

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

    fn matches_binding(&self, key: u32) -> bool {
        self.bindings.iter().any(|binding| {
            self.ctrl == binding.ctrl
                && self.shift == binding.shift
                && self.alt == binding.alt
                && self.super_key == binding.super_key
                && key_matches(binding.key, key)
        })
    }
}

fn key_matches(key: Key, code: u32) -> bool {
    match key {
        Key::Character(character) => match character.to_ascii_uppercase() {
            'A' => code == 30,
            'B' => code == 48,
            'C' => code == 46,
            'D' => code == 32,
            'E' => code == 18,
            'F' => code == 33,
            'G' => code == 34,
            'H' => code == 35,
            'I' => code == 23,
            'J' => code == 36,
            'K' => code == 37,
            'L' => code == 38,
            'M' => code == 50,
            'N' => code == 49,
            'O' => code == 24,
            'P' => code == 25,
            'Q' => code == 16,
            'R' => code == 19,
            'S' => code == 31,
            'T' => code == 20,
            'U' => code == 22,
            'V' => code == 47,
            'W' => code == 17,
            'X' => code == 45,
            'Y' => code == 21,
            'Z' => code == 44,
            _ => false,
        },

        Key::Enter => code == 28,
        Key::Escape => code == 1,
        Key::Tab => code == 15,
        Key::Space => code == 57,
        Key::Backspace => code == 14,
        Key::Delete => code == 111,
        Key::Insert => code == 110,
        Key::Home => code == 102,
        Key::End => code == 107,
        Key::PageUp => code == 104,
        Key::PageDown => code == 109,
        Key::Left => code == 105,
        Key::Right => code == 106,
        Key::Up => code == 103,
        Key::Down => code == 108,
        Key::F(number) => code == function_key_code(number),
        Key::Plus => code == 13,
        Key::Minus => code == 12,
    }
}

fn function_key_code(number: u8) -> u32 {
    match number {
        1 => 59,
        2 => 60,
        3 => 61,
        4 => 62,
        5 => 63,
        6 => 64,
        7 => 65,
        8 => 66,
        9 => 67,
        10 => 68,
        11 => 87,
        12 => 88,
        13 => 183,
        14 => 184,
        15 => 185,
        16 => 186,
        17 => 187,
        18 => 188,
        19 => 189,
        20 => 190,
        21 => 191,
        22 => 192,
        23 => 193,
        24 => 194,
        _ => 0,
    }
}
