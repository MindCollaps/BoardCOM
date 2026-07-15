//! Zone management: owns per-zone rendering state, applies each zone's
//! widget output to its declared region of the shared framebuffer, and
//! tracks what actually changed so the driver only flushes hardware when
//! necessary.
//!
//! Generic over pixel color only — a [`Compositor`] borrows a `DrawTarget`
//! per call rather than owning one, so it never needs to know which physical
//! display (or even color depth) it's ultimately talking to. Panel
//! dimensions are passed in by the caller (a hardware fact the concrete
//! display driver legitimately knows) rather than hardcoded here.

use std::time::{Duration, Instant};

use embedded_graphics::draw_target::{DrawTarget, DrawTargetExt};
use embedded_graphics::geometry::{OriginDimensions, Size};
use embedded_graphics::pixelcolor::PixelColor;
use embedded_graphics::primitives::Rectangle;
use embedded_graphics::Pixel;
use serde::Deserialize;

use super::widgets::{WidgetConfig, WidgetPalette};
use crate::entities::state::Availability;
use crate::entities::{is_valid_id, ActuatorType, Entity, EntityId, EntityType, SensorValue};

/// Default render-tick rate: within the 10-15 Hz range suggested for display
/// updates — near-real-time sensors (like `rpm`) can update far faster than
/// this without every update reaching the physical display, the same
/// bandwidth/CPU concern the sensor-side update-rate/QoS contract exists for.
pub const DEFAULT_RENDER_INTERVAL: Duration = Duration::from_millis(80);

/// A single zone render slower than this is logged as a warning — see the
/// timing check in [`Compositor::tick`].
const SLOW_ZONE_RENDER_WARN: Duration = Duration::from_millis(150);

/// One zone of a partitioned display, as declared in config.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ZoneConfig {
    pub id: String,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub widget: WidgetConfig,
}

/// Validate a zone layout: ids, panel bounds, pairwise overlap, and each
/// zone's own widget config. `panel_width`/`panel_height` are the physical
/// display's resolution — supplied by the caller, not assumed here.
pub fn validate_layout(
    layout: &[ZoneConfig],
    panel_width: u32,
    panel_height: u32,
) -> Result<(), String> {
    if layout.is_empty() {
        return Err("layout must define at least one zone".to_owned());
    }
    for (i, zone) in layout.iter().enumerate() {
        if !is_valid_id(&zone.id) {
            return Err(format!(
                "zone id '{}' is invalid: ids must match [a-z][a-z0-9_]*",
                zone.id
            ));
        }
        if layout[..i].iter().any(|z| z.id == zone.id) {
            return Err(format!("duplicate zone id '{}'", zone.id));
        }
        if zone.width == 0 || zone.height == 0 {
            return Err(format!("zone '{}' has zero width or height", zone.id));
        }
        if zone.x + zone.width > panel_width || zone.y + zone.height > panel_height {
            return Err(format!(
                "zone '{}' exceeds the {panel_width}x{panel_height} panel",
                zone.id
            ));
        }
        for other in &layout[..i] {
            let disjoint = zone.x >= other.x + other.width
                || other.x >= zone.x + zone.width
                || zone.y >= other.y + other.height
                || other.y >= zone.y + zone.height;
            if !disjoint {
                return Err(format!("zones '{}' and '{}' overlap", other.id, zone.id));
            }
        }
        zone.widget
            .validate()
            .map_err(|e| format!("zone '{}': {e}", zone.id))?;
    }
    Ok(())
}

/// Zone -> entity expansion: one `display_region` actuator entity per zone,
/// addressed as `<declared_entity_id>.<zone_id>` — the dotted names stay
/// ordinary `device_id.entity_id` addresses, dots in the entity part are a
/// naming convention only. Called by config validation before any
/// hardware/driver instance exists — `declared` must be a `display_panel`
/// entity; the panel itself is never directly addressable once partitioned.
pub fn zone_entities(declared: &Entity, layout: &[ZoneConfig]) -> Result<Vec<Entity>, String> {
    if declared.entity_type != EntityType::Actuator(ActuatorType::DisplayPanel) {
        return Err(format!(
            "a partitioned display must be declared as a 'display_panel' entity, not '{}'",
            declared.entity_type.as_str()
        ));
    }
    layout
        .iter()
        .map(|zone| {
            let local = format!("{}.{}", declared.id.entity(), zone.id);
            let id =
                EntityId::new(declared.id.device().clone(), &local).map_err(|e| e.to_string())?;
            Ok(Entity {
                id,
                entity_type: EntityType::Actuator(ActuatorType::DisplayRegion),
                name: Some(zone.id.clone()),
            })
        })
        .collect()
}

struct ZoneState {
    entity_id: EntityId,
    rect: Rectangle,
    widget: WidgetConfig,
    /// Latest state a Binding has delivered via [`Compositor::stage`],
    /// whether or not it has actually been rendered yet.
    staged: (Availability, Option<SensorValue>),
    /// What's currently drawn into the framebuffer for this zone, if
    /// anything has been rendered yet.
    rendered: Option<(Availability, Option<SensorValue>)>,
}

/// Shared framebuffer/zone-state manager for one partitioned display.
///
/// Value updates and render ticks are deliberately decoupled: [`Self::stage`]
/// just records the latest state for a zone (no drawing, no `DrawTarget`
/// access), and [`Self::tick`] is what actually samples staged state into
/// pixels — at most once per `render_interval`, regardless of how often
/// `stage` was called in between. This is the display-side counterpart of
/// the sensor-side update-rate/QoS contract: a near-real-time entity can
/// update far faster than any display needs to physically redraw.
pub struct Compositor<C: PixelColor> {
    zones: Vec<ZoneState>,
    palette: WidgetPalette<C>,
    dirty: bool,
    render_interval: Duration,
    last_render: Option<Instant>,
    /// When present, zones render into this RAM buffer and reach the target
    /// as one contiguous blit — see [`Self::with_zone_buffering`].
    scratch: Option<ZoneScratch<C>>,
}

/// Zone-sized RAM scratch buffer, reused across renders (its allocation
/// grows to the largest zone and stays there).
///
/// Exists for write-through displays (e.g. `mipidsi`-driven SPI panels, which
/// have no RAM framebuffer): widgets draw fine-grained output — dither
/// patterns, arc strokes, glyphs — that degenerates into thousands of
/// one-pixel bus transactions when drawn straight to such a display, each
/// carrying fixed address-window overhead. Rendering a zone here first and
/// then blitting it with a single `fill_contiguous` turns that into one
/// streamed write. (Displays whose driver crate already buffers in RAM, like
/// the SSD1306, skip this — it would only add a copy.)
struct ZoneScratch<C> {
    size: Size,
    pixels: Vec<C>,
}

impl<C: PixelColor> ZoneScratch<C> {
    fn new() -> Self {
        Self {
            size: Size::zero(),
            pixels: Vec::new(),
        }
    }

    fn reset(&mut self, size: Size, background: C) {
        self.size = size;
        self.pixels.clear();
        self.pixels
            .resize((size.width * size.height) as usize, background);
    }
}

impl<C: PixelColor> OriginDimensions for ZoneScratch<C> {
    fn size(&self) -> Size {
        self.size
    }
}

impl<C: PixelColor> DrawTarget for ZoneScratch<C> {
    type Color = C;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<C>>,
    {
        for Pixel(p, color) in pixels {
            if p.x >= 0
                && p.y >= 0
                && (p.x as u32) < self.size.width
                && (p.y as u32) < self.size.height
            {
                self.pixels[(p.y as u32 * self.size.width + p.x as u32) as usize] = color;
            }
        }
        Ok(())
    }
}

impl<C: PixelColor> Compositor<C> {
    pub fn new(zones: Vec<(EntityId, Rectangle, WidgetConfig)>, palette: WidgetPalette<C>) -> Self {
        Self {
            zones: zones
                .into_iter()
                .map(|(entity_id, rect, widget)| ZoneState {
                    entity_id,
                    rect,
                    widget,
                    staged: (Availability::Unavailable, None),
                    rendered: None,
                })
                .collect(),
            palette,
            dirty: false,
            render_interval: DEFAULT_RENDER_INTERVAL,
            last_render: None,
            scratch: None,
        }
    }

    /// Override the default render-tick rate.
    #[must_use]
    pub fn with_render_interval(mut self, interval: Duration) -> Self {
        self.render_interval = interval;
        self
    }

    /// Render zones via a RAM scratch buffer and blit each as one contiguous
    /// write (see [`ZoneScratch`]). For write-through displays; pointless
    /// (one extra copy) for displays that already buffer a frame in RAM.
    #[must_use]
    pub fn with_zone_buffering(mut self) -> Self {
        self.scratch = Some(ZoneScratch::new());
        self
    }

    /// The zone index for a bound entity, if any zone this compositor
    /// manages is addressed by it.
    pub fn zone_index(&self, entity_id: &EntityId) -> Option<usize> {
        self.zones.iter().position(|z| z.entity_id == *entity_id)
    }

    /// Record the latest state for a zone (by index from
    /// [`Self::zone_index`]). Does not draw anything — only [`Self::tick`]
    /// renders, and only at the configured interval.
    pub fn stage(
        &mut self,
        zone_idx: usize,
        availability: Availability,
        value: Option<SensorValue>,
    ) {
        self.zones[zone_idx].staged = (availability, value);
    }

    /// Sample every zone's latest staged state into the framebuffer, but
    /// only if `render_interval` has elapsed since the last render (the very
    /// first call always renders, giving every zone its initial frame for
    /// free). Zones whose staged state hasn't changed since they were last
    /// rendered are skipped even when a render does happen.
    pub fn tick<D>(&mut self, target: &mut D, now: Instant) -> Result<(), D::Error>
    where
        D: DrawTarget<Color = C>,
    {
        if let Some(last) = self.last_render {
            if now.duration_since(last) < self.render_interval {
                return Ok(());
            }
        }
        self.last_render = Some(now);

        for zone in &mut self.zones {
            if zone.rendered == Some(zone.staged) {
                continue;
            }
            let (availability, value) = zone.staged;
            let started = Instant::now();
            match &mut self.scratch {
                Some(scratch) => {
                    render_zone_buffered(target, zone, &self.palette, scratch, availability, value)?
                }
                None => render_zone(target, zone, &self.palette, availability, value)?,
            }
            // A zone render blocks the main loop, so one taking longer than
            // the render interval itself is a real problem (a too-slow bus
            // path, a pathological widget) — make it visible instead of
            // letting it silently starve the rest of the system.
            let elapsed = started.elapsed();
            if elapsed > SLOW_ZONE_RENDER_WARN {
                log::warn!(
                    "zone '{}' took {}ms to render+write (interval {}ms)",
                    zone.entity_id,
                    elapsed.as_millis(),
                    self.render_interval.as_millis()
                );
            }
            zone.rendered = Some(zone.staged);
            self.dirty = true;
        }
        Ok(())
    }

    /// Whether any zone changed since the last call to this method; clears
    /// the flag. The driver calls this once per tick to decide whether the
    /// physical display needs a flush at all.
    pub fn take_dirty(&mut self) -> bool {
        let dirty = self.dirty;
        self.dirty = false;
        dirty
    }
}

/// Re-render one zone. Drawing is clipped to the zone's rectangle: a zone can
/// never touch pixels outside its declared geometry.
fn render_zone<D>(
    target: &mut D,
    zone: &ZoneState,
    palette: &WidgetPalette<D::Color>,
    availability: Availability,
    value: Option<SensorValue>,
) -> Result<(), D::Error>
where
    D: DrawTarget,
{
    target.fill_solid(&zone.rect, palette.background)?;
    let mut region = target.clipped(&zone.rect);
    zone.widget
        .render(&mut region, zone.rect, *palette, availability, value)
}

/// [`render_zone`], but via the scratch buffer: the widget draws into RAM
/// (translated so the zone's absolute coordinates land at the buffer's
/// origin), then the finished zone reaches the target as one contiguous
/// blit. The zone's geometry guarantee holds structurally here: the buffer
/// *is* the zone rectangle, so there are no outside pixels to touch.
fn render_zone_buffered<D>(
    target: &mut D,
    zone: &ZoneState,
    palette: &WidgetPalette<D::Color>,
    scratch: &mut ZoneScratch<D::Color>,
    availability: Availability,
    value: Option<SensorValue>,
) -> Result<(), D::Error>
where
    D: DrawTarget,
{
    scratch.reset(zone.rect.size, palette.background);
    let mut local = scratch.translated(-zone.rect.top_left);
    let Ok(()) = zone
        .widget
        .render(&mut local, zone.rect, *palette, availability, value);
    target.fill_contiguous(&zone.rect, scratch.pixels.iter().copied())
}

#[cfg(test)]
mod tests {
    use super::*;
    use embedded_graphics::geometry::Point;
    use embedded_graphics::mock_display::MockDisplay;
    use embedded_graphics::pixelcolor::BinaryColor;

    fn declared() -> Entity {
        Entity {
            id: "gateway.display".parse().unwrap(),
            entity_type: EntityType::Actuator(ActuatorType::DisplayPanel),
            name: None,
        }
    }

    fn text_zone(id: &str, x: u32, y: u32, width: u32, height: u32) -> ZoneConfig {
        ZoneConfig {
            id: id.to_owned(),
            x,
            y,
            width,
            height,
            widget: WidgetConfig::Text {
                template: "{{value}}".to_owned(),
                font: Default::default(),
                align: Default::default(),
            },
        }
    }

    #[test]
    fn zones_expand_to_dotted_display_region_entities() {
        let layout = vec![
            text_zone("top", 0, 0, 128, 40),
            text_zone("bottom", 0, 40, 128, 24),
        ];
        let entities = zone_entities(&declared(), &layout).unwrap();
        assert_eq!(entities.len(), 2);
        assert_eq!(entities[0].id.to_string(), "gateway.display.top");
        assert_eq!(entities[1].id.to_string(), "gateway.display.bottom");
        assert!(entities
            .iter()
            .all(|e| e.entity_type == EntityType::Actuator(ActuatorType::DisplayRegion)));
    }

    #[test]
    fn zone_count_is_config_driven_not_fixed() {
        let layout: Vec<_> = (0..4)
            .map(|i| text_zone(&format!("z{i}"), 0, i * 16, 128, 16))
            .collect();
        let entities = zone_entities(&declared(), &layout).unwrap();
        assert_eq!(entities.len(), 4);
    }

    #[test]
    fn rejects_wrong_declared_entity_type() {
        let mut wrong = declared();
        wrong.entity_type = EntityType::Actuator(ActuatorType::AudioPlayer);
        let layout = vec![text_zone("a", 0, 0, 64, 16)];
        assert!(zone_entities(&wrong, &layout).is_err());
    }

    #[test]
    fn rejects_overlapping_zones() {
        let layout = vec![text_zone("a", 0, 0, 64, 32), text_zone("b", 32, 16, 64, 32)];
        let err = validate_layout(&layout, 128, 64).unwrap_err();
        assert!(err.contains("overlap"), "{err}");
    }

    #[test]
    fn rejects_zone_outside_panel() {
        let layout = vec![text_zone("a", 100, 0, 40, 16)];
        let err = validate_layout(&layout, 128, 64).unwrap_err();
        assert!(err.contains("exceeds"), "{err}");
    }

    #[test]
    fn rejects_duplicate_zone_ids_and_bad_gauge_ranges() {
        let dup = vec![text_zone("a", 0, 0, 64, 16), text_zone("a", 0, 16, 64, 16)];
        assert!(validate_layout(&dup, 128, 64)
            .unwrap_err()
            .contains("duplicate zone id"));

        let bad_range = vec![ZoneConfig {
            id: "a".to_owned(),
            x: 0,
            y: 0,
            width: 64,
            height: 16,
            widget: WidgetConfig::GaugeLinear {
                min: 5.0,
                max: 5.0,
                redline: None,
            },
        }];
        assert!(validate_layout(&bad_range, 128, 64)
            .unwrap_err()
            .contains("min < max"));
    }

    #[test]
    fn rejects_missing_placeholder_via_widget_validate() {
        let layout = vec![ZoneConfig {
            id: "a".to_owned(),
            x: 0,
            y: 0,
            width: 64,
            height: 16,
            widget: WidgetConfig::Text {
                template: "Speed".to_owned(),
                font: Default::default(),
                align: Default::default(),
            },
        }];
        assert!(validate_layout(&layout, 128, 64)
            .unwrap_err()
            .contains("{{value}}"));
    }

    fn one_zone_compositor() -> Compositor<BinaryColor> {
        Compositor::new(
            vec![(
                "gateway.display.top".parse().unwrap(),
                Rectangle::new(
                    Point::zero(),
                    embedded_graphics::geometry::Size::new(60, 16),
                ),
                WidgetConfig::Text {
                    template: "{{value}}".to_owned(),
                    font: Default::default(),
                    align: Default::default(),
                },
            )],
            WidgetPalette {
                foreground: BinaryColor::On,
                background: BinaryColor::Off,
                dim: BinaryColor::On,
            },
        )
    }

    fn mock_display() -> MockDisplay<BinaryColor> {
        let mut display = MockDisplay::new();
        display.set_allow_overdraw(true);
        display
    }

    #[test]
    fn tick_skips_unchanged_state_and_tracks_dirty() {
        let mut compositor = one_zone_compositor();
        let mut display = mock_display();
        let t0 = Instant::now();

        // First tick always renders, even with nothing staged yet.
        compositor.tick(&mut display, t0).unwrap();
        assert!(compositor.take_dirty());

        compositor.stage(0, Availability::Online, Some(SensorValue::Rpm(1200)));
        compositor
            .tick(&mut display, t0 + compositor.render_interval)
            .unwrap();
        assert!(compositor.take_dirty());
        assert!(!compositor.take_dirty(), "dirty flag should clear itself");

        // Same state staged again: the next tick renders nothing new.
        compositor.stage(0, Availability::Online, Some(SensorValue::Rpm(1200)));
        compositor
            .tick(&mut display, t0 + compositor.render_interval * 2)
            .unwrap();
        assert!(!compositor.take_dirty());
    }

    #[test]
    fn tick_does_not_render_faster_than_the_configured_interval() {
        let mut compositor = one_zone_compositor().with_render_interval(Duration::from_millis(100));
        let mut display = mock_display();
        let t0 = Instant::now();

        compositor.tick(&mut display, t0).unwrap();
        assert!(compositor.take_dirty(), "the first tick always renders");

        // Stage a rapid burst of distinct updates, all within one interval:
        // no matter how many raw updates arrive, only the interval boundary
        // should trigger a render.
        for i in 1..=20u32 {
            compositor.stage(0, Availability::Online, Some(SensorValue::Rpm(i)));
            compositor
                .tick(&mut display, t0 + Duration::from_millis(u64::from(i)))
                .unwrap();
            assert!(
                !compositor.take_dirty(),
                "tick before the interval elapsed must not render (i={i})"
            );
        }

        // Once the interval has elapsed, the next tick renders whatever the
        // latest staged value happens to be.
        compositor.stage(0, Availability::Online, Some(SensorValue::Rpm(999)));
        compositor
            .tick(&mut display, t0 + Duration::from_millis(150))
            .unwrap();
        assert!(
            compositor.take_dirty(),
            "tick past the interval must render"
        );
    }

    #[test]
    fn zone_index_finds_zones_by_entity_id() {
        let compositor = Compositor::new(
            vec![(
                "gateway.display.top".parse().unwrap(),
                Rectangle::new(
                    Point::zero(),
                    embedded_graphics::geometry::Size::new(60, 16),
                ),
                WidgetConfig::Text {
                    template: "{{value}}".to_owned(),
                    font: Default::default(),
                    align: Default::default(),
                },
            )],
            WidgetPalette {
                foreground: BinaryColor::On,
                background: BinaryColor::Off,
                dim: BinaryColor::On,
            },
        );
        assert_eq!(
            compositor.zone_index(&"gateway.display.top".parse().unwrap()),
            Some(0)
        );
        assert_eq!(
            compositor.zone_index(&"gateway.display.bottom".parse().unwrap()),
            None
        );
    }

    /// The zone-buffered path is a pure transport optimization: its pixel
    /// output must be identical to direct rendering. Exercised with the
    /// filled arc gauge — the dithered widget whose per-pixel output is the
    /// reason zone buffering exists.
    #[test]
    fn zone_buffering_renders_identically_to_direct() {
        let zones = || -> Vec<(EntityId, Rectangle, WidgetConfig)> {
            vec![(
                "gateway.display.dial".parse().unwrap(),
                Rectangle::new(
                    Point::new(2, 3),
                    embedded_graphics::geometry::Size::new(56, 28),
                ),
                WidgetConfig::GaugeArc {
                    min: 0.0,
                    max: 100.0,
                    redline: Some(80.0),
                    style: crate::drivers::display::widgets::ArcGaugeStyle::Filled,
                },
            )]
        };
        let palette = WidgetPalette {
            foreground: BinaryColor::On,
            background: BinaryColor::Off,
            dim: BinaryColor::On,
        };

        let mut direct = Compositor::new(zones(), palette);
        let mut buffered = Compositor::new(zones(), palette).with_zone_buffering();

        let mut direct_display = mock_display();
        // No overdraw allowance needed: the blit writes each pixel once.
        let mut buffered_display = MockDisplay::new();

        let t0 = Instant::now();
        for compositor in [&mut direct, &mut buffered] {
            compositor.stage(0, Availability::Online, Some(SensorValue::Rpm(65)));
        }
        direct.tick(&mut direct_display, t0).unwrap();
        buffered.tick(&mut buffered_display, t0).unwrap();
        assert!(buffered.take_dirty());

        assert_eq!(direct_display, buffered_display);
    }
}
