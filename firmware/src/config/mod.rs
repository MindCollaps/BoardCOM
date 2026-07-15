//! JSON configuration: schema types and the pure validation step.
//!
//! [`parse_config`] turns a JSON document into validated devices, driver
//! specs, and bindings. It is a pure function of its inputs (the document and
//! the set of known driver kinds) with no hardware or ESP-IDF dependency, so
//! everything here is unit-tested on the host. Hardware only enters the
//! picture later, when the driver registry instantiates the returned specs.

use std::collections::HashSet;

use serde::Deserialize;

use crate::automations::Binding;
use crate::drivers::DriverSpec;
use crate::entities::{Device, DeviceId, Entity, EntityDomain, EntityId, EntityType};
use crate::error::ConfigError;

/// Raw JSON schema, exactly as written by the user / management app.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    devices: Vec<RawDevice>,
    #[serde(default)]
    bindings: Vec<RawBinding>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDevice {
    id: String,
    name: Option<String>,
    entities: Vec<RawEntity>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntity {
    id: String,
    #[serde(rename = "type")]
    entity_type: EntityType,
    driver: String,
    #[serde(default)]
    driver_config: serde_json::Value,
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBinding {
    source: EntityId,
    target: EntityId,
}

/// The validated result: real domain types, ready for instantiation.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedConfig {
    pub devices: Vec<Device>,
    pub driver_specs: Vec<DriverSpec>,
    pub bindings: Vec<Binding>,
}

/// Parse and validate a JSON config document.
///
/// `known_driver_kinds` is passed in (rather than read from the registry) to
/// keep this a pure function; callers pass
/// [`crate::drivers::BUILTIN_DRIVER_KINDS`] plus any plugin kinds.
///
/// Currently each entity gets its own driver instance (`instance_id` =
/// entity address). Bridge devices will extend the schema with device-level
/// drivers exposing several entities per instance; [`DriverSpec::entities`]
/// is already a list for that reason.
pub fn parse_config(
    json: &str,
    known_driver_kinds: &[&str],
) -> Result<ValidatedConfig, ConfigError> {
    let raw: RawConfig = serde_json::from_str(json)?;
    let kinds: HashSet<&str> = known_driver_kinds.iter().copied().collect();

    let mut devices = Vec::new();
    let mut driver_specs = Vec::new();
    // Addressable entities (what bindings may reference): the *expanded*
    // list. Declared ids of expanding drivers (e.g. `gateway.display`) are
    // not addressable but still must be unique — tracked in `seen_ids`.
    let mut entity_types: Vec<(EntityId, EntityType)> = Vec::new();
    let mut seen_ids: HashSet<EntityId> = HashSet::new();

    for raw_device in &raw.devices {
        let device_id = DeviceId::new(&raw_device.id)?;
        if devices.iter().any(|d: &Device| d.id == device_id) {
            return Err(ConfigError::DuplicateDevice(raw_device.id.clone()));
        }
        devices.push(Device {
            id: device_id.clone(),
            name: raw_device
                .name
                .clone()
                .unwrap_or_else(|| raw_device.id.clone()),
        });

        for raw_entity in &raw_device.entities {
            let entity_id = EntityId::new(device_id.clone(), &raw_entity.id)?;
            if !seen_ids.insert(entity_id.clone()) {
                return Err(ConfigError::DuplicateEntity(entity_id));
            }
            if !kinds.contains(raw_entity.driver.as_str()) {
                return Err(ConfigError::UnknownDriverKind {
                    entity: entity_id,
                    kind: raw_entity.driver.clone(),
                });
            }
            let declared = Entity {
                id: entity_id.clone(),
                entity_type: raw_entity.entity_type,
                name: raw_entity.name.clone(),
            };
            // A driver's entity count is config-driven, not fixed at 1: a
            // partitioned actuator (display zones) or a bridge device
            // expands one declared entity into several addressable ones.
            let exposed = crate::drivers::expand_entities(
                &raw_entity.driver,
                &declared,
                &raw_entity.driver_config,
            )
            .map_err(|message| ConfigError::InvalidDriverConfig {
                entity: entity_id.clone(),
                message,
            })?;
            for entity in &exposed {
                if entity.id != entity_id && !seen_ids.insert(entity.id.clone()) {
                    return Err(ConfigError::DuplicateEntity(entity.id.clone()));
                }
                entity_types.push((entity.id.clone(), entity.entity_type));
            }
            driver_specs.push(DriverSpec {
                instance_id: entity_id.to_string(),
                kind: raw_entity.driver.clone(),
                entities: exposed,
                config: raw_entity.driver_config.clone(),
            });
        }
    }

    let domain_of = |id: &EntityId| {
        entity_types
            .iter()
            .find(|(known, _)| known == id)
            .map(|(_, t)| t.domain())
    };

    let mut bindings = Vec::new();
    for raw_binding in &raw.bindings {
        match domain_of(&raw_binding.source) {
            None => {
                return Err(ConfigError::UnknownBindingSource(
                    raw_binding.source.clone(),
                ))
            }
            Some(EntityDomain::Actuator) => {
                return Err(ConfigError::BindingSourceNotSensor(
                    raw_binding.source.clone(),
                ))
            }
            Some(EntityDomain::Sensor) => {}
        }
        match domain_of(&raw_binding.target) {
            None => {
                return Err(ConfigError::UnknownBindingTarget(
                    raw_binding.target.clone(),
                ))
            }
            Some(EntityDomain::Sensor) => {
                return Err(ConfigError::BindingTargetNotActuator(
                    raw_binding.target.clone(),
                ))
            }
            Some(EntityDomain::Actuator) => {}
        }
        bindings.push(Binding {
            source: raw_binding.source.clone(),
            target: raw_binding.target.clone(),
        });
    }

    Ok(ValidatedConfig {
        devices,
        driver_specs,
        bindings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::BUILTIN_DRIVER_KINDS;
    use crate::entities::{ActuatorType, SensorType};

    const VALID: &str = r#"{
        "devices": [
            {
                "id": "gateway",
                "name": "ESP32 Gateway",
                "entities": [
                    {
                        "id": "rpm",
                        "type": "rpm",
                        "driver": "pulse_counter",
                        "driver_config": { "pin": 4, "sample_window_ms": 500 }
                    },
                    {
                        "id": "display",
                        "type": "display_panel",
                        "driver": "ssd1306_display",
                        "driver_config": {
                            "i2c_sda": 21,
                            "i2c_scl": 22,
                            "layout": [
                                { "id": "dial", "x": 0, "y": 0, "width": 128, "height": 40,
                                  "widget": { "kind": "gauge_arc", "min": 0, "max": 10000 } },
                                { "id": "bottom", "x": 0, "y": 40, "width": 128, "height": 24,
                                  "widget": { "kind": "text", "template": "RPM: {{value}}" } }
                            ]
                        }
                    }
                ]
            }
        ],
        "bindings": [
            { "source": "gateway.rpm", "target": "gateway.display.dial" }
        ]
    }"#;

    #[test]
    fn valid_config_parses() {
        let config = parse_config(VALID, BUILTIN_DRIVER_KINDS).unwrap();

        assert_eq!(config.devices.len(), 1);
        assert_eq!(config.devices[0].name, "ESP32 Gateway");

        assert_eq!(config.driver_specs.len(), 2);
        let rpm = &config.driver_specs[0];
        assert_eq!(rpm.instance_id, "gateway.rpm");
        assert_eq!(rpm.kind, "pulse_counter");
        assert_eq!(
            rpm.entities[0].entity_type,
            EntityType::Sensor(SensorType::Rpm)
        );
        assert_eq!(rpm.config["pin"], 4);

        // One display driver instance exposing one entity per config zone.
        let display = &config.driver_specs[1];
        assert_eq!(display.instance_id, "gateway.display");
        assert_eq!(display.entities.len(), 2);
        assert_eq!(display.entities[0].id.to_string(), "gateway.display.dial");
        assert_eq!(display.entities[1].id.to_string(), "gateway.display.bottom");
        assert!(display
            .entities
            .iter()
            .all(|e| e.entity_type == EntityType::Actuator(ActuatorType::DisplayRegion)));

        assert_eq!(config.bindings.len(), 1);
        assert_eq!(config.bindings[0].source.to_string(), "gateway.rpm");
        assert_eq!(
            config.bindings[0].target.to_string(),
            "gateway.display.dial"
        );
    }

    #[test]
    fn binding_may_target_zone_but_not_the_panel_itself() {
        let json = VALID.replace("gateway.display.dial\" }", "gateway.display\" }");
        assert!(matches!(
            parse_config(&json, BUILTIN_DRIVER_KINDS),
            Err(ConfigError::UnknownBindingTarget(id)) if id.to_string() == "gateway.display"
        ));
    }

    #[test]
    fn invalid_zone_layout_is_a_config_error() {
        // Second zone moved up so it overlaps the first.
        let json = VALID.replace("\"y\": 40", "\"y\": 30");
        assert!(matches!(
            parse_config(&json, BUILTIN_DRIVER_KINDS),
            Err(ConfigError::InvalidDriverConfig { entity, message })
                if entity.to_string() == "gateway.display" && message.contains("overlap")
        ));
    }

    #[test]
    fn rejects_unknown_driver_kind() {
        let json = VALID.replace("pulse_counter", "quantum_counter");
        assert!(matches!(
            parse_config(&json, BUILTIN_DRIVER_KINDS),
            Err(ConfigError::UnknownDriverKind { kind, .. }) if kind == "quantum_counter"
        ));
    }

    #[test]
    fn rejects_unknown_entity_type() {
        let json = VALID.replace("\"type\": \"rpm\"", "\"type\": \"warp_factor\"");
        assert!(matches!(
            parse_config(&json, BUILTIN_DRIVER_KINDS),
            Err(ConfigError::Json(_))
        ));
    }

    #[test]
    fn rejects_duplicate_entity_ids() {
        let json = VALID.replace("\"id\": \"display\"", "\"id\": \"rpm\"");
        assert!(matches!(
            parse_config(&json, BUILTIN_DRIVER_KINDS),
            Err(ConfigError::DuplicateEntity(id)) if id.to_string() == "gateway.rpm"
        ));
    }

    #[test]
    fn rejects_binding_to_missing_entity() {
        let json = VALID.replace("gateway.display", "gateway.ghost");
        assert!(matches!(
            parse_config(&json, BUILTIN_DRIVER_KINDS),
            Err(ConfigError::UnknownBindingTarget(_))
        ));
    }

    #[test]
    fn rejects_binding_with_swapped_domains() {
        let json = VALID.replace(
            r#""source": "gateway.rpm", "target": "gateway.display.dial""#,
            r#""source": "gateway.display.dial", "target": "gateway.rpm""#,
        );
        assert!(matches!(
            parse_config(&json, BUILTIN_DRIVER_KINDS),
            Err(ConfigError::BindingSourceNotSensor(_))
        ));
    }

    #[test]
    fn rejects_bare_entity_type_addressing_in_bindings() {
        let json = VALID.replace("gateway.rpm", "rpm");
        assert!(matches!(
            parse_config(&json, BUILTIN_DRIVER_KINDS),
            Err(ConfigError::Json(_))
        ));
    }

    #[test]
    fn embedded_default_config_is_valid() {
        let json = include_str!("../../config/default_config.json");
        let config = parse_config(json, BUILTIN_DRIVER_KINDS).unwrap();
        assert!(!config.driver_specs.is_empty());
        assert!(!config.bindings.is_empty());
    }
}
