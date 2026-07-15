//! Device / Entity types and `device_id.entity_id` addressing.
//!
//! Entities carry their domain (sensor / actuator) and concrete type (rpm,
//! display_panel, …) in the type system. Automations, bindings, and any UI
//! only ever interact with entity types and entity ids — never with drivers.

pub mod contract;
pub mod state;

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::IdParseError;
use contract::ValueKind;

pub(crate) fn is_valid_id(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Entity-local ids are plain strings, but hierarchical dotted names are a
/// permitted *convention* inside them (e.g. `display.top` for a display
/// zone) — not a new addressing level. Each dot-separated segment must be a
/// valid plain id.
fn is_valid_entity_local_id(s: &str) -> bool {
    !s.is_empty() && s.split('.').all(is_valid_id)
}

/// Identifier of a [`Device`] (`gateway`, `phone`, …).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeviceId(String);

impl DeviceId {
    pub fn new(id: &str) -> Result<Self, IdParseError> {
        if is_valid_id(id) {
            Ok(Self(id.to_owned()))
        } else {
            Err(IdParseError::InvalidId(id.to_owned()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Fully qualified entity address: `device_id.entity_id` (e.g. `gateway.rpm`).
///
/// Entities are *always* referenced through this two-part address — never by
/// bare entity type — so multiple devices (or eventually multiple bikes on a
/// mesh) can expose the same entity type without ambiguity.
///
/// The entity-local part may itself contain dots as a naming convention
/// (`gateway.display.top` = device `gateway`, entity `display.top`); the
/// first dot is the only addressing-level separator.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntityId {
    device: DeviceId,
    entity: String,
}

impl EntityId {
    pub fn new(device: DeviceId, entity: &str) -> Result<Self, IdParseError> {
        if is_valid_entity_local_id(entity) {
            Ok(Self {
                device,
                entity: entity.to_owned(),
            })
        } else {
            Err(IdParseError::InvalidId(entity.to_owned()))
        }
    }

    pub fn device(&self) -> &DeviceId {
        &self.device
    }

    /// The local (per-device) part of the address.
    pub fn entity(&self) -> &str {
        &self.entity
    }
}

impl fmt::Display for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.device, self.entity)
    }
}

impl FromStr for EntityId {
    type Err = IdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (device, entity) = s
            .split_once('.')
            .ok_or_else(|| IdParseError::InvalidAddress(s.to_owned()))?;
        Self::new(DeviceId::new(device)?, entity)
    }
}

impl Serialize for EntityId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for EntityId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// The two entity domains. Sensors produce read-only data; actuators are
/// commanded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntityDomain {
    Sensor,
    Actuator,
}

/// Sensor entity types. Every variant must have an entry in the centralized
/// type contract registry ([`contract::sensor_contract`]) — the exhaustive
/// match there enforces this at compile time. Add variants as drivers for
/// them land (gps_position, gyroscope, … arrive with the phone_bridge).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensorType {
    Rpm,
    Speed,
}

impl SensorType {
    /// Human-facing label, e.g. for display bindings.
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Rpm => "RPM",
            Self::Speed => "Speed",
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Rpm => "rpm",
            Self::Speed => "speed",
        }
    }
}

/// Actuator entity types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActuatorType {
    /// A whole physical display, as declared in config. Drivers that
    /// partition their display into zones expose `DisplayRegion` entities
    /// instead of one entity of this type.
    DisplayPanel,
    /// One config-defined zone of a partitioned display
    /// (`gateway.display.top`); the addressable unit bindings target.
    DisplayRegion,
    AudioPlayer,
}

impl ActuatorType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DisplayPanel => "display_panel",
            Self::DisplayRegion => "display_region",
            Self::AudioPlayer => "audio_player",
        }
    }
}

/// An entity's concrete type, carrying its domain structurally: a sensor
/// entity of type `display_panel` is unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EntityType {
    Sensor(SensorType),
    Actuator(ActuatorType),
}

impl EntityType {
    pub fn domain(&self) -> EntityDomain {
        match self {
            Self::Sensor(_) => EntityDomain::Sensor,
            Self::Actuator(_) => EntityDomain::Actuator,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Sensor(t) => t.as_str(),
            Self::Actuator(t) => t.as_str(),
        }
    }
}

/// A single capability a device exposes.
#[derive(Debug, Clone, PartialEq)]
pub struct Entity {
    pub id: EntityId,
    pub entity_type: EntityType,
    /// Human-facing name; defaults to the entity type's label when absent.
    pub name: Option<String>,
}

/// A physical/logical host exposing one or more entities.
#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    pub id: DeviceId,
    pub name: String,
}

/// A typed sensor reading. The variant fixes both the sensor type and the
/// value representation, so a driver cannot produce an `rpm` value with the
/// wrong kind — the contract registry's `value_kind` is enforced by
/// construction (see the round-trip test below).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SensorValue {
    /// Revolutions per minute. Contract: `u32`, unit `rpm`.
    Rpm(u32),
    /// Speed. Contract: `f32`, unit `km/h`.
    Speed(f32),
}

impl SensorValue {
    pub fn sensor_type(&self) -> SensorType {
        match self {
            Self::Rpm(_) => SensorType::Rpm,
            Self::Speed(_) => SensorType::Speed,
        }
    }

    pub fn value_kind(&self) -> ValueKind {
        match self {
            Self::Rpm(_) => ValueKind::U32,
            Self::Speed(_) => ValueKind::F32,
        }
    }

    /// Format the numeric part per the type contract's precision (no unit).
    pub fn format_value(&self) -> String {
        let precision = contract::sensor_contract(self.sensor_type()).precision as usize;
        match self {
            Self::Rpm(v) => format!("{v}"),
            Self::Speed(v) => format!("{v:.precision$}"),
        }
    }

    /// The contract-defined unit of measurement for this value's type.
    pub fn unit(&self) -> &'static str {
        contract::sensor_contract(self.sensor_type()).unit_of_measurement
    }

    /// The value as f64, for range math (gauge scaling etc.).
    pub fn as_f64(&self) -> f64 {
        match self {
            Self::Rpm(v) => f64::from(*v),
            Self::Speed(v) => f64::from(*v),
        }
    }
}

/// Commands that can be sent to actuator entities. Which commands an entity
/// accepts follows from its [`ActuatorType`].
#[derive(Debug, Clone, PartialEq)]
pub enum ActuatorCommand {
    /// Reflect the current state of a bound sensor entity on this actuator —
    /// what a Binding continuously delivers. How the state is presented
    /// (text, gauge, placeholder while stale, …) is entirely the receiving
    /// driver's concern; the value must not be pre-formatted upstream.
    SourceUpdate {
        availability: state::Availability,
        /// Last known value; may be present while stale/unavailable — the
        /// availability decides whether it is trustworthy.
        value: Option<SensorValue>,
    },
}

/// Lookup from entity id to its declared type, built once at boot from the
/// validated config. Shared by the state store and the binding engine.
#[derive(Debug, Default, Clone)]
pub struct EntityIndex {
    map: HashMap<EntityId, EntityType>,
}

impl EntityIndex {
    pub fn from_entities<'a>(entities: impl IntoIterator<Item = &'a Entity>) -> Self {
        Self {
            map: entities
                .into_iter()
                .map(|e| (e.id.clone(), e.entity_type))
                .collect(),
        }
    }

    pub fn entity_type(&self, id: &EntityId) -> Option<EntityType> {
        self.map.get(id).copied()
    }

    pub fn sensor_type(&self, id: &EntityId) -> Option<SensorType> {
        match self.entity_type(id)? {
            EntityType::Sensor(t) => Some(t),
            EntityType::Actuator(_) => None,
        }
    }

    pub fn sensor_ids(&self) -> impl Iterator<Item = &EntityId> {
        self.map
            .iter()
            .filter(|(_, t)| t.domain() == EntityDomain::Sensor)
            .map(|(id, _)| id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_id_round_trips() {
        let id: EntityId = "gateway.rpm".parse().unwrap();
        assert_eq!(id.device().as_str(), "gateway");
        assert_eq!(id.entity(), "rpm");
        assert_eq!(id.to_string(), "gateway.rpm");
    }

    #[test]
    fn entity_id_supports_dotted_zone_convention() {
        let id: EntityId = "gateway.display.top".parse().unwrap();
        assert_eq!(id.device().as_str(), "gateway");
        assert_eq!(id.entity(), "display.top");
        assert_eq!(id.to_string(), "gateway.display.top");
    }

    #[test]
    fn entity_id_rejects_bare_and_malformed_addresses() {
        assert!("rpm".parse::<EntityId>().is_err());
        assert!("Gateway.rpm".parse::<EntityId>().is_err());
        assert!("gateway.".parse::<EntityId>().is_err());
        assert!(".rpm".parse::<EntityId>().is_err());
        assert!("gateway.r-pm".parse::<EntityId>().is_err());
        assert!("gateway.display..top".parse::<EntityId>().is_err());
        assert!("gateway.display.".parse::<EntityId>().is_err());
    }

    #[test]
    fn entity_type_deserializes_by_domain() {
        let t: EntityType = serde_json::from_str("\"rpm\"").unwrap();
        assert_eq!(t, EntityType::Sensor(SensorType::Rpm));
        assert_eq!(t.domain(), EntityDomain::Sensor);

        let t: EntityType = serde_json::from_str("\"display_panel\"").unwrap();
        assert_eq!(t, EntityType::Actuator(ActuatorType::DisplayPanel));
        assert_eq!(t.domain(), EntityDomain::Actuator);

        assert!(serde_json::from_str::<EntityType>("\"warp_drive\"").is_err());
    }

    #[test]
    fn sensor_values_match_their_contracts() {
        for value in [SensorValue::Rpm(1200), SensorValue::Speed(48.25)] {
            let contract = contract::sensor_contract(value.sensor_type());
            assert_eq!(value.value_kind(), contract.value_kind);
        }
    }

    #[test]
    fn values_format_per_contract_precision() {
        assert_eq!(SensorValue::Rpm(1200).format_value(), "1200");
        assert_eq!(SensorValue::Rpm(1200).unit(), "rpm");
        assert_eq!(SensorValue::Speed(48.25).format_value(), "48.2");
        assert_eq!(SensorValue::Speed(48.25).unit(), "km/h");
    }
}
