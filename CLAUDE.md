# Project: Open-Source Motorcycle Telemetry & Connectivity System

## What this is

A custom, open-source system to gather sensor data from a motorcycle, display it,
and connect Bluetooth peripherals — designed to be secure by default, plugin-extendable,
easy to use for non-technical riders, and easy to hack on for technical ones.

Not reinventing the wheel where avoidable — see "Prior Art" below for existing
open-source projects we're drawing on.

## Architecture — 3 device roles

1. **Microcontroller (ESP32)** — the main gateway. Talks to peripherals: sensors
   (RPM, speed, etc.) and external displays. This is the core of the system and the
   current build focus.
2. **Old Android phone (optional peripheral extender)** — provides GPS, an extra
   screen, gyroscope/other phone sensors, and a SIM for remote connectivity. Connects
   to the microcontroller, extending its capabilities without needing extra hardware.
3. **Management app (phone)** — used for configuration/setup while parked, not while
   riding. Connects to the microcontroller via Bluetooth.

## Secondary feature (selling point, not core)

Intercom communication hub: microcontrollers can form a Bluetooth mesh with other
same-project controllers and bridge to different intercom systems (up to 5 in the
basic variant). **This is explicitly de-scoped from v1** — see "Known Hard Problems"
below. Do not start implementing real-time audio mesh without discussing scope first.

## Design principles

- End-to-end encrypted and secure by default — not an afterthought bolted on later.
- Plugin-based, open-source, extendable architecture.
- Must be usable by non-technical riders out of the box, while remaining easy to
  extend/upgrade for technical users.

## Current phase / priorities

Prototyping, sensor data gathering, and software development. **Custom PCB/board
design is a future goal, not current work** — don't suggest hardware/board design
tasks unless explicitly asked.

## Tech stack

- **Microcontroller firmware: Rust**, from the start (not C/C++ first) — Rust is the
  intended long-term language, so no plan to prototype in C/C++ and rewrite later.
  - Using `esp-idf-svc` (`std`, FreeRTOS-backed) as the current approach, not
    `no_std`/`esp-hal`, to reduce the amount of new-to-Rust + new-to-embedded
    friction at once. Revisit `no_std` later once comfortable.
  - Chip: ESP32 (has built-in WiFi + BLE; some variants also Classic BT; has a
    hardware PCNT peripheral for pulse counting — used for RPM/speed sensing).
  - Dev workflow: VS Code + Wokwi extension. Compile locally with `cargo`,
    simulate in Wokwi (no physical ESP32 owned yet). Code ports directly to real
    hardware later.
- **Management app: leaning native Android (Kotlin)**, not Electron — BLE support is
  first-class on Android, and the old-phone peripheral extender is Android anyway,
  so BLE/GATT work is built once and reused in both places.

## Sensor acquisition plan

- **RPM**: contactless inductive pickup on the ignition coil. Needs signal
  conditioning before the ESP32 (clamp diodes, comparator/Schmitt-trigger, ideally
  opto-isolation for noise immunity) → feeds ESP32's hardware PCNT peripheral for
  pulse counting.
- **Speed**: prefer tapping the bike's existing wheel speed sensor wire (already a
  clean pulse train) via an opto-isolated buffer, rather than building a new
  inductive pickup from scratch.

## Naming / taxonomy (use these terms consistently in code, docs, and discussion)

- **Device** — a physical/logical host that exposes one or more capabilities.
  Examples: `ESP32 Gateway`, `Phone` (bridge device), `External Display`.
- **Entity** — a single capability a device exposes. A device can expose many
  entities. **Automations, bindings, and the UI only ever interact with entity
  types — never with specific drivers.** This decoupling is what makes plugin-based
  sensor/actuator addition work: a new driver just declares which entity type(s) it
  produces, and everything built on entities keeps working unchanged.
  - Two domains: **sensor** (read-only data in, e.g. `rpm`, `speed`,
    `gps_position`, `gyroscope`, `accelerometer`) and **actuator** (something you
    command, e.g. `audio_player`, `display_panel`).
- **Driver** — built-in or plugin code implementing *how* an entity type is
  actually produced/controlled (e.g. `pulse_counter`, `phone_bridge`,
  `spi_display`). Multiple drivers can implement the same entity type
  interchangeably (e.g. a future `can_bus` driver could also produce `rpm`).
  - New sensor support should be pure JSON config when it's a new *instance* of an
    existing driver type (e.g. "another pulse counter on a different pin").
  - A genuinely new protocol/driver still needs real code — one-time cost, then
    it's "just another type" for everyone after. Don't oversell plugin support as
    "any sensor, zero code" — see ESPHome-style generic protocol drivers
    (pulse_counter, adc, i2c-with-register-map, uart-with-parser) as the model for
    keeping the driver count small and the config-only surface large.

- **Bridge Device** — a device type that federates *multiple* entities over one
  connection/driver, rather than one wire = one entity. The Phone is the primary
  example: one `phone_bridge` driver exposes `gps_position`, `gyroscope`,
  `accelerometer`, and `audio_player` as separate entities of one Phone device.
  There can be more than one bridge device type (e.g. another ESP32 acting as a
  bridge), not just the phone.

- **Config-partitioned entities (display zones)** — a second, distinct reason a
  single driver can expose multiple entities, alongside Bridge Devices. A Bridge
  Device federates entities that exist independently on an external device (the
  phone's sensors). A **partitioned actuator** instead splits *one* physical
  actuator into multiple addressable zones purely because config says to — e.g.
  a display driver whose `layout` config defines zones, each becoming its own
  actuator entity (type `display_region` or similar). Same underlying mechanism
  as Bridge Devices (one driver, N entities, driven by config) but for a
  different reason — worth keeping both examples in mind as the general pattern:
  **a driver's entity count is generally config-driven, not fixed at 1.**
  - **Addressing convention**: `entity_id` is a plain string, but hierarchical
    dotted names are a permitted *convention* inside it, not a new addressing
    level. So `gateway.display.top` is still valid `device_id.entity_id`
    addressing — `device_id = gateway`, `entity_id = "display.top"`. This also
    means zones can be prefix-matched (`gateway.display.*`) later for UI
    purposes (e.g. "list all zones on this display").
  - **Bindings target zones directly**, no change needed to the Binding concept:
    `Binding: rpm → gateway.display.top`, `Binding: now_playing_eq →
    gateway.display.bottom` — each zone is just an ordinary actuator entity as
    far as Bindings/Automations are concerned.
  - **Implementation note (not a taxonomy concern, but easy to get wrong)**: a
    display driver with multiple zone-entities needs one shared internal
    framebuffer/compositor. Each zone update should only touch its own region and
    the driver should only redraw the physical screen on actual change — treating
    zones as fully independent writers to the same screen risks flicker or zones
    stomping on each other. This lives inside the display driver's
    implementation, not in the entity/binding types.
  - **Display driver internal architecture — decouple hardware from rendering.**
    Widget/layout rendering logic (how a gauge or a templated text zone actually
    draws pixels) must not be tied to any one specific display chip. This is an
    internal implementation-architecture decision for display-type drivers, not
    a new taxonomy concept — zones remain ordinary actuator entities to the rest
    of the system either way. Three layers:
    - **Widget** — pure rendering: given a value, widget config (e.g. a
      `{{value}}` template string, or a gauge's `min`/`max`/redline), and a
      target region, draws onto any `embedded-graphics` `DrawTarget`. No
      hardware/I2C/SPI knowledge at all. `Text`, `GaugeLinear`, `GaugeArc` live
      here. Must be generic over pixel color (`PixelColor`), not hardcoded to
      monochrome — a future color TFT must reuse this code unchanged.
    - **Compositor** — zone management: owns the shared framebuffer, applies
      each zone's widget output to its region, tracks what actually changed,
      decides when to flush. Also generic over `DrawTarget` — doesn't know or
      care which physical display it's ultimately talking to.
    - **Display Driver** (e.g. `ssd1306.rs`) — thin: hardware init (I2C
      address/rotation/etc.), provides the `DrawTarget` (most display crates,
      including `ssd1306`, implement this already), and the actual flush call
      over the wire. No widget or layout logic should live here. A future
      display type (color TFT, e-ink) should only need a new thin driver like
      this, reusing Widget/Compositor code untouched.
  - **`DisplayHardware` trait — the last mile of decoupling.** Widget/Compositor
    decouple *rendering* from hardware, but without this, every new display
    chip still hand-writes identical `ActuatorDriver` plumbing (layout config
    parsing, Compositor wiring, `execute()`/`flush()` glue) — only the hardware
    init/`DrawTarget`/flush actually differs between chips. Introduce a small
    trait (roughly: `init`, `draw_target()`, `flush()`, `dimensions()`) plus one
    generic `GenericDisplayDriver<H: DisplayHardware>` that implements
    `ActuatorDriver` **once**, wiring Compositor/zone dispatch internally. A new
    display chip then only implements `DisplayHardware` (a handful of methods)
    and gets all zone/widget/dispatch plumbing for free, rather than being a
    copy-paste of the SSD1306 driver.
  - **Decouple value updates from render ticks.** Don't re-render/flush on every
    raw entity update — for a near-real-time entity like `rpm` this could mean
    flushing over I2C many times a second, wasting bus bandwidth and CPU for
    changes imperceptible to a human. The Compositor should sample the latest
    value at a fixed render interval (e.g. 10–15 Hz) instead. This is the
    display-side version of the same problem the update-rate/QoS entity
    contract exists to solve on the sensor/BLE side.
  - **Per-widget availability contract.** Each widget kind (`Text`,
    `GaugeLinear`, `GaugeArc`) must have explicit, tested behavior for
    `Stale`/`Unavailable` states, not just `Online` — e.g. `Text` might render
    `"RPM: --"`, a gauge might gray out or freeze its indicator. This should be
    a defined contract per widget kind, not whatever the rendering code
    happens to do incidentally.
  - **Noted for later, not being built now**: refresh-strategy differences for
    non-OLED displays (e-ink needs occasional full-refresh-to-avoid-ghosting
    logic, fundamentally different from "redraw on dirty" — would eventually
    live as an optional hint on `DisplayHardware`, not needed until an e-ink
    display actually exists to test against) and display power
    management/dimming (a real feature eventually, layered on top of this
    architecture, not part of the driver abstraction itself).

- **Binding** — a continuous, always-active mapping from an entity to an actuator,
  with no trigger or condition (e.g. "always show RPM on the display").
- **Automation** — Trigger (+ optional Condition) → Action (e.g. "RPM > 0 for 4s →
  play a sound on the phone's `audio_player`"). Keep Bindings and Automations as
  separate concepts — don't model an "always on" binding as a degenerate
  always-true automation.
  - **Action is a "service call" concept**, slightly broader than "command an
    actuator entity." Commanding an actuator is the default/common case, but the
    Action type should leave room for things like logging, notifications, or
    triggering other automations — don't hardcode Actions as strictly
    actuator-only.

### Entity identity, availability, units, and rate — decide these now, before scaffolding locks in assumptions

- **Addressing**: entities are namespaced as `device_id.entity_id` (e.g.
  `gateway.rpm`, `phone.gps_position`), never referenced by bare entity type.
  This avoids ambiguity once there's more than one device, or eventually more
  than one bike on the mesh. Bake this into the entity ID type from the start —
  retrofitting after automations/bindings reference bare types is painful.
- **Availability state**: every entity carries an availability state
  (`online` / `stale` / `unavailable`), not just a value. Bindings and
  Automations must be able to react to it — e.g. a display Binding should show a
  placeholder instead of a frozen last-known value once its source entity goes
  unavailable, and an Automation trigger should not fire against stale data.
  This matters concretely because Bridge Devices (the Phone) connect over BLE
  and *will* drop out — this isn't a hypothetical edge case.
- **`unit_of_measurement`**: centralized and fixed per entity type, not left to
  per-driver convention. One shared registry defines the unit, value type, and
  precision for each entity type (e.g. `rpm` is always `u32` in revolutions per
  minute). This is what actually makes "any driver producing `rpm` is
  interchangeable" true in practice, rather than just in theory — without a
  shared, enforced contract, two RPM-producing drivers could silently disagree
  on units or precision.
- **Update-rate / QoS**: part of an entity type's declared contract, alongside
  its unit. E.g. `rpm` expects near-real-time updates, `gps_position` is fine at
  ~1Hz, most config values almost never change. Not used for anything yet, but
  needed later to make sane decisions about prioritizing traffic on constrained
  BLE links (relevant once the sensor mesh exists). Add as a schema field now
  rather than retrofitting once several drivers have inconsistent implicit
  rates.

### Explicitly deferred (noted, not forgotten, not urgent)

- **Permissions/trust levels** for entities/drivers (e.g. "only the paired
  management app can write to actuators") are *not* being built now. The system
  is closed and config changes are already restricted to trusted devices, so
  this is sufficient for now. Revisit as a v1.1+ concern if the system ever
  needs to support less-trusted or third-party bridge devices.

## Known hard problems (don't casually reopen scope on these)

- **Real-time voice intercom mesh is the hardest, most speculative part of the
  project.** No open solution exists — Sena/Cardo use proprietary protocols on
  custom silicon, and standard BLE Mesh isn't built for continuous audio streaming.
  Decision: build the sensor/data mesh first (tractable on standard BLE); consider
  handling group audio at the phone-app layer later (e.g. WebRTC-style over local
  WiFi) instead of at the raw BLE radio level.

## Prior art worth reusing rather than reinventing

- **MotoMonkey** — open-source, extensible motorcycle data logger (ECU + BT + IMU + GPS).
- **moto32-hardware** — open-source ESP32-based alternative to Motogadget M-Unit Blue.
- **bonoGPS**, **ESP32-Motorcycle-Dash**, **Zero-Dashboard** — ESP32 dash/telemetry projects.
- **SensorServer**, **ros2-phone-sensors** — patterns for streaming phone sensors (GPS/IMU) to a microcontroller.
- **Meshtastic** — best reference architecture for encrypted, plugin-friendly mesh + phone app pairing (LoRa, not BLE, but the architecture maps well).
- **OpenXC**, **Freematics** — open vehicle-data platforms; Freematics ONE+ (ESP32-based, BLE/WiFi/optional cellular) may be usable hardware rather than something to design from scratch.

## Conventions / working style

- **Build for v1.0 directly — do not treat this as throwaway prototyping.** The
  intent is to build the real thing, not a disposable proof-of-concept to later
  rewrite. Write proper code: real error handling, clean module boundaries,
  following the Device/Entity/Driver taxonomy above, not quick hacks "just to see
  if it works." Whether the overall approach works is not in question — don't
  hedge structure or code quality against the possibility that this gets
  scrapped.
- That said, stay within "Current phase / priorities" above — v1.0 quality doesn't
  mean building every feature at once. Build the gateway + sensor + display loop
  properly first; don't start on the intercom mesh, custom PCB, or other
  out-of-scope items just because "build it properly" might tempt broader scope.
- When in doubt about scope (e.g. "should we build X now"), check "Current phase /
  priorities" above before proposing new work streams.