//! Per-entity runtime state: last value + availability.
//!
//! Availability is derived here, in one place, from what drivers report and
//! the entity type's contract — drivers themselves never set availability.
//! Bindings and automations react to availability, not just values: a display
//! binding shows a placeholder instead of a frozen last-known value once its
//! source goes stale (bridge devices over BLE *will* drop out).

use std::collections::HashMap;
use std::time::Instant;

use super::contract::sensor_contract;
use super::{EntityId, EntityIndex, SensorValue};

/// Availability of an entity's data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    /// Fresh data within the type contract's staleness window.
    Online,
    /// Last reading is older than the staleness window; value is untrustworthy.
    Stale,
    /// No reading yet, or the driver reported an error.
    Unavailable,
}

/// Runtime state of one sensor entity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EntityState {
    pub availability: Availability,
    pub value: Option<SensorValue>,
    pub last_update: Option<Instant>,
}

impl EntityState {
    const UNAVAILABLE: Self = Self {
        availability: Availability::Unavailable,
        value: None,
        last_update: None,
    };
}

/// State store for all sensor entities, keyed by `device_id.entity_id`.
#[derive(Debug, Default)]
pub struct StateStore {
    states: HashMap<EntityId, EntityState>,
}

impl StateStore {
    /// Every configured sensor entity starts `Unavailable` until its driver
    /// produces a first reading.
    pub fn new(sensor_ids: impl IntoIterator<Item = EntityId>) -> Self {
        Self {
            states: sensor_ids
                .into_iter()
                .map(|id| (id, EntityState::UNAVAILABLE))
                .collect(),
        }
    }

    /// Record a fresh reading. Returns `true` if the stored value or
    /// availability changed (used for change-only logging).
    pub fn apply_reading(&mut self, id: &EntityId, value: SensorValue, now: Instant) -> bool {
        let Some(state) = self.states.get_mut(id) else {
            log::warn!("reading for unknown entity '{id}' dropped");
            return false;
        };
        let changed = state.value != Some(value) || state.availability != Availability::Online;
        *state = EntityState {
            availability: Availability::Online,
            value: Some(value),
            last_update: Some(now),
        };
        changed
    }

    /// Record a driver error for an entity: its data can no longer be trusted.
    pub fn mark_unavailable(&mut self, id: &EntityId) {
        if let Some(state) = self.states.get_mut(id) {
            state.availability = Availability::Unavailable;
            state.value = None;
        }
    }

    /// Demote entities whose last reading has gone stale, and further to
    /// `Unavailable` if no fresh reading arrives before `unavailable_after`
    /// elapses. Without the second step, a driver that silently stops
    /// producing readings without its `poll()` ever erroring (a BLE bridge
    /// device dropping out, say) would leave the entity `Stale` — and
    /// anything bound to it (e.g. a gauge that freezes at its last position
    /// while `Stale`) — looking merely delayed, forever.
    pub fn refresh_availability(&mut self, now: Instant, index: &EntityIndex) {
        for (id, state) in &mut self.states {
            if state.availability == Availability::Unavailable {
                continue;
            }
            let (Some(last), Some(sensor_type)) = (state.last_update, index.sensor_type(id)) else {
                continue;
            };
            let contract = sensor_contract(sensor_type);
            let elapsed = now.duration_since(last);
            if elapsed > contract.unavailable_after {
                state.availability = Availability::Unavailable;
                state.value = None;
            } else if elapsed > contract.staleness_after {
                state.availability = Availability::Stale;
            }
        }
    }

    pub fn get(&self, id: &EntityId) -> Option<&EntityState> {
        self.states.get(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::{Entity, EntityType, SensorType};
    use std::time::Duration;

    fn rpm_id() -> EntityId {
        "gateway.rpm".parse().unwrap()
    }

    fn index() -> EntityIndex {
        EntityIndex::from_entities(&[Entity {
            id: rpm_id(),
            entity_type: EntityType::Sensor(SensorType::Rpm),
            name: None,
        }])
    }

    #[test]
    fn starts_unavailable_then_goes_online() {
        let mut store = StateStore::new([rpm_id()]);
        assert_eq!(
            store.get(&rpm_id()).unwrap().availability,
            Availability::Unavailable
        );

        let changed = store.apply_reading(&rpm_id(), SensorValue::Rpm(600), Instant::now());
        assert!(changed);
        let state = store.get(&rpm_id()).unwrap();
        assert_eq!(state.availability, Availability::Online);
        assert_eq!(state.value, Some(SensorValue::Rpm(600)));
    }

    #[test]
    fn unchanged_reading_reports_no_change() {
        let mut store = StateStore::new([rpm_id()]);
        assert!(store.apply_reading(&rpm_id(), SensorValue::Rpm(0), Instant::now()));
        assert!(!store.apply_reading(&rpm_id(), SensorValue::Rpm(0), Instant::now()));
        assert!(store.apply_reading(&rpm_id(), SensorValue::Rpm(120), Instant::now()));
    }

    #[test]
    fn goes_stale_after_contract_window() {
        let mut store = StateStore::new([rpm_id()]);
        let start = Instant::now();
        store.apply_reading(&rpm_id(), SensorValue::Rpm(600), start);

        store.refresh_availability(start + Duration::from_millis(500), &index());
        assert_eq!(
            store.get(&rpm_id()).unwrap().availability,
            Availability::Online
        );

        store.refresh_availability(start + Duration::from_secs(3), &index());
        assert_eq!(
            store.get(&rpm_id()).unwrap().availability,
            Availability::Stale
        );
    }

    #[test]
    fn goes_unavailable_after_extended_silence_and_drops_the_stale_value() {
        let mut store = StateStore::new([rpm_id()]);
        let start = Instant::now();
        store.apply_reading(&rpm_id(), SensorValue::Rpm(600), start);

        // Well past staleness_after (2s) but not yet unavailable_after (10s):
        // still Stale, and the last value is still there for a gauge to
        // freeze on.
        store.refresh_availability(start + Duration::from_secs(5), &index());
        let state = store.get(&rpm_id()).unwrap();
        assert_eq!(state.availability, Availability::Stale);
        assert_eq!(state.value, Some(SensorValue::Rpm(600)));

        // Past unavailable_after: no fresh reading ever arrived, so a driver
        // that silently stopped updating (no poll() error) must not leave
        // this frozen as Stale forever.
        store.refresh_availability(start + Duration::from_secs(11), &index());
        let state = store.get(&rpm_id()).unwrap();
        assert_eq!(state.availability, Availability::Unavailable);
        assert_eq!(state.value, None);
    }

    #[test]
    fn fresh_reading_returns_to_online_from_any_prior_state() {
        let mut store = StateStore::new([rpm_id()]);
        let start = Instant::now();
        store.apply_reading(&rpm_id(), SensorValue::Rpm(600), start);
        store.refresh_availability(start + Duration::from_secs(11), &index());
        assert_eq!(
            store.get(&rpm_id()).unwrap().availability,
            Availability::Unavailable
        );

        store.apply_reading(
            &rpm_id(),
            SensorValue::Rpm(700),
            start + Duration::from_secs(11),
        );
        let state = store.get(&rpm_id()).unwrap();
        assert_eq!(state.availability, Availability::Online);
        assert_eq!(state.value, Some(SensorValue::Rpm(700)));
    }

    #[test]
    fn driver_error_marks_unavailable() {
        let mut store = StateStore::new([rpm_id()]);
        store.apply_reading(&rpm_id(), SensorValue::Rpm(600), Instant::now());
        store.mark_unavailable(&rpm_id());
        let state = store.get(&rpm_id()).unwrap();
        assert_eq!(state.availability, Availability::Unavailable);
        assert_eq!(state.value, None);
    }
}
