fn main() {
    // The embuild output is only meaningful (and only available) when building
    // for the ESP32 itself; host builds (unit tests) must not require ESP-IDF.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("espidf") {
        embuild::espidf::sysenv::output();
    }
}
