//! Driver trait definitions and driver instance specs.
//!
//! A *driver* implements how an entity type is actually produced or
//! controlled. The rest of the system (main loop, bindings, automations)
//! only ever sees these traits and entity ids — never a concrete driver.
//!
//! Trait design: `SensorDriver` and `ActuatorDriver` are separate traits
//! because the two domains have opposite data directions (polled readings vs
//! received commands). Both extend the common [`Driver`] trait, and a single
//! instance may implement *both* — that is exactly what a future bridge
//! device driver (`phone_bridge`: gps/gyro sensors + audio_player actuator
//! over one BLE connection) will do, which is why [`Driver`] exposes
//! `as_sensor` / `as_actuator` accessors instead of the registry deciding a
//! driver is one or the other.

// Not target-gated as a whole: the widget/compositor code (which determines
// the entities a partitioned display exposes) is needed host-side by config
// validation and its tests; only actual rendering/I2C hardware is espidf-only.
pub mod display;

#[cfg(target_os = "espidf")]
pub mod hw;
#[cfg(target_os = "espidf")]
mod pulse_counter;
#[cfg(target_os = "espidf")]
pub mod registry;
// Same rationale as `display`: only the `hardware` submodules are espidf-only.
pub mod ili9341_display;
pub mod ssd1306_display;

use std::time::Instant;

use crate::entities::{ActuatorCommand, Entity, EntityId, SensorValue};
use crate::error::DriverError;

/// Driver kind names as used in the JSON config's `driver` field.
pub const PULSE_COUNTER_KIND: &str = "pulse_counter";
pub const SSD1306_DISPLAY_KIND: &str = "ssd1306_display";
pub const ILI9341_DISPLAY_KIND: &str = "ili9341_display";

/// All built-in driver kinds. The config validation layer takes the known
/// kinds as a parameter (so it stays a pure, host-testable function); this
/// constant is the canonical list callers pass in.
pub const BUILTIN_DRIVER_KINDS: &[&str] = &[
    PULSE_COUNTER_KIND,
    SSD1306_DISPLAY_KIND,
    ILI9341_DISPLAY_KIND,
];

/// A driver's entity count is generally config-driven, not fixed at 1
/// (bridge devices, config-partitioned actuators like display zones). Given
/// the entity as declared in config, this returns the entities the driver
/// will actually expose — config validation calls it so bindings against
/// derived entities (e.g. `gateway.display.top`) validate without hardware.
///
/// Pure function of its inputs; errors are human-readable messages for the
/// config layer to wrap with entity context.
pub fn expand_entities(
    kind: &str,
    declared: &Entity,
    driver_config: &serde_json::Value,
) -> Result<Vec<Entity>, String> {
    match kind {
        SSD1306_DISPLAY_KIND => {
            let config = ssd1306_display::parse_config(driver_config)?;
            let (width, height) = config.dimensions();
            display::compositor::validate_layout(&config.layout, width, height)?;
            display::compositor::zone_entities(declared, &config.layout)
        }
        ILI9341_DISPLAY_KIND => {
            let config = ili9341_display::parse_config(driver_config)?;
            let (width, height) = config.dimensions();
            display::compositor::validate_layout(&config.layout, width, height)?;
            display::compositor::zone_entities(declared, &config.layout)
        }
        _ => Ok(vec![declared.clone()]),
    }
}

/// Everything the runtime needs to instantiate one driver instance, produced
/// by config validation. Target-independent: contains no hardware handles.
#[derive(Debug, Clone, PartialEq)]
pub struct DriverSpec {
    /// Unique instance id; currently the address of the entity it backs.
    pub instance_id: String,
    /// Driver kind (`pulse_counter`, …), resolved via the registry.
    pub kind: String,
    /// The entities this instance must expose. One for simple drivers;
    /// bridge-device drivers will list several.
    pub entities: Vec<Entity>,
    /// Driver-specific configuration, deserialized by the driver itself.
    pub config: serde_json::Value,
}

/// One fresh sensor reading.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub entity: EntityId,
    pub value: SensorValue,
}

/// Common surface of every driver instance.
pub trait Driver {
    fn instance_id(&self) -> &str;

    /// The entities this instance exposes.
    fn entities(&self) -> &[Entity];

    /// Sensor-domain capability, if this driver has one.
    fn as_sensor(&mut self) -> Option<&mut dyn SensorDriver> {
        None
    }

    /// Actuator-domain capability, if this driver has one.
    fn as_actuator(&mut self) -> Option<&mut dyn ActuatorDriver> {
        None
    }
}

/// A driver that produces sensor readings.
pub trait SensorDriver: Driver {
    /// Called every main-loop tick. The driver decides when it actually has
    /// new data (e.g. a sampling window elapsed) and returns only fresh
    /// readings — an empty vec means "nothing new", not an error.
    fn poll(&mut self, now: Instant) -> Result<Vec<Reading>, DriverError>;
}

/// A driver that executes commands against actuator entities it exposes.
pub trait ActuatorDriver: Driver {
    fn execute(&mut self, target: &EntityId, command: &ActuatorCommand) -> Result<(), DriverError>;

    /// Called once per main-loop tick, after all commands for this tick were
    /// delivered. Drivers that batch output — e.g. a display compositor
    /// where several zone updates share one framebuffer — push accumulated
    /// changes to the hardware here, exactly once, and only if something
    /// actually changed. Default: nothing to batch.
    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}
