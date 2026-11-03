//! Full-screen message: white screen, centred lines from one third of the height down.
//! (The prototype's `draw_message`, pixel for pixel.)

use ink_device::Canvas;
use ink_text::{Fonts, Style};

/// Clears `canvas` and draws `lines` centred, 44 px, 1.6 line spacing, from y = h/3.
pub fn draw_message(canvas: &mut Canvas, fonts: &mut Fonts, lines: &[&str]) {
    canvas.clear(255);
    let px = 44.0;
    let mut y = canvas.height as f32 / 3.0;
    for line in lines {
        let w = fonts.text_width(Style::Regular, line, px);
        fonts.draw_text(canvas, Style::Regular, px, (canvas.width as f32 - w) / 2.0, y, line);
        y += px * 1.6;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_something_in_the_middle_third() {
        let mut c = Canvas::new(600, 900);
        let mut f = Fonts::load();
        draw_message(&mut c, &mut f, &["Opening", "book.epub"]);
        let dark_rows: Vec<usize> = (0..900).filter(|y| c.pixels[y * 600..(y + 1) * 600].iter().any(|v| *v < 100)).collect();
        assert!(dark_rows.first().unwrap() > &250 && dark_rows.last().unwrap() < &450, "{dark_rows:?}");
    }
}
