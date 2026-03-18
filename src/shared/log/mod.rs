use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn info(message: impl AsRef<str>) {
    print("INFO", "\x1b[36m", message.as_ref());
}

pub fn warn(message: impl AsRef<str>) {
    print("WARN", "\x1b[33m", message.as_ref());
}

pub fn error(message: impl AsRef<str>) {
    print("ERROR", "\x1b[31m", message.as_ref());
}

pub fn success(message: impl AsRef<str>) {
    print("OK", "\x1b[32m", message.as_ref());
}

pub fn format_duration(duration: Duration) -> String {
    if duration.as_secs() > 0 {
        format!("{}.{:03}s", duration.as_secs(), duration.subsec_millis())
    } else {
        format!("{}ms", duration.as_millis())
    }
}

pub fn bold(value: impl AsRef<str>) -> String {
    format!("\x1b[1m{}\x1b[0m", value.as_ref())
}

fn print(level: &str, color: &str, message: &str) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    println!(
        "[{}] {}{:5}\x1b[0m {}",
        format_local_time(now),
        color,
        level,
        message
    );
}

#[cfg(unix)]
fn format_local_time(now: Duration) -> String {
    use std::os::raw::{c_int, c_long};

    #[repr(C)]
    struct Tm {
        tm_sec: c_int,
        tm_min: c_int,
        tm_hour: c_int,
        tm_mday: c_int,
        tm_mon: c_int,
        tm_year: c_int,
        tm_wday: c_int,
        tm_yday: c_int,
        tm_isdst: c_int,
        tm_gmtoff: c_long,
        tm_zone: *const u8,
    }

    unsafe extern "C" {
        fn localtime_r(timer: *const c_long, result: *mut Tm) -> *mut Tm;
    }

    let seconds = now.as_secs() as c_long;
    let mut local = Tm {
        tm_sec: 0,
        tm_min: 0,
        tm_hour: 0,
        tm_mday: 0,
        tm_mon: 0,
        tm_year: 0,
        tm_wday: 0,
        tm_yday: 0,
        tm_isdst: 0,
        tm_gmtoff: 0,
        tm_zone: std::ptr::null(),
    };

    // SAFETY: `seconds` points to a valid `time_t`-compatible integer for this process and
    // `local` points to writable storage for the result.
    let converted = unsafe { localtime_r(&seconds, &mut local) };
    if converted.is_null() {
        return format!("{:02}:{:02}:{:02}.{:03}", 0, 0, 0, now.subsec_millis());
    }

    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        local.tm_year + 1900,
        local.tm_mon + 1,
        local.tm_mday,
        local.tm_hour,
        local.tm_min,
        local.tm_sec,
        now.subsec_millis()
    )
}

#[cfg(not(unix))]
fn format_local_time(now: Duration) -> String {
    format!("{}.{:03}", now.as_secs(), now.subsec_millis())
}
