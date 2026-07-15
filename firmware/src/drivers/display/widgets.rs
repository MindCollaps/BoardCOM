//! Widget rendering: pure drawing logic with no hardware, I2C/SPI, or
//! specific-chip knowledge. Given a bound value, a widget's own config, and a
//! target region, a widget draws onto any `embedded-graphics` `DrawTarget`.
//! Generic over pixel color, so the same code serves monochrome and color
//! displays.
//!
//! `text`, `gauge_linear`, and `gauge_arc` are a closed set of widget kinds;
//! a new kind is a new [`WidgetConfig`] variant plus a render/validate arm,
//! not a restructuring of the others.

use embedded_graphics::geometry::AngleUnit;
use embedded_graphics::mono_font::ascii::{FONT_10X20, FONT_6X10, FONT_8X13};
use embedded_graphics::mono_font::{MonoFont, MonoTextStyle};
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Arc, Line, PrimitiveStyle, Rectangle};
use embedded_graphics::text::{Baseline, Text};
use embedded_graphics::Pixel;
use serde::Deserialize;

use crate::entities::state::Availability;
use crate::entities::SensorValue;

/// Colors a widget draws with. Kept separate from any one `PixelColor`
/// space's own semantics — every widget receives an explicit
/// foreground/background/dim triple instead of assuming a two-color (on/off)
/// duality that doesn't hold for e.g. RGB targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WidgetPalette<C> {
    pub foreground: C,
    pub background: C,
    /// Color for "already swept"/dimmed regions (e.g. the filled arc gauge
    /// style). On a monochrome palette this typically equals `foreground` —
    /// checkerboard dithering, not a third color value, is what makes it read
    /// as dim there. A color palette can instead supply a literal distinct
    /// shade; the rendering code doesn't care which.
    pub dim: C,
}

/// The closed set of widget kinds a zone can render. Each kind owns its
/// config shape; adding a kind is a new variant + render/validate arm.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WidgetConfig {
    /// A template string with a single `{{value}}` placeholder (deliberately
    /// not a templating engine), e.g. `"Speed: {{value}}"`.
    Text {
        template: String,
        #[serde(default)]
        font: FontSize,
        #[serde(default)]
        align: Align,
    },
    /// Bar-style gauge: value mapped onto `min..=max`, drawn as a filled bar.
    GaugeLinear {
        min: f64,
        max: f64,
        #[serde(default)]
        redline: Option<f64>,
    },
    /// Arc-style gauge (analog-dial look): same range concept along a
    /// half-circle arc with a needle.
    GaugeArc {
        min: f64,
        max: f64,
        #[serde(default)]
        redline: Option<f64>,
        /// Presentation style; a config-level number selects the variant
        /// (0 = classic, 1 = filled) so a third style later is a new variant
        /// + renderer arm, not a restructuring of this one.
        #[serde(default)]
        style: ArcGaugeStyle,
    },
}

impl WidgetConfig {
    /// Validate this widget's own config, independent of zone geometry
    /// (callers wrap the message with zone context).
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Text { template, .. } => {
                if !template.contains("{{value}}") {
                    return Err("text template must contain '{{value}}'".to_owned());
                }
                Ok(())
            }
            Self::GaugeLinear { min, max, redline } => validate_gauge_range(*min, *max, *redline),
            Self::GaugeArc {
                min, max, redline, ..
            } => validate_gauge_range(*min, *max, *redline),
        }
    }

    /// Draw this widget's current state into `rect` on `target`. No hardware
    /// or chip knowledge — works on any `DrawTarget`.
    pub fn render<D>(
        &self,
        target: &mut D,
        rect: Rectangle,
        palette: WidgetPalette<D::Color>,
        availability: Availability,
        value: Option<SensorValue>,
    ) -> Result<(), D::Error>
    where
        D: DrawTarget,
    {
        match self {
            Self::Text {
                template,
                font,
                align,
            } => render_text(
                target,
                rect,
                palette,
                template,
                *font,
                *align,
                availability,
                value,
            ),
            Self::GaugeLinear { min, max, redline } => render_gauge_linear(
                target,
                rect,
                palette,
                *min,
                *max,
                *redline,
                availability,
                value,
            ),
            Self::GaugeArc {
                min,
                max,
                redline,
                style,
            } => render_gauge_arc(
                target,
                rect,
                palette,
                *min,
                *max,
                *redline,
                *style,
                availability,
                value,
            ),
        }
    }
}

fn validate_gauge_range(min: f64, max: f64, redline: Option<f64>) -> Result<(), String> {
    if min >= max {
        return Err("gauge needs min < max".to_owned());
    }
    if let Some(r) = redline {
        if r < min || r > max {
            return Err("redline must lie within min..=max".to_owned());
        }
    }
    Ok(())
}

/// The two presentation styles for [`WidgetConfig::GaugeArc`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(try_from = "u8")]
pub enum ArcGaugeStyle {
    /// Thin arc track + needle line (the original look).
    #[default]
    Classic,
    /// Solid rim hugging the zone's edges; the range already swept by the
    /// needle is filled with a dithered (checkerboard) pattern to read as
    /// dimmer on a monochrome panel, with a solid needle on top.
    Filled,
}

impl TryFrom<u8> for ArcGaugeStyle {
    type Error = String;

    fn try_from(value: u8) -> Result<Self, String> {
        match value {
            0 => Ok(Self::Classic),
            1 => Ok(Self::Filled),
            other => Err(format!("gauge_arc style must be 0 or 1, got {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FontSize {
    Small,
    #[default]
    Medium,
    Large,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

/// Never freeze a last-known value on screen: only `Online` data with a
/// value renders as text; anything else (stale, unavailable, no reading yet)
/// is a placeholder.
fn value_text(availability: Availability, value: Option<SensorValue>) -> String {
    match (availability, value) {
        (Availability::Online, Some(v)) => v.format_value(),
        _ => "--".to_owned(),
    }
}

/// Fraction of a gauge's range the value covers, if any value is present.
///
/// Availability contract for gauges: **freeze at the last position** rather
/// than gray out or hide. Deliberately ignores `availability` — the entity
/// layer's contract (`entities::state::StateStore`) already guarantees a
/// `Stale` reading still carries its last real value forward, and clears the
/// value to `None` only once an entity actually goes `Unavailable`. So
/// gating on `value.is_some()` alone already gives exactly "keep showing the
/// last position while merely stale, show nothing once truly gone" — with no
/// need to thread `availability` through here at all.
///
/// Freeze was chosen over graying out with `palette.dim`: `dim` is already
/// used by the arc gauge's `Filled` style for an unrelated aesthetic reason
/// (the dithered "already swept" pattern), typically set equal to
/// `foreground` on a monochrome palette so the dither is visible against the
/// background. Overloading the same field to *also* mean "de-emphasized for
/// staleness" can't be satisfied at once on a 2-value monochrome palette —
/// freeze needs no color decision at all, so it doesn't collide.
fn gauge_fraction(value: Option<SensorValue>, min: f64, max: f64) -> Option<f64> {
    Some(((value?.as_f64() - min) / (max - min)).clamp(0.0, 1.0))
}

#[allow(clippy::too_many_arguments)]
fn render_text<D>(
    target: &mut D,
    rect: Rectangle,
    palette: WidgetPalette<D::Color>,
    template: &str,
    font: FontSize,
    align: Align,
    availability: Availability,
    value: Option<SensorValue>,
) -> Result<(), D::Error>
where
    D: DrawTarget,
{
    let rendered = template.replace("{{value}}", &value_text(availability, value));
    let font_ref: &MonoFont = match font {
        FontSize::Small => &FONT_6X10,
        FontSize::Medium => &FONT_8X13,
        FontSize::Large => &FONT_10X20,
    };
    let text_width = (rendered.chars().count() as u32 * font_ref.character_size.width
        + rendered.chars().count().saturating_sub(1) as u32 * font_ref.character_spacing)
        as i32;
    let x = match align {
        Align::Left => rect.top_left.x,
        Align::Center => rect.top_left.x + (rect.size.width as i32 - text_width).max(0) / 2,
        Align::Right => rect.top_left.x + (rect.size.width as i32 - text_width).max(0),
    };
    let y = rect.top_left.y
        + (rect.size.height as i32 - font_ref.character_size.height as i32).max(0) / 2;
    Text::with_baseline(
        &rendered,
        Point::new(x, y),
        MonoTextStyle::new(font_ref, palette.foreground),
        Baseline::Top,
    )
    .draw(target)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn render_gauge_linear<D>(
    target: &mut D,
    rect: Rectangle,
    palette: WidgetPalette<D::Color>,
    min: f64,
    max: f64,
    redline: Option<f64>,
    // Gauges freeze at the last position rather than reacting to
    // availability directly — see `gauge_fraction`'s doc comment.
    _availability: Availability,
    value: Option<SensorValue>,
) -> Result<(), D::Error>
where
    D: DrawTarget,
{
    let stroke = PrimitiveStyle::with_stroke(palette.foreground, 1);
    rect.into_styled(stroke).draw(target)?;
    let inner_width = rect.size.width.saturating_sub(2) as f64;
    if let Some(t) = gauge_fraction(value, min, max) {
        let fill = (inner_width * t) as u32;
        if fill > 0 {
            Rectangle::new(
                rect.top_left + Point::new(1, 1),
                Size::new(fill, rect.size.height.saturating_sub(2)),
            )
            .into_styled(PrimitiveStyle::with_fill(palette.foreground))
            .draw(target)?;
        }
    }
    if let Some(r) = redline {
        let t_red = ((r - min) / (max - min)).clamp(0.0, 1.0);
        let x = rect.top_left.x + 1 + (inner_width * t_red) as i32;
        // Inverted where the fill has already passed the mark so it stays
        // visible against a solid fill.
        let filled = gauge_fraction(value, min, max).is_some_and(|t| t >= t_red);
        let color = if filled {
            palette.background
        } else {
            palette.foreground
        };
        Line::new(
            Point::new(x, rect.top_left.y + 1),
            Point::new(x, rect.top_left.y + rect.size.height as i32 - 2),
        )
        .into_styled(PrimitiveStyle::with_stroke(color, 1))
        .draw(target)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn render_gauge_arc<D>(
    target: &mut D,
    rect: Rectangle,
    palette: WidgetPalette<D::Color>,
    min: f64,
    max: f64,
    redline: Option<f64>,
    style: ArcGaugeStyle,
    // Gauges freeze at the last position rather than reacting to
    // availability directly — see `gauge_fraction`'s doc comment.
    _availability: Availability,
    value: Option<SensorValue>,
) -> Result<(), D::Error>
where
    D: DrawTarget,
{
    // Half-dial: center on the middle of the zone's bottom edge, arc sweeping
    // over the top (9 o'clock -> 12 -> 3 o'clock).
    let center = Point::new(
        rect.top_left.x + rect.size.width as i32 / 2,
        rect.top_left.y + rect.size.height as i32 - 1,
    );
    // Filled hugs the zone's edges (smaller margin); Classic keeps the
    // original margin.
    let margin = match style {
        ArcGaugeStyle::Classic => 2,
        ArcGaugeStyle::Filled => 1,
    };
    let radius = (rect.size.height as i32 - margin).min(rect.size.width as i32 / 2 - margin);
    if radius < 4 {
        return Ok(());
    }
    let stroke = PrimitiveStyle::with_stroke(palette.foreground, 1);
    let diameter = (radius * 2) as u32;
    Arc::new(
        center - Point::new(radius, radius),
        diameter,
        0.0.deg(),
        180.0.deg(),
    )
    .into_styled(stroke)
    .draw(target)?;

    // Angle on screen for a range fraction t: 180° (left) at t=0 shrinking to
    // 0° (right) at t=1; y grows downward.
    let dir = |t: f64| {
        let phi = (180.0 * (1.0 - t)).to_radians();
        (phi.cos(), -phi.sin())
    };
    let fraction = gauge_fraction(value, min, max);

    if style == ArcGaugeStyle::Filled {
        if let Some(t) = fraction {
            draw_dithered_sweep(target, center, radius - 1, t, palette.dim)?;
        }
    }
    if let Some(r) = redline {
        let (dx, dy) = dir(((r - min) / (max - min)).clamp(0.0, 1.0));
        let inner = 0.8 * f64::from(radius);
        let outer = f64::from(radius);
        Line::new(
            center + Point::new((inner * dx) as i32, (inner * dy) as i32),
            center + Point::new((outer * dx) as i32, (outer * dy) as i32),
        )
        .into_styled(stroke)
        .draw(target)?;
    }
    if let Some(t) = fraction {
        let (dx, dy) = dir(t);
        let len = f64::from(radius) - 1.0;
        Line::new(
            center,
            center + Point::new((len * dx) as i32, (len * dy) as i32),
        )
        .into_styled(stroke)
        .draw(target)?;
    }
    Ok(())
}

/// Fill the part of a half-dial arc already swept from angle 180° (fraction
/// 0) down to the current fraction's angle, with a checkerboard dither — the
/// caller's chosen `dim` color, at ~50% pixel density, is how "already
/// covered but not the current value" is represented without assuming the
/// target has true intermediate shades.
fn draw_dithered_sweep<D>(
    target: &mut D,
    center: Point,
    radius: i32,
    fraction: f64,
    dim: D::Color,
) -> Result<(), D::Error>
where
    D: DrawTarget,
{
    if fraction <= 0.0 {
        return Ok(());
    }
    // A pixel at offset (dx, dy) from the center is inside the swept range
    // iff its angle (atan2(-dy, dx): up 90°, left 180°, right 0°, matching
    // the needle's `dir()` mapping) is >= the needle angle phi. Evaluated as
    // the sign of the 2D cross product needle x point in 16.16 fixed point:
    // per-pixel trig is unaffordable here — f64 math is software-emulated on
    // the ESP32 (f32-only FPU), and this loop covers tens of thousands of
    // pixels on a large zone.
    let phi = (180.0 * (1.0 - fraction)).to_radians();
    let needle_x = (phi.cos() * 65536.0) as i64;
    let needle_y = (phi.sin() * 65536.0) as i64;
    let mut pixels = Vec::new();
    for dy in -radius..=0 {
        for dx in -radius..=radius {
            let dist_sq = dx * dx + dy * dy;
            if dist_sq == 0 || dist_sq > radius * radius {
                continue;
            }
            if (dx + dy) & 1 != 0 {
                continue; // checkerboard: only every other pixel
            }
            // Cross product in y-up coordinates (screen y grows downward,
            // hence -dy): >= 0 means the pixel's angle is >= phi.
            if needle_x * i64::from(-dy) - needle_y * i64::from(dx) >= 0 {
                pixels.push(Pixel(center + Point::new(dx, dy), dim));
            }
        }
    }
    target.draw_iter(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;
    use embedded_graphics::mock_display::MockDisplay;
    use embedded_graphics::pixelcolor::{BinaryColor, Rgb565, RgbColor};

    fn mono_palette() -> WidgetPalette<BinaryColor> {
        WidgetPalette {
            foreground: BinaryColor::On,
            background: BinaryColor::Off,
            dim: BinaryColor::On,
        }
    }

    fn color_palette() -> WidgetPalette<Rgb565> {
        WidgetPalette {
            foreground: Rgb565::WHITE,
            background: Rgb565::BLACK,
            dim: Rgb565::new(8, 16, 8),
        }
    }

    #[test]
    fn value_text_is_placeholder_unless_online_with_a_value() {
        assert_eq!(
            value_text(Availability::Online, Some(SensorValue::Rpm(4200))),
            "4200"
        );
        assert_eq!(
            value_text(Availability::Stale, Some(SensorValue::Rpm(4200))),
            "--"
        );
        assert_eq!(value_text(Availability::Unavailable, None), "--");
    }

    #[test]
    fn text_validate_requires_value_placeholder() {
        let widget = WidgetConfig::Text {
            template: "Speed".to_owned(),
            font: FontSize::default(),
            align: Align::default(),
        };
        assert!(widget.validate().unwrap_err().contains("{{value}}"));
    }

    #[test]
    fn gauge_validate_requires_min_less_than_max_and_redline_in_range() {
        assert!(WidgetConfig::GaugeLinear {
            min: 5.0,
            max: 5.0,
            redline: None
        }
        .validate()
        .is_err());
        assert!(WidgetConfig::GaugeArc {
            min: 0.0,
            max: 100.0,
            redline: Some(200.0),
            style: ArcGaugeStyle::default()
        }
        .validate()
        .is_err());
        assert!(WidgetConfig::GaugeLinear {
            min: 0.0,
            max: 100.0,
            redline: Some(80.0)
        }
        .validate()
        .is_ok());
    }

    #[test]
    fn arc_style_defaults_to_classic_and_accepts_0_or_1() {
        let no_style: WidgetConfig =
            serde_json::from_str(r#"{"kind":"gauge_arc","min":0,"max":100}"#).unwrap();
        assert_eq!(
            no_style,
            WidgetConfig::GaugeArc {
                min: 0.0,
                max: 100.0,
                redline: None,
                style: ArcGaugeStyle::Classic
            }
        );

        for (style, expected) in [(0, ArcGaugeStyle::Classic), (1, ArcGaugeStyle::Filled)] {
            let json = format!(r#"{{"kind":"gauge_arc","min":0,"max":100,"style":{style}}}"#);
            let widget: WidgetConfig = serde_json::from_str(&json).unwrap();
            assert_eq!(
                widget,
                WidgetConfig::GaugeArc {
                    min: 0.0,
                    max: 100.0,
                    redline: None,
                    style: expected
                }
            );
        }
    }

    #[test]
    fn rejects_unknown_arc_style() {
        let err = serde_json::from_str::<WidgetConfig>(
            r#"{"kind":"gauge_arc","min":0,"max":100,"style":2}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("style must be 0 or 1"), "{err}");
    }

    #[test]
    fn all_widgets_render_on_a_monochrome_target() {
        let mut display = MockDisplay::<BinaryColor>::new();
        display.set_allow_overdraw(true);
        display.set_allow_out_of_bounds_drawing(true);
        let rect = Rectangle::new(Point::zero(), Size::new(60, 32));

        for widget in [
            WidgetConfig::Text {
                template: "RPM: {{value}}".to_owned(),
                font: FontSize::Small,
                align: Align::Center,
            },
            WidgetConfig::GaugeLinear {
                min: 0.0,
                max: 100.0,
                redline: Some(80.0),
            },
            WidgetConfig::GaugeArc {
                min: 0.0,
                max: 100.0,
                redline: Some(80.0),
                style: ArcGaugeStyle::Classic,
            },
            WidgetConfig::GaugeArc {
                min: 0.0,
                max: 100.0,
                redline: Some(80.0),
                style: ArcGaugeStyle::Filled,
            },
        ] {
            widget
                .render(
                    &mut display,
                    rect,
                    mono_palette(),
                    Availability::Online,
                    Some(SensorValue::Rpm(50)),
                )
                .unwrap();
        }
    }

    /// Color-genericity check: the same `render()` calls must compile and
    /// run against an `Rgb565` target with a three-color palette, not only
    /// against monochrome `BinaryColor`.
    #[test]
    fn all_widgets_render_on_a_color_target() {
        let mut display = MockDisplay::<Rgb565>::new();
        display.set_allow_overdraw(true);
        display.set_allow_out_of_bounds_drawing(true);
        let rect = Rectangle::new(Point::zero(), Size::new(60, 32));

        for widget in [
            WidgetConfig::Text {
                template: "Speed: {{value}}".to_owned(),
                font: FontSize::Medium,
                align: Align::Right,
            },
            WidgetConfig::GaugeLinear {
                min: 0.0,
                max: 100.0,
                redline: None,
            },
            WidgetConfig::GaugeArc {
                min: 0.0,
                max: 100.0,
                redline: Some(80.0),
                style: ArcGaugeStyle::Filled,
            },
        ] {
            widget
                .render(
                    &mut display,
                    rect,
                    color_palette(),
                    Availability::Online,
                    Some(SensorValue::Speed(42.0)),
                )
                .unwrap();
        }
    }

    /// Renders `widget` under the given availability/value onto a fresh
    /// `MockDisplay` and returns it, for pixel-level before/after comparison
    /// — the actual verification the availability contract needs, rather
    /// than just confirming a render call doesn't error.
    fn render_to_mock(
        widget: &WidgetConfig,
        rect: Rectangle,
        availability: Availability,
        value: Option<SensorValue>,
    ) -> MockDisplay<BinaryColor> {
        let mut display = MockDisplay::new();
        display.set_allow_overdraw(true);
        display.set_allow_out_of_bounds_drawing(true);
        widget
            .render(&mut display, rect, mono_palette(), availability, value)
            .unwrap();
        display
    }

    #[test]
    fn text_shows_a_placeholder_for_stale_and_unavailable_instead_of_freezing() {
        let widget = WidgetConfig::Text {
            template: "{{value}}".to_owned(),
            font: FontSize::Small,
            align: Align::Left,
        };
        let rect = Rectangle::new(Point::zero(), Size::new(60, 16));

        let online = render_to_mock(
            &widget,
            rect,
            Availability::Online,
            Some(SensorValue::Rpm(4200)),
        );
        let stale = render_to_mock(
            &widget,
            rect,
            Availability::Stale,
            Some(SensorValue::Rpm(4200)),
        );
        let unavailable = render_to_mock(&widget, rect, Availability::Unavailable, None);

        assert_ne!(
            online, stale,
            "text must show a placeholder, not the frozen last value, once stale"
        );
        assert_eq!(
            stale, unavailable,
            "stale and unavailable render the same '--' placeholder for text"
        );
    }

    #[test]
    fn gauge_linear_freezes_at_the_last_position_when_stale_and_hides_when_unavailable() {
        let widget = WidgetConfig::GaugeLinear {
            min: 0.0,
            max: 100.0,
            redline: None,
        };
        let rect = Rectangle::new(Point::zero(), Size::new(40, 16));

        let online = render_to_mock(
            &widget,
            rect,
            Availability::Online,
            Some(SensorValue::Rpm(80)),
        );
        let stale = render_to_mock(
            &widget,
            rect,
            Availability::Stale,
            Some(SensorValue::Rpm(80)),
        );
        let unavailable = render_to_mock(&widget, rect, Availability::Unavailable, None);

        assert_eq!(
            online, stale,
            "a stale reading freezes the fill at its last position, unchanged"
        );
        assert_ne!(
            online, unavailable,
            "an unavailable reading (no value) must not show a frozen fill"
        );
    }

    #[test]
    fn gauge_arc_freezes_at_the_last_position_when_stale_and_hides_when_unavailable() {
        for style in [ArcGaugeStyle::Classic, ArcGaugeStyle::Filled] {
            let widget = WidgetConfig::GaugeArc {
                min: 0.0,
                max: 100.0,
                redline: Some(80.0),
                style,
            };
            let rect = Rectangle::new(Point::zero(), Size::new(60, 32));

            let online = render_to_mock(
                &widget,
                rect,
                Availability::Online,
                Some(SensorValue::Rpm(50)),
            );
            let stale = render_to_mock(
                &widget,
                rect,
                Availability::Stale,
                Some(SensorValue::Rpm(50)),
            );
            let unavailable = render_to_mock(&widget, rect, Availability::Unavailable, None);

            assert_eq!(
                online, stale,
                "a stale reading freezes the needle at its last position ({style:?})"
            );
            assert_ne!(
                online, unavailable,
                "an unavailable reading (no value) must not show a frozen needle ({style:?})"
            );
        }
    }
}
