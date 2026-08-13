//! WebAssembly + WebGL browser frontend for the CHIP-8 core.
//!
//! The same `chip8-core` that drives the SDL2 desktop app is compiled to
//! wasm here. Each animation frame, JavaScript calls [`Emulator::frame`],
//! which executes a batch of instructions, ticks the 60 Hz timers, uploads
//! the 64×32 framebuffer as a WebGL texture, and draws it onto a
//! full-canvas quad with an amber-phosphor palette in the fragment shader.

use chip8_core::{Chip8, DISPLAY_HEIGHT, DISPLAY_WIDTH};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{HtmlCanvasElement, WebGlProgram, WebGlRenderingContext as Gl, WebGlShader};

const VERTEX_SHADER: &str = r#"
attribute vec2 a_pos;
varying vec2 v_uv;
void main() {
    // Map clip space onto the texture with row 0 at the top.
    v_uv = vec2((a_pos.x + 1.0) * 0.5, (1.0 - a_pos.y) * 0.5);
    gl_Position = vec4(a_pos, 0.0, 1.0);
}
"#;

const FRAGMENT_SHADER: &str = r#"
precision mediump float;
varying vec2 v_uv;
uniform sampler2D u_framebuffer;
uniform vec3 u_lit;
uniform vec3 u_unlit;
void main() {
    float on = texture2D(u_framebuffer, v_uv).r;
    gl_FragColor = vec4(mix(u_unlit, u_lit, on), 1.0);
}
"#;

/// Amber phosphor on charcoal — the iron-chip palette.
const LIT: [f32; 3] = [0.961, 0.655, 0.231]; // #F5A73B
const UNLIT: [f32; 3] = [0.071, 0.086, 0.059]; // #12160F

fn compile_shader(gl: &Gl, kind: u32, source: &str) -> Result<WebGlShader, String> {
    let shader = gl.create_shader(kind).ok_or("failed to create shader")?;
    gl.shader_source(&shader, source);
    gl.compile_shader(&shader);
    if gl
        .get_shader_parameter(&shader, Gl::COMPILE_STATUS)
        .as_bool()
        .unwrap_or(false)
    {
        Ok(shader)
    } else {
        Err(gl
            .get_shader_info_log(&shader)
            .unwrap_or_else(|| "unknown shader compile error".into()))
    }
}

fn link_program(gl: &Gl, vertex: &str, fragment: &str) -> Result<WebGlProgram, String> {
    let program = gl.create_program().ok_or("failed to create program")?;
    gl.attach_shader(&program, &compile_shader(gl, Gl::VERTEX_SHADER, vertex)?);
    gl.attach_shader(
        &program,
        &compile_shader(gl, Gl::FRAGMENT_SHADER, fragment)?,
    );
    gl.link_program(&program);
    if gl
        .get_program_parameter(&program, Gl::LINK_STATUS)
        .as_bool()
        .unwrap_or(false)
    {
        Ok(program)
    } else {
        Err(gl
            .get_program_info_log(&program)
            .unwrap_or_else(|| "unknown program link error".into()))
    }
}

#[wasm_bindgen]
pub struct Emulator {
    chip8: Chip8,
    gl: Gl,
    pixels: [u8; DISPLAY_WIDTH * DISPLAY_HEIGHT],
}

#[wasm_bindgen]
impl Emulator {
    /// Set up the VM and the WebGL pipeline on the given canvas.
    #[wasm_bindgen(constructor)]
    pub fn new(canvas: HtmlCanvasElement, seed: u32) -> Result<Emulator, JsValue> {
        let gl: Gl = canvas
            .get_context("webgl")?
            .ok_or_else(|| JsValue::from_str("WebGL is not available"))?
            .dyn_into()?;

        let program =
            link_program(&gl, VERTEX_SHADER, FRAGMENT_SHADER).map_err(|e| JsValue::from_str(&e))?;
        gl.use_program(Some(&program));

        // A full-canvas quad as a triangle strip.
        let vertices: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
        let buffer = gl.create_buffer().ok_or("failed to create buffer")?;
        gl.bind_buffer(Gl::ARRAY_BUFFER, Some(&buffer));
        gl.buffer_data_with_array_buffer_view(
            Gl::ARRAY_BUFFER,
            &js_sys::Float32Array::from(&vertices[..]),
            Gl::STATIC_DRAW,
        );
        let a_pos = gl.get_attrib_location(&program, "a_pos") as u32;
        gl.enable_vertex_attrib_array(a_pos);
        gl.vertex_attrib_pointer_with_i32(a_pos, 2, Gl::FLOAT, false, 0, 0);

        // The framebuffer lives in a 64×32 single-channel texture.
        let texture = gl.create_texture().ok_or("failed to create texture")?;
        gl.bind_texture(Gl::TEXTURE_2D, Some(&texture));
        gl.tex_parameteri(Gl::TEXTURE_2D, Gl::TEXTURE_MIN_FILTER, Gl::NEAREST as i32);
        gl.tex_parameteri(Gl::TEXTURE_2D, Gl::TEXTURE_MAG_FILTER, Gl::NEAREST as i32);
        gl.tex_parameteri(Gl::TEXTURE_2D, Gl::TEXTURE_WRAP_S, Gl::CLAMP_TO_EDGE as i32);
        gl.tex_parameteri(Gl::TEXTURE_2D, Gl::TEXTURE_WRAP_T, Gl::CLAMP_TO_EDGE as i32);

        gl.uniform3fv_with_f32_array(gl.get_uniform_location(&program, "u_lit").as_ref(), &LIT);
        gl.uniform3fv_with_f32_array(
            gl.get_uniform_location(&program, "u_unlit").as_ref(),
            &UNLIT,
        );
        gl.viewport(0, 0, canvas.width() as i32, canvas.height() as i32);

        let mut emulator = Emulator {
            chip8: Chip8::new(seed),
            gl,
            pixels: [0; DISPLAY_WIDTH * DISPLAY_HEIGHT],
        };
        emulator.render()?;
        Ok(emulator)
    }

    /// Reset the machine and load a new ROM.
    pub fn load_rom(&mut self, rom: &[u8]) -> Result<(), JsValue> {
        self.chip8.reset();
        self.chip8
            .load_rom(rom)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Run one 60 Hz frame: `instructions` CPU steps, one timer tick, redraw.
    pub fn frame(&mut self, instructions: u32) -> Result<(), JsValue> {
        for _ in 0..instructions {
            self.chip8
                .step()
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
        }
        self.chip8.tick_timers();
        self.render()
    }

    pub fn key_down(&mut self, key: u8) {
        self.chip8.key_down(key);
    }

    pub fn key_up(&mut self, key: u8) {
        self.chip8.key_up(key);
    }

    pub fn beeping(&self) -> bool {
        self.chip8.beeping()
    }

    /// Restart the currently loaded ROM.
    pub fn reset(&mut self) {
        self.chip8.reset();
    }

    fn render(&mut self) -> Result<(), JsValue> {
        for (pixel, &lit) in self.pixels.iter_mut().zip(self.chip8.display().iter()) {
            *pixel = if lit { 0xFF } else { 0x00 };
        }
        self.gl
            .tex_image_2d_with_i32_and_i32_and_i32_and_format_and_type_and_opt_u8_array(
                Gl::TEXTURE_2D,
                0,
                Gl::LUMINANCE as i32,
                DISPLAY_WIDTH as i32,
                DISPLAY_HEIGHT as i32,
                0,
                Gl::LUMINANCE,
                Gl::UNSIGNED_BYTE,
                Some(&self.pixels),
            )?;
        self.gl.draw_arrays(Gl::TRIANGLE_STRIP, 0, 4);
        Ok(())
    }
}
