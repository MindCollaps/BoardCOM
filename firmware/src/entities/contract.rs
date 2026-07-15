//! Centralized entity-type contract registry.
//!
//! One shared registry fixes, per sensor entity type: the unit of
//! measurement, the value kind, the display precision, the expected update
//! rate (QoS), and the staleness window. This is what makes "any driver
//! producing `rpm` is interchangeable" true in practice — two RPM-producing
//! drivers cannot disagree on units or representation, because neither of
//! them defines those things.

use std::time::Duration;

use super::SensorType;

/// The value representation an entity type's readings must use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    U32,
    F32,
}

/// Declared update-rate / QoS class of an entity type. Not consumed by
/// anything yet — declared now so drivers never invent inconsistent implicit
/// rates, and so traffic prioritization on constrained BLE links has the
/// schema it needs later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateRate {
    /// Near-real-time; consumers assume sub-second freshness (e.g. rpm).
    Realtime,
    /// Periodic updates at roughly the given interval (e.g. gps_position ~1s).
    Periodic(Duration),
    /// Rarely changes (e.g. configuration-like values).
    Static,
}

/// The fixed, centrally defined contract for a sensor entity type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypeContract {
    pub unit_of_measurement: &'static str,
    pub value_kind: ValueKind,
    /// Decimal places when formatting for humans.
    pub precision: u8,
    pub update_rate: UpdateRate,
    /// With no fresh reading for this long, the entity is `Stale`.
    pub staleness_after: Duration,
    /// With no fresh reading for this long, the entity is `Unavailable` —
    /// bounds how long a `Stale` reading can sit unrefreshed before it's
    /// treated as gone rather than merely delayed. Without this, a driver
    /// that silently stops producing readings without its `poll()` ever
    /// erroring (a BLE bridge device dropping out, say) would leave the
    /// entity `Stale` forever.
    pub unavailable_after: Duration,
}

/// The registry itself. Exhaustive over [`SensorType`]: adding a sensor type
/// without deciding its contract is a compile error.
pub const fn sensor_contract(sensor_type: SensorType) -> TypeContract {
    match sensor_type {
        SensorType::Rpm => TypeContract {
            unit_of_measurement: "rpm",
            value_kind: ValueKind::U32,
            precision: 0,
            update_rate: UpdateRate::Realtime,
            staleness_after: Duration::from_secs(2),
            unavailable_after: Duration::from_secs(10),
        },
        SensorType::Speed => TypeContract {
            unit_of_measurement: "km/h",
            value_kind: ValueKind::F32,
            precision: 1,
            update_rate: UpdateRate::Realtime,
            staleness_after: Duration::from_secs(2),
            unavailable_after: Duration::from_secs(10),
        },
    }
}
