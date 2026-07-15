//! `DisplayHardware` trait + `GenericDisplayDriver<H>`: the last mile of
//! decoupling hardware from rendering.
//!
//! Widget/Compositor already decouple *rendering* from hardware, but without
//! this layer, every new display chip still hand-writes identical
//! `ActuatorDriver` plumbing — layout config parsing, Compositor wiring,
//! `execute()`/`flush()` glue — when only hardware init/`DrawTarget`/flush
//! actually differs between chips. `GenericDisplayDriver` implements
//! `ActuatorDriver` **once**, generically over [`DisplayHardware`]; a new
//! display chip only needs to implement that trait's handful of methods and
//! gets all zone/widget/dispatch plumbing for free.
//!
//! Not `target_os = "espidf"`-gated, like the rest of `drivers::display`, so
//! the unit tests can exercise the full driver plumbing on the host against
//! a fake [`DisplayHardware`].

use std::time::Instant;

use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::pixelcolor::PixelColor;

use super::compositor::Compositor;
use crate::drivers::{ActuatorDriver, Driver};
use crate::entities::{ActuatorCommand, Entity, EntityId};
use crate::error::DriverError;

/// What a concrete display chip driver must provide. Construction is
/// deliberately *not* part of this trait: each chip's config shape differs
/// (I2C pins vs. SPI pins vs. whatever an e-ink controller needs), so there
/// is no sensible uniform `init` signature — a chip's own driver module
/// exposes an inherent constructor instead and calls it directly before
/// building a [`GenericDisplayDriver`].
pub trait DisplayHardware {
    type Color: PixelColor;
    type Target: DrawTarget<Color = Self::Color>;

    /// Physical panel resolution (width, height) in pixels.
    fn dimensions(&self) -> (u32, u32);

    /// The target all rendering draws onto.
    fn draw_target(&mut self) -> &mut Self::Target;

    /// Push the current framebuffer contents to the physical display.
    fn flush(&mut self) -> Result<(), DriverError>;
}

/// One `ActuatorDriver` implementation, shared by every display chip: owns a
/// [`Compositor`] and a [`DisplayHardware`], and wires `execute()`/`flush()`
/// against them. `execute()` only stages the update (see `Compositor::stage`
/// — value updates and render ticks are decoupled); `flush()` is where the
/// tick actually happens and, only if anything changed, the hardware flush.
pub struct GenericDisplayDriver<H: DisplayHardware> {
    instance_id: String,
    entities: Vec<Entity>,
    compositor: Compositor<H::Color>,
    hardware: H,
}

impl<H: DisplayHardware> GenericDisplayDriver<H> {
    pub fn new(
        instance_id: String,
        entities: Vec<Entity>,
        compositor: Compositor<H::Color>,
        hardware: H,
    ) -> Self {
        Self {
            instance_id,
            entities,
            compositor,
            hardware,
        }
    }
}

impl<H> Driver for GenericDisplayDriver<H>
where
    H: DisplayHardware,
    <H::Target as DrawTarget>::Error: core::fmt::Debug,
{
    fn instance_id(&self) -> &str {
        &self.instance_id
    }

    fn entities(&self) -> &[Entity] {
        &self.entities
    }

    fn as_actuator(&mut self) -> Option<&mut dyn ActuatorDriver> {
        Some(self)
    }
}

impl<H> ActuatorDriver for GenericDisplayDriver<H>
where
    H: DisplayHardware,
    <H::Target as DrawTarget>::Error: core::fmt::Debug,
{
    fn execute(&mut self, target: &EntityId, command: &ActuatorCommand) -> Result<(), DriverError> {
        let Some(idx) = self.compositor.zone_index(target) else {
            return Err(DriverError::UnknownEntity {
                instance: self.instance_id.clone(),
                entity: target.clone(),
            });
        };
        let ActuatorCommand::SourceUpdate {
            availability,
            value,
        } = command;
        self.compositor.stage(idx, *availability, *value);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        self.compositor
            .tick(self.hardware.draw_target(), Instant::now())
            .map_err(|e| DriverError::Display(format!("{e:?}")))?;
        if self.compositor.take_dirty() {
            self.hardware.flush()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::display::widgets::{WidgetConfig, WidgetPalette};
    use crate::entities::state::Availability;
    use crate::entities::{ActuatorType, EntityType, SensorValue};
    use embedded_graphics::geometry::{Point, Size};
    use embedded_graphics::mock_display::MockDisplay;
    use embedded_graphics::pixelcolor::BinaryColor;
    use embedded_graphics::primitives::Rectangle;

    /// Minimal fake hardware: a `MockDisplay` behind the `DisplayHardware`
    /// trait. Counts `flush()` calls so tests can observe that hardware is
    /// only touched when the compositor actually has something new to show.
    struct FakeHardware {
        target: MockDisplay<BinaryColor>,
        flush_count: usize,
    }

    impl FakeHardware {
        fn new() -> Self {
            let mut target = MockDisplay::new();
            target.set_allow_overdraw(true);
            target.set_allow_out_of_bounds_drawing(true);
            Self {
                target,
                flush_count: 0,
            }
        }
    }

    impl DisplayHardware for FakeHardware {
        type Color = BinaryColor;
        type Target = MockDisplay<BinaryColor>;

        fn dimensions(&self) -> (u32, u32) {
            (64, 64)
        }

        fn draw_target(&mut self) -> &mut Self::Target {
            &mut self.target
        }

        fn flush(&mut self) -> Result<(), DriverError> {
            self.flush_count += 1;
            Ok(())
        }
    }

    fn zone_id() -> EntityId {
        "gateway.display.top".parse().unwrap()
    }

    fn driver() -> GenericDisplayDriver<FakeHardware> {
        // Render-tick rate-limiting is exercised thoroughly at the
        // Compositor level (see compositor.rs); this test is only about
        // GenericDisplayDriver's execute()/flush() plumbing, so a zero
        // interval keeps it independent of wall-clock timing between calls.
        let compositor = Compositor::new(
            vec![(
                zone_id(),
                Rectangle::new(Point::zero(), Size::new(40, 16)),
                WidgetConfig::Text {
                    template: "{{value}}".to_owned(),
                    font: Default::default(),
                    align: Default::default(),
                },
            )],
            WidgetPalette {
                foreground: BinaryColor::On,
                background: BinaryColor::Off,
                dim: BinaryColor::On,
            },
        )
        .with_render_interval(std::time::Duration::ZERO);
        GenericDisplayDriver::new(
            "gateway.display".to_owned(),
            vec![Entity {
                id: zone_id(),
                entity_type: EntityType::Actuator(ActuatorType::DisplayRegion),
                name: None,
            }],
            compositor,
            FakeHardware::new(),
        )
    }

    #[test]
    fn execute_stages_without_touching_hardware() {
        let mut driver = driver();
        driver
            .execute(
                &zone_id(),
                &ActuatorCommand::SourceUpdate {
                    availability: Availability::Online,
                    value: Some(SensorValue::Rpm(4200)),
                },
            )
            .unwrap();
        assert_eq!(driver.hardware.flush_count, 0, "execute() must not flush");
    }

    #[test]
    fn execute_on_unknown_target_reports_unknown_entity() {
        let mut driver = driver();
        let unknown: EntityId = "gateway.display.ghost".parse().unwrap();
        let err = driver
            .execute(
                &unknown,
                &ActuatorCommand::SourceUpdate {
                    availability: Availability::Online,
                    value: Some(SensorValue::Rpm(1)),
                },
            )
            .unwrap_err();
        assert!(matches!(err, DriverError::UnknownEntity { .. }));
    }

    #[test]
    fn flush_renders_and_calls_hardware_flush_only_when_dirty() {
        let mut driver = driver();

        // First flush: the compositor's first tick always renders its
        // initial ("no data yet") frame, so hardware.flush() is called once.
        driver.flush().unwrap();
        assert_eq!(driver.hardware.flush_count, 1);

        // No new execute() in between: nothing changed, so no further
        // hardware flush, even though flush() runs again.
        driver.flush().unwrap();
        assert_eq!(driver.hardware.flush_count, 1);

        // A genuine update, then flush: hardware is touched again.
        driver
            .execute(
                &zone_id(),
                &ActuatorCommand::SourceUpdate {
                    availability: Availability::Online,
                    value: Some(SensorValue::Rpm(4200)),
                },
            )
            .unwrap();
        driver.flush().unwrap();
        assert_eq!(driver.hardware.flush_count, 2);
    }
}
