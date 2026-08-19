use chip8_core::{Chip8, DISPLAY_HEIGHT, DISPLAY_WIDTH};
use std::fs::File;
use std::io::BufWriter;

const SCALE: usize = 12;
const MARGIN: usize = 28;
const LIT: [u8; 3] = [0xF5, 0xA7, 0x3B];
const UNLIT: [u8; 3] = [0x1E, 0x24, 0x19]; // slightly lifted from bg for the pixel grid
const BG: [u8; 3] = [0x12, 0x16, 0x0F];

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rom = std::fs::read(&args[1]).expect("rom");
    let out = &args[2];
    let steps: usize = args[3].parse().unwrap();

    let mut chip8 = Chip8::new(0xC0FFEE);
    chip8.load_rom(&rom).unwrap();
    for cycle in 0..steps {
        chip8.step().unwrap();
        if cycle % 11 == 0 {
            chip8.tick_timers();
        }
    }

    let width = DISPLAY_WIDTH * SCALE + MARGIN * 2;
    let height = DISPLAY_HEIGHT * SCALE + MARGIN * 2;
    let mut buf = vec![0u8; width * height * 3];
    for y in 0..height {
        for x in 0..width {
            let idx = (y * width + x) * 3;
            let color = if x >= MARGIN && x < width - MARGIN && y >= MARGIN && y < height - MARGIN {
                let cx = (x - MARGIN) / SCALE;
                let cy = (y - MARGIN) / SCALE;
                // 1px gap between cells for a subtle pixel-grid feel
                let inner_x = (x - MARGIN) % SCALE;
                let inner_y = (y - MARGIN) % SCALE;
                let on = chip8.display()[cy * DISPLAY_WIDTH + cx];
                if on && inner_x < SCALE - 1 && inner_y < SCALE - 1 {
                    LIT
                } else if on {
                    [0x8a, 0x60, 0x25] // dimmed edge
                } else if inner_x < SCALE - 1 && inner_y < SCALE - 1 {
                    UNLIT
                } else {
                    BG
                }
            } else {
                BG
            };
            buf[idx..idx + 3].copy_from_slice(&color);
        }
    }

    let file = File::create(out).unwrap();
    let mut encoder = png::Encoder::new(BufWriter::new(file), width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&buf)
        .unwrap();
    println!("wrote {out} ({width}x{height})");
}
