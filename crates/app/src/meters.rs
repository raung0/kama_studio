use kama_ui::{BuildCtx, Color, Rect, Size};

use crate::theme;

pub struct MetersState {
    level: [f32; 2],
    peak: [f32; 2],
    hold: [f32; 2],
}

impl Default for MetersState {
    fn default() -> Self {
        Self {
            level: [0.0; 2],
            peak: [0.0; 2],
            hold: [0.0; 2],
        }
    }
}

impl MetersState {
    pub fn tick(&mut self, input: [f32; 2], dt: f32) {
        for channel in 0..2 {
            let input = input[channel].max(0.0);
            self.level[channel] = if input >= self.level[channel] {
                input
            } else {
                self.level[channel] * (-3.8 * dt).exp()
            };
            if input >= self.peak[channel] {
                self.peak[channel] = input;
                self.hold[channel] = 1.1;
            } else if self.hold[channel] > 0.0 {
                self.hold[channel] = (self.hold[channel] - dt).max(0.0);
            } else {
                self.peak[channel] *= (-1.35 * dt).exp();
            }
        }
    }

    pub fn is_animating(&self) -> bool {
        self.level
            .iter()
            .chain(self.peak.iter())
            .any(|level| *level > 0.000_5)
    }

    pub fn build(&self, ctx: &mut BuildCtx, rect: Rect) {
        let local = Rect::new(0.0, 0.0, rect.width, rect.height);
        kama_ui::ui!(ctx, {
            Rect("meters-bg", local) {
                fill: theme::panel();
            }
        });
        let padding = 10.0;
        let gap = 10.0;
        let meter_h = (local.height - padding * 2.0).max(1.0);
        let meter_w = (((local.width - padding * 2.0 - gap).max(2.0)) * 0.5).min(96.0);
        let meter_parts = kama_ui::layout::row(
            local,
            &[
                kama_ui::layout::Item::fill(),
                kama_ui::layout::Item::new(Size::Pixels(meter_w), Size::Pixels(meter_h)),
                kama_ui::layout::Item::width(gap),
                kama_ui::layout::Item::new(Size::Pixels(meter_w), Size::Pixels(meter_h)),
                kama_ui::layout::Item::fill(),
            ],
            0.0,
            0.0,
            kama_ui::Align::Center,
        );
        for (channel, meter) in [meter_parts[1], meter_parts[3]].into_iter().enumerate() {
            let fill_h = meter.height * db_amount(self.level[channel]);
            let fill = Rect::new(
                meter.x + 4.0,
                meter.bottom() - fill_h + 4.0,
                meter.width - 8.0,
                (fill_h - 8.0).max(0.0),
            );
            let level_color = if self.level[channel] >= 1.0 {
                Color::rgb8(0xf0, 0x4d, 0x3e)
            } else if self.level[channel] >= 0.707 {
                theme::accent()
            } else {
                Color::rgb8(0x48, 0xb8, 0x63)
            };
            let peak_y = meter.bottom() - meter.height * db_amount(self.peak[channel]);
            kama_ui::ui!(ctx, {
                Rect(("master-meter-bg", channel), meter) {
                    fill: theme::control(); border: 1; border_color: theme::line(); border_radius: 4.0;
                }
                @if fill.height > 0.0 {
                    Rect(("master-meter-fill", channel), fill) {
                        fill: level_color;
                        border_radius: 2.0;
                    }
                }
                Rect(("master-meter-peak", channel), Rect::new(
                    meter.x + 3.0, peak_y.clamp(meter.y, meter.bottom() - 2.0), meter.width - 6.0, 2.0,
                )) { fill: if self.peak[channel] >= 1.0 { Color::rgb8(0xff, 0x3a, 0x32) } else { theme::accent() }; }
                @for db in [-48.0_f32, -24.0, -12.0, -6.0, -3.0, 0.0] {
                    Rect(("master-meter-tick", channel, db.to_bits()), Rect::new(
                        meter.x - 3.0, meter.bottom() - meter.height * ((db + 60.0) / 60.0), 3.0, 1.0,
                    )) { fill: theme::muted(); }
                }
                Rect(("master-meter-db", channel), meter) {
                    font_size: 9.0; text_color: theme::text(); text_centered;
                    text: format!("{:.1} dB", amplitude_db(self.level[channel]));
                }
            });
        }
    }
}

fn amplitude_db(amplitude: f32) -> f32 {
    20.0 * amplitude.max(0.001).log10()
}
fn db_amount(amplitude: f32) -> f32 {
    ((amplitude_db(amplitude).clamp(-60.0, 0.0) + 60.0) / 60.0).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::MetersState;
    #[test]
    fn meter_attack_is_immediate_and_decay_is_slower() {
        let mut meter = MetersState::default();
        meter.tick([0.8, 0.4], 0.016);
        assert_eq!(meter.level, [0.8, 0.4]);
        meter.tick([0.0, 0.0], 0.016);
        assert!(meter.level[0] > 0.7);
    }
}
