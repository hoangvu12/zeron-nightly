//! A fixed frame (a filled rounded rectangle and a line of text) drawn,
//! encoded as a PNG, read back and probed at known pixels.

use zeron_design::skia_safe::image::CachingHint;
use zeron_design::skia_safe::textlayout::{ParagraphBuilder, ParagraphStyle, TextStyle};
use zeron_design::skia_safe::{Color, ColorType, Data, Image, Paint, RRect, Rect};

// The zgx app's own Geist, so the text needs no font of this computer's.
const GEIST: &[u8] = include_bytes!("../../../apps/zgx/fonts/Geist.ttf");

const W: i32 = 320;
const H: i32 = 160;
const CARD: Color = Color::from_rgb(79, 70, 229);
const INK: Color = Color::WHITE;

struct Frame {
    pixels: Vec<u8>,
    width: i32,
    /// Where the text's line ends (its left edge is at 40).
    text_right: f32,
}

impl Frame {
    fn rgba(&self, x: i32, y: i32) -> [u8; 4] {
        let at = ((y * self.width + x) * 4) as usize;
        self.pixels[at..at + 4].try_into().unwrap()
    }
}

/// Draws the frame, encodes it and decodes the PNG back to RGBA.
fn render() -> (Vec<u8>, Frame) {
    let mut surface = zeron_design::surface(W, H).expect("raster surface");
    let canvas = surface.canvas();
    let mut fill = Paint::default();
    fill.set_anti_alias(true).set_color(CARD);
    canvas.draw_rrect(
        RRect::new_rect_xy(Rect::from_xywh(20., 20., 280., 120.), 24., 24.),
        &fill,
    );

    let mut style = TextStyle::new();
    style
        .set_font_families(&["Geist"])
        .set_font_size(32.)
        .set_color(INK);
    let mut paragraph_style = ParagraphStyle::new();
    paragraph_style.set_text_style(&style);
    let mut builder = ParagraphBuilder::new(&paragraph_style, zeron_design::fonts(&[GEIST]));
    builder.push_style(&style);
    builder.add_text("Hello, Design");
    let mut paragraph = builder.build();
    paragraph.layout(240.);
    paragraph.paint(canvas, (40., 60.));
    let text_right = 40. + paragraph.longest_line();

    let png = zeron_design::png(&mut surface).expect("png");
    let image = Image::from_encoded(Data::new_copy(&png)).expect("decodes");
    let info = image.image_info().with_color_type(ColorType::RGBA8888);
    let mut pixels = vec![0; (W * H * 4) as usize];
    assert!(image.read_pixels(
        &info,
        &mut pixels,
        (W * 4) as usize,
        (0, 0),
        CachingHint::Allow
    ));
    (
        png,
        Frame {
            pixels,
            width: W,
            text_right,
        },
    )
}

#[test]
fn renders_a_rounded_rectangle_and_a_line_of_text() {
    let (png, frame) = render();
    assert_eq!(&png[1..4], b"PNG");

    // The rectangle's fill inside it, and nothing outside it or in its
    // rounded corner.
    assert_eq!(frame.rgba(30, 130), [79, 70, 229, 255]);
    assert_eq!(frame.rgba(290, 30), [79, 70, 229, 255]);
    assert_eq!(frame.rgba(10, 10)[3], 0);
    assert_eq!(frame.rgba(22, 22)[3], 0);

    // The text: light pixels along its line, none past its end.
    let lit = |x0: i32, x1: i32| {
        (x0..x1)
            .flat_map(|x| (60..100).map(move |y| (x, y)))
            .filter(|&(x, y)| frame.rgba(x, y)[0] > 200)
            .count()
    };
    let right = frame.text_right.ceil() as i32;
    assert!(right > 150 && right < 290, "text ends at {right}");
    assert!(
        lit(40, right) > 300,
        "{} lit pixels in the text",
        lit(40, right)
    );
    assert_eq!(lit(right + 2, 290), 0);
}
