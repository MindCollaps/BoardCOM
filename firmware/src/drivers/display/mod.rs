//! Display driver internals, decoupled from any specific display chip.
//!
//! Three layers (architecture documented in `docs/driver/display.md`):
//! - [`widgets`] — pure rendering: given a value, a widget's own config, and
//!   a target region, draws onto any `embedded-graphics` `DrawTarget`. No
//!   hardware/I2C/SPI knowledge, and generic over pixel color.
//! - [`compositor`] — zone management: owns the shared framebuffer state,
//!   applies each zone's widget output to its region, tracks what changed,
//!   and decouples render ticks from raw value updates (samples the latest
//!   staged value at a fixed interval instead of rendering on every update).
//!   Also generic over pixel color; doesn't know which physical display it's
//!   ultimately talking to.
//! - [`generic`] — [`generic::DisplayHardware`] + [`generic::GenericDisplayDriver`]:
//!   the last mile of decoupling. A concrete chip only implements
//!   `DisplayHardware` (hardware init stays outside the trait as an inherent
//!   constructor, init/`draw_target`/`flush`/`dimensions` are the trait) and
//!   gets the entire `ActuatorDriver` implementation for free.
//!
//! None of these modules are `target_os = "espidf"`-gated: all three compile
//! and unit-test on the host (including against a fake `DisplayHardware` that
//! isn't any real chip). A concrete display driver (e.g. `ssd1306_display.rs`)
//! stays thin — hardware init, providing the `DrawTarget`, and the flush call
//! over the wire — reusing these unchanged.

pub mod compositor;
pub mod generic;
pub mod widgets;

/// Serde default for the `render_interval_ms` config field every display
/// driver exposes; sourced from the compositor's default so there is one
/// canonical rate.
pub fn default_render_interval_ms() -> u32 {
    compositor::DEFAULT_RENDER_INTERVAL.as_millis() as u32
}

/// Shared config validation for the `rotation` field every display driver
/// exposes (degrees; the four right angles only).
pub fn validate_rotation(rotation: u16) -> Result<(), String> {
    if matches!(rotation, 0 | 90 | 180 | 270) {
        Ok(())
    } else {
        Err(format!(
            "rotation must be 0, 90, 180 or 270, got {rotation}"
        ))
    }
}
