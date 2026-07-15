//! BoardCOM gateway firmware entry point.
//!
//! `main` only wires the layers together: parse config → instantiate drivers
//! through the registry → run the poll/refresh/bind loop over trait objects.
//! No concrete driver type is ever named here.

#[cfg(target_os = "espidf")]
fn main() -> Result<(), firmware::error::AppError> {
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    use firmware::automations::BindingEngine;
    use firmware::config::parse_config;
    use firmware::drivers::hw::HardwarePool;
    use firmware::drivers::registry::DriverRegistry;
    use firmware::drivers::{Driver, BUILTIN_DRIVER_KINDS};
    use firmware::entities::state::StateStore;
    use firmware::entities::{EntityId, EntityIndex};
    use firmware::error::DriverError;

    // It is necessary to call this function once. Otherwise, some patches to
    // the runtime implemented by esp-idf-sys might not link properly.
    // See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    // The config document is embedded at compile time. The parsing layer
    // treats it as runtime input, so a runtime-loaded source (NVS / BLE
    // provisioning) only replaces this constant, not the parsing code.
    const CONFIG_JSON: &str = include_str!("../config/default_config.json");
    const LOOP_TICK: Duration = Duration::from_millis(100);

    let config = parse_config(CONFIG_JSON, BUILTIN_DRIVER_KINDS).inspect_err(|e| {
        log::error!("invalid embedded config: {e}");
    })?;

    let peripherals =
        esp_idf_svc::hal::peripherals::Peripherals::take().map_err(DriverError::from)?;
    let mut hw = HardwarePool::new(peripherals);
    let registry = DriverRegistry::with_builtins();

    let mut drivers: Vec<Box<dyn Driver>> = Vec::with_capacity(config.driver_specs.len());
    for spec in &config.driver_specs {
        let driver = registry.instantiate(spec, &mut hw).inspect_err(|e| {
            log::error!("failed to start driver '{}': {e}", spec.instance_id);
        })?;
        log::info!(
            "driver '{}' ({}) up, exposing {} entity(ies)",
            driver.instance_id(),
            spec.kind,
            driver.entities().len()
        );
        drivers.push(driver);
    }

    let all_entities: Vec<_> = drivers.iter().flat_map(|d| d.entities()).cloned().collect();
    let index = EntityIndex::from_entities(&all_entities);
    let mut states = StateStore::new(index.sensor_ids().cloned().collect::<Vec<_>>());
    let engine = BindingEngine::new(config.bindings, &index).inspect_err(|e| {
        log::error!("invalid binding: {e}");
    })?;

    // Route actuator commands to the driver exposing the target entity.
    let actuator_by_entity: HashMap<EntityId, usize> = drivers
        .iter()
        .enumerate()
        .flat_map(|(i, d)| d.entities().iter().map(move |e| (e.id.clone(), i)))
        .collect();

    log::info!(
        "boardcom gateway online: {} device(s), {} entity(ies)",
        config.devices.len(),
        all_entities.len()
    );

    loop {
        let now = Instant::now();

        for driver in &mut drivers {
            let entity_ids: Vec<EntityId> =
                driver.entities().iter().map(|e| e.id.clone()).collect();
            let Some(sensor) = driver.as_sensor() else {
                continue;
            };
            match sensor.poll(now) {
                Ok(readings) => {
                    for reading in readings {
                        if states.apply_reading(&reading.entity, reading.value, now) {
                            log::info!(
                                "{} = {} {}",
                                reading.entity,
                                reading.value.format_value(),
                                reading.value.unit()
                            );
                        }
                    }
                }
                Err(e) => {
                    log::error!("sensor poll failed: {e}");
                    for id in &entity_ids {
                        states.mark_unavailable(id);
                    }
                }
            }
        }

        states.refresh_availability(now, &index);

        for (target, command) in engine.desired_commands(&states) {
            let Some(&driver_idx) = actuator_by_entity.get(&target) else {
                log::error!("no driver exposes binding target '{target}'");
                continue;
            };
            let Some(actuator) = drivers[driver_idx].as_actuator() else {
                log::error!("driver for '{target}' has no actuator capability");
                continue;
            };
            if let Err(e) = actuator.execute(&target, &command) {
                log::error!("command for '{target}' failed: {e}");
            }
        }

        // All commands for this tick are delivered; let batching actuators
        // (the display compositor) push accumulated changes to hardware once.
        for driver in &mut drivers {
            if let Some(actuator) = driver.as_actuator() {
                if let Err(e) = actuator.flush() {
                    log::error!("actuator flush failed: {e}");
                }
            }
        }

        std::thread::sleep(LOOP_TICK);
    }
}

#[cfg(not(target_os = "espidf"))]
fn main() {
    // The firmware binary only makes sense on the ESP32; host builds exist
    // solely so `cargo test --target x86_64-unknown-linux-gnu` can run the
    // hardware-independent unit tests in the library.
    eprintln!("firmware runs on the ESP32 (target_os = \"espidf\"); nothing to do on the host");
}
