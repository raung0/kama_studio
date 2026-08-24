use super::*;

pub(super) fn project_background(
    project: &Project,
    composition: CompositionId,
) -> ProjectBackground {
    project
        .composition(composition)
        .map(|composition| composition.settings.background)
        .unwrap_or_default()
}

pub(super) fn project_option_rows(rect: Rect) -> [Rect; 5] {
    let slots = kama_ui::layout::column(
        rect,
        &[
            kama_ui::layout::Item::height(PANEL_HEADER_H),
            kama_ui::layout::Item::height(PANEL_GAP),
            kama_ui::layout::Item::height(ROW_H),
            kama_ui::layout::Item::height(ROW_H),
            kama_ui::layout::Item::height(ROW_H),
            kama_ui::layout::Item::height(PANEL_GAP),
            kama_ui::layout::Item::height(ROW_H),
            kama_ui::layout::Item::height(ROW_H),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
        None,
    );
    [slots[2], slots[3], slots[4], slots[6], slots[7]]
}

pub(super) fn project_option_control_rect(rect: Rect, y: f32) -> Rect {
    let row = project_row_hit(rect, y);
    kama_ui::layout::row(
        row,
        &[
            kama_ui::layout::Item::fill_portion(0.38),
            kama_ui::layout::Item::new(
                Size::FillPortion(0.62),
                Size::Pixels((row.height - 4.0).max(1.0)),
            ),
            kama_ui::layout::Item::width(5.0),
        ],
        0.0,
        0.0,
        kama_ui::Align::Center,
    )[1]
}

pub(super) fn project_option_y(index: usize) -> f32 {
    project_option_rows(Rect::new(0.0, 0.0, 1.0, 256.0))[index].y
}

pub(super) fn project_resolution_y() -> f32 {
    project_option_y(0)
}

pub(super) fn project_preset_y() -> f32 {
    project_option_y(1)
}

pub(super) fn project_frame_rate_y() -> f32 {
    project_option_y(2)
}

pub(super) fn project_resolution_rects(rect: Rect, y: f32) -> ([Rect; 2], Rect) {
    let control = project_option_control_rect(rect, y);
    let parts = kama_ui::layout::row(
        control,
        &[
            kama_ui::layout::Item::fill(),
            kama_ui::layout::Item::width(16.0),
            kama_ui::layout::Item::fill(),
        ],
        0.0,
        0.0,
        kama_ui::Align::Center,
    );
    ([parts[0], parts[2]], parts[1])
}

pub(super) fn project_background_mode_rects(row: Rect) -> [Rect; 2] {
    let parts = kama_ui::layout::row(
        row,
        &[
            kama_ui::layout::Item::fill_portion(0.38),
            kama_ui::layout::Item::new(
                Size::FillPortion(0.31),
                Size::Pixels((row.height - 4.0).max(1.0)),
            ),
            kama_ui::layout::Item::width(4.0),
            kama_ui::layout::Item::new(
                Size::FillPortion(0.31),
                Size::Pixels((row.height - 4.0).max(1.0)),
            ),
            kama_ui::layout::Item::width(5.0),
        ],
        0.0,
        0.0,
        kama_ui::Align::Center,
    );
    [parts[1], parts[3]]
}

pub(super) fn project_background_y() -> f32 {
    project_option_y(3)
}

pub(super) fn project_background_color_y() -> f32 {
    project_option_y(4)
}

pub(super) fn project_background_color_rect(rect: Rect) -> Rect {
    project_option_control_rect(rect, project_background_color_y())
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum ProjectNumberField {
    ResolutionX,
    ResolutionY,
    FrameRate,
}

impl ProjectNumberField {
    fn control(self) -> ((f64, f64), f64, usize, &'static str) {
        match self {
            Self::ResolutionX | Self::ResolutionY => {
                ((1.0, MAX_CANVAS_DIMENSION as f64), 4.0, 0, " px")
            }
            Self::FrameRate => ((1.0, MAX_FRAME_RATE), 0.25, 2, " fps"),
        }
    }

    fn value(self, project: &Project, composition: CompositionId) -> f64 {
        let settings = &project
            .composition(composition)
            .unwrap_or_else(|| project.active_composition())
            .settings;
        match self {
            Self::ResolutionX => settings.canvas_size[0] as f64,
            Self::ResolutionY => settings.canvas_size[1] as f64,
            Self::FrameRate => settings.frame_rate,
        }
    }

    fn apply(self, value: f64, project: &mut Project, composition: CompositionId) {
        let settings = &mut project
            .composition_mut(composition)
            .expect("selected composition disappeared")
            .settings;
        match self {
            Self::ResolutionX => settings.canvas_size[0] = value.round().max(1.0) as u32,
            Self::ResolutionY => settings.canvas_size[1] = value.round().max(1.0) as u32,
            Self::FrameRate => settings.frame_rate = value.max(1.0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResolutionPreset {
    Custom,
    Hd,
    FullHd,
    Qhd,
    Uhd4k,
    Dci4k,
    Uhd8k,
    VerticalFullHd,
    Square,
}

impl ResolutionPreset {
    const ALL: [Self; 9] = [
        Self::Custom,
        Self::Hd,
        Self::FullHd,
        Self::Qhd,
        Self::Uhd4k,
        Self::Dci4k,
        Self::Uhd8k,
        Self::VerticalFullHd,
        Self::Square,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Custom => "Custom",
            Self::Hd => "HD: 1280 × 720",
            Self::FullHd => "Full HD: 1920×1080",
            Self::Qhd => "QHD: 2560×1440",
            Self::Uhd4k => "UHD 4K: 3840×2160",
            Self::Dci4k => "DCI 4K: 4096×2160",
            Self::Uhd8k => "UHD 8K: 7680×4320",
            Self::VerticalFullHd => "Vertical FHD: 1080×1920",
            Self::Square => "Square: 1080×1080",
        }
    }

    fn dimensions(self) -> Option<[u32; 2]> {
        match self {
            Self::Custom => None,
            Self::Hd => Some([1280, 720]),
            Self::FullHd => Some([1920, 1080]),
            Self::Qhd => Some([2560, 1440]),
            Self::Uhd4k => Some([3840, 2160]),
            Self::Dci4k => Some([4096, 2160]),
            Self::Uhd8k => Some([7680, 4320]),
            Self::VerticalFullHd => Some([1080, 1920]),
            Self::Square => Some([1080, 1080]),
        }
    }

    fn index_for(size: [u32; 2]) -> usize {
        Self::ALL
            .iter()
            .position(|preset| preset.dimensions() == Some(size))
            .unwrap_or(0)
    }
}

default_state! {
    pub struct ProjectOptionsState {
        numbers: NumberControls<ProjectNumberField>,
        resolution_preset: ComboBox = ComboBox::new(0),
        background_color: ColorPicker = ColorPicker::new(Color::BLACK),
        last_rect: Option<Rect>,
    }
}

impl ProjectOptionsState {
    fn edit_number(
        &mut self,
        edit: impl FnOnce(&mut NumberInput) -> Option<f64>,
        project: &mut Project,
        composition: CompositionId,
    ) -> bool {
        let Some(field) = self.numbers.editing_target() else {
            return false;
        };
        if let Some(value) = self.numbers.edit(&field, edit) {
            field.apply(value, project, composition);
        }
        true
    }

    pub fn tick(&mut self, dt: f32) {
        self.numbers.tick(dt);
        self.resolution_preset.tick(dt);
        self.background_color.tick(dt);
    }

    pub fn is_animating(&self) -> bool {
        self.numbers.is_animating()
            || self.resolution_preset.is_animating()
            || self.background_color.is_animating()
    }

    pub fn is_value_dragging(&self) -> bool {
        self.numbers.is_dragging() || self.background_color.is_dragging()
    }

    pub fn sync_color_picker_textures(&mut self, renderer: &mut Renderer) -> Result<()> {
        self.background_color.sync_textures(renderer)
    }

    pub fn set_focused(&mut self, focused: bool) {
        if focused {
            return;
        }
        self.numbers.blur();
        self.resolution_preset.close();
        self.background_color.close();
    }

    pub fn popup_contains(&self, rect: Rect, point: [f32; 2]) -> bool {
        let local = Rect::new(0.0, 0.0, rect.width, rect.height);
        let preset = offset_rect(
            project_option_control_rect(local, project_preset_y()),
            rect.x,
            rect.y,
        );
        self.resolution_preset
            .popup_contains(preset, point, ResolutionPreset::ALL.len())
            || self.color_picker_contains(rect, point)
    }

    pub fn scroll_popup(&self, rect: Rect, point: [f32; 2], delta: [f32; 2]) -> bool {
        let local = Rect::new(0.0, 0.0, rect.width, rect.height);
        let preset = offset_rect(
            project_option_control_rect(local, project_preset_y()),
            rect.x,
            rect.y,
        );
        self.resolution_preset
            .scroll(preset, point, delta, ResolutionPreset::ALL.len())
    }

    fn color_picker_contains(&self, rect: Rect, point: [f32; 2]) -> bool {
        if self.last_rect.is_none() {
            return false;
        }
        let local = Rect::new(0.0, 0.0, rect.width, rect.height);
        let point = [point[0] - rect.x, point[1] - rect.y];
        self.background_color
            .popup_contains_in(project_background_color_rect(local), local, point)
    }

    pub fn build(
        &mut self,
        ctx: &mut kama_ui::BuildCtx,
        rect: Rect,
        project: &Project,
        composition: CompositionId,
        chevron: IconId,
    ) {
        self.last_rect = Some(rect);
        self.numbers.clear_layout();

        let selected = project
            .composition(composition)
            .unwrap_or_else(|| project.active_composition());
        let (solid, linear) = match selected.settings.background {
            ProjectBackground::Solid { color } => (true, color),
            ProjectBackground::Transparent => (false, [0.0, 0.0, 0.0, 1.0]),
        };
        self.background_color.set_linear(linear);

        let local = Rect::new(0.0, 0.0, rect.width, rect.height);
        kama_ui::ui!(ctx, {
            Rect("project-options-bg", local) {
                fill: theme::panel();
            }
        });
        panel_title(
            ctx,
            "project-options-title",
            local,
            &format!("Composition: {}", selected.name),
            0.0,
        );

        let resolution_y = project_resolution_y();
        let resolution_row = project_row_hit(local, resolution_y);
        property_row_frame(ctx, "project-resolution-row", resolution_row);
        property_label(
            ctx,
            "project-resolution-label",
            resolution_row,
            "Resolution",
        );
        let (resolution_rects, separator) = project_resolution_rects(local, resolution_y);
        for (field, rect, axis) in [
            (ProjectNumberField::ResolutionX, resolution_rects[0], "x"),
            (ProjectNumberField::ResolutionY, resolution_rects[1], "y"),
        ] {
            let (bounds, sensitivity, precision, suffix) = field.control();
            self.numbers.build(
                ctx,
                FormatKey::new(format_args!("project-resolution-{axis}")),
                field,
                (
                    rect,
                    field.value(project, composition),
                    (bounds, sensitivity, precision, suffix),
                ),
                crate::widgets::component_style(),
            );
        }
        kama_ui::ui!(ctx, {
            Rect("project-resolution-separator", separator) {
                font_size: 10.5; text_color: theme::muted(); text_centered; text: "×";
            }
        });

        let preset_y = project_preset_y();
        let preset_row = project_row_hit(local, preset_y);
        property_row_frame(ctx, "project-preset-row", preset_row);
        property_label(ctx, "project-preset-label", preset_row, "Preset");
        let preset_options = ResolutionPreset::ALL.map(ResolutionPreset::label);
        if !self.resolution_preset.is_open() {
            self.resolution_preset
                .set_selected(ResolutionPreset::index_for(selected.settings.canvas_size));
        }
        self.resolution_preset.build(
            ctx,
            "project-resolution-preset",
            project_option_control_rect(local, preset_y),
            &preset_options,
            chevron,
            crate::widgets::component_style(),
        );

        let frame_rate_y = project_frame_rate_y();
        let frame_rate_row = project_row_hit(local, frame_rate_y);
        property_row_frame(ctx, "project-frame-rate-row", frame_rate_row);
        property_label(
            ctx,
            "project-frame-rate-label",
            frame_rate_row,
            "Frame Rate",
        );
        let field = ProjectNumberField::FrameRate;
        let (bounds, sensitivity, precision, suffix) = field.control();
        self.numbers.build(
            ctx,
            "project-frame-rate",
            field,
            (
                project_option_control_rect(local, frame_rate_y),
                field.value(project, composition),
                (bounds, sensitivity, precision, suffix),
            ),
            crate::widgets::component_style(),
        );

        let row = project_row_hit(local, project_background_y());
        property_row_frame(ctx, "project-background-row", row);
        property_label(ctx, "project-background-label", row, "Background");
        let [solid_rect, transparent_rect] = project_background_mode_rects(row);
        for (id, rect, label, selected) in [
            ("project-background-solid", solid_rect, "Solid", solid),
            (
                "project-background-transparent",
                transparent_rect,
                "Transparent",
                !solid,
            ),
        ] {
            ToggleButton::build(
                ctx,
                id,
                rect,
                label,
                selected,
                crate::widgets::component_style(),
            );
        }

        if solid {
            let swatch = project_background_color_rect(local);
            let row = project_row_hit(local, project_background_color_y());
            property_row_frame(ctx, "project-background-color-row", row);
            property_label(ctx, "project-background-color-label", row, "Color");
            self.background_color.build_in(
                ctx,
                "project-background-color",
                swatch,
                local,
                crate::widgets::component_style(),
            );
        }
    }

    pub fn pointer_pressed(
        &mut self,
        rect: Rect,
        point: [f32; 2],
        modifiers: ModifiersState,
        project: &mut Project,
        composition: CompositionId,
    ) -> bool {
        let local = Rect::new(0.0, 0.0, rect.width, rect.height);
        let local_point = [point[0] - rect.x, point[1] - rect.y];
        let preset_rect = offset_rect(
            project_option_control_rect(local, project_preset_y()),
            rect.x,
            rect.y,
        );
        let preset_count = ResolutionPreset::ALL.len();
        if let Some(index) = self
            .resolution_preset
            .option_at(preset_rect, point, preset_count)
        {
            self.resolution_preset.select(index, true);
            self.apply_resolution_preset(index, project, composition);
            return true;
        }
        if self
            .resolution_preset
            .popup_contains(preset_rect, point, preset_count)
        {
            return true;
        }
        if matches!(
            project_background(project, composition),
            ProjectBackground::Solid { .. }
        ) && self.background_color.pointer_pressed_in(
            project_background_color_rect(local),
            local,
            local_point,
            modifiers,
        ) {
            self.numbers.blur();
            self.resolution_preset.close();
            self.apply_background_color(project, composition);
            return true;
        }
        if !rect.contains(point) {
            return false;
        }
        if preset_rect.contains(point) {
            self.numbers.blur();
            self.background_color.close();
            self.resolution_preset.toggle();
            return true;
        }
        self.resolution_preset.close();
        if let Some((field, value)) = self.numbers.pointer_pressed(rect, point, modifiers) {
            if let Some(value) = value {
                field.apply(value, project, composition);
            }
            return true;
        }

        let [solid, transparent] =
            project_background_mode_rects(project_row_hit(local, project_background_y()));
        if solid.contains(local_point) {
            let color = match project_background(project, composition) {
                ProjectBackground::Solid { color } => color,
                ProjectBackground::Transparent => self.background_color.linear(),
            };
            if let Some(composition) = project.composition_mut(composition) {
                composition.settings.background = ProjectBackground::Solid { color };
            }
        } else if transparent.contains(local_point) {
            if let Some(composition) = project.composition_mut(composition) {
                composition.settings.background = ProjectBackground::Transparent;
            }
            self.background_color.close();
        }
        true
    }

    pub fn pointer_moved(
        &mut self,
        point: [f32; 2],
        project: &mut Project,
        composition: CompositionId,
    ) -> bool {
        let Some(rect) = self.last_rect else {
            return false;
        };
        let local = Rect::new(0.0, 0.0, rect.width, rect.height);
        let local_point = [point[0] - rect.x, point[1] - rect.y];
        let mut changed = false;
        if let Some((field, value)) = self.numbers.pointer_moved(point) {
            field.apply(value, project, composition);
            changed = true;
        }
        if matches!(
            project_background(project, composition),
            ProjectBackground::Solid { .. }
        ) && self.background_color.pointer_moved_in(
            project_background_color_rect(local),
            local,
            local_point,
        ) {
            self.apply_background_color(project, composition);
            changed = true;
        }
        changed
    }

    pub fn pointer_released(&mut self) -> bool {
        self.background_color.pointer_released() | self.numbers.pointer_released()
    }

    pub fn handle_key(
        &mut self,
        event: &KeyEvent,
        modifiers: ModifiersState,
        project: &mut Project,
        composition: CompositionId,
    ) -> bool {
        if self.background_color.handle_key(event, modifiers) {
            self.apply_background_color(project, composition);
            return true;
        }
        self.edit_number(
            |input| input.handle_key(event, modifiers),
            project,
            composition,
        )
    }

    pub fn handle_ime(
        &mut self,
        event: &Ime,
        project: &mut Project,
        composition: CompositionId,
    ) -> bool {
        if self.background_color.handle_ime(event) {
            self.apply_background_color(project, composition);
            return true;
        }
        self.edit_number(|input| input.handle_ime(event), project, composition)
    }

    pub fn ime_area(&self, rect: Rect) -> Option<Rect> {
        let local = Rect::new(0.0, 0.0, rect.width, rect.height);
        if let Some(caret) = self
            .background_color
            .caret_rect_in(project_background_color_rect(local), local)
        {
            return Some(offset_rect(caret, rect.x, rect.y));
        }
        self.numbers.caret_rect(rect)
    }

    fn apply_resolution_preset(
        &self,
        index: usize,
        project: &mut Project,
        composition: CompositionId,
    ) {
        let Some(size) = ResolutionPreset::ALL
            .get(index)
            .and_then(|preset| preset.dimensions())
        else {
            return;
        };
        if let Some(composition) = project.composition_mut(composition) {
            composition.settings.canvas_size = size;
        }
    }

    fn apply_background_color(&self, project: &mut Project, composition: CompositionId) {
        if let Some(composition) = project.composition_mut(composition) {
            composition.settings.background = ProjectBackground::Solid {
                color: self.background_color.linear(),
            };
        }
    }
}
