# 🏍️ BoardCOM

**An open-source telemetry, display, and connectivity system for motorcycles.**

Gather real data from your bike, show it however you want, and connect the
Bluetooth devices you already own — all on hardware you control, running
firmware you can actually read.

> ⚠️ **Status: early prototyping.** Nothing here is ready for a real bike yet.
> See [Current Status](#-current-status) below.

---

## 🤔 What is this?

Most bikes either give you nothing beyond a stock dash, or lock you into a
single vendor's closed ecosystem if you want more. BoardCOM is a different
approach: a small microcontroller gateway that reads your bike's sensors,
drives your displays, and talks to your phone — open, extensible, and yours.

- 📡 **Gather data** — RPM, speed, and whatever else you wire up next
- 🖥️ **Show data** — on small physical displays, with a real layout system
  (gauges, text, multiple zones on one screen)
- 🔗 **Connect devices** — Bluetooth to your phone for setup, and eventually
  to other riders

No cloud dependency. No subscription. No vendor lock-in.

## 🧩 How it's put together

Three roles, three devices:

1. **🧠 The Gateway (ESP32 + Rust)** — the brain. Talks to sensors and
   displays directly. This is the current build focus.
2. **📱 An old Android phone (optional)** — repurposed as a peripheral
   extender. Free GPS, a spare screen, a gyroscope, and a SIM for remote
   connectivity — hardware you probably already have in a drawer.
3. **⚙️ A management app** — pairs with the gateway over Bluetooth to
   configure everything while you're parked, not while you're riding.

## ✨ Design principles

- 🔒 **Secure by default** — encryption isn't an afterthought
- 🧱 **Plugin-friendly** — new sensor types should be easy to add without
  forking the world
- 🙋 **Easy for non-technical riders** — it should just work out of the box
- 🛠️ **Easy to extend for technical ones** — the internals are yours to hack

## 🚧 Current status

Actively prototyping. Priorities right now, in order:

- [x] Core architecture & naming taxonomy (Device / Entity / Driver)
- [x] Simulated ESP32 dev loop (no physical hardware required yet, via Wokwi)
- [x] First sensor → display pipeline (RPM via pulse counting)
- [x] Zoned display layouts with gauges & templated text
- [ ] Real sensor wiring (RPM + speed on actual hardware)
- [ ] Android management app
- [ ] Multi-device mesh / intercom bridging *(exploratory, not started)*

Custom PCB design is a future goal, not a current one — software and
prototyping come first.

## 🛠️ Tech stack

| Layer | Choice | Why |
|---|---|---|
| Firmware | **Rust** on **ESP32** | Rust from day one — no C++ detour, since it's the intended long-term language anyway |
| Dev loop | **Wokwi** simulation | Build and test sensor/display logic before touching real hardware |
| Management app | **Native Android (Kotlin)**, leaning | BLE is first-class on Android, and the phone-peripheral piece is Android anyway |

## 📁 Repo structure

```
firmware/       ESP32 firmware (Rust)
  src/
    drivers/    Sensor & actuator drivers (pulse_counter, display, ...)
    entities/   Device/Entity/Driver taxonomy types
    config/     JSON config parsing → instantiated devices & entities
    automations/  Bindings & Automations
  wokwi/        Simulation setup
app/            Management app (placeholder, not started)
CLAUDE.md       Project context & conventions for AI-assisted development
```

## 🙏 Built on the shoulders of others

Not reinventing what already exists. Drawing on ideas and code from
**MotoMonkey**, **moto32-hardware**, **bonoGPS**, **ESPHome**,
**Meshtastic**, **OpenXC**, and **Freematics** — see `CLAUDE.md` for details
on what's borrowed from where.

## 🤖 A note on AI-assisted development

This project uses AI tools (Claude, Claude Code) to help with scaffolding,
boilerplate, and implementation grunt work. **Every architectural decision,
the taxonomy, the tech stack choices, and the overall design are human-made —
mine** — and all AI-generated code is reviewed by me before it lands. AI here
is a tool for moving faster, not a replacement for the thinking behind the
project.

## 📜 License

*(TBD — pick and add a license here before accepting outside contributions.)*

## 🤝 Contributing

Not quite ready for outside contributions yet — the core architecture is
still settling. Watch this space, or open an issue if you've got thoughts.