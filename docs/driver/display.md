# Display drivers

How display support is layered, and what it takes to add a new display chip.
See also the "Display driver internal architecture" section in `CLAUDE.md` for
the design rationale; this file documents the implementation as built.

## The three layers

All chip-agnostic code lives in `firmware/src/drivers/display/` and compiles
(and is unit-tested) on the host — no ESP32, simulator, or hardware involved.

### 1. Widgets — `display/widgets.rs`

Pure rendering: given a bound value, the widget's own config, and a target
region, a widget draws onto any `embedded-graphics` `DrawTarget`. No I2C/SPI
or chip knowledge, and generic over pixel color (`WidgetPalette<C: PixelColor>`
supplies `foreground` / `background` / `dim`).

| Kind | Config | Notes |
|---|---|---|
| `text` | `template` (single `{{value}}` placeholder), `font` (`small`/`medium`/`large`), `align` (`left`/`center`/`right`) | Renders `--` when the source is stale/unavailable |
| `gauge_linear` | `min`, `max`, optional `redline` | Filled horizontal bar |
| `gauge_arc` | `min`, `max`, optional `redline`, optional `style` (`0` classic needle, `1` filled/edge-hugging) | On monochrome panels the filled style dithers; on color panels it uses the palette's literal `dim` shade |

Adding a widget kind = one new enum variant with its config shape + one render
arm. Nothing else moves.

### 2. Compositor — `display/compositor.rs`

Zone management: layout validation (zones must fit the panel, not overlap,
have unique ids), mapping each zone to its own `display_region` entity
(`<device>.<display_entity>.<zone>`), staging incoming values, and rendering
zones **only when their content changed**, clipped to the zone's rectangle.
Render ticks are decoupled from value updates: values are staged as they
arrive and sampled at a render interval (default 80 ms, per-instance
`render_interval_ms` in config), so a fast sensor cannot cause excessive
redraws or bus traffic.

**Zone buffering** (`with_zone_buffering()`, opt-in per chip driver): renders
each zone into a reused RAM scratch buffer and blits it to the panel as one
contiguous write. Essential for write-through displays (no RAM framebuffer of
their own — e.g. `mipidsi` SPI panels), where fine-grained widget output
(dither patterns, arc strokes, glyphs) otherwise degenerates into thousands
of per-pixel bus transactions, each with fixed address-window overhead — in
practice multi-second stalls that starve the idle task and trip the ESP32
task watchdog. Buffered displays like the SSD1306 skip it; it would only add
a copy. A test guarantees the buffered path's pixel output is identical to
direct rendering.

### 3. `DisplayHardware` + `GenericDisplayDriver` — `display/generic.rs`

`DisplayHardware` is what a concrete chip must provide: `dimensions()`,
`draw_target()`, `flush()`. Construction is deliberately *not* in the trait —
every chip's config shape differs, so each chip module has an inherent
constructor instead. `GenericDisplayDriver<H: DisplayHardware>` implements the
`ActuatorDriver` plumbing (zone dispatch, tick, flush-only-when-dirty)
**once**, for every chip.

## The thin chip drivers

Panel geometry, rotation, bus speed, and render rate are per-instance config
on both drivers — a module's glass size varies independently of its
controller, so nothing that can vary is a constant in the driver.

| | `ssd1306_display.rs` | `ili9341_display.rs` |
|---|---|---|
| Panel | monochrome OLED; `width`×`height` must be one of the module sizes the `ssd1306` crate supports (128×64 default, 128×32, 96×16, 72×40, 64×48, 64×32) | color TFT, `Rgb565`; `width`×`height` + `offset_x`/`offset_y` within the controller's 240×320 framebuffer (defaults to all of it) |
| Bus | I2C (`i2c_sda`, `i2c_scl`, optional `address`, `baudrate_hz`) | SPI2 (`spi_sck`, `spi_mosi`, `spi_cs`, `spi_dc`, `spi_rst`, optional `baudrate_hz` — default 26 MHz, the ESP32's GPIO-matrix routing limit) |
| Crate | `ssd1306` (buffered graphics mode) | `mipidsi` (`ILI9341Rgb565` model) |
| Flush model | RAM framebuffer pushed on `flush()` | Write-through: draws go straight over SPI, `flush()` is a no-op — a full 320×240×16-bit framebuffer (150 KiB) doesn't fit ESP32 RAM. Uses compositor zone buffering (see above) |
| Rotation | `rotation`: 0/90/180/270 | same, plus `mirrored` (MADCTL mirror bit) for modules that scan opposite to the mipidsi default — Wokwi's ILI9341 needs it |

Both also take `render_interval_ms` (default 80). `rotation` 90/270 swaps the
logical width/height, and the zone layout is validated against the rotated
size.

Each file contains *only* hardware facts, init, and the flush call — roughly
250 lines each, most of it config schema and the factory's entity cross-check.

## Adding a new display chip

1. New `src/drivers/<chip>_display.rs`: config struct (bus pins + `layout:
   Vec<ZoneConfig>`), `parse_config()`, a `DisplayHardware` impl wrapping the
   chip crate's `DrawTarget`, and a registry `factory()`. Use
   `ili9341_display.rs` as the template — it is the minimal shape.
2. Register the kind: constant + `BUILTIN_DRIVER_KINDS` + `expand_entities()`
   arm in `drivers/mod.rs`, one `registry.register(...)` line in
   `drivers/registry.rs`.
3. If the chip needs a bus the `HardwarePool` doesn't hand out yet, add a
   `claim_*` method in `drivers/hw.rs`. Configure DMA on it (see
   `claim_spi`): without DMA, esp-idf splits every transfer into 64-byte
   FIFO transactions, and bulk display writes degenerate into thousands of
   them — multi-second stalls that trip the task watchdog.

Widgets, zones, entities, bindings, and dirty-tracking come along for free;
zones on the new display are ordinary actuator entities the moment the config
declares them.

## Known deferrals

- E-ink refresh strategies (periodic full refresh to avoid ghosting) would be
  an optional hint on `DisplayHardware` — not built until real e-ink hardware
  exists to test against.
- Display power management / dimming is a future feature layered on top, not
  part of the driver abstraction.
