use anyhow::Result;
use kama_ui as ui;
use kama_ui::components::{
    Accordion, AccordionContent, Button, ColorPicker, ComboBox, Label, NumberInput, Slider, Style,
    TextEdit, ToggleButton,
};
use kama_ui::dock::{Rect, StackId};
use kama_ui::{Align, BlockId, Color, CursorShape, Renderer, ScrollState, Size};
use winit::{
    event::{Ime, KeyEvent},
    keyboard::ModifiersState,
};

use super::{
    assets::{AppIcon, Icons},
    theme, RADIUS_MD, RADIUS_SM,
};

const PAD: f32 = 16.0;
const TITLE_H: f32 = 34.0;
const ROW_H: f32 = 28.0;
const TEXT_H: f32 = 72.0;
const ROW_GAP: f32 = 9.0;
const LABEL_W: f32 = 86.0;
const FORM_GAP: f32 = 12.0;
const ACCORDION_BODY_H: f32 = 58.0;
const OPTIONS: [&str; 3] = ["Option A", "Option B", "Option C"];

pub(crate) const CONTEXT_MENU_W: f32 = 218.0;
pub(crate) const CONTEXT_MENU_ROW_H: f32 = 26.0;

pub(crate) struct ContextMenuItem<'a> {
    pub(crate) label: &'a str,
    pub(crate) shortcut: Option<String>,
    pub(crate) icon: Option<AppIcon>,
    pub(crate) enabled: bool,
}

pub(crate) fn context_menu_rect(panel: Rect, point: [f32; 2], item_count: usize) -> Rect {
    let height = item_count.max(1) as f32 * CONTEXT_MENU_ROW_H + 4.0;
    Rect::new(
        (panel.x + point[0]).clamp(
            panel.x + 2.0,
            (panel.right() - CONTEXT_MENU_W - 2.0).max(panel.x + 2.0),
        ),
        (panel.y + point[1]).clamp(
            panel.y + 2.0,
            (panel.bottom() - height - 2.0).max(panel.y + 2.0),
        ),
        CONTEXT_MENU_W,
        height,
    )
}

pub(crate) fn context_menu_row(rect: Rect, index: usize) -> Rect {
    Rect::new(
        rect.x + 2.0,
        rect.y + 2.0 + index as f32 * CONTEXT_MENU_ROW_H,
        rect.width - 4.0,
        CONTEXT_MENU_ROW_H,
    )
}

pub(crate) fn context_menu_hit(rect: Rect, point: [f32; 2], item_count: usize) -> Option<usize> {
    let local = [point[0] - rect.x - 2.0, point[1] - rect.y - 2.0];
    (local[0] >= 0.0
        && local[0] < rect.width - 4.0
        && local[1] >= 0.0
        && local[1] < item_count as f32 * CONTEXT_MENU_ROW_H)
        .then_some((local[1] / CONTEXT_MENU_ROW_H) as usize)
}

pub(crate) fn build_context_menu(
    ctx: &mut ui::BuildCtx,
    id: &str,
    rect: Rect,
    cursor: [f32; 2],
    items: &[ContextMenuItem<'_>],
    icons: Icons,
) {
    ui::ui!(ctx, {
        Block {
            id: @format("{}-context-menu", id);
            overlay;
            bounds: (rect.x, rect.y, rect.width, rect.height);
            backdrop_blur: 22.0;
            backdrop_tint: theme::popup_tint();
            fill: theme::floating_bg();
            border: 1;
            border_color: theme::accent();
            border_radius: RADIUS_MD;
            padding: 2.0;

            @for (index, item) in items.iter().enumerate() {
                @let row = context_menu_row(rect, index);
                Row {
                    id: @format("{}-context-row-{}", id, index);
                    width: Size::Fill;
                    height: Size::Pixels(CONTEXT_MENU_ROW_H);
                    padding: 6.0;
                    gap: 8.0;
                    fill: if item.enabled && row.contains(cursor) {
                        theme::popup_hover()
                    } else {
                        Color::TRANSPARENT
                    };
                    border_radius: RADIUS_SM;
                    interactive;
                    cursor: if item.enabled { CursorShape::Pointer } else { CursorShape::Passthrough };

                    Block {
                        id: @format("{}-context-icon-slot-{}", id, index);
                        width: Size::Pixels(16.0);
                        height: Size::Fill;
                        content_centered;
                        @if item.icon.is_some() {
                            Icon {
                                id: @format("{}-context-icon-{}", id, index);
                                icon!: icons.get(item.icon.unwrap());
                                color!: if item.enabled { theme::popup_muted() } else { theme::popup_dim() };
                                width: Size::Pixels(16.0);
                                height: Size::Pixels(16.0);
                            }
                        }
                    }
                    Block {
                        width: Size::Fill;
                        height: Size::Fill;
                        font_size: 10.0;
                        text_color: if item.enabled { theme::popup_text() } else { theme::popup_dim() };
                        text: item.label;
                    }
                    @if item.shortcut.is_some() {
                        Block {
                            width: Size::Pixels(62.0);
                            height: Size::Fill;
                            font_size: 9.0;
                            text_color: if item.enabled { theme::popup_muted() } else { theme::popup_dim() };
                            text_align: Align::End;
                            text: item.shortcut.as_deref().unwrap_or_default();
                        }
                    }
                }
            }
        }
    });
}

#[derive(Clone, Copy)]
struct Field {
    label: Rect,
    control: Rect,
}

#[derive(Clone, Copy)]
struct GalleryLayout {
    title: Rect,
    label: Field,
    text_edit: Field,
    slider: Field,
    number: Field,
    color: Field,
    button: Field,
    button_action: Rect,
    button_note: Rect,
    toggle: Field,
    toggle_action: Rect,
    combo: Field,
    combo_action: Rect,
    accordion: Field,
    content_bottom: f32,
}

#[derive(Clone, Copy)]
struct GalleryFieldIds {
    label: BlockId,
    control: BlockId,
}

fn gallery_field(
    ctx: &mut ui::BuildCtx,
    label_width: f32,
    height: f32,
    control_width: Size,
) -> GalleryFieldIds {
    let mut ids = None;
    ctx.new()
        .row()
        .width(Size::Fill)
        .height(Size::Pixels(height))
        .gap(FORM_GAP)
        .children(|ctx| {
            ids = Some(GalleryFieldIds {
                label: ctx
                    .new()
                    .width(Size::Pixels(label_width))
                    .height(Size::Fill)
                    .build(),
                control: ctx.new().width(control_width).height(Size::Fill).build(),
            });
        })
        .build();
    ids.expect("gallery field ids")
}

impl GalleryLayout {
    fn new(bounds: Rect) -> Self {
        let label_width = LABEL_W.min((bounds.width - PAD * 2.0).max(0.0) * 0.32);
        let viewport = Rect::new(bounds.x, bounds.y, bounds.width, 4096.0);
        let (ids, measured) = ui::measure_layout(viewport, |ctx| {
            let mut title = BlockId(0);
            let mut label = None;
            let mut text_edit = None;
            let mut slider = None;
            let mut number = None;
            let mut color = None;
            let mut button = None;
            let mut toggle = None;
            let mut combo = None;
            let mut accordion = None;

            let root = ctx
                .new()
                .width(Size::Fill)
                .height(Size::Fit)
                .padding(PAD)
                .gap(ROW_GAP)
                .children(|ctx| {
                    title = ctx
                        .new()
                        .width(Size::Fill)
                        .height(Size::Pixels(TITLE_H))
                        .build();
                    label = Some(gallery_field(ctx, label_width, ROW_H, Size::Fill));
                    text_edit = Some(gallery_field(ctx, label_width, TEXT_H, Size::Fill));
                    slider = Some(gallery_field(ctx, label_width, ROW_H, Size::Fill));
                    number = Some(gallery_field(ctx, label_width, ROW_H, Size::Fill));
                    color = Some(gallery_field(ctx, label_width, ROW_H, Size::Fill));

                    button = Some(gallery_field(ctx, label_width, ROW_H, Size::Fill));
                    toggle = Some(gallery_field(ctx, label_width, ROW_H, Size::Pixels(112.0)));
                    combo = Some(gallery_field(ctx, label_width, ROW_H, Size::Pixels(220.0)));
                    accordion = Some(gallery_field(ctx, label_width, ROW_H, Size::Fill));
                    ctx.new()
                        .width(Size::Fill)
                        .height(Size::Pixels((ACCORDION_BODY_H + 4.0 - ROW_GAP).max(0.0)))
                        .build();
                })
                .build();

            let button = button.expect("button field ids");
            (
                root,
                title,
                label.expect("label field ids"),
                text_edit.expect("text field ids"),
                slider.expect("slider field ids"),
                number.expect("number field ids"),
                color.expect("color field ids"),
                button,
                toggle.expect("toggle field ids"),
                combo.expect("combo field ids"),
                accordion.expect("accordion field ids"),
            )
        });

        let (
            root,
            title,
            label,
            text_edit,
            slider,
            number,
            color,
            button,
            toggle,
            combo,
            accordion,
        ) = ids;
        let rect = |id| measured.rect(id).expect("gallery layout rect");
        let field = |ids: GalleryFieldIds| Field {
            label: rect(ids.label),
            control: rect(ids.control),
        };
        let button = field(button);
        let toggle = field(toggle);
        let combo = field(combo);

        let pair_layout = |control: Rect, first_width: f32, gap: f32| {
            let ((first, second), measured) = ui::measure_layout(control, |ctx| {
                let mut first = BlockId(0);
                let mut second = BlockId(0);
                ctx.new()
                    .row()
                    .width(Size::Fill)
                    .height(Size::Fill)
                    .gap(gap)
                    .children(|ctx| {
                        first = ctx
                            .new()
                            .width(Size::Pixels(first_width.min(control.width)))
                            .height(Size::Fill)
                            .build();
                        second = ctx.new().width(Size::Fill).height(Size::Fill).build();
                    })
                    .build();
                (first, second)
            });
            (
                measured.rect(first).expect("gallery action rect"),
                measured.rect(second).expect("gallery note rect"),
            )
        };
        let (button_action, button_note) = pair_layout(button.control, 112.0, 10.0);
        let (toggle_action, _) = pair_layout(toggle.control, 112.0, 0.0);
        let (combo_action, _) = pair_layout(combo.control, 220.0, 0.0);

        Self {
            title: rect(title),
            label: field(label),
            text_edit: field(text_edit),
            slider: field(slider),
            number: field(number),
            color: field(color),
            button,
            button_action,
            button_note,
            toggle,
            toggle_action,
            combo,
            combo_action,
            accordion: field(accordion),
            content_bottom: rect(root).bottom(),
        }
    }

    fn button(self) -> Rect {
        self.button_action
    }

    fn toggle(self) -> Rect {
        self.toggle_action
    }

    fn combo(self) -> Rect {
        self.combo_action
    }

    fn content_bottom(self) -> f32 {
        self.content_bottom
    }
}

default_state! {
    pub struct WidgetGallery {
        text: TextEdit = TextEdit::multiline("Edit me"),
        slider: Slider = Slider::new(0.55),
        number: NumberInput = NumberInput::new(42.0).bounds(-999.0, 999.0).sensitivity(0.25),
        color: ColorPicker = ColorPicker::new(Color::rgb8(0xe4, 0x42, 0x42)),
        button_clicks: u32,
        toggled: bool,
        combo: ComboBox = ComboBox::new(0),
        accordion: Accordion = Accordion::new(true),
        vertical_scroll: ScrollState,
        focused_stack: Option<StackId>,
        color_rect: Option<Rect>,
    }
}

impl WidgetGallery {
    pub fn set_focused(&mut self, stack: Option<StackId>) {
        if self.focused_stack != stack {
            self.text.set_focused(false);
            self.combo.close();
            self.slider.pointer_released();
            self.number.set_focused(false);
            self.color.close();
        }
        self.focused_stack = stack;
    }

    fn layout(&self, bounds: Rect) -> GalleryLayout {
        let content = crate::ui_layout::column(
            bounds,
            &[crate::ui_layout::Item::height(bounds.height)],
            0.0,
            0.0,
            Align::Start,
            Some(self.vertical_scroll),
        )[0];
        GalleryLayout::new(content)
    }

    pub fn scroll(&mut self, bounds: Rect, cursor: [f32; 2], delta: [f32; 2]) -> bool {
        let layout = self.layout(bounds);
        if self
            .combo
            .scroll(layout.combo(), cursor, delta, OPTIONS.len())
        {
            return true;
        }
        if layout.text_edit.control.contains(cursor) {
            self.text.scroll(layout.text_edit.control, delta);
            return true;
        }
        let content = GalleryLayout::new(bounds).content_bottom() - bounds.y;
        self.vertical_scroll
            .scroll_by(-delta[1], (content - bounds.height).max(0.0))
    }

    pub fn tick(&mut self, dt: f32) {
        self.text.tick(dt);
        self.slider.tick(dt);
        self.number.tick(dt);
        self.color.tick(dt);
        self.combo.tick(dt);
        self.accordion.tick(dt);
    }

    pub fn is_value_dragging(&self) -> bool {
        self.slider.is_dragging() || self.number.is_dragging() || self.color.is_dragging()
    }

    pub fn is_animating(&self) -> bool {
        self.text.is_animating()
            || self.slider.is_animating()
            || self.number.is_animating()
            || self.color.is_animating()
            || self.combo.is_animating()
            || self.accordion.is_animating()
    }

    pub fn ime_area(&self, bounds: Rect) -> Option<Rect> {
        let layout = self.layout(bounds);
        if self.text.is_focused() {
            return Some(self.text.caret_rect(layout.text_edit.control));
        }
        if let Some(rect) = self.color.caret_rect(layout.color.control) {
            return Some(rect);
        }
        self.number.caret_rect(layout.number.control)
    }

    pub fn popup_contains(&self, bounds: Rect, point: [f32; 2]) -> bool {
        let layout = self.layout(bounds);
        self.color.popup_contains(layout.color.control, point)
            || self.combo.is_open()
                && self
                    .combo
                    .option_at(layout.combo(), point, OPTIONS.len())
                    .is_some()
    }

    pub fn pointer_pressed(
        &mut self,
        stack: StackId,
        bounds: Rect,
        point: [f32; 2],
        modifiers: ModifiersState,
    ) -> bool {
        self.set_focused(Some(stack));
        let layout = self.layout(bounds);
        let combo = layout.combo();
        self.color_rect = Some(layout.color.control);
        if self
            .color
            .pointer_pressed(layout.color.control, point, modifiers)
        {
            self.combo.close();
            self.text.set_focused(false);
            self.number.set_focused(false);
            return true;
        }
        if let Some(index) = self.combo.option_at(combo, point, OPTIONS.len()) {
            self.combo.select(index, !modifiers.shift_key());
            self.text.set_focused(false);
            return true;
        }
        if !bounds.contains(point) {
            self.text.set_focused(false);
            self.combo.close();
            return false;
        }
        if self
            .text
            .pointer_pressed(layout.text_edit.control, point, modifiers)
        {
            self.combo.close();
            return true;
        }
        if self.slider.pointer_pressed(layout.slider.control, point) {
            self.combo.close();
            self.number.set_focused(false);
            return true;
        }
        if layout.number.control.contains(point) {
            self.combo.close();
            self.text.set_focused(false);
            self.number
                .pointer_pressed(layout.number.control, point, modifiers);
            return true;
        }
        if layout.button().contains(point) {
            self.combo.close();
            self.button_clicks = self.button_clicks.wrapping_add(1);
            return true;
        }
        if layout.toggle().contains(point) {
            self.combo.close();
            self.toggled = !self.toggled;
            return true;
        }
        if combo.contains(point) {
            self.combo.toggle();
            return true;
        }
        if layout.accordion.control.contains(point) {
            self.combo.close();
            self.accordion.toggle();
            return true;
        }
        self.combo.close();
        true
    }

    pub fn pointer_moved(&mut self, point: [f32; 2]) -> bool {
        let color = self
            .color_rect
            .is_some_and(|rect| self.color.pointer_moved(rect, point));
        let number = self.number.pointer_moved(point).is_some();
        color || self.text.pointer_moved(point) || self.slider.pointer_moved(point) || number
    }

    pub fn pointer_released(&mut self) -> bool {
        self.color.pointer_released()
            | self.text.pointer_released()
            | self.slider.pointer_released()
            | self.number.pointer_released()
    }

    pub fn handle_key(&mut self, event: &KeyEvent, modifiers: ModifiersState) -> bool {
        if self.color.handle_key(event, modifiers) {
            return true;
        }
        if self.number.is_editing() {
            self.number.handle_key(event, modifiers);
            return true;
        }
        self.text.handle_key(event, modifiers).handled
    }

    pub fn handle_ime(&mut self, event: &Ime) -> bool {
        if self.color.handle_ime(event) {
            return true;
        }
        if self.number.is_editing() {
            self.number.handle_ime(event);
            return true;
        }
        self.text.handle_ime(event).handled
    }

    pub fn sync_color_picker_textures(&mut self, renderer: &mut Renderer) -> Result<()> {
        self.color.sync_textures(renderer)
    }

    pub fn build(&mut self, ctx: &mut ui::BuildCtx, stack: StackId, content: Rect, icons: Icons) {
        let layout = GalleryLayout::new(Rect::new(0.0, 0.0, content.width, content.height));
        let style = component_style();
        let labels = [
            ("Label", layout.label.label),
            ("Text edit", layout.text_edit.label),
            ("Slider", layout.slider.label),
            ("Number", layout.number.label),
            ("Color", layout.color.label),
            ("Button", layout.button.label),
            ("Toggle", layout.toggle.label),
            ("Combobox", layout.combo.label),
            ("Accordion", layout.accordion.label),
        ];

        ui::ui!(ctx, {
            Block {
                id: @format("widget-gallery {}", stack.0);
                fill: theme::panel();
                width: Size::Fill;
                height: Size::Fill;
                vertical_scroll: self.vertical_scroll;

                Block {
                    bounds: (
                        layout.title.x,
                        layout.title.y,
                        layout.title.width,
                        layout.title.height,
                    );
                    text_color: theme::text();
                    text: "UI Widgets";
                }

                @for (index, (text, rect)) in labels.into_iter().enumerate() {
                    Block {
                        id: @format("gallery-field-label {} {}", stack.0, index);
                        bounds: (rect.x, rect.y, rect.width, rect.height);
                        font_size: 10.5;
                        text_color: theme::muted();
                        text: text;
                    }
                }

                @rust {
                    Label::build(
                        ctx,
                        format_args!("{} plain", stack.0),
                        layout.label.control,
                        "A plain label",
                        style,
                    );
                    self.text.build(
                        ctx,
                        format_args!("{} edit", stack.0),
                        layout.text_edit.control,
                        "Edit text…",
                        style,
                    );
                    self.slider.build(
                        ctx,
                        format_args!("{} slider", stack.0),
                        layout.slider.control,
                        style,
                    );
                    self.number.build(
                        ctx,
                        format_args!("{} number", stack.0),
                        layout.number.control,
                        "",
                        style,
                    );
                    self.color.build(
                        ctx,
                        format_args!("{} color", stack.0),
                        layout.color.control,
                        style,
                    );
                    let button = layout.button();
                    Button::build(
                        ctx,
                        format_args!("{} button", stack.0),
                        button,
                        "Click me",
                        style,
                    );
                }

                Block {
                    bounds: (
                        layout.button_note.x,
                        layout.button_note.y,
                        layout.button_note.width,
                        layout.button_note.height,
                    );
                    font_size: 10.5;
                    text_color: theme::muted();
                    text: format!("{} clicks", self.button_clicks);
                }

                @rust {
                    ToggleButton::build(
                        ctx,
                        format_args!("{} toggle", stack.0),
                        layout.toggle(),
                        if self.toggled { "On" } else { "Off" },
                        self.toggled,
                        style,
                    );
                    self.combo.build(
                        ctx,
                        format_args!("{} combo", stack.0),
                        layout.combo(),
                        &OPTIONS,
                        icons.get(AppIcon::Chevron),
                        style,
                    );
                    self.accordion.build(
                        ctx,
                        format_args!("{} accordion", stack.0),
                        layout.accordion.control,
                        ACCORDION_BODY_H,
                        AccordionContent {
                            title: "Accordion section",
                            body: "Accordion content expands and collapses without replacing the header.",
                            chevron: icons.get(AppIcon::Chevron),
                        },
                        style,
                    );
                }
            }
        });
    }
}

pub fn component_style() -> Style {
    Style {
        text: theme::text(),
        muted: theme::muted(),
        accent: theme::accent(),
        accent_text: theme::accent_text(),
        control: theme::control(),
        focused: theme::focused(),
        border: theme::line_soft(),
        radius_sm: RADIUS_SM,
        radius_md: RADIUS_MD,
        text_scale: 1.0,
        ui_scale: 1.0,
    }
}
