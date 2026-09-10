use std::num::NonZeroU16;

use ratatui::{
    buffer::{Buffer, CellDiffOption},
    layout::Rect,
    widgets::Widget,
};
use ratatui_image::{Image, protocol::Protocol};

/// Native protocols put escape sequences and image payloads in the first cell
/// of each row. Their string lengths are not terminal cell widths. Without an
/// explicit width, Ratatui's diff skips subsequent rows (and even the footer).
pub(super) struct Graphic<'a>(pub &'a Protocol);

impl Widget for Graphic<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Image::new(self.0).render(area, buf);
        if matches!(self.0, Protocol::Halfblocks(_)) {
            return;
        }
        for y in area.top()..area.bottom() {
            if let Some(cell) = buf.cell_mut((area.x, y))
                && cell.symbol().contains('\x1b')
            {
                // The protocol already marks its covered cells as skipped.
                cell.set_diff_option(CellDiffOption::ForcedWidth(NonZeroU16::MIN));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, Rgb, RgbImage};
    use ratatui::{style::Color, widgets::Paragraph};
    use ratatui_image::{
        Resize,
        picker::{Picker, ProtocolType},
    };

    #[test]
    fn kitty_diff_emits_every_image_row_and_footer_on_first_draw_and_repaint() {
        let mut picker = Picker::halfblocks();
        picker.set_protocol_type(ProtocolType::Kitty);
        let area = Rect::new(0, 0, 40, 10);
        let content = Rect::new(0, 1, 40, 8);
        let image = picker
            .new_protocol(
                DynamicImage::ImageRgb8(RgbImage::from_pixel(400, 160, Rgb([240, 60, 30]))),
                content,
                Resize::Fit(None),
            )
            .unwrap();
        let mut previous = Buffer::empty(area);
        for iteration in 0..3 {
            if iteration == 2 {
                previous = Buffer::empty(area); // Ctrl+L or modal dismissal.
            }
            let mut next = Buffer::empty(area);
            Graphic(&image).render(content, &mut next);
            Paragraph::new("Esc/q Close")
                .style(Color::White)
                .render(Rect::new(0, 9, 40, 1), &mut next);
            let diff = previous.diff(&next);
            if iteration != 1 {
                let rows: Vec<_> = diff
                    .iter()
                    .filter(|(_, _, c)| c.symbol().contains('\u{10eeee}'))
                    .map(|(_, y, _)| *y)
                    .collect();
                assert_eq!(rows, (1..9).collect::<Vec<_>>());
                assert!(diff.iter().any(|(_, y, c)| *y == 9 && c.symbol() == "E"));
            }
            assert_eq!(
                diff.iter().any(|(_, _, c)| c.symbol().contains("a=T")),
                iteration == 0
            );
            previous = next;
        }
    }
}
