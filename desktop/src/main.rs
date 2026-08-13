//! SDL2 desktop frontend for the CHIP-8 core.
//!
//! Runs the machine at ~60 frames per second, executing a configurable
//! number of instructions per frame, ticking the timers once per frame,
//! and rendering the 64×32 framebuffer scaled up in an amber-on-charcoal
//! palette. The sound timer drives a square-wave beep.

use std::time::{Duration, Instant};

use chip8_core::{Chip8, DISPLAY_HEIGHT, DISPLAY_WIDTH};
use sdl2::audio::{AudioCallback, AudioSpecDesired};
use sdl2::event::Event;
use sdl2::keyboard::Keycode;
use sdl2::pixels::Color;
use sdl2::rect::Rect;

const LIT: Color = Color::RGB(0xF5, 0xA7, 0x3B);
const UNLIT: Color = Color::RGB(0x12, 0x16, 0x0F);
const FRAME: Duration = Duration::from_nanos(1_000_000_000 / 60);

struct SquareWave {
    phase: f32,
    phase_inc: f32,
    volume: f32,
}

impl AudioCallback for SquareWave {
    type Channel = f32;

    fn callback(&mut self, out: &mut [f32]) {
        for sample in out.iter_mut() {
            *sample = if self.phase < 0.5 { self.volume } else { -self.volume };
            self.phase = (self.phase + self.phase_inc) % 1.0;
        }
    }
}

/// Map a physical key to the CHIP-8 4×4 keypad (1234 / QWER / ASDF / ZXCV).
fn keymap(key: Keycode) -> Option<u8> {
    Some(match key {
        Keycode::Num1 => 0x1,
        Keycode::Num2 => 0x2,
        Keycode::Num3 => 0x3,
        Keycode::Num4 => 0xC,
        Keycode::Q => 0x4,
        Keycode::W => 0x5,
        Keycode::E => 0x6,
        Keycode::R => 0xD,
        Keycode::A => 0x7,
        Keycode::S => 0x8,
        Keycode::D => 0x9,
        Keycode::F => 0xE,
        Keycode::Z => 0xA,
        Keycode::X => 0x0,
        Keycode::C => 0xB,
        Keycode::V => 0xF,
        _ => return None,
    })
}

fn usage() -> ! {
    eprintln!("usage: iron-chip <rom.ch8> [--scale N] [--ipf N]");
    eprintln!("  --scale N   pixel size on screen (default 12)");
    eprintln!("  --ipf N     instructions per 60 Hz frame (default 11, ~700/s)");
    std::process::exit(2);
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut rom_path = None;
    let mut scale: u32 = 12;
    let mut ipf: u32 = 11;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--scale" => scale = args.next().and_then(|v| v.parse().ok()).unwrap_or_else(|| usage()),
            "--ipf" => ipf = args.next().and_then(|v| v.parse().ok()).unwrap_or_else(|| usage()),
            "--help" | "-h" => usage(),
            _ if rom_path.is_none() => rom_path = Some(arg),
            _ => usage(),
        }
    }
    let rom_path = rom_path.unwrap_or_else(|| usage());
    let rom = std::fs::read(&rom_path)?;

    let mut chip8 = Chip8::new(std::process::id());
    chip8.load_rom(&rom)?;

    let sdl = sdl2::init()?;
    let video = sdl.video()?;
    let window = video
        .window(
            &format!("iron-chip — {rom_path}"),
            DISPLAY_WIDTH as u32 * scale,
            DISPLAY_HEIGHT as u32 * scale,
        )
        .position_centered()
        .build()?;
    let mut canvas = window.into_canvas().present_vsync().build()?;

    let audio = sdl.audio()?;
    let desired = AudioSpecDesired {
        freq: Some(44_100),
        channels: Some(1),
        samples: None,
    };
    let device = audio.open_playback(None, &desired, |spec| SquareWave {
        phase: 0.0,
        phase_inc: 440.0 / spec.freq as f32,
        volume: 0.04,
    })?;

    let mut events = sdl.event_pump()?;
    let mut paused = false;

    'running: loop {
        let frame_start = Instant::now();

        for event in events.poll_iter() {
            match event {
                Event::Quit { .. }
                | Event::KeyDown { keycode: Some(Keycode::Escape), .. } => break 'running,
                Event::KeyDown { keycode: Some(Keycode::P), repeat: false, .. } => {
                    paused = !paused;
                }
                Event::KeyDown { keycode: Some(Keycode::Backspace), repeat: false, .. } => {
                    chip8.reset();
                    chip8.load_rom(&rom)?;
                }
                Event::KeyDown { keycode: Some(key), repeat: false, .. } => {
                    if let Some(k) = keymap(key) {
                        chip8.key_down(k);
                    }
                }
                Event::KeyUp { keycode: Some(key), .. } => {
                    if let Some(k) = keymap(key) {
                        chip8.key_up(k);
                    }
                }
                _ => {}
            }
        }

        if !paused {
            for _ in 0..ipf {
                chip8.step()?;
            }
            chip8.tick_timers();
        }

        if chip8.beeping() && !paused {
            device.resume();
        } else {
            device.pause();
        }

        canvas.set_draw_color(UNLIT);
        canvas.clear();
        canvas.set_draw_color(LIT);
        for (y, row) in chip8.display().chunks(DISPLAY_WIDTH).enumerate() {
            for (x, &lit) in row.iter().enumerate() {
                if lit {
                    canvas.fill_rect(Rect::new(
                        (x as u32 * scale) as i32,
                        (y as u32 * scale) as i32,
                        scale,
                        scale,
                    ))?;
                }
            }
        }
        canvas.present();

        let elapsed = frame_start.elapsed();
        if elapsed < FRAME {
            std::thread::sleep(FRAME - elapsed);
        }
    }

    Ok(())
}
