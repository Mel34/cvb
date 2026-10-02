#include <stdarg.h>
#include <libinput.h>

static void
cvb_libinput_log_handler(
    struct libinput *libinput,
    enum libinput_log_priority priority,
    const char *format,
    va_list args)
{
    (void)libinput;
    (void)priority;
    (void)format;
    (void)args;
}

void
cvb_libinput_silence(struct libinput *libinput)
{
    libinput_log_set_handler(libinput, cvb_libinput_log_handler);
}
