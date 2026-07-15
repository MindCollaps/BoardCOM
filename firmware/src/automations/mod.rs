//! Bindings and (stub) Automations.
//!
//! A **Binding** is a continuous, always-active mapping from a sensor entity
//! to an actuator entity, with no trigger or condition ("always show RPM on
//! the display"). An **Automation** is Trigger (+ optional Condition) →
//! Action. They are deliberately separate concepts — a binding is *not*
//! modeled as an always-true automation.
//!
//! The engine deliberately knows nothing about presentation: it forwards the
//! source entity's state (value + availability) to the target actuator and
//! the actuator's driver decides how to render it (text, gauge, placeholder
//! while stale, …). Display zones are ordinary actuator entities here — no
//! zone- or widget-awareness exists at this layer.

use std::collections::HashSet;

use crate::entities::state::{Availability, StateStore};
use crate::entities::{ActuatorCommand, ActuatorType, EntityId, EntityIndex, EntityType};
use crate::error::BindingError;

/// Continuous mapping: the source entity's value is always reflected on the
/// target actuator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub source: EntityId,
    pub target: EntityId,
}

/// Evaluates bindings against the current entity states and produces the
/// actuator commands that would realize them. Pure with respect to hardware:
/// the main loop routes the returned commands to actuator drivers.
pub struct BindingEngine {
    bindings: Vec<Binding>,
}

impl BindingEngine {
    /// Validates that every binding can actually be served: the source must
    /// be a known sensor entity, the target an actuator entity type that
    /// accepts continuous source updates (displays and display zones), and
    /// each target may have at most one binding — a continuous mapping with
    /// two sources has no meaning.
    pub fn new(bindings: Vec<Binding>, index: &EntityIndex) -> Result<Self, BindingError> {
        let mut targets = HashSet::new();
        for binding in &bindings {
            if index.sensor_type(&binding.source).is_none() {
                return Err(BindingError::UnknownSource(binding.source.clone()));
            }
            match index.entity_type(&binding.target) {
                Some(EntityType::Actuator(
                    ActuatorType::DisplayPanel | ActuatorType::DisplayRegion,
                )) => {}
                Some(entity_type) => {
                    return Err(BindingError::UnsupportedTarget {
                        target: binding.target.clone(),
                        entity_type: entity_type.as_str(),
                    })
                }
                None => return Err(BindingError::UnknownTarget(binding.target.clone())),
            }
            if !targets.insert(&binding.target) {
                return Err(BindingError::DuplicateTarget(binding.target.clone()));
            }
        }
        Ok(Self { bindings })
    }

    /// The commands that bring all binding targets in line with the current
    /// source states.
    pub fn desired_commands(&self, states: &StateStore) -> Vec<(EntityId, ActuatorCommand)> {
        self.bindings
            .iter()
            .map(|binding| {
                let command = match states.get(&binding.source) {
                    Some(state) => ActuatorCommand::SourceUpdate {
                        availability: state.availability,
                        value: state.value,
                    },
                    None => ActuatorCommand::SourceUpdate {
                        availability: Availability::Unavailable,
                        value: None,
                    },
                };
                (binding.target.clone(), command)
            })
            .collect()
    }
}

/// Automation stubs — real trigger engine is future work. Kept as distinct
/// types now so the concepts (and the Action-as-service-call shape) are
/// locked in before anything grows around them.
pub mod automation {
    use crate::entities::{ActuatorCommand, EntityId};

    /// What starts an automation. (Stub: variants arrive with the engine.)
    #[derive(Debug, Clone, PartialEq)]
    pub enum Trigger {
        /// Placeholder example shape: a sensor entity crossing a threshold
        /// for a sustained duration.
        SensorAbove {
            entity: EntityId,
            threshold: f64,
            for_ms: u64,
        },
    }

    /// Optional gate between trigger and action. (Stub.)
    #[derive(Debug, Clone, PartialEq)]
    pub enum Condition {
        EntityOnline(EntityId),
    }

    /// A "service call", deliberately broader than "command an actuator":
    /// logging, notifications, or triggering other automations belong here
    /// too. Commanding an actuator is just the most common case.
    #[derive(Debug, Clone, PartialEq)]
    pub enum Action {
        CommandActuator {
            target: EntityId,
            command: ActuatorCommand,
        },
        Log {
            message: String,
        },
    }

    /// Trigger (+ optional Condition) → Action.
    #[derive(Debug, Clone, PartialEq)]
    pub struct Automation {
        pub trigger: Trigger,
        pub condition: Option<Condition>,
        pub action: Action,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::state::StateStore;
    use crate::entities::{Entity, SensorType, SensorValue};
    use std::time::Instant;

    fn rpm_id() -> EntityId {
        "gateway.rpm".parse().unwrap()
    }

    fn zone_id() -> EntityId {
        "gateway.display.top".parse().unwrap()
    }

    fn index() -> EntityIndex {
        EntityIndex::from_entities(&[
            Entity {
                id: rpm_id(),
                entity_type: EntityType::Sensor(SensorType::Rpm),
                name: None,
            },
            Entity {
                id: zone_id(),
                entity_type: EntityType::Actuator(ActuatorType::DisplayRegion),
                name: None,
            },
        ])
    }

    fn engine() -> BindingEngine {
        BindingEngine::new(
            vec![Binding {
                source: rpm_id(),
                target: zone_id(),
            }],
            &index(),
        )
        .unwrap()
    }

    #[test]
    fn online_source_forwards_value_and_availability() {
        let mut states = StateStore::new([rpm_id()]);
        states.apply_reading(&rpm_id(), SensorValue::Rpm(600), Instant::now());

        let commands = engine().desired_commands(&states);
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].0, zone_id());
        assert_eq!(
            commands[0].1,
            ActuatorCommand::SourceUpdate {
                availability: Availability::Online,
                value: Some(SensorValue::Rpm(600)),
            }
        );
    }

    #[test]
    fn unavailable_source_forwards_unavailability_not_a_value() {
        let mut states = StateStore::new([rpm_id()]);
        states.apply_reading(&rpm_id(), SensorValue::Rpm(600), Instant::now());
        states.mark_unavailable(&rpm_id());

        let commands = engine().desired_commands(&states);
        assert_eq!(
            commands[0].1,
            ActuatorCommand::SourceUpdate {
                availability: Availability::Unavailable,
                value: None,
            }
        );
    }

    #[test]
    fn rejects_binding_to_unsupported_actuator_type() {
        let audio_id: EntityId = "phone.audio".parse().unwrap();
        let index = EntityIndex::from_entities(&[
            Entity {
                id: rpm_id(),
                entity_type: EntityType::Sensor(SensorType::Rpm),
                name: None,
            },
            Entity {
                id: audio_id.clone(),
                entity_type: EntityType::Actuator(ActuatorType::AudioPlayer),
                name: None,
            },
        ]);
        let result = BindingEngine::new(
            vec![Binding {
                source: rpm_id(),
                target: audio_id,
            }],
            &index,
        );
        assert!(matches!(
            result,
            Err(BindingError::UnsupportedTarget { .. })
        ));
    }

    #[test]
    fn rejects_two_bindings_onto_one_target() {
        let result = BindingEngine::new(
            vec![
                Binding {
                    source: rpm_id(),
                    target: zone_id(),
                },
                Binding {
                    source: rpm_id(),
                    target: zone_id(),
                },
            ],
            &index(),
        );
        assert!(matches!(result, Err(BindingError::DuplicateTarget(_))));
    }
}
