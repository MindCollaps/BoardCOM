//! Driver registry: maps config-level driver kind names to factories.
//!
//! The main loop never names a concrete driver type; it asks the registry to
//! instantiate whatever the validated config specifies and works with the
//! resulting trait objects. A future plugin mechanism registers additional
//! factories here — nothing downstream changes.

use std::collections::BTreeMap;

use super::hw::HardwarePool;
use super::{ili9341_display, pulse_counter, ssd1306_display, Driver, DriverSpec};
use super::{ILI9341_DISPLAY_KIND, PULSE_COUNTER_KIND, SSD1306_DISPLAY_KIND};
use crate::error::DriverError;

/// Builds one driver instance from its spec, claiming hardware from the pool.
pub type DriverFactory = fn(&DriverSpec, &mut HardwarePool) -> Result<Box<dyn Driver>, DriverError>;

pub struct DriverRegistry {
    factories: BTreeMap<&'static str, DriverFactory>,
}

impl DriverRegistry {
    /// Registry with all built-in drivers registered.
    pub fn with_builtins() -> Self {
        let mut registry = Self {
            factories: BTreeMap::new(),
        };
        registry.register(PULSE_COUNTER_KIND, pulse_counter::factory);
        registry.register(SSD1306_DISPLAY_KIND, ssd1306_display::factory);
        registry.register(ILI9341_DISPLAY_KIND, ili9341_display::factory);
        registry
    }

    pub fn register(&mut self, kind: &'static str, factory: DriverFactory) {
        self.factories.insert(kind, factory);
    }

    pub fn instantiate(
        &self,
        spec: &DriverSpec,
        hw: &mut HardwarePool,
    ) -> Result<Box<dyn Driver>, DriverError> {
        let factory = self
            .factories
            .get(spec.kind.as_str())
            .ok_or_else(|| DriverError::UnknownKind(spec.kind.clone()))?;
        factory(spec, hw)
    }
}
