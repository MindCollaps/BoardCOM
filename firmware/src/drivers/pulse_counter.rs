//! `pulse_counter` driver: counts pulses on a GPIO via the ESP32's hardware
//! PCNT peripheral and converts them to a rate-based sensor value.
//!
//! One generic driver covers every "pulse train in, rate out" sensor:
//! - entity type `rpm`: rate = pulses / pulses_per_revolution, in rev/min
//! - entity type `speed`: additionally scaled by `meters_per_revolution`
//!
//! A new instance (another pickup on another pin) is pure JSON config.

use std::time::{Duration, Instant};

use esp_idf_svc::hal::gpio::AnyInputPin;
use esp_idf_svc::hal::pcnt::config::{
    ChannelConfig, ChannelEdgeAction, GlitchFilterConfig, UnitConfig,
};
use esp_idf_svc::hal::pcnt::PcntUnitDriver;
use esp_idf_svc::sys::{esp, gpio_num_t, gpio_pull_mode_t_GPIO_PULLUP_ONLY, gpio_set_pull_mode};
use serde::Deserialize;

use super::hw::HardwarePool;
use super::{Driver, DriverSpec, Reading, SensorDriver};
use crate::entities::{Entity, EntityId, EntityType, SensorType, SensorValue};
use crate::error::DriverError;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PulseCounterConfig {
    /// GPIO the (already signal-conditioned) pulse train arrives on.
    pin: i32,
    /// Pulses per revolution of the measured shaft/wheel.
    #[serde(default = "default_pulses_per_revolution")]
    pulses_per_revolution: u32,
    /// Length of one counting window; one reading is produced per window.
    #[serde(default = "default_sample_window_ms")]
    sample_window_ms: u64,
    /// Distance travelled per revolution; required for `speed` entities.
    /// f64 rather than f32: serde-deserializing f32 crashes the Xtensa LLVM
    /// backend (constant-pool instruction-selection bug), and f64 is
    /// serde_json's native number type.
    meters_per_revolution: Option<f64>,
}

fn default_pulses_per_revolution() -> u32 {
    1
}

fn default_sample_window_ms() -> u64 {
    500
}

pub struct PulseCounterDriver {
    instance_id: String,
    entities: Vec<Entity>,
    entity_id: EntityId,
    output: SensorType,
    config: PulseCounterConfig,
    pcnt: PcntUnitDriver<'static>,
    window_start: Instant,
}

/// Registry factory for `pulse_counter`.
pub fn factory(spec: &DriverSpec, hw: &mut HardwarePool) -> Result<Box<dyn Driver>, DriverError> {
    let invalid = |message: String| DriverError::InvalidConfig {
        instance: spec.instance_id.clone(),
        message,
    };

    let config: PulseCounterConfig =
        serde_json::from_value(spec.config.clone()).map_err(|e| invalid(e.to_string()))?;
    if config.pulses_per_revolution == 0 {
        return Err(invalid("pulses_per_revolution must be >= 1".to_owned()));
    }
    if config.sample_window_ms == 0 {
        return Err(invalid("sample_window_ms must be >= 1".to_owned()));
    }

    let [entity] = spec.entities.as_slice() else {
        return Err(invalid(format!(
            "pulse_counter exposes exactly one entity, got {}",
            spec.entities.len()
        )));
    };
    let output = match entity.entity_type {
        EntityType::Sensor(t) => t,
        EntityType::Actuator(_) => {
            return Err(invalid(format!(
                "pulse_counter cannot provide actuator entity '{}'",
                entity.id
            )))
        }
    };
    if output == SensorType::Speed && config.meters_per_revolution.is_none() {
        return Err(invalid(
            "meters_per_revolution is required for a 'speed' entity".to_owned(),
        ));
    }

    let pin = hw.claim_pin(config.pin)?;
    // Counting only goes up; the counter self-resets at the limits, which
    // stay unreachable because every sampling window ends in clear_count().
    let mut pcnt = hw.claim_pcnt_unit(&UnitConfig {
        low_limit: -32_767,
        high_limit: 32_767,
        ..Default::default()
    })?;
    // Hardware glitch filter (max ~1 µs): rejects spikes, not switch bounce —
    // real debounce belongs in the signal conditioning ahead of the pin.
    pcnt.set_glitch_filter(Some(&GlitchFilterConfig {
        max_glitch: Duration::from_nanos(1_000),
        ..Default::default()
    }))?;
    pcnt.add_channel(Some(pin), AnyInputPin::none(), &ChannelConfig::default())?
        // Count falling edges only: with the pull-up below, one pulse to
        // ground is exactly one count.
        .set_edge_action(ChannelEdgeAction::Hold, ChannelEdgeAction::Increase)?;
    // The PCNT driver claims the pin but leaves pull configuration alone;
    // an open (button/open-collector) input needs the internal pull-up.
    esp!(unsafe {
        gpio_set_pull_mode(config.pin as gpio_num_t, gpio_pull_mode_t_GPIO_PULLUP_ONLY)
    })?;
    pcnt.enable()?;
    pcnt.clear_count()?;
    pcnt.start()?;

    Ok(Box::new(PulseCounterDriver {
        instance_id: spec.instance_id.clone(),
        entities: spec.entities.clone(),
        entity_id: entity.id.clone(),
        output,
        config,
        pcnt,
        window_start: Instant::now(),
    }))
}

impl Driver for PulseCounterDriver {
    fn instance_id(&self) -> &str {
        &self.instance_id
    }

    fn entities(&self) -> &[Entity] {
        &self.entities
    }

    fn as_sensor(&mut self) -> Option<&mut dyn SensorDriver> {
        Some(self)
    }
}

impl SensorDriver for PulseCounterDriver {
    fn poll(&mut self, now: Instant) -> Result<Vec<Reading>, DriverError> {
        let elapsed = now.duration_since(self.window_start);
        if elapsed.as_millis() < u128::from(self.config.sample_window_ms) {
            return Ok(Vec::new());
        }

        // Pulses arriving between get and clear are lost; at most one pulse
        // per window, which is far below the sensor's noise floor.
        let pulses = self.pcnt.get_count()?.max(0);
        self.pcnt.clear_count()?;
        self.window_start = now;

        let revs_per_minute = f64::from(pulses) / f64::from(self.config.pulses_per_revolution)
            * (60_000.0 / elapsed.as_millis() as f64);

        let value = match self.output {
            SensorType::Rpm => SensorValue::Rpm(revs_per_minute.round() as u32),
            SensorType::Speed => {
                // rev/min * m/rev = m/min; * 60 / 1000 = km/h
                let meters_per_rev = self
                    .config
                    .meters_per_revolution
                    .expect("validated in factory");
                SensorValue::Speed((revs_per_minute * meters_per_rev * 60.0 / 1000.0) as f32)
            }
        };

        Ok(vec![Reading {
            entity: self.entity_id.clone(),
            value,
        }])
    }
}
