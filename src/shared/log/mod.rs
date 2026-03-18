use std::time::{SystemTime, UNIX_EPOCH};

pub fn info(message: impl AsRef<str>) {
    print("INFO", message.as_ref());
}

pub fn warn(message: impl AsRef<str>) {
    print("WARN", message.as_ref());
}

pub fn error(message: impl AsRef<str>) {
    print("ERROR", message.as_ref());
}

fn print(level: &str, message: &str) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    println!(
        "[{}.{:03}] {:5} {}",
        now.as_secs(),
        now.subsec_millis(),
        level,
        message
    );
}
