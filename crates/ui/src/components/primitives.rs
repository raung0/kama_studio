use crate::{ui_component, Color, IconId, Size};

#[derive(Default)]
pub struct RowComponent;

#[ui_component("Row", stateless)]
impl Component for RowComponent {
    fn ui(&mut self, ctx: &mut BuildCtx) -> BlockBuilder<'_> {
        ctx.new().row()
    }
}

#[derive(Default)]
pub struct ColumnComponent;

#[ui_component("Column", stateless)]
impl Component for ColumnComponent {
    fn ui(&mut self, ctx: &mut BuildCtx) -> BlockBuilder<'_> {
        ctx.new().column()
    }
}

#[derive(Default)]
pub struct HSpacerComponent;

#[ui_component("HSpacer", stateless)]
impl Component for HSpacerComponent {
    fn ui(&mut self, ctx: &mut BuildCtx) -> BlockBuilder<'_> {
        ctx.new().width(Size::Fill).height(Size::Fit)
    }
}

#[derive(Default)]
pub struct VSpacerComponent;

#[ui_component("VSpacer", stateless)]
impl Component for VSpacerComponent {
    fn ui(&mut self, ctx: &mut BuildCtx) -> BlockBuilder<'_> {
        ctx.new().width(Size::Fit).height(Size::Fill)
    }
}

#[derive(Default)]
pub struct IconComponent;

#[ui_component("Icon", stateless)]
impl Component for IconComponent {
    fn ui(&mut self, ctx: &mut BuildCtx, icon: IconId, color: Color) -> BlockBuilder<'_> {
        ctx.new().fill(color).fill_texture(icon)
    }
}
