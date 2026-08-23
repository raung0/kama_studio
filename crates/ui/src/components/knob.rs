use std::{
    fmt::Display,
    sync::Arc,
    time::{Duration, Instant},
};

use crate::{BuildCtx, Color, FormatKey, Rect, Size};

use super::{ease, Style};

const DOUBLE_CLICK: Duration = Duration::from_millis(360);
const PRESENTATION_SPEED: f32 = 24.0;

type Formatter = Arc<dyn Fn(f64, usize) -> String + Send + Sync>;

#[derive(Clone, Copy, Debug)]
enum KnobDrag {
    Linear { start_y: f32, start_value: f64 },
    Circular { center: [f32; 2], last_angle: f64 },
}


pub struct Knob {
    value: f64,
    shown: f32,
    minimum: f64,
    maximum: f64,
    default: f64,
    step: f64,
    precision: usize,
    units_per_pixel: f64,
    formatter: Formatter,
    circular: bool,
    drag: Option<KnobDrag>,
    last_click: Option<Instant>,
}

impl std::fmt::Debug for Knob {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Knob")
            .field("value", &self.value)
            .field("minimum", &self.minimum)
            .field("maximum", &self.maximum)
            .field("default", &self.default)
            .finish_non_exhaustive()
    }
}

impl Knob {
    pub fn new(minimum: f64, maximum: f64, default: f64) -> Self {
        let (minimum, maximum) = ordered_bounds(minimum, maximum);
        let value = default.clamp(minimum, maximum);
        Self {
            value,
            shown: normalized(value, minimum, maximum),
            minimum,
            maximum,
            default: value,
            step: 0.0,
            precision: 2,
            units_per_pixel: ((maximum - minimum) / 160.0).max(f64::EPSILON),
            formatter: Arc::new(|value, precision| format!("{value:.precision$}")),
            circular: false,
            drag: None,
            last_click: None,
        }
    }

    pub fn step(mut self, step: f64) -> Self {
        self.step = step.max(0.0);
        self
    }
    pub fn precision(mut self, precision: usize) -> Self {
        self.precision = precision;
        self
    }
    pub fn sensitivity(mut self, units_per_pixel: f64) -> Self {
        self.units_per_pixel = units_per_pixel.max(f64::EPSILON);
        self
    }
    pub fn circular(mut self) -> Self {
        self.circular = true;
        self
    }
    pub fn formatter(
        mut self,
        formatter: impl Fn(f64, usize) -> String + Send + Sync + 'static,
    ) -> Self {
        self.formatter = Arc::new(formatter);
        self
    }

    pub fn value(&self) -> f64 {
        self.value
    }
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }
    pub fn set_value(&mut self, value: f64) {
        self.value = self.quantize(value);
    }

    pub fn tick(&mut self, dt: f32) {
        ease(
            &mut self.shown,
            normalized(self.value, self.minimum, self.maximum),
            PRESENTATION_SPEED,
            dt,
        );
    }

    pub fn is_animating(&self) -> bool {
        (self.shown - normalized(self.value, self.minimum, self.maximum)).abs() > 0.001
    }

    
    pub fn pointer_pressed(&mut self, rect: Rect, point: [f32; 2]) -> Option<f64> {
        if !rect.contains(point) {
            return None;
        }
        let now = Instant::now();
        let double = self
            .last_click
            .is_some_and(|last| now.saturating_duration_since(last) <= DOUBLE_CLICK);
        if double {
            self.last_click = None;
            self.drag = None;
            self.value = self.default;
            return Some(self.value);
        }
        self.last_click = Some(now);
        self.drag = Some(if self.circular {
            let center = [rect.x + rect.width * 0.5, rect.y + rect.height * 0.5];
            KnobDrag::Circular {
                center,
                last_angle: pointer_angle(center, point),
            }
        } else {
            KnobDrag::Linear {
                start_y: point[1],
                start_value: self.value,
            }
        });
        None
    }

    
    
    pub fn pointer_moved(&mut self, point: [f32; 2]) -> Option<f64> {
        let next = match self.drag? {
            KnobDrag::Linear {
                start_y,
                start_value,
            } => self.quantize(start_value + (start_y - point[1]) as f64 * self.units_per_pixel),
            KnobDrag::Circular { center, last_angle } => {
                let angle = pointer_angle(center, point);
                let delta = wrap_radians(angle - last_angle).to_degrees();
                self.drag = Some(KnobDrag::Circular {
                    center,
                    last_angle: angle,
                });
                self.quantize(self.value + delta)
            }
        };
        if (next - self.value).abs() <= f64::EPSILON {
            return None;
        }
        self.value = next;
        Some(next)
    }

    pub fn pointer_released(&mut self) -> bool {
        self.drag.take().is_some()
    }

    pub fn build(&self, ctx: &mut BuildCtx, id: impl Display, rect: Rect, style: Style) {
        let ui_scale = style.ui_scale.clamp(0.25, 4.0);
        let dial_size = rect.height.min(rect.width * 0.58).max(12.0 * ui_scale);
        let (dial, measured) = crate::measure_layout(rect, |ctx| {
            ctx.new()
                .overlay()
                .centered()
                .width(Size::Pixels(dial_size))
                .height(Size::Pixels(dial_size))
                .build()
        });
        let dial = measured.rect(dial).expect("knob dial layout");
        let center = [dial.x + dial.width * 0.5, dial.y + dial.height * 0.5];
        let angle = if self.circular {
            ((self.value.rem_euclid(360.0) - 90.0).to_radians()) as f32
        } else {
            let start = -std::f32::consts::PI * 1.25;
            start + self.shown.clamp(0.0, 1.0) * std::f32::consts::PI * 1.5
        };
        let radius = dial_size * 0.34;
        let indicator_size = 2.0 * ui_scale;
        let indicator = Rect::new(
            center[0] + angle.cos() * radius - indicator_size * 0.5,
            center[1] + angle.sin() * radius - indicator_size * 0.5,
            indicator_size,
            indicator_size,
        );
        crate::ui!(ctx, {
            Rect(FormatKey::new(format_args!("knob-hit {id}")), rect) {
                fill: Color::TRANSPARENT; border_radius: 4.0 * ui_scale; interactive;
            }
            Rect(FormatKey::new(format_args!("knob-dial {id}")), dial) {
                fill: style.control; border: 1;
                border_color: if self.drag.is_some() { style.accent } else { style.border };
                border_radius: dial_size * 0.5; reveal;
            }
            Rect(FormatKey::new(format_args!("knob-indicator {id}")), indicator) {
                fill: style.accent; border_radius: ui_scale;
            }
            Rect(FormatKey::new(format_args!("knob-value {id}")), dial) {
                font_size: (dial_size * 0.27).clamp(4.0, 18.0); text_color: style.text; text_centered;
                text: (self.formatter)(self.value, self.precision);
            }
        });
    }

    fn quantize(&self, value: f64) -> f64 {
        let value = value.clamp(self.minimum, self.maximum);
        if self.step > 0.0 {
            (self.minimum + ((value - self.minimum) / self.step).round() * self.step)
                .clamp(self.minimum, self.maximum)
        } else {
            value
        }
    }
}

fn pointer_angle(center: [f32; 2], point: [f32; 2]) -> f64 {
    f64::from(point[1] - center[1]).atan2(f64::from(point[0] - center[0]))
}

fn wrap_radians(mut value: f64) -> f64 {
    while value > std::f64::consts::PI {
        value -= std::f64::consts::TAU;
    }
    while value < -std::f64::consts::PI {
        value += std::f64::consts::TAU;
    }
    value
}

fn ordered_bounds(a: f64, b: f64) -> (f64, f64) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}
fn normalized(value: f64, minimum: f64, maximum: f64) -> f32 {
    if maximum <= minimum {
        0.0
    } else {
        ((value - minimum) / (maximum - minimum)).clamp(0.0, 1.0) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::Knob;
    use crate::Rect;

    #[test]
    fn circular_drag_accumulates_across_angle_wrap() {
        let mut knob = Knob::new(-3600.0, 3600.0, 350.0).step(0.1).circular();
        let rect = Rect::new(0.0, 0.0, 100.0, 100.0);
        
        
        assert_eq!(knob.pointer_pressed(rect, [0.1, 51.0]), None);
        let before = knob.value();
        let after = knob.pointer_moved([0.1, 49.0]).unwrap();
        assert!(after > before);
        assert!(after - before < 10.0);
    }

    #[test]
    fn upward_drag_changes_model_immediately_and_quantizes() {
        let mut knob = Knob::new(0.0, 100.0, 50.0).step(1.0).sensitivity(1.0);
        let rect = Rect::new(0.0, 0.0, 40.0, 30.0);
        assert_eq!(knob.pointer_pressed(rect, [20.0, 20.0]), None);
        assert_eq!(knob.pointer_moved([20.0, 10.0]), Some(60.0));
        assert_eq!(knob.value(), 60.0);
    }
}
