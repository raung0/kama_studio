use std::hash::Hash;

use crate::{
    components::{ComboBox, Knob, NumberInput, Style, VerticalSlider},
    BuildCtx, IconId, Rect,
};
use winit::keyboard::ModifiersState;

pub struct ComboControl {
    pub combo: crate::components::ComboBox,
    pub option_count: usize,
}

pub type NumberInputSpec<'a> = (Rect, f64, ((f64, f64), f64, usize, &'a str));

impl AnimatedControl for ComboControl {
    fn tick(&mut self, dt: f32) {
        self.combo.tick(dt);
    }
    fn is_animating(&self) -> bool {
        self.combo.is_animating()
    }
}

impl AnimatedControl for crate::components::NumberInput {
    fn tick(&mut self, dt: f32) {
        self.tick(dt);
    }
    fn is_animating(&self) -> bool {
        self.is_animating()
    }
}
impl DragControl for crate::components::NumberInput {
    fn is_dragging(&self) -> bool {
        self.is_dragging()
    }
    fn pointer_released(&mut self) -> bool {
        self.pointer_released()
    }
}
impl AnimatedControl for crate::components::Knob {
    fn tick(&mut self, dt: f32) {
        self.tick(dt);
    }
    fn is_animating(&self) -> bool {
        self.is_animating()
    }
}
impl DragControl for crate::components::Knob {
    fn is_dragging(&self) -> bool {
        self.is_dragging()
    }
    fn pointer_released(&mut self) -> bool {
        self.pointer_released()
    }
}
impl AnimatedControl for crate::components::VerticalSlider {
    fn tick(&mut self, dt: f32) {
        self.tick(dt);
    }
    fn is_animating(&self) -> bool {
        self.is_animating()
    }
}
impl DragControl for crate::components::VerticalSlider {
    fn is_dragging(&self) -> bool {
        self.is_dragging()
    }
    fn pointer_released(&mut self) -> bool {
        self.pointer_released()
    }
}

pub trait AnimatedControl {
    fn tick(&mut self, dt: f32);
    fn is_animating(&self) -> bool;
}

pub trait DragControl: AnimatedControl {
    fn is_dragging(&self) -> bool;
    fn pointer_released(&mut self) -> bool;
}

pub struct ControlRegistry<K, C> {
    slots: std::collections::HashMap<K, (C, Option<Rect>)>,
}

impl<K, C> Default for ControlRegistry<K, C> {
    fn default() -> Self {
        Self {
            slots: Default::default(),
        }
    }
}

impl<K: Eq + Hash, C> ControlRegistry<K, C> {
    pub fn clear_layout(&mut self) {
        for (_, rect) in self.slots.values_mut() {
            *rect = None;
        }
    }

    pub fn prepare(&mut self, target: K, rect: Rect, create: impl FnOnce() -> C) -> &mut C {
        let (control, stored_rect) = self.slots.entry(target).or_insert_with(|| (create(), None));
        *stored_rect = Some(rect);
        control
    }

    pub fn get(&self, target: &K) -> Option<&C> {
        self.slots.get(target).map(|(control, _)| control)
    }

    pub fn get_mut(&mut self, target: &K) -> Option<&mut C> {
        self.slots.get_mut(target).map(|(control, _)| control)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&K, &C, Option<Rect>)> {
        self.slots
            .iter()
            .map(|(key, (control, rect))| (key, control, *rect))
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&K, &mut C, Option<Rect>)> {
        self.slots
            .iter_mut()
            .map(|(key, (control, rect))| (key, control, *rect))
    }
}

impl<K: Clone + Eq + Hash, C> ControlRegistry<K, C> {
    pub fn target_at(&self, panel: Rect, point: [f32; 2]) -> Option<K> {
        self.iter().find_map(|(target, _, local)| {
            let local = local?;
            Rect::new(
                local.x + panel.x,
                local.y + panel.y,
                local.width,
                local.height,
            )
            .contains(point)
            .then(|| target.clone())
        })
    }

    pub fn hit_mut(&mut self, panel: Rect, point: [f32; 2]) -> Option<(K, Rect, &mut C)> {
        for (target, control, local) in self.iter_mut() {
            let local = local?;
            let rect = Rect::new(
                local.x + panel.x,
                local.y + panel.y,
                local.width,
                local.height,
            );
            if rect.contains(point) {
                return Some((target.clone(), rect, control));
            }
        }
        None
    }
}

impl<K: Eq + Hash, C: AnimatedControl> ControlRegistry<K, C> {
    pub fn tick(&mut self, dt: f32) {
        for (_, control, _) in self.iter_mut() {
            control.tick(dt);
        }
    }

    pub fn is_animating(&self) -> bool {
        self.iter().any(|(_, control, _)| control.is_animating())
    }
}

impl<K: Eq + Hash, C: DragControl> ControlRegistry<K, C> {
    pub fn is_dragging(&self) -> bool {
        self.iter().any(|(_, control, _)| control.is_dragging())
    }

    pub fn pointer_released(&mut self) -> bool {
        self.iter_mut().fold(false, |handled, (_, control, _)| {
            control.pointer_released() || handled
        })
    }
}

fn absolute_rect(local: Rect, panel: Rect) -> Rect {
    Rect::new(
        local.x + panel.x,
        local.y + panel.y,
        local.width,
        local.height,
    )
}

pub trait NumberControlsExt<K> {
    fn build(
        &mut self,
        ctx: &mut BuildCtx,
        id: impl std::fmt::Display,
        target: K,
        input: NumberInputSpec<'_>,
        style: Style,
    );
    fn blur(&mut self);
    fn pointer_pressed(
        &mut self,
        panel: Rect,
        point: [f32; 2],
        modifiers: ModifiersState,
    ) -> Option<(K, Option<f64>)>;
    fn pointer_moved(&mut self, point: [f32; 2]) -> Option<(K, f64)>;
    fn pointer_dragged(&mut self, delta_y: f32) -> Option<(K, f64)>;
    fn editing_target(&self) -> Option<K>;
    fn edit(
        &mut self,
        target: &K,
        edit: impl FnOnce(&mut NumberInput) -> Option<f64>,
    ) -> Option<f64>;
    fn caret_rect(&self, panel: Rect) -> Option<Rect>;
}

impl<K: Clone + Eq + Hash> NumberControlsExt<K> for ControlRegistry<K, NumberInput> {
    fn build(
        &mut self,
        ctx: &mut BuildCtx,
        id: impl std::fmt::Display,
        target: K,
        input: NumberInputSpec<'_>,
        style: Style,
    ) {
        let (rect, value, ((min, max), sensitivity, precision, suffix)) = input;
        let input = self.prepare(target, rect, || NumberInput::new(value));
        input.set_bounds(min, max);
        input.set_sensitivity(sensitivity);
        input.set_precision(precision);
        input.set_value(value);
        input.build(ctx, id, rect, suffix, style);
    }

    fn blur(&mut self) {
        self.iter_mut()
            .for_each(|(_, control, _)| control.set_focused(false));
    }

    fn pointer_pressed(
        &mut self,
        panel: Rect,
        point: [f32; 2],
        modifiers: ModifiersState,
    ) -> Option<(K, Option<f64>)> {
        let (target, rect) = self.iter().find_map(|(target, _, local)| {
            let rect = absolute_rect(local?, panel);
            rect.contains(point).then(|| (target.clone(), rect))
        })?;
        for (candidate, control, _) in self.iter_mut() {
            if candidate != &target {
                control.set_focused(false);
            }
        }
        let input = self.get_mut(&target)?;
        Some((target, input.pointer_pressed(rect, point, modifiers)))
    }

    fn pointer_moved(&mut self, point: [f32; 2]) -> Option<(K, f64)> {
        self.iter_mut().find_map(|(target, control, _)| {
            control
                .pointer_moved(point)
                .map(|value| (target.clone(), value))
        })
    }

    fn pointer_dragged(&mut self, delta_y: f32) -> Option<(K, f64)> {
        self.iter_mut().find_map(|(target, control, _)| {
            control
                .pointer_dragged(delta_y)
                .map(|value| (target.clone(), value))
        })
    }

    fn editing_target(&self) -> Option<K> {
        self.iter()
            .find_map(|(target, control, _)| control.is_editing().then(|| target.clone()))
    }

    fn edit(
        &mut self,
        target: &K,
        edit: impl FnOnce(&mut NumberInput) -> Option<f64>,
    ) -> Option<f64> {
        self.get_mut(target).and_then(edit)
    }

    fn caret_rect(&self, panel: Rect) -> Option<Rect> {
        self.iter()
            .find_map(|(_, control, local)| control.caret_rect(absolute_rect(local?, panel)))
    }
}

pub trait EnumControlsExt<K> {
    #[allow(clippy::too_many_arguments)]
    fn build<T: AsRef<str>>(
        &mut self,
        ctx: &mut BuildCtx,
        id: impl std::fmt::Display,
        target: K,
        selection: (Rect, usize),
        options: &[T],
        chrome: (IconId, Style),
        window_bounds: Rect,
    );
    fn close(&mut self);
    fn popup_contains(&self, panel: Rect, point: [f32; 2]) -> bool;
    fn scroll_popup(&self, panel: Rect, point: [f32; 2], delta: [f32; 2]) -> bool;
    fn select_option(&mut self, panel: Rect, point: [f32; 2]) -> Option<(K, usize)>;
    fn toggle_at(&mut self, panel: Rect, point: [f32; 2]) -> Option<K>;
}

impl<K: Clone + Eq + Hash> EnumControlsExt<K> for ControlRegistry<K, ComboControl> {
    #[allow(clippy::too_many_arguments)]
    fn build<T: AsRef<str>>(
        &mut self,
        ctx: &mut BuildCtx,
        id: impl std::fmt::Display,
        target: K,
        selection: (Rect, usize),
        options: &[T],
        chrome: (IconId, Style),
        window_bounds: Rect,
    ) {
        let (rect, selected) = selection;
        let (chevron, style) = chrome;
        let input = self.prepare(target, rect, || ComboControl {
            combo: ComboBox::new(selected),
            option_count: options.len(),
        });
        input.option_count = options.len();
        if !input.combo.is_open() {
            input.combo.set_selected(selected);
        }
        let options = options.iter().map(AsRef::as_ref).collect::<Vec<_>>();
        input
            .combo
            .build_in(ctx, id, rect, &options, chevron, window_bounds, style);
    }

    fn close(&mut self) {
        self.iter_mut()
            .for_each(|(_, control, _)| control.combo.close());
    }

    fn popup_contains(&self, panel: Rect, point: [f32; 2]) -> bool {
        self.iter().any(|(_, control, local)| {
            let Some(local) = local else { return false };
            control
                .combo
                .popup_contains(absolute_rect(local, panel), point, control.option_count)
        })
    }

    fn scroll_popup(&self, panel: Rect, point: [f32; 2], delta: [f32; 2]) -> bool {
        self.iter().any(|(_, control, local)| {
            let Some(local) = local else { return false };
            control.combo.scroll(
                absolute_rect(local, panel),
                point,
                delta,
                control.option_count,
            )
        })
    }

    fn select_option(&mut self, panel: Rect, point: [f32; 2]) -> Option<(K, usize)> {
        for (target, control, local) in self.iter_mut() {
            let Some(local) = local else { continue };
            let Some(index) =
                control
                    .combo
                    .option_at(absolute_rect(local, panel), point, control.option_count)
            else {
                continue;
            };
            control.combo.select(index, true);
            return Some((target.clone(), index));
        }
        None
    }

    fn toggle_at(&mut self, panel: Rect, point: [f32; 2]) -> Option<K> {
        let target = self.iter().find_map(|(target, _, local)| {
            absolute_rect(local?, panel)
                .contains(point)
                .then(|| target.clone())
        })?;
        for (candidate, control, _) in self.iter_mut() {
            if candidate == &target {
                control.combo.toggle();
            } else {
                control.combo.close();
            }
        }
        Some(target)
    }
}

pub trait KnobControlsExt<K> {
    fn build(
        &mut self,
        ctx: &mut BuildCtx,
        id: impl std::fmt::Display,
        target: K,
        input: (Rect, f64),
        create: impl FnOnce() -> Knob,
        style: Style,
    );
    fn pointer_pressed(&mut self, panel: Rect, point: [f32; 2]) -> Option<(K, Option<f64>)>;
    fn pointer_moved(&mut self, point: [f32; 2]) -> Option<(K, f64)>;
}

impl<K: Clone + Eq + Hash> KnobControlsExt<K> for ControlRegistry<K, Knob> {
    fn build(
        &mut self,
        ctx: &mut BuildCtx,
        id: impl std::fmt::Display,
        target: K,
        input: (Rect, f64),
        create: impl FnOnce() -> Knob,
        style: Style,
    ) {
        let (rect, value) = input;
        let knob = self.prepare(target, rect, create);
        if !knob.is_dragging() {
            knob.set_value(value);
        }
        knob.build(ctx, id, rect, style);
    }

    fn pointer_pressed(&mut self, panel: Rect, point: [f32; 2]) -> Option<(K, Option<f64>)> {
        let (target, rect, knob) = self.hit_mut(panel, point)?;
        Some((target, knob.pointer_pressed(rect, point)))
    }

    fn pointer_moved(&mut self, point: [f32; 2]) -> Option<(K, f64)> {
        self.iter_mut().find_map(|(target, control, _)| {
            control
                .pointer_moved(point)
                .map(|value| (target.clone(), value))
        })
    }
}

pub trait SliderControlsExt<K> {
    fn build(
        &mut self,
        ctx: &mut BuildCtx,
        id: impl std::fmt::Display,
        target: K,
        rect: Rect,
        value: f32,
        style: Style,
    );
    fn pointer_pressed(&mut self, panel: Rect, point: [f32; 2]) -> Option<(K, f32)>;
    fn pointer_moved(&mut self, point: [f32; 2]) -> Option<(K, f32)>;
}

impl<K: Copy + Eq + Hash> SliderControlsExt<K> for ControlRegistry<K, VerticalSlider> {
    fn build(
        &mut self,
        ctx: &mut BuildCtx,
        id: impl std::fmt::Display,
        target: K,
        rect: Rect,
        value: f32,
        style: Style,
    ) {
        let slider = self.prepare(target, rect, || VerticalSlider::new(value));
        if !slider.is_dragging() {
            slider.set_value(value);
        }
        slider.build(ctx, id, rect, style);
    }

    fn pointer_pressed(&mut self, panel: Rect, point: [f32; 2]) -> Option<(K, f32)> {
        let (target, rect, slider) = self.hit_mut(panel, point)?;
        slider
            .pointer_pressed(rect, point)
            .then(|| (target, slider.value()))
    }

    fn pointer_moved(&mut self, point: [f32; 2]) -> Option<(K, f32)> {
        self.iter_mut().find_map(|(target, control, _)| {
            control
                .pointer_moved(point)
                .then(|| (*target, control.value()))
        })
    }
}
