//! `ili9341_display` driver: thin ILI9341-specific layer (SPI color TFT,
//! `Rgb565`) over the chip-agnostic [`crate::drivers::display`]
//! widget/compositor/generic-driver code.
//!
//! Everything here is either a hardware fact (SPI pins, panel resolution,
//! rotation) or hardware I/O (init via `mipidsi`). No widget, zone-layout, or
//! `ActuatorDriver` plumbing lives in this file — see
//! `docs/driver/display.md` for the layer architecture.
//!
//! Unlike the SSD1306 (whose crate buffers a full frame in RAM and pushes it
//! on `flush()`), `mipidsi` writes draw operations straight to the panel over
//! SPI: a 320x240 Rgb565 framebuffer would be 150 KiB — half the ESP32's
//! usable RAM — so buffering is not an option. `DisplayHardware::flush()` is
//! therefore a no-op; the compositor's dirty-tracking still ensures zones are
//! only redrawn (i.e. bytes only cross the bus) when their content changed.

use serde::Deserialize;

use crate::drivers::display::compositor::ZoneConfig;
use crate::drivers::display::{default_render_interval_ms, validate_rotation};

/// The ILI9341 controller's framebuffer (rotation 0). Modules with smaller
/// glass configure `width`/`height`/`offset_*` below within these limits.
pub const NATIVE_WIDTH: u32 = 240;
pub const NATIVE_HEIGHT: u32 = 320;

fn default_width() -> u32 {
    NATIVE_WIDTH
}

fn default_height() -> u32 {
    NATIVE_HEIGHT
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
// The SPI fields are only read by the espidf half; host builds still need
// the full schema so config validation rejects the same documents.
#[cfg_attr(not(target_os = "espidf"), allow(dead_code))]
pub struct Ili9341Config {
    pub spi_sck: i32,
    pub spi_mosi: i32,
    pub spi_cs: i32,
    /// Data/command select pin (often labelled D/C or RS).
    pub spi_dc: i32,
    /// Hardware reset pin.
    pub spi_rst: i32,
    #[serde(default = "default_baudrate_hz")]
    pub baudrate_hz: u32,
    /// Native (pre-rotation) visible panel size; defaults to the
    /// controller's full 240x320 framebuffer. Modules with smaller glass set
    /// this (plus `offset_x`/`offset_y` for where their glass sits in the
    /// framebuffer).
    #[serde(default = "default_width")]
    pub width: u32,
    #[serde(default = "default_height")]
    pub height: u32,
    #[serde(default)]
    pub offset_x: u16,
    #[serde(default)]
    pub offset_y: u16,
    /// Panel rotation in degrees: 0, 90, 180, or 270. 90/270 swap the
    /// panel's logical width/height (landscape).
    #[serde(default)]
    pub rotation: u16,
    /// Mirror the output (MADCTL mirror bit). Some modules — and Wokwi's
    /// ILI9341 — scan in the opposite direction from the mipidsi default,
    /// which shows as horizontally mirrored output.
    #[serde(default)]
    pub mirrored: bool,
    /// How often changed zones are actually redrawn, in milliseconds.
    #[serde(default = "default_render_interval_ms")]
    pub render_interval_ms: u32,
    /// The zones this display is partitioned into; each becomes one
    /// `display_region` entity named `<display_entity_id>.<zone_id>`.
    pub layout: Vec<ZoneConfig>,
}

fn default_baudrate_hz() -> u32 {
    // The ESP32 caps SPI routed through the GPIO matrix at 26.67 MHz and
    // aborts at bus setup above that; the higher 40 MHz the ILI9341 itself
    // supports is only reachable on a controller's dedicated IOMUX pins
    // (SPI2: SCLK=14, MOSI=13). Default to the safe rate that works on any
    // pins — a wiring that uses the IOMUX pins can raise this in config.
    26_000_000
}

impl Ili9341Config {
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
pub fn parse_config(config: &serde_json::Value) -> Result<Ili9341Config, String> {
    let config: Ili9341Config =
        serde_json::from_value(config.clone()).map_err(|e| e.to_string())?;
    validate_rotation(config.rotation)?;
    if config.width == 0
        || config.height == 0
        || config.width + u32::from(config.offset_x) > NATIVE_WIDTH
        || config.height + u32::from(config.offset_y) > NATIVE_HEIGHT
    {
        return Err(format!(
            "panel area {}x{} at offset ({}, {}) does not fit the ILI9341's \
             {NATIVE_WIDTH}x{NATIVE_HEIGHT} framebuffer",
            config.width, config.height, config.offset_x, config.offset_y
        ));
    }
    Ok(config)
}

#[cfg(target_os = "espidf")]
pub use hardware::factory;

#[cfg(target_os = "espidf")]
mod hardware {
    use embedded_graphics::pixelcolor::{Rgb565, RgbColor, WebColors};
    use embedded_graphics::prelude::*;
    use embedded_graphics::primitives::Rectangle;
    use std::time::Duration;

    use esp_idf_svc::hal::delay::Delay;
    use esp_idf_svc::hal::gpio::{Output, PinDriver};
    use esp_idf_svc::hal::spi::{SpiDeviceDriver, SpiDriver};
    use esp_idf_svc::hal::units::Hertz;
    use mipidsi::interface::SpiInterface;
    use mipidsi::models::ILI9341Rgb565;
    use mipidsi::options::{Orientation, Rotation};
    use mipidsi::Builder;

    use super::{parse_config, Ili9341Config};
    use crate::drivers::display::compositor::{self, Compositor};
    use crate::drivers::display::generic::{DisplayHardware, GenericDisplayDriver};
    use crate::drivers::display::widgets::WidgetPalette;
    use crate::drivers::hw::HardwarePool;
    use crate::drivers::{ActuatorDriver, Driver, DriverSpec};
    use crate::entities::{ActuatorType, Entity, EntityId, EntityType};
    use crate::error::DriverError;

    type OutPin = PinDriver<'static, Output>;
    type Display = mipidsi::Display<
        SpiInterface<'static, SpiDeviceDriver<'static, SpiDriver<'static>>, OutPin>,
        ILI9341Rgb565,
        OutPin,
    >;

    fn display_err(e: impl core::fmt::Debug) -> DriverError {
        DriverError::Display(format!("{e:?}"))
    }

    /// ILI9341-specific hardware: owns the SPI-backed `mipidsi` draw target.
    /// No widget/zone/tick logic lives here — that's entirely
    /// [`GenericDisplayDriver`]'s job.
    pub struct Ili9341Hardware {
        display: Display,
        width: u32,
        height: u32,
    }

    impl Ili9341Hardware {
        fn new(config: &Ili9341Config, hw: &mut HardwarePool) -> Result<Self, DriverError> {
            let spi = hw.claim_spi(
                config.spi_sck,
                config.spi_mosi,
                config.spi_cs,
                Hertz(config.baudrate_hz),
            )?;
            let dc = PinDriver::output(hw.claim_pin(config.spi_dc)?)?;
            let rst = PinDriver::output(hw.claim_pin(config.spi_rst)?)?;

            // mipidsi's SpiInterface borrows a scratch buffer for pixel
            // batching; each buffer-full is one SPI transaction, so its size
            // is matched to the DMA transfer size configured in
            // `HardwarePool::claim_spi` (a zone blit then costs area/4096
            // transactions). Drivers are instantiated once at boot and never
            // dropped, so the buffer is leaked to obtain the `'static`
            // borrow the interface type requires.
            let buffer: &'static mut [u8] = Box::leak(Box::new([0u8; 4096]));
            let interface = SpiInterface::new(spi, dc, buffer);

            let rotation = match config.rotation {
                90 => Rotation::Deg90,
                180 => Rotation::Deg180,
                270 => Rotation::Deg270,
                _ => Rotation::Deg0,
            };
            let mut delay = Delay::new_default();
            let mut display = Builder::new(ILI9341Rgb565, interface)
                .reset_pin(rst)
                .display_size(config.width as u16, config.height as u16)
                .display_offset(config.offset_x, config.offset_y)
                .orientation(Orientation {
                    rotation,
                    mirrored: config.mirrored,
                })
                .init(&mut delay)
                .map_err(display_err)?;
            display.clear(Rgb565::BLACK).map_err(display_err)?;

            let (width, height) = config.dimensions();
            Ok(Self {
                display,
                width,
                height,
            })
        }
    }

    impl DisplayHardware for Ili9341Hardware {
        type Color = Rgb565;
        type Target = Display;

        fn dimensions(&self) -> (u32, u32) {
            (self.width, self.height)
        }

        fn draw_target(&mut self) -> &mut Self::Target {
            &mut self.display
        }

        /// No-op: `mipidsi` draws write-through over SPI (see module docs);
        /// there is no RAM framebuffer to push.
        fn flush(&mut self) -> Result<(), DriverError> {
            Ok(())
        }
    }

    /// Registry factory for `ili9341_display`.
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

        // Same cross-check as the SSD1306 factory: re-derive the zone
        // entities from the layout so the zone list and the entity list
        // cannot silently desynchronize.
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
        // A color panel gets a literal dim shade — unlike the monochrome
        // palette, where `dim == foreground` and dithering does the work.
        let palette = WidgetPalette {
            foreground: Rgb565::WHITE,
            background: Rgb565::BLACK,
            dim: Rgb565::CSS_DIM_GRAY,
        };
        // Zone buffering is required on a write-through panel: drawn
        // directly, fine-grained widget output (dither, arc strokes, glyphs)
        // becomes one bus transaction per pixel, each carrying full
        // address-window overhead.
        let compositor = Compositor::new(zones, palette)
            .with_render_interval(Duration::from_millis(u64::from(config.render_interval_ms)))
            .with_zone_buffering();

        let hardware = Ili9341Hardware::new(&config, hw)?;
        log::info!(
            "ili9341 display '{}' up: {width}x{height} (rotation {})",
            spec.instance_id,
            config.rotation
        );

        let mut driver = GenericDisplayDriver::new(
            spec.instance_id.clone(),
            spec.entities.clone(),
            compositor,
            hardware,
        );
        // Render the initial ("no data yet") frame synchronously during
        // init, rather than waiting for the first main-loop tick.
        driver.flush()?;

        Ok(Box::new(driver))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn base_config(rotation: u16) -> serde_json::Value {
        json!({
            "spi_sck": 18,
            "spi_mosi": 23,
            "spi_cs": 15,
            "spi_dc": 2,
            "spi_rst": 16,
            "rotation": rotation,
            "layout": [
                { "id": "dial", "x": 0, "y": 0, "width": 200, "height": 200,
                  "widget": { "kind": "gauge_arc", "min": 0, "max": 500 } },
            ]
        })
    }

    #[test]
    fn parses_full_config_and_applies_defaults() {
        let config = parse_config(&base_config(0)).unwrap();
        assert_eq!(config.spi_sck, 18);
        assert_eq!(config.baudrate_hz, 26_000_000);
        assert_eq!(config.dimensions(), (240, 320));
    }

    #[test]
    fn rotation_swaps_dimensions() {
        assert_eq!(
            parse_config(&base_config(90)).unwrap().dimensions(),
            (320, 240)
        );
        assert_eq!(
            parse_config(&base_config(180)).unwrap().dimensions(),
            (240, 320)
        );
        assert_eq!(
            parse_config(&base_config(270)).unwrap().dimensions(),
            (320, 240)
        );
    }

    #[test]
    fn rotation_defaults_to_portrait() {
        let mut config = base_config(0);
        config.as_object_mut().unwrap().remove("rotation");
        assert_eq!(parse_config(&config).unwrap().dimensions(), (240, 320));
    }

    #[test]
    fn rejects_invalid_rotation() {
        let err = parse_config(&base_config(45)).unwrap_err();
        assert!(err.contains("rotation"), "{err}");
    }

    #[test]
    fn sub_native_panel_size_flows_into_dimensions() {
        let mut config = base_config(90);
        let obj = config.as_object_mut().unwrap();
        obj.insert("width".to_owned(), json!(200));
        obj.insert("height".to_owned(), json!(280));
        obj.insert("offset_x".to_owned(), json!(40));
        obj.insert("offset_y".to_owned(), json!(40));
        let config = parse_config(&config).unwrap();
        assert_eq!(config.dimensions(), (280, 200), "rotated sub-native size");
    }

    #[test]
    fn rejects_panel_area_exceeding_the_framebuffer() {
        for (width, height, offset_x, offset_y) in [
            (241, 320, 0, 0),
            (240, 321, 0, 0),
            (240, 320, 1, 0),
            (0, 320, 0, 0),
        ] {
            let mut config = base_config(0);
            let obj = config.as_object_mut().unwrap();
            obj.insert("width".to_owned(), json!(width));
            obj.insert("height".to_owned(), json!(height));
            obj.insert("offset_x".to_owned(), json!(offset_x));
            obj.insert("offset_y".to_owned(), json!(offset_y));
            let err = parse_config(&config).unwrap_err();
            assert!(
                err.contains("framebuffer"),
                "{width}x{height}+({offset_x},{offset_y}): {err}"
            );
        }
    }

    #[test]
    fn rejects_unknown_top_level_field() {
        let mut config = base_config(0);
        config
            .as_object_mut()
            .unwrap()
            .insert("wat".to_owned(), json!(true));
        let err = parse_config(&config).unwrap_err();
        assert!(
            err.contains("wat") || err.to_lowercase().contains("unknown"),
            "{err}"
        );
    }

    #[test]
    fn rejects_missing_spi_pins() {
        assert!(parse_config(&json!({ "layout": [] })).is_err());
    }
}
