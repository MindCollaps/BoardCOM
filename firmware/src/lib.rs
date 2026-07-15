//! BoardCOM gateway firmware library.
//!
//! Layering (Device / Entity / Driver / Binding terminology is defined in
//! the project design notes, `/CLAUDE.md`):
//! - [`entities`] — Device / Entity types, `device_id.entity_id` addressing,
//!   availability state, and the centralized entity-type contract registry
//!   (unit of measurement, value kind, precision, update rate).
//! - [`drivers`] — the `SensorDriver` / `ActuatorDriver` traits, the driver
//!   registry, and built-in driver implementations (hardware-facing parts are
//!   only compiled for `target_os = "espidf"`).
//! - [`config`] — JSON config schema and the pure parsing/validation step that
//!   turns a config document into device/entity/driver specs.
//! - [`automations`] — Bindings (continuous entity → actuator mappings) and
//!   the Automation stub types (Trigger/Condition/Action).
//!
//! Everything except the hardware-facing driver implementations compiles for
//! the host, so `cargo test --target x86_64-unknown-linux-gnu` exercises the
//! config and binding logic without hardware or a simulator.

pub mod automations;
pub mod config;
pub mod drivers;
pub mod entities;
pub mod error;
