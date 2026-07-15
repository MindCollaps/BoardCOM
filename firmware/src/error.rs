//! Central error types, one enum per layer.

use crate::entities::EntityId;

/// Failure to parse a `device_id` / `entity_id` / `device_id.entity_id` string.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdParseError {
    #[error("invalid id '{0}': ids must match [a-z][a-z0-9_]*")]
    InvalidId(String),
    #[error("invalid entity address '{0}': expected 'device_id.entity_id'")]
    InvalidAddress(String),
}

/// Errors from parsing and validating the JSON configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Id(#[from] IdParseError),
    #[error("duplicate device id '{0}'")]
    DuplicateDevice(String),
    #[error("duplicate entity id '{0}'")]
    DuplicateEntity(EntityId),
    #[error("entity '{entity}' uses unknown driver kind '{kind}'")]
    UnknownDriverKind { entity: EntityId, kind: String },
    #[error("invalid driver config for entity '{entity}': {message}")]
    InvalidDriverConfig { entity: EntityId, message: String },
    #[error(
        "entity '{entity}' declares domain '{declared}' but driver kind '{kind}' cannot provide it"
    )]
    DomainMismatch {
        entity: EntityId,
        declared: &'static str,
        kind: String,
    },
    #[error("binding source '{0}' does not refer to a configured entity")]
    UnknownBindingSource(EntityId),
    #[error("binding target '{0}' does not refer to a configured entity")]
    UnknownBindingTarget(EntityId),
    #[error("binding source '{0}' is not a sensor entity")]
    BindingSourceNotSensor(EntityId),
    #[error("binding target '{0}' is not an actuator entity")]
    BindingTargetNotActuator(EntityId),
}

/// Errors from instantiating or operating a driver.
#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error("unknown driver kind '{0}'")]
    UnknownKind(String),
    #[error("invalid config for driver instance '{instance}': {message}")]
    InvalidConfig { instance: String, message: String },
    #[error("hardware resource conflict: {0}")]
    ResourceConflict(String),
    #[error("driver instance '{instance}' does not expose entity '{entity}'")]
    UnknownEntity { instance: String, entity: EntityId },
    #[error("driver instance '{instance}' cannot handle command for '{entity}'")]
    UnsupportedCommand { instance: String, entity: EntityId },
    #[error("display error: {0}")]
    Display(String),
    #[cfg(target_os = "espidf")]
    #[error("esp-idf error: {0}")]
    Esp(#[from] esp_idf_svc::sys::EspError),
}

/// Errors from constructing the binding engine.
#[derive(Debug, thiserror::Error)]
pub enum BindingError {
    #[error("binding source '{0}' is not a known sensor entity")]
    UnknownSource(EntityId),
    #[error("binding target '{0}' is not a known actuator entity")]
    UnknownTarget(EntityId),
    #[error("binding target '{target}' has entity type '{entity_type}', which cannot receive continuous source updates")]
    UnsupportedTarget {
        target: EntityId,
        entity_type: &'static str,
    },
    #[error("binding target '{0}' is bound more than once; a target can have at most one binding")]
    DuplicateTarget(EntityId),
}

/// Top-level application error, for `main` to report.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("configuration error: {0}")]
    Config(#[from] ConfigError),
    #[error("driver error: {0}")]
    Driver(#[from] DriverError),
    #[error("binding error: {0}")]
    Binding(#[from] BindingError),
}
