use super::*;

pub(super) struct GraphicEqBuild<'a> {
    pub(super) viewport: Rect,
    pub(super) count: usize,
    pub(super) scroll: f32,
    pub(super) values: &'a [f32],
    pub(super) min_slot_width: f32,
    pub(super) radius: f32,
    pub(super) zero_inset: f32,
    pub(super) enabled: bool,
    pub(super) style: Style,
}

#[derive(Clone, Copy)]
pub(super) struct GraphicEqLayout {
    viewport: Rect,
    slider_top: f32,
    pub(super) slider_bottom: f32,
    slot_width: f32,
    count: usize,
    pub(super) scroll: f32,
    pub(super) max_scroll: f32,
}

pub(super) fn graphic_eq_layout(
    viewport: Rect,
    count: usize,
    scroll: f32,
    min_slot_width: f32,
) -> GraphicEqLayout {
    let content_width = viewport.width.max(count.max(1) as f32 * min_slot_width);
    let max_scroll = (content_width - viewport.width).max(0.0);
    let scroll = scroll.clamp(0.0, max_scroll);
    GraphicEqLayout {
        viewport,
        slider_top: viewport.y + 15.0,
        slider_bottom: viewport.bottom() - 28.0,
        slot_width: content_width / count.max(1) as f32,
        count,
        scroll,
        max_scroll,
    }
}

pub(super) fn graphic_eq_slider_rect(layout: GraphicEqLayout, index: usize) -> Rect {
    let slider_area = crate::ui_layout::fit_column_at(
        layout.viewport,
        [layout.viewport.x, layout.slider_top],
        layout.viewport.width,
        &[crate::ui_layout::Item::height(
            (layout.slider_bottom - layout.slider_top).max(1.0),
        )],
        0.0,
        0.0,
    )
    .1[0];
    crate::ui_layout::row_scrolled(
        slider_area,
        &vec![crate::ui_layout::Item::width(layout.slot_width.max(1.0)); layout.count.max(1)],
        0.0,
        0.0,
        kama_ui::Align::Start,
        kama_ui::ScrollState {
            offset: layout.scroll,
        },
    )[index]
}

pub(super) fn graphic_eq_visible_slider(layout: GraphicEqLayout, slider: Rect) -> bool {
    slider.x >= layout.viewport.x && slider.right() <= layout.viewport.right()
}

pub(super) fn build_graphic_eq_controls<K: Copy + Eq + Hash, S: Hash, Z: Hash>(
    ctx: &mut kama_ui::BuildCtx,
    controls: &mut SliderControls<K>,
    keys: (S, Z),
    build: GraphicEqBuild<'_>,
    mut control_key: impl FnMut(usize) -> K,
    mut control_id: impl FnMut(usize) -> String,
) -> GraphicEqLayout {
    let (surface_key, zero_key) = keys;
    let GraphicEqBuild {
        viewport,
        count,
        scroll,
        values,
        min_slot_width,
        radius,
        zero_inset,
        enabled,
        style,
    } = build;
    let layout = graphic_eq_layout(viewport, count, scroll, min_slot_width);
    let zero_y = layout.slider_top + (layout.slider_bottom - layout.slider_top) * 0.5;
    kama_ui::ui!(ctx, {
        Rect(surface_key, viewport) {
            fill: theme::control(); border: 1; border_color: theme::line(); border_radius: radius;
        }
        Rect(zero_key, Rect::new(viewport.x + zero_inset, zero_y, (viewport.width - zero_inset * 2.0).max(1.0), 1.0)) {
            fill: theme::line_soft();
        }
    });
    if enabled {
        for band in 0..count {
            let rect = graphic_eq_slider_rect(layout, band);
            if graphic_eq_visible_slider(layout, rect) {
                let value = values.get(band).copied().unwrap_or(0.0).clamp(-24.0, 24.0);
                controls.build(
                    ctx,
                    control_id(band),
                    control_key(band),
                    rect,
                    (value + 24.0) / 48.0,
                    style,
                );
            }
        }
    }
    layout
}

pub(super) fn eq_frequency_label(index: usize, count: usize) -> String {
    let low = 40.0_f32;
    let high = 16_000.0_f32;
    let t = if count <= 1 {
        0.5
    } else {
        index as f32 / (count - 1) as f32
    };
    let hz = low * (high / low).powf(t);
    if hz >= 1000.0 {
        format!("{:.1}k", hz / 1000.0)
    } else {
        format!("{}", hz.round() as u32)
    }
}

pub(super) fn set_graphic_eq_band(
    project: &mut Project,
    timeline: &mut TimelineState,
    node: u64,
    index: usize,
    normalized: f32,
) -> bool {
    let mut values = timeline
        .pipeline_host_input_value(project, node, "band_values")
        .and_then(|value| match value {
            crate::project::HostValue::F32List(values) => Some(values),
            _ => None,
        })
        .unwrap_or_else(|| vec![0.0; 31]);
    set_eq_band(&mut values, index, normalized);
    timeline.set_pipeline_host_input_value(
        project,
        node,
        "band_values",
        crate::project::HostValue::F32List(values),
    )
}

pub(super) fn plugin_node_inputs<'a>(
    plugins: &'a PluginRegistry,
    node_type: &str,
) -> Option<&'a [PluginInput]> {
    plugins
        .effect(node_type)
        .map(|definition| definition.inputs.as_slice())
        .or_else(|| {
            plugins
                .audio_effect(node_type)
                .map(|definition| definition.inputs.as_slice())
        })
        .or_else(|| {
            plugins
                .generator(node_type)
                .map(|definition| definition.inputs.as_slice())
        })
}

pub(super) fn plugin_node_input<'a>(
    plugins: &'a PluginRegistry,
    node_type: &str,
    input: &str,
) -> Option<&'a PluginInput> {
    plugin_node_inputs(plugins, node_type)?
        .iter()
        .find(|definition| definition.id == input)
}

pub(super) fn plugin_node_name(plugins: &PluginRegistry, node_type: &str) -> String {
    if node_type == crate::effects::PIPELINE_NODE_TYPE {
        return "Pipeline".into();
    }
    plugins
        .effect(node_type)
        .map(|definition| definition.name.as_str())
        .or_else(|| {
            plugins
                .audio_effect(node_type)
                .map(|definition| definition.name.as_str())
        })
        .or_else(|| {
            plugins
                .generator(node_type)
                .map(|definition| definition.name.as_str())
        })
        .map(str::to_owned)
        .unwrap_or_else(|| friendly_name(node_type))
}

pub(super) fn friendly_name(raw: &str) -> String {
    let raw = raw.rsplit('.').next().unwrap_or(raw);
    let mut output = String::with_capacity(raw.len());
    let mut uppercase = true;
    for character in raw.chars() {
        if matches!(character, '_' | '-') {
            output.push(' ');
            uppercase = true;
        } else if uppercase {
            output.extend(character.to_uppercase());
            uppercase = false;
        } else {
            output.push(character);
        }
    }
    output
}

pub(super) fn host_value_summary(
    value: Option<crate::project::HostValue>,
    monitor_edit: bool,
    pen_edit: bool,
) -> String {
    match value {
        Some(crate::project::HostValue::Vec2Array(points)) => {
            let hint = if pen_edit {
                "Pen"
            } else if monitor_edit {
                "Monitor"
            } else {
                ""
            };
            format!("{} points{hint}", points.len())
        }
        Some(crate::project::HostValue::F32List(values)) => format!("{} values", values.len()),
        Some(crate::project::HostValue::String(value)) => value,
        Some(crate::project::HostValue::Bytes(value)) => format!("{} bytes", value.len()),
        Some(crate::project::HostValue::Gpu(_)) => "host value".into(),
        None => "-".into(),
    }
}

pub(super) fn format_gpu_value(value: GpuValue) -> String {
    match value {
        GpuValue::F32(value) => format!("{value:.2}"),
        GpuValue::I32(value) => value.to_string(),
        GpuValue::U32(value) => value.to_string(),
        GpuValue::Bool(value) => {
            if value {
                "On".into()
            } else {
                "Off".into()
            }
        }
        GpuValue::Enum(value) => format!("#{value}"),
        GpuValue::Vec2(value) => format!("{:.2}, {:.2}", value[0], value[1]),
        GpuValue::Vec3(value) => format!("{:.2}, {:.2}, {:.2}", value[0], value[1], value[2]),
        GpuValue::Vec4(value) => format!(
            "{:.2}, {:.2}, {:.2}, {:.2}",
            value[0], value[1], value[2], value[3]
        ),
        GpuValue::Color(value) => format!(
            "{:.2}, {:.2}, {:.2}, {:.2}",
            value[0], value[1], value[2], value[3]
        ),
    }
}

pub(super) fn node_header_rect(rect: Rect, y: f32) -> Rect {
    crate::ui_layout::fit_column_at(
        rect,
        [rect.x, y],
        rect.width.max(1.0),
        &[crate::ui_layout::Item::height(EFFECT_NODE_HEADER_H)],
        0.0,
        0.0,
    )
    .1[0]
}

pub(super) fn effect_property_rect(rect: Rect) -> Rect {
    crate::ui_layout::row(
        rect,
        &[
            crate::ui_layout::Item::width(EFFECT_NODE_CONTENT_PAD),
            crate::ui_layout::Item::fill(),
            crate::ui_layout::Item::width(EFFECT_NODE_CONTENT_PAD),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
    )[1]
}

pub(super) fn node_action_rects(rect: Rect, y: f32) -> [Rect; 5] {
    let header = node_header_rect(rect, y);
    let parts = crate::ui_layout::row(
        header,
        &[
            crate::ui_layout::Item::fill(),
            crate::ui_layout::Item::new(Size::Pixels(18.0), Size::Pixels(18.0)),
            crate::ui_layout::Item::width(3.0),
            crate::ui_layout::Item::new(Size::Pixels(18.0), Size::Pixels(18.0)),
            crate::ui_layout::Item::width(3.0),
            crate::ui_layout::Item::new(Size::Pixels(18.0), Size::Pixels(18.0)),
            crate::ui_layout::Item::width(3.0),
            crate::ui_layout::Item::new(Size::Pixels(18.0), Size::Pixels(18.0)),
            crate::ui_layout::Item::width(3.0),
            crate::ui_layout::Item::new(Size::Pixels(18.0), Size::Pixels(18.0)),
            crate::ui_layout::Item::width(4.0),
        ],
        0.0,
        0.0,
        kama_ui::Align::Center,
    );
    [parts[1], parts[3], parts[5], parts[7], parts[9]]
}

pub(super) fn draw_effect_header(
    ctx: &mut kama_ui::BuildCtx,
    position: (Rect, f32),
    node: &crate::effects::EffectNode,
    section: &Accordion,
    body_height: f32,
    chrome: (Icons, &PluginRegistry),
    state: (bool, KeyframeControl),
) {
    let (rect, y) = position;
    let (icons, plugins) = chrome;
    let chevron = icons.get(AppIcon::Chevron);
    let (enabled, enabled_keyframe) = state;
    let header = node_header_rect(rect, y);
    let style = crate::widgets::component_style();
    let label = plugin_node_name(plugins, &node.node_type);
    section.build_header(
        ctx,
        FormatKey::new(format_args!("inspector-effect-{}", node.id)),
        header,
        &label,
        chevron,
        style,
    );
    let _ = section.build_body(
        ctx,
        FormatKey::new(format_args!("inspector-effect-{}", node.id)),
        header,
        body_height,
        style,
    );
    let buttons = node_action_rects(rect, y);
    for (index, icon, tooltip) in [
        (0, AppIcon::ArrowUp, "Move effect up"),
        (1, AppIcon::ArrowDown, "Move effect down"),
        (4, AppIcon::Delete, "Delete effect"),
    ] {
        icon_button(
            ctx,
            &format!("inspector-effect-action-{}-{index}", node.id),
            buttons[index],
            icons.get(icon),
            tooltip,
            style,
        );
    }
    ToggleButton::build(
        ctx,
        format!("inspector-effect-enabled-{}", node.id),
        buttons[2],
        "E",
        enabled,
        style,
    );
    kama_ui::ui!(ctx, {
        Rect(("inspector-effect-enabled-tip", node.id), buttons[2]) {
            interactive; tooltip: if enabled { "Disable effect" } else { "Enable effect" };
        }
    });
    toggle_icon_button(
        ctx,
        &format!("inspector-effect-enabled-key-{}", node.id),
        buttons[3],
        enabled_keyframe.icon,
        enabled_keyframe.keyed,
        if enabled_keyframe.keyed {
            "Remove enabled keyframe"
        } else {
            "Add enabled keyframe"
        },
        style,
    );
}

pub(super) fn transform_vec2_row_hit(
    rect: Rect,
    y: f32,
    point: [f32; 2],
    timeline: &mut TimelineState,
    input: &str,
) -> bool {
    if !row_hit(rect, y).contains(point) {
        return false;
    }
    if keyframe_rect(rect, y).contains(point) {
        timeline.toggle_transform_keyframe(input);
    }
    true
}

pub(super) fn rotation_row_hit(rect: Rect, y: f32) -> Rect {
    crate::ui_layout::fit_column_at(
        rect,
        [rect.x, y],
        rect.width,
        &[crate::ui_layout::Item::height(ANGLE_ROW_H)],
        0.0,
        0.0,
    )
    .1[0]
}

pub(super) fn transform_rotation_parts(rect: Rect, y: f32) -> (Rect, Rect, Rect, Rect, Rect) {
    let row = rotation_row_hit(rect, y);
    let (label_column, control_column, key_column) = property_row_parts(row);
    let label = Rect::new(label_column.x, row.y + 2.0, label_column.width, 24.0);
    let fields = Rect::new(control_column.x, row.y + 2.0, control_column.width, 24.0);
    let values = crate::ui_layout::row(
        fields,
        &[
            crate::ui_layout::Item::fill(),
            crate::ui_layout::Item::width(5.0),
            crate::ui_layout::Item::fill(),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
    );
    let knob = Rect::new(
        control_column.x,
        row.y + 31.0,
        control_column.width,
        (ANGLE_ROW_H - 34.0).max(1.0),
    );
    let key = Rect::new(key_column.x, row.y + 4.0, key_column.width, 18.0);
    (label, values[0], values[2], knob, key)
}

pub(super) fn split_angle(rotation: f32) -> (i32, f32) {
    let turns = (rotation / 360.0).trunc() as i32;
    (turns, rotation - turns as f32 * 360.0)
}

pub(super) fn angle_knob(value: f32) -> Knob {
    Knob::new(-36000.0, 36000.0, value as f64)
        .step(0.1)
        .circular()
        .formatter(|value, _| format!("{:.0}°", value.rem_euclid(360.0)))
}

pub(super) fn property_row(
    ctx: &mut kama_ui::BuildCtx,
    rect: Rect,
    y: f32,
    label: &str,
    value: &str,
    keyframe: KeyframeControl,
) {
    let spin = property_chrome(ctx, rect, y, label, "value", Some(keyframe));
    SpinInput::build(
        ctx,
        format!("property-{label}-{}", y.to_bits()),
        spin,
        value,
        crate::widgets::component_style(),
    );
}

pub(super) fn color_property_row(
    ctx: &mut kama_ui::BuildCtx,
    rect: Rect,
    y: f32,
    label: &str,
    color: [f32; 4],
    keyframe: KeyframeControl,
) {
    property_chrome(ctx, rect, y, label, "color", Some(keyframe));
    let swatch = color_swatch_rect(rect, y);
    ColorButton::build(
        ctx,
        format!("inspector-color-swatch-{label}-{}", y.to_bits()),
        swatch,
        ui_color(color),
        crate::widgets::component_style(),
    );
}

pub(super) fn gradient_header_parts(rect: Rect, y: f32) -> (Rect, Rect, Rect) {
    let controls = property_control_rect(rect, y);
    let parts = crate::ui_layout::row(
        controls,
        &[
            crate::ui_layout::Item::fill(),
            crate::ui_layout::Item::width(6.0),
            crate::ui_layout::Item::new(
                Size::Pixels(18.0),
                Size::Pixels((controls.height - 4.0).max(1.0)),
            ),
            crate::ui_layout::Item::width(4.0),
            crate::ui_layout::Item::new(
                Size::Pixels(18.0),
                Size::Pixels((controls.height - 4.0).max(1.0)),
            ),
            crate::ui_layout::Item::width(2.0),
        ],
        0.0,
        0.0,
        kama_ui::Align::Center,
    );
    (parts[0], parts[2], parts[4])
}

pub(super) fn gradient_color_header_row(
    ctx: &mut kama_ui::BuildCtx,
    rect: Rect,
    y: f32,
    count: usize,
    keyframe: KeyframeControl,
    icons: Icons,
) {
    property_chrome(ctx, rect, y, "Colors", "gradient-colors", Some(keyframe));
    let (summary, remove, add) = gradient_header_parts(rect, y);
    ui_text!(
        ctx,
        ("gradient-colors-summary", y.to_bits()),
        summary,
        9.0,
        theme::muted(),
        &format!("{count} stops")
    );
    icon_button(
        ctx,
        &format!("gradient-stop-remove-{}", y.to_bits()),
        remove,
        icons.get(AppIcon::Delete),
        "Remove last stop",
        crate::widgets::component_style(),
    );
    icon_button(
        ctx,
        &format!("gradient-stop-add-{}", y.to_bits()),
        add,
        icons.get(AppIcon::Plus),
        "Add stop",
        crate::widgets::component_style(),
    );
}

pub(super) fn gradient_stop_row(
    ctx: &mut kama_ui::BuildCtx,
    rect: Rect,
    y: f32,
    index: usize,
    color: [f32; 4],
) {
    property_chrome(
        ctx,
        rect,
        y,
        &format!("Stop {}", index + 1),
        "gradient-stop",
        None,
    );
    let swatch = color_swatch_rect(rect, y);
    ColorButton::build(
        ctx,
        format!("gradient-stop-color-{}-{}", index, y.to_bits()),
        swatch,
        ui_color(color),
        crate::widgets::component_style(),
    );
}

pub(super) fn gradient_stop_add_rect(rect: Rect, y: f32) -> Rect {
    gradient_header_parts(rect, y).2
}

pub(super) fn gradient_stop_remove_rect(rect: Rect, y: f32) -> Rect {
    gradient_header_parts(rect, y).1
}

pub(super) fn color_swatch_rect(rect: Rect, y: f32) -> Rect {
    property_control_rect(rect, y).inset(2.0)
}

pub(super) fn ui_color(linear: [f32; 4]) -> Color {
    Color::from_linear(linear)
}

pub(super) const BUILTIN_GRADIENT_GENERATOR: &str = "builtin.gradient";
