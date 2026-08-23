use std::{
    fmt::Display,
    time::{Duration, Instant},
};

use winit::{
    event::{ElementState, Ime, KeyEvent},
    keyboard::{Key, ModifiersState, NamedKey},
};

use crate::{BuildCtx, Rect};

use super::{SpinInput, Style, TextEdit};

const DOUBLE_CLICK: Duration = Duration::from_millis(360);


pub struct NumberInput {
    value: f64,
    minimum: f64,
    maximum: f64,
    units_per_pixel: f64,
    precision: usize,
    drag: Option<(f32, f64)>,
    last_click: Option<Instant>,
    edit: TextEdit,
    editing: bool,
}

impl NumberInput {
    pub fn new(value: f64) -> Self {
        Self {
            value,
            minimum: f64::NEG_INFINITY,
            maximum: f64::INFINITY,
            units_per_pixel: 1.0,
            precision: 2,
            drag: None,
            last_click: None,
            edit: TextEdit::single_line(format!("{value:.2}")),
            editing: false,
        }
    }

    pub fn bounds(mut self, minimum: f64, maximum: f64) -> Self {
        self.minimum = minimum;
        self.maximum = maximum;
        self.value = self.value.clamp(minimum, maximum);
        self
    }

    pub fn sensitivity(mut self, units_per_pixel: f64) -> Self {
        self.units_per_pixel = units_per_pixel.max(f64::EPSILON);
        self
    }

    pub fn precision(mut self, precision: usize) -> Self {
        self.precision = precision;
        self
    }

    pub fn value(&self) -> f64 {
        self.value
    }

    pub fn is_editing(&self) -> bool {
        self.editing
    }

    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    pub fn is_animating(&self) -> bool {
        self.edit.is_animating()
    }

    pub fn tick(&mut self, dt: f32) {
        self.edit.tick(dt);
    }

    pub fn set_value(&mut self, value: f64) {
        if self.drag.is_some() || self.editing {
            return;
        }
        self.value = value.clamp(self.minimum, self.maximum);
    }

    pub fn set_bounds(&mut self, minimum: f64, maximum: f64) {
        self.minimum = minimum;
        self.maximum = maximum;
        self.value = self.value.clamp(minimum, maximum);
    }

    pub fn set_sensitivity(&mut self, units_per_pixel: f64) {
        self.units_per_pixel = units_per_pixel.max(f64::EPSILON);
    }

    pub fn set_precision(&mut self, precision: usize) {
        self.precision = precision;
    }

    pub fn build(
        &mut self,
        ctx: &mut BuildCtx,
        id: impl Display,
        rect: Rect,
        suffix: &str,
        style: Style,
    ) {
        if self.editing {
            self.edit.build(ctx, id, rect, "Number", style);
        } else {
            let value = format!("{:.*}{}", self.precision, self.value, suffix);
            SpinInput::build(ctx, id, rect, &value, style);
        }
    }

    
    pub fn pointer_pressed(
        &mut self,
        rect: Rect,
        point: [f32; 2],
        modifiers: ModifiersState,
    ) -> Option<f64> {
        if self.editing {
            self.edit.pointer_pressed(rect, point, modifiers);
            return None;
        }
        if !rect.contains(point) {
            return None;
        }
        let now = Instant::now();
        let double = self
            .last_click
            .is_some_and(|last| now.saturating_duration_since(last) <= DOUBLE_CLICK);
        self.last_click = Some(now);
        if double {
            self.drag = None;
            self.editing = true;
            self.edit
                .reset(format!("{:.*}", self.precision + 2, self.value));
            self.edit.set_focused(true);
            self.edit.pointer_pressed(rect, point, modifiers);
        } else {
            self.drag = Some((point[1], self.value));
        }
        None
    }

    pub fn pointer_moved(&mut self, point: [f32; 2]) -> Option<f64> {
        if self.editing {
            self.edit.pointer_moved(point);
            return None;
        }
        let (start_y, start_value) = self.drag?;
        let next = (start_value + (start_y - point[1]) as f64 * self.units_per_pixel)
            .clamp(self.minimum, self.maximum);
        if (next - self.value).abs() > f64::EPSILON {
            self.value = next;
            return Some(next);
        }
        None
    }

    
    
    pub fn pointer_dragged(&mut self, delta_y: f32) -> Option<f64> {
        if self.editing || self.drag.is_none() {
            return None;
        }
        let next = (self.value - delta_y as f64 * self.units_per_pixel)
            .clamp(self.minimum, self.maximum);
        if (next - self.value).abs() > f64::EPSILON {
            self.value = next;
            
            
            if let Some((start_y, start_value)) = &mut self.drag {
                *start_y += delta_y;
                *start_value = next;
            }
            return Some(next);
        }
        None
    }

    pub fn pointer_released(&mut self) -> bool {
        let dragged = self.drag.take().is_some();
        dragged || (self.editing && self.edit.pointer_released())
    }

    pub fn handle_key(&mut self, event: &KeyEvent, modifiers: ModifiersState) -> Option<f64> {
        if !self.editing {
            return None;
        }
        if event.state == ElementState::Pressed {
            match &event.logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.editing = false;
                    self.edit.set_focused(false);
                    return None;
                }
                Key::Named(NamedKey::Enter) => {
                    let changed = self.commit_text();
                    self.editing = false;
                    self.edit.set_focused(false);
                    return changed;
                }
                _ => {}
            }
        }
        let previous = self.edit.text().to_string();
        let response = self.edit.handle_key(event, modifiers);
        if response.changed && !numeric_edit_text_is_valid(self.edit.text()) {
            self.edit.reset(previous);
            self.edit.set_focused(true);
            return None;
        }
        response.changed.then(|| self.preview_text()).flatten()
    }

    pub fn handle_ime(&mut self, event: &Ime) -> Option<f64> {
        if !self.editing {
            return None;
        }
        let previous = self.edit.text().to_string();
        let response = self.edit.handle_ime(event);
        if response.changed && !numeric_edit_text_is_valid(self.edit.text()) {
            self.edit.reset(previous);
            self.edit.set_focused(true);
            return None;
        }
        response.changed.then(|| self.preview_text()).flatten()
    }

    pub fn caret_rect(&self, rect: Rect) -> Option<Rect> {
        (self.editing && self.edit.is_focused()).then(|| self.edit.caret_rect(rect))
    }

    pub fn set_focused(&mut self, focused: bool) {
        if !focused {
            self.drag = None;
            self.editing = false;
            self.edit.set_focused(false);
        }
    }

    fn preview_text(&mut self) -> Option<f64> {
        let value = self
            .edit
            .text()
            .trim()
            .parse::<f64>()
            .ok()?
            .clamp(self.minimum, self.maximum);
        self.value = value;
        Some(value)
    }

    fn commit_text(&mut self) -> Option<f64> {
        let value = self.preview_text()?;
        self.edit.reset(format!("{:.*}", self.precision + 2, value));
        Some(value)
    }
}

fn numeric_edit_text_is_valid(text: &str) -> bool {
    if text.is_empty() {
        return true;
    }
    let bytes = text.as_bytes();
    let mut index = 0usize;
    if matches!(bytes.first(), Some(b'+' | b'-')) {
        index += 1;
    }

    let mut mantissa_digits = 0usize;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
        mantissa_digits += 1;
    }
    if index < bytes.len() && bytes[index] == b'.' {
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
            mantissa_digits += 1;
        }
    }
    if index == bytes.len() {
        return true;
    }
    if mantissa_digits == 0 || !matches!(bytes[index], b'e' | b'E') {
        return false;
    }
    index += 1;
    if index < bytes.len() && matches!(bytes[index], b'+' | b'-') {
        index += 1;
    }
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
    index == bytes.len()
}
