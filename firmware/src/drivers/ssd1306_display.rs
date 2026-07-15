//! `ssd1306_display` driver: thin SSD1306-specific layer over the
//! chip-agnostic [`crate::drivers::display`] widget/compositor/generic-driver
//! code.
//!
//! Everything here is either a hardware fact (I2C pins/address/baudrate,
//! module resolution, rotation) or hardware I/O (init, the flush call). No
//! widget, zone-layout, or `ActuatorDriver` plumbing lives in this file — see
//! `docs/driver/display.md` for the layer architecture. A new display chip
//! needs only a similarly thin file implementing
//! [`crate::drivers::display::generic::DisplayHardware`], reusing
//! [`crate::drivers::display`] unchanged.

use serde::Deserialize;

use crate::drivers::display::compositor::ZoneConfig;
use crate::drivers::display::{default_render_interval_ms, validate_rotation};

/// Module resolutions the `ssd1306` crate supports. The controller drives
/// many glass sizes; which one is attached is a per-instance config fact,
pub const SUPPORTED_SIZES: &[(u32, u32)] =
    &[(128, 64), (128, 32), (96, 16), (72, 40), (64, 48), (64, 32)];

fn default_width() -> u32 {
    128
}

fn default_height() -> u32 {
    64
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
// The I2C fields are only read by the espidf rendering half; host builds
// still need the full schema so config validation rejects the same documents.
#[cfg_attr(not(target_os = "espidf"), allow(dead_code))]
pub struct Ssd1306Config {
    pub i2c_sda: i32,
    pub i2c_scl: i32,
    /// 7-bit I2C address; 0x3C on virtually every SSD1306 module.
    #[serde(default = "default_address")]
    pub address: u8,
    #[serde(default = "default_baudrate_hz")]
    pub baudrate_hz: u32,
    /// Native (pre-rotation) module resolution; must be one of
    /// [`SUPPORTED_SIZES`]. Defaults to the ubiquitous 128x64.
    #[serde(default = "default_width")]
    pub width: u32,
    #[serde(default = "default_height")]
    pub height: u32,
    /// Panel rotation in degrees: 0, 90, 180, or 270. 90/270 swap the
    /// module's logical width/height.
    #[serde(default)]
    pub rotation: u16,
    /// How often changed zones are actually redrawn, in milliseconds.
    #[serde(default = "default_render_interval_ms")]
    pub render_interval_ms: u32,
    /// The zones this display is partitioned into; each becomes one
    /// `display_region` entity named `<display_entity_id>.<zone_id>`.
    pub layout: Vec<ZoneConfig>,
}

fn default_address() -> u8 {
    0x3C
}

fn default_baudrate_hz() -> u32 {
    400_000
}

impl Ssd1306Config {
    /// Logical panel resolution after rotation — what the zone layout is
    /// validated against.
    pub fn dimensions(&self) -> (u32, u32) {
        if self.rotation == 90 || self.rotation == 270 {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        }
    }
}

/// Parse the driver's own config shape. Shared by config-time entity
/// expansion (host) and the hardware factory (target), so both reject the
/// same malformed documents with the same messages.
pub fn parse_config(config: &serde_json::Value) -> Result<Ssd1306Config, String> {
    let config: Ssd1306Config =
        serde_json::from_value(config.clone()).map_err(|e| e.to_string())?;
    if !SUPPORTED_SIZES.contains(&(config.width, config.height)) {
        return Err(format!(
            "unsupported SSD1306 module size {}x{}; supported: {}",
            config.width,
            config.height,
            SUPPORTED_SIZES
                .iter()
                .map(|(w, h)| format!("{w}x{h}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    validate_rotation(config.rotation)?;
    Ok(config)
}

#[cfg(target_os = "espidf")]
pub use hardware::factory;

#[cfg(target_os = "espidf")]
mod hardware {
    use std::time::Duration;

    use embedded_graphics::pixelcolor::BinaryColor;
    use embedded_graphics::prelude::*;
    use embedded_graphics::primitives::Rectangle;
    use esp_idf_svc::hal::i2c::I2cDriver;
    use esp_idf_svc::hal::units::Hertz;
    use ssd1306::mode::BufferedGraphicsMode;
    use ssd1306::prelude::*;
    use ssd1306::size::{
        DisplaySize128x32, DisplaySize128x64, DisplaySize64x32, DisplaySize64x48, DisplaySize72x40,
        DisplaySize96x16,
    };
    use ssd1306::{I2CDisplayInterface, Ssd1306};

    use super::{parse_config, Ssd1306Config};
    use crate::drivers::display::compositor::{self, Compositor};
    use crate::drivers::display::generic::{DisplayHardware, GenericDisplayDriver};
    use crate::drivers::display::widgets::WidgetPalette;
    use crate::drivers::hw::HardwarePool;
    use crate::drivers::{ActuatorDriver, Driver, DriverSpec};
    use crate::entities::{ActuatorType, Entity, EntityId, EntityType};
    use crate::error::DriverError;

    type Display<S> = Ssd1306<I2CInterface<I2cDriver<'static>>, S, BufferedGraphicsMode<S>>;

    fn display_err(e: impl core::fmt::Debug) -> DriverError {
        DriverError::Display(format!("{e:?}"))
    }

    /// SSD1306-specific hardware: owns the I2C-backed `Ssd1306` draw target
    /// and the actual flush-over-I2C call. No widget/zone/tick logic lives
    /// here — that's entirely [`GenericDisplayDriver`]'s job.
    pub struct Ssd1306Hardware<S: DisplaySize> {
        display: Display<S>,
        width: u32,
        height: u32,
    }

    impl<S: DisplaySize> DisplayHardware for Ssd1306Hardware<S> {
        type Color = BinaryColor;
        type Target = Display<S>;

        fn dimensions(&self) -> (u32, u32) {
            (self.width, self.height)
        }

        fn draw_target(&mut self) -> &mut Self::Target {
            &mut self.display
        }

        fn flush(&mut self) -> Result<(), DriverError> {
            self.display.flush().map_err(display_err)
        }
    }

    /// The size-generic tail of the factory: everything after the config
    /// layer's `(width, height)` value has been dispatched to the `ssd1306`
    /// crate's type-level display size.
    fn build<S: DisplaySize + 'static>(
        size: S,
        config: &Ssd1306Config,
        spec: &DriverSpec,
        compositor: Compositor<BinaryColor>,
        hw: &mut HardwarePool,
    ) -> Result<Box<dyn Driver>, DriverError> {
        let i2c = hw.claim_i2c(config.i2c_sda, config.i2c_scl, Hertz(config.baudrate_hz))?;
        let interface = I2CDisplayInterface::new_custom_address(i2c, config.address);
        let rotation = match config.rotation {
            90 => DisplayRotation::Rotate90,
            180 => DisplayRotation::Rotate180,
            270 => DisplayRotation::Rotate270,
            _ => DisplayRotation::Rotate0,
        };
        let mut display = Ssd1306::new(interface, size, rotation).into_buffered_graphics_mode();
        display.init().map_err(display_err)?;
        display.clear(BinaryColor::Off).map_err(display_err)?;

        let (width, height) = config.dimensions();
        let hardware = Ssd1306Hardware {
            display,
            width,
            height,
        };
        log::info!(
            "ssd1306 display '{}' up: {width}x{height} (rotation {})",
            spec.instance_id,
            config.rotation
        );

        let mut driver = GenericDisplayDriver::new(
            spec.instance_id.clone(),
            spec.entities.clone(),
            compositor,
            hardware,
        );
        // Render + flush the initial ("no data yet") frame synchronously
        // during init, rather than waiting for the first main-loop tick.
        driver.flush()?;

        Ok(Box::new(driver))
    }

    /// Registry factory for `ssd1306_display`.
    pub fn factory(
        spec: &DriverSpec,
        hw: &mut HardwarePool,
    ) -> Result<Box<dyn Driver>, DriverError> {
        let invalid = |message: String| DriverError::InvalidConfig {
            instance: spec.instance_id.clone(),
            message,
        };

        let config = parse_config(&spec.config).map_err(invalid)?;
        let (width, height) = config.dimensions();
        compositor::validate_layout(&config.layout, width, height).map_err(invalid)?;

        // spec.entities was derived by the config layer through
        // display::compositor::zone_entities(); re-derive it from the layout
        // here and cross-check, so the zone list and the entity list cannot
        // silently desynchronize.
        let declared_id: EntityId = spec
            .instance_id
            .parse()
            .map_err(|_| invalid("instance id is not a valid entity address".to_owned()))?;
        let declared = Entity {
            id: declared_id,
            entity_type: EntityType::Actuator(ActuatorType::DisplayPanel),
            name: None,
        };
        let expected = compositor::zone_entities(&declared, &config.layout).map_err(invalid)?;
        if expected
            .iter()
            .map(|e| &e.id)
            .ne(spec.entities.iter().map(|e| &e.id))
        {
            return Err(invalid(
                "spec entities do not match the zones in the layout config".to_owned(),
            ));
        }

        let zones = config
            .layout
            .iter()
            .zip(&spec.entities)
            .map(|(zone, entity)| {
                (
                    entity.id.clone(),
                    Rectangle::new(
                        Point::new(zone.x as i32, zone.y as i32),
                        Size::new(zone.width, zone.height),
                    ),
                    zone.widget.clone(),
                )
            })
            .collect();
        let palette = WidgetPalette {
            foreground: BinaryColor::On,
            background: BinaryColor::Off,
            dim: BinaryColor::On,
        };
        let compositor = Compositor::new(zones, palette)
            .with_render_interval(Duration::from_millis(u64::from(config.render_interval_ms)));

        // Bridge the config-level (width, height) value to the ssd1306
        // crate's type-level sizes. Must stay in sync with SUPPORTED_SIZES,
        // which parse_config already validated against.
        match (config.width, config.height) {
            (128, 64) => build(DisplaySize128x64, &config, spec, compositor, hw),
            (128, 32) => build(DisplaySize128x32, &config, spec, compositor, hw),
            (96, 16) => build(DisplaySize96x16, &config, spec, compositor, hw),
            (72, 40) => build(DisplaySize72x40, &config, spec, compositor, hw),
            (64, 48) => build(DisplaySize64x48, &config, spec, compositor, hw),
            (64, 32) => build(DisplaySize64x32, &config, spec, compositor, hw),
            (w, h) => Err(invalid(format!(
                "SUPPORTED_SIZES allows {w}x{h} but no type-level size is wired up for it"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_full_config_with_layout() {
        let config = parse_config(&json!({
            "i2c_sda": 21,
            "i2c_scl": 22,
            "layout": [
                { "id": "top", "x": 0, "y": 0, "width": 128, "height": 40,
                  "widget": { "kind": "text", "template": "{{value}}" } },
            ]
        }))
        .unwrap();
        assert_eq!(config.i2c_sda, 21);
        assert_eq!(config.address, 0x3C);
        assert_eq!(config.baudrate_hz, 400_000);
        assert_eq!(config.dimensions(), (128, 64), "size defaults to 128x64");
        assert_eq!(config.render_interval_ms, 80);
        assert_eq!(config.layout.len(), 1);
    }

    #[test]
    fn accepts_supported_module_sizes_and_rotation_swaps_dimensions() {
        let config = parse_config(&json!({
            "i2c_sda": 21, "i2c_scl": 22, "width": 128, "height": 32,
            "rotation": 90, "layout": []
        }))
        .unwrap();
        assert_eq!(config.dimensions(), (32, 128));
    }

    #[test]
    fn rejects_unsupported_module_size() {
        let err = parse_config(&json!({
            "i2c_sda": 21, "i2c_scl": 22, "width": 100, "height": 50, "layout": []
        }))
        .unwrap_err();
        assert!(err.contains("100x50"), "{err}");
        assert!(err.contains("128x64"), "should list supported sizes: {err}");
    }

    #[test]
    fn rejects_invalid_rotation() {
        let err = parse_config(&json!({
            "i2c_sda": 21, "i2c_scl": 22, "rotation": 45, "layout": []
        }))
        .unwrap_err();
        assert!(err.contains("rotation"), "{err}");
    }

    #[test]
    fn rejects_unknown_top_level_field() {
        let err = parse_config(&json!({
            "i2c_sda": 21, "i2c_scl": 22, "layout": [], "wat": true
        }))
        .unwrap_err();
        assert!(
            err.contains("wat") || err.to_lowercase().contains("unknown"),
            "{err}"
        );
    }

    #[test]
    fn rejects_missing_i2c_pins() {
        assert!(parse_config(&json!({ "layout": [] })).is_err());
    }
}
