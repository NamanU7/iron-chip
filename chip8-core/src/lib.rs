//! A CHIP-8 virtual machine.
//!
//! The interpreter owns the full machine state — 4 KiB of memory, sixteen
//! 8-bit registers, the index register, program counter, call stack, delay
//! and sound timers, a 64×32 monochrome framebuffer, and the 16-key keypad —
//! and executes the complete CHIP-8 instruction set one fetch/decode/execute
//! cycle at a time.
//!
//! The core is frontend-agnostic: it knows nothing about windows, audio, or
//! input devices. Frontends drive it by calling [`Chip8::step`] at their
//! chosen instruction rate, [`Chip8::tick_timers`] at 60 Hz, and rendering
//! [`Chip8::display`] however they like. This is what lets the same core run
//! under SDL2 on the desktop and WebGL in the browser.

pub const DISPLAY_WIDTH: usize = 64;
pub const DISPLAY_HEIGHT: usize = 32;

const MEMORY_SIZE: usize = 4096;
const STACK_DEPTH: usize = 16;
const PROGRAM_START: usize = 0x200;
const FONT_START: usize = 0x050;

/// The built-in hexadecimal font: sprites 0–F, five bytes each.
const FONT: [u8; 80] = [
    0xF0, 0x90, 0x90, 0x90, 0xF0, // 0
    0x20, 0x60, 0x20, 0x20, 0x70, // 1
    0xF0, 0x10, 0xF0, 0x80, 0xF0, // 2
    0xF0, 0x10, 0xF0, 0x10, 0xF0, // 3
    0x90, 0x90, 0xF0, 0x10, 0x10, // 4
    0xF0, 0x80, 0xF0, 0x10, 0xF0, // 5
    0xF0, 0x80, 0xF0, 0x90, 0xF0, // 6
    0xF0, 0x10, 0x20, 0x40, 0x40, // 7
    0xF0, 0x90, 0xF0, 0x90, 0xF0, // 8
    0xF0, 0x90, 0xF0, 0x10, 0xF0, // 9
    0xF0, 0x90, 0xF0, 0x90, 0x90, // A
    0xE0, 0x90, 0xE0, 0x90, 0xE0, // B
    0xF0, 0x80, 0x80, 0x80, 0xF0, // C
    0xE0, 0x90, 0x90, 0x90, 0xE0, // D
    0xF0, 0x80, 0xF0, 0x80, 0xF0, // E
    0xF0, 0x80, 0xF0, 0x80, 0x80, // F
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chip8Error {
    /// The fetched 16-bit word does not decode to any CHIP-8 instruction.
    UnknownOpcode(u16),
    /// A `CALL` was executed with all sixteen stack slots in use.
    StackOverflow,
    /// A `RET` was executed with an empty call stack.
    StackUnderflow,
    /// The ROM does not fit in the 3,584 bytes of program memory.
    RomTooLarge(usize),
}

impl core::fmt::Display for Chip8Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Chip8Error::UnknownOpcode(op) => write!(f, "unknown opcode {op:#06X}"),
            Chip8Error::StackOverflow => write!(f, "call stack overflow"),
            Chip8Error::StackUnderflow => write!(f, "return with empty call stack"),
            Chip8Error::RomTooLarge(n) => {
                write!(f, "ROM is {n} bytes; at most {} fit", MEMORY_SIZE - PROGRAM_START)
            }
        }
    }
}

impl std::error::Error for Chip8Error {}

pub struct Chip8 {
    memory: [u8; MEMORY_SIZE],
    /// General-purpose registers V0–VF. VF doubles as the flag register.
    v: [u8; 16],
    /// Index register, used for memory addressing.
    i: u16,
    pc: u16,
    stack: [u16; STACK_DEPTH],
    sp: u8,
    delay_timer: u8,
    sound_timer: u8,
    display: [bool; DISPLAY_WIDTH * DISPLAY_HEIGHT],
    keys: [bool; 16],
    /// `Some(x)` while `Fx0A` is blocked waiting to store a key in Vx.
    waiting_for_key: Option<u8>,
    /// xorshift32 state for the `RND` instruction.
    rng: u32,
}

impl Chip8 {
    /// Create a machine with the font loaded and the PC at 0x200.
    ///
    /// `seed` drives the deterministic PRNG behind `Cxkk RND`; any value is
    /// fine (zero is remapped, since xorshift has a fixed point there).
    pub fn new(seed: u32) -> Self {
        let mut memory = [0u8; MEMORY_SIZE];
        memory[FONT_START..FONT_START + FONT.len()].copy_from_slice(&FONT);
        Self {
            memory,
            v: [0; 16],
            i: 0,
            pc: PROGRAM_START as u16,
            stack: [0; STACK_DEPTH],
            sp: 0,
            delay_timer: 0,
            sound_timer: 0,
            display: [false; DISPLAY_WIDTH * DISPLAY_HEIGHT],
            keys: [false; 16],
            waiting_for_key: None,
            rng: if seed == 0 { 0x2A2A_2A2A } else { seed },
        }
    }

    /// Reset everything except the loaded ROM image and RNG state.
    pub fn reset(&mut self) {
        self.v = [0; 16];
        self.i = 0;
        self.pc = PROGRAM_START as u16;
        self.stack = [0; STACK_DEPTH];
        self.sp = 0;
        self.delay_timer = 0;
        self.sound_timer = 0;
        self.display = [false; DISPLAY_WIDTH * DISPLAY_HEIGHT];
        self.keys = [false; 16];
        self.waiting_for_key = None;
    }

    /// Copy a ROM into memory starting at 0x200.
    pub fn load_rom(&mut self, rom: &[u8]) -> Result<(), Chip8Error> {
        if rom.len() > MEMORY_SIZE - PROGRAM_START {
            return Err(Chip8Error::RomTooLarge(rom.len()));
        }
        self.memory[PROGRAM_START..PROGRAM_START + rom.len()].copy_from_slice(rom);
        Ok(())
    }

    /// The 64×32 framebuffer, row-major; `true` means the pixel is lit.
    pub fn display(&self) -> &[bool; DISPLAY_WIDTH * DISPLAY_HEIGHT] {
        &self.display
    }

    pub fn beeping(&self) -> bool {
        self.sound_timer > 0
    }

    pub fn key_down(&mut self, key: u8) {
        if key < 16 {
            self.keys[key as usize] = true;
            // Fx0A resolves on the first key press seen while waiting.
            if let Some(x) = self.waiting_for_key.take() {
                self.v[x as usize] = key;
            }
        }
    }

    pub fn key_up(&mut self, key: u8) {
        if key < 16 {
            self.keys[key as usize] = false;
        }
    }

    /// Decrement the delay and sound timers. Call at 60 Hz.
    pub fn tick_timers(&mut self) {
        self.delay_timer = self.delay_timer.saturating_sub(1);
        self.sound_timer = self.sound_timer.saturating_sub(1);
    }

    fn rand(&mut self) -> u8 {
        // xorshift32 (Marsaglia): small, fast, and deterministic per seed.
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        (x >> 24) as u8
    }

    /// Fetch, decode, and execute a single instruction.
    ///
    /// While `Fx0A` is waiting for a key press this is a no-op, so frontends
    /// can keep calling it at their normal rate.
    pub fn step(&mut self) -> Result<(), Chip8Error> {
        if self.waiting_for_key.is_some() {
            return Ok(());
        }

        let pc = self.pc as usize & (MEMORY_SIZE - 1);
        let opcode =
            u16::from_be_bytes([self.memory[pc], self.memory[(pc + 1) & (MEMORY_SIZE - 1)]]);
        self.pc = self.pc.wrapping_add(2) & 0x0FFF;

        let nnn = opcode & 0x0FFF;
        let kk = (opcode & 0x00FF) as u8;
        let n = (opcode & 0x000F) as usize;
        let x = ((opcode >> 8) & 0x000F) as usize;
        let y = ((opcode >> 4) & 0x000F) as usize;

        match opcode & 0xF000 {
            0x0000 => match opcode {
                // 00E0 — CLS: clear the display.
                0x00E0 => self.display = [false; DISPLAY_WIDTH * DISPLAY_HEIGHT],
                // 00EE — RET: return from subroutine.
                0x00EE => {
                    if self.sp == 0 {
                        return Err(Chip8Error::StackUnderflow);
                    }
                    self.sp -= 1;
                    self.pc = self.stack[self.sp as usize];
                }
                // 0nnn (SYS) called into machine code on the original COSMAC
                // VIP; it is universally ignored by interpreters.
                _ => {}
            },
            // 1nnn — JP addr.
            0x1000 => self.pc = nnn,
            // 2nnn — CALL addr.
            0x2000 => {
                if self.sp as usize >= STACK_DEPTH {
                    return Err(Chip8Error::StackOverflow);
                }
                self.stack[self.sp as usize] = self.pc;
                self.sp += 1;
                self.pc = nnn;
            }
            // 3xkk — SE Vx, byte: skip next if equal.
            0x3000 => {
                if self.v[x] == kk {
                    self.pc = self.pc.wrapping_add(2) & 0x0FFF;
                }
            }
            // 4xkk — SNE Vx, byte: skip next if not equal.
            0x4000 => {
                if self.v[x] != kk {
                    self.pc = self.pc.wrapping_add(2) & 0x0FFF;
                }
            }
            // 5xy0 — SE Vx, Vy.
            0x5000 if n == 0 => {
                if self.v[x] == self.v[y] {
                    self.pc = self.pc.wrapping_add(2) & 0x0FFF;
                }
            }
            // 6xkk — LD Vx, byte.
            0x6000 => self.v[x] = kk,
            // 7xkk — ADD Vx, byte (no carry flag).
            0x7000 => self.v[x] = self.v[x].wrapping_add(kk),
            0x8000 => match n {
                // 8xy0 — LD Vx, Vy.
                0x0 => self.v[x] = self.v[y],
                // 8xy1 — OR Vx, Vy.
                0x1 => self.v[x] |= self.v[y],
                // 8xy2 — AND Vx, Vy.
                0x2 => self.v[x] &= self.v[y],
                // 8xy3 — XOR Vx, Vy.
                0x3 => self.v[x] ^= self.v[y],
                // 8xy4 — ADD Vx, Vy; VF = carry.
                0x4 => {
                    let (sum, carry) = self.v[x].overflowing_add(self.v[y]);
                    self.v[x] = sum;
                    self.v[0xF] = carry as u8;
                }
                // 8xy5 — SUB Vx, Vy; VF = NOT borrow.
                0x5 => {
                    let (diff, borrow) = self.v[x].overflowing_sub(self.v[y]);
                    self.v[x] = diff;
                    self.v[0xF] = (!borrow) as u8;
                }
                // 8xy6 — SHR Vx; VF = bit shifted out.
                0x6 => {
                    let bit = self.v[x] & 1;
                    self.v[x] >>= 1;
                    self.v[0xF] = bit;
                }
                // 8xy7 — SUBN Vx, Vy: Vx = Vy - Vx; VF = NOT borrow.
                0x7 => {
                    let (diff, borrow) = self.v[y].overflowing_sub(self.v[x]);
                    self.v[x] = diff;
                    self.v[0xF] = (!borrow) as u8;
                }
                // 8xyE — SHL Vx; VF = bit shifted out.
                0xE => {
                    let bit = self.v[x] >> 7;
                    self.v[x] <<= 1;
                    self.v[0xF] = bit;
                }
                _ => return Err(Chip8Error::UnknownOpcode(opcode)),
            },
            // 9xy0 — SNE Vx, Vy.
            0x9000 if n == 0 => {
                if self.v[x] != self.v[y] {
                    self.pc = self.pc.wrapping_add(2) & 0x0FFF;
                }
            }
            // Annn — LD I, addr.
            0xA000 => self.i = nnn,
            // Bnnn — JP V0, addr.
            0xB000 => self.pc = nnn.wrapping_add(self.v[0] as u16) & 0x0FFF,
            // Cxkk — RND Vx, byte: random byte AND kk.
            0xC000 => self.v[x] = self.rand() & kk,
            // Dxyn — DRW Vx, Vy, n: XOR an n-byte sprite at (Vx, Vy);
            // VF = collision. Start coordinates wrap; the sprite clips at
            // the display edges.
            0xD000 => {
                let x0 = self.v[x] as usize % DISPLAY_WIDTH;
                let y0 = self.v[y] as usize % DISPLAY_HEIGHT;
                self.v[0xF] = 0;
                for row in 0..n {
                    let py = y0 + row;
                    if py >= DISPLAY_HEIGHT {
                        break;
                    }
                    let sprite = self.memory[(self.i as usize + row) & (MEMORY_SIZE - 1)];
                    for bit in 0..8 {
                        let px = x0 + bit;
                        if px >= DISPLAY_WIDTH {
                            break;
                        }
                        if sprite & (0x80 >> bit) != 0 {
                            let idx = py * DISPLAY_WIDTH + px;
                            if self.display[idx] {
                                self.v[0xF] = 1;
                            }
                            self.display[idx] ^= true;
                        }
                    }
                }
            }
            0xE000 => match kk {
                // Ex9E — SKP Vx: skip if the key in Vx is down.
                0x9E => {
                    if self.keys[(self.v[x] & 0xF) as usize] {
                        self.pc = self.pc.wrapping_add(2) & 0x0FFF;
                    }
                }
                // ExA1 — SKNP Vx: skip if the key in Vx is up.
                0xA1 => {
                    if !self.keys[(self.v[x] & 0xF) as usize] {
                        self.pc = self.pc.wrapping_add(2) & 0x0FFF;
                    }
                }
                _ => return Err(Chip8Error::UnknownOpcode(opcode)),
            },
            0xF000 => match kk {
                // Fx07 — LD Vx, DT.
                0x07 => self.v[x] = self.delay_timer,
                // Fx0A — LD Vx, K: block until a key is pressed.
                0x0A => self.waiting_for_key = Some(x as u8),
                // Fx15 — LD DT, Vx.
                0x15 => self.delay_timer = self.v[x],
                // Fx18 — LD ST, Vx.
                0x18 => self.sound_timer = self.v[x],
                // Fx1E — ADD I, Vx.
                0x1E => self.i = self.i.wrapping_add(self.v[x] as u16) & 0x0FFF,
                // Fx29 — LD F, Vx: I = address of the font sprite for Vx.
                0x29 => self.i = (FONT_START + (self.v[x] & 0xF) as usize * 5) as u16,
                // Fx33 — LD B, Vx: BCD of Vx into memory[I..I+3].
                0x33 => {
                    let value = self.v[x];
                    let i = self.i as usize;
                    self.memory[i & (MEMORY_SIZE - 1)] = value / 100;
                    self.memory[(i + 1) & (MEMORY_SIZE - 1)] = (value / 10) % 10;
                    self.memory[(i + 2) & (MEMORY_SIZE - 1)] = value % 10;
                }
                // Fx55 — LD [I], V0..Vx. (I itself is left unchanged, the
                // common modern behavior.)
                0x55 => {
                    let i = self.i as usize;
                    for (offset, &value) in self.v[..=x].iter().enumerate() {
                        self.memory[(i + offset) & (MEMORY_SIZE - 1)] = value;
                    }
                }
                // Fx65 — LD V0..Vx, [I].
                0x65 => {
                    let i = self.i as usize;
                    for (offset, register) in self.v[..=x].iter_mut().enumerate() {
                        *register = self.memory[(i + offset) & (MEMORY_SIZE - 1)];
                    }
                }
                _ => return Err(Chip8Error::UnknownOpcode(opcode)),
            },
            _ => return Err(Chip8Error::UnknownOpcode(opcode)),
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a machine executing the given opcodes from 0x200.
    fn machine(opcodes: &[u16]) -> Chip8 {
        let mut chip8 = Chip8::new(1);
        let rom: Vec<u8> = opcodes.iter().flat_map(|op| op.to_be_bytes()).collect();
        chip8.load_rom(&rom).unwrap();
        chip8
    }

    fn run(chip8: &mut Chip8, steps: usize) {
        for _ in 0..steps {
            chip8.step().unwrap();
        }
    }

    #[test]
    fn load_and_add_immediate() {
        let mut c = machine(&[0x6A2B, 0x7A05]); // LD VA, 0x2B; ADD VA, 5
        run(&mut c, 2);
        assert_eq!(c.v[0xA], 0x30);
    }

    #[test]
    fn add_immediate_wraps_without_flag() {
        let mut c = machine(&[0x60FF, 0x7002]); // LD V0, 0xFF; ADD V0, 2
        c.v[0xF] = 7;
        run(&mut c, 2);
        assert_eq!(c.v[0], 1);
        assert_eq!(c.v[0xF], 7, "7xkk must not touch VF");
    }

    #[test]
    fn jump_and_jump_v0() {
        let mut c = machine(&[0x1300]); // JP 0x300
        run(&mut c, 1);
        assert_eq!(c.pc, 0x300);

        let mut c = machine(&[0x6005, 0xB300]); // LD V0, 5; JP V0, 0x300
        run(&mut c, 2);
        assert_eq!(c.pc, 0x305);
    }

    #[test]
    fn call_and_return() {
        let mut c = machine(&[0x2206, 0x0000, 0x0000, 0x00EE]); // CALL 0x206 ... RET
        run(&mut c, 1);
        assert_eq!(c.pc, 0x206);
        assert_eq!(c.sp, 1);
        run(&mut c, 1); // RET
        assert_eq!(c.pc, 0x202);
        assert_eq!(c.sp, 0);
    }

    #[test]
    fn stack_underflow_and_overflow() {
        let mut c = machine(&[0x00EE]);
        assert_eq!(c.step(), Err(Chip8Error::StackUnderflow));

        // CALL 0x200 forever: the 17th call must overflow.
        let mut c = machine(&[0x2200]);
        for _ in 0..16 {
            c.step().unwrap();
        }
        assert_eq!(c.step(), Err(Chip8Error::StackOverflow));
    }

    #[test]
    fn skip_instructions() {
        // SE taken: V0 == 0x11 skips the JP.
        let mut c = machine(&[0x6011, 0x3011, 0x1200, 0x6499]);
        run(&mut c, 3);
        assert_eq!(c.v[4], 0x99);

        // SNE not taken: V0 == 0x11 falls through into LD V4.
        let mut c = machine(&[0x6011, 0x4011, 0x6499]);
        run(&mut c, 3);
        assert_eq!(c.v[4], 0x99);

        // 5xy0 / 9xy0 register compares.
        let mut c = machine(&[0x6007, 0x6107, 0x5010, 0x1200, 0x6499]);
        run(&mut c, 4);
        assert_eq!(c.v[4], 0x99);
    }

    #[test]
    fn logic_ops() {
        let mut c = machine(&[0x60CC, 0x61AA, 0x8011]); // OR
        run(&mut c, 3);
        assert_eq!(c.v[0], 0xEE);

        let mut c = machine(&[0x60CC, 0x61AA, 0x8012]); // AND
        run(&mut c, 3);
        assert_eq!(c.v[0], 0x88);

        let mut c = machine(&[0x60CC, 0x61AA, 0x8013]); // XOR
        run(&mut c, 3);
        assert_eq!(c.v[0], 0x66);
    }

    #[test]
    fn add_with_carry() {
        let mut c = machine(&[0x60FF, 0x6101, 0x8014]);
        run(&mut c, 3);
        assert_eq!(c.v[0], 0x00);
        assert_eq!(c.v[0xF], 1);

        let mut c = machine(&[0x6010, 0x6101, 0x8014]);
        run(&mut c, 3);
        assert_eq!(c.v[0], 0x11);
        assert_eq!(c.v[0xF], 0);
    }

    #[test]
    fn sub_sets_not_borrow() {
        let mut c = machine(&[0x600A, 0x6103, 0x8015]); // 10 - 3
        run(&mut c, 3);
        assert_eq!(c.v[0], 7);
        assert_eq!(c.v[0xF], 1);

        let mut c = machine(&[0x6003, 0x610A, 0x8015]); // 3 - 10
        run(&mut c, 3);
        assert_eq!(c.v[0], 0xF9);
        assert_eq!(c.v[0xF], 0);

        let mut c = machine(&[0x6003, 0x610A, 0x8017]); // SUBN: 10 - 3
        run(&mut c, 3);
        assert_eq!(c.v[0], 7);
        assert_eq!(c.v[0xF], 1);
    }

    #[test]
    fn shifts_capture_shifted_bit() {
        let mut c = machine(&[0x6005, 0x8006]); // SHR 0b101
        run(&mut c, 2);
        assert_eq!(c.v[0], 2);
        assert_eq!(c.v[0xF], 1);

        let mut c = machine(&[0x6081, 0x800E]); // SHL 0b1000_0001
        run(&mut c, 2);
        assert_eq!(c.v[0], 2);
        assert_eq!(c.v[0xF], 1);
    }

    #[test]
    fn vf_result_wins_when_vf_is_operand() {
        // 8FF4: VF = VF + VF; the flag must overwrite the sum.
        let mut c = machine(&[0x6FFF, 0x8FF4]);
        run(&mut c, 2);
        assert_eq!(c.v[0xF], 1);
    }

    #[test]
    fn rnd_is_masked_and_deterministic() {
        let mut a = machine(&[0xC00F, 0xC1FF]);
        let mut b = machine(&[0xC00F, 0xC1FF]);
        run(&mut a, 2);
        run(&mut b, 2);
        assert!(a.v[0] <= 0x0F);
        assert_eq!(a.v[0], b.v[0]);
        assert_eq!(a.v[1], b.v[1]);
    }

    #[test]
    fn draw_xor_collision_and_erase() {
        // Draw the font sprite "0" twice at (0,0): the second draw erases it
        // and reports a collision in VF.
        let rom = [0x6000, 0xF029, 0xD005, 0xD005];
        let mut c = machine(&rom);
        run(&mut c, 3);
        assert!(c.display().iter().any(|&p| p));
        assert_eq!(c.v[0xF], 0);
        run(&mut c, 1);
        assert!(c.display().iter().all(|&p| !p));
        assert_eq!(c.v[0xF], 1);
    }

    #[test]
    fn draw_wraps_start_and_clips_overflow() {
        // Start coordinates wrap modulo the display size…
        let mut c = machine(&[0x6040, 0x6120, 0xF029, 0xD015]); // (64, 32) ≡ (0, 0)
        run(&mut c, 4);
        assert!(c.display()[0..4].iter().any(|&p| p));

        // …but a sprite hanging off the bottom edge clips instead of wrapping.
        let mut c = machine(&[0x6000, 0x611E, 0xF029, 0xD015]); // y = 30
        run(&mut c, 4);
        let top_row_lit = c.display()[..DISPLAY_WIDTH].iter().any(|&p| p);
        assert!(!top_row_lit);
    }

    #[test]
    fn keypad_skips() {
        // SKP taken when the key in V0 is held.
        let mut c = machine(&[0x6005, 0xE09E, 0x1200, 0x6499]);
        c.key_down(5);
        run(&mut c, 3);
        assert_eq!(c.v[4], 0x99);

        // SKNP taken when it is not.
        let mut c = machine(&[0x6005, 0xE0A1, 0x1200, 0x6499]);
        run(&mut c, 3);
        assert_eq!(c.v[4], 0x99);
    }

    #[test]
    fn wait_for_key_blocks_then_stores() {
        let mut c = machine(&[0xF30A, 0x6499]);
        run(&mut c, 5); // stays parked on the wait
        assert_eq!(c.v[4], 0);
        c.key_down(0xB);
        run(&mut c, 1);
        assert_eq!(c.v[3], 0xB);
        assert_eq!(c.v[4], 0x99);
    }

    #[test]
    fn timers_load_read_and_tick() {
        let mut c = machine(&[0x603C, 0xF015, 0xF018, 0xF107]);
        run(&mut c, 4);
        assert_eq!(c.v[1], 60);
        assert!(c.beeping());
        for _ in 0..60 {
            c.tick_timers();
        }
        assert_eq!(c.delay_timer, 0);
        assert!(!c.beeping());
    }

    #[test]
    fn bcd_and_register_store_load() {
        // BCD of 234 at I, then read the digits back through Fx65.
        let mut c = machine(&[0x60EA, 0xA400, 0xF033, 0xF265]);
        run(&mut c, 4);
        assert_eq!((c.v[0], c.v[1], c.v[2]), (2, 3, 4));

        // Fx55 round-trip, and I must be left unchanged.
        let mut c = machine(&[0x6011, 0x6122, 0xA400, 0xF155]);
        run(&mut c, 4);
        assert_eq!(c.i, 0x400);
        assert_eq!(c.memory[0x400], 0x11);
        assert_eq!(c.memory[0x401], 0x22);
    }

    #[test]
    fn font_addressing() {
        let mut c = machine(&[0x600A, 0xF029]); // sprite for 'A'
        run(&mut c, 2);
        assert_eq!(c.i as usize, FONT_START + 10 * 5);
        assert_eq!(c.memory[c.i as usize], 0xF0);
    }

    #[test]
    fn unknown_opcode_is_reported() {
        let mut c = machine(&[0xF0FF]);
        assert_eq!(c.step(), Err(Chip8Error::UnknownOpcode(0xF0FF)));
    }

    #[test]
    fn rom_too_large_is_rejected() {
        let mut c = Chip8::new(1);
        assert!(matches!(
            c.load_rom(&[0u8; 4096]),
            Err(Chip8Error::RomTooLarge(4096))
        ));
    }

    /// Run a bundled test-suite ROM headless as a smoke test: the full
    /// decode/execute path must hold up for thousands of cycles and leave
    /// pixels on screen.
    fn smoke(rom: &[u8]) {
        let mut c = Chip8::new(0xC0FFEE);
        c.load_rom(rom).unwrap();
        for cycle in 0..5000 {
            c.step().unwrap();
            if cycle % 11 == 0 {
                c.tick_timers();
            }
        }
        assert!(c.display().iter().any(|&p| p), "ROM drew nothing");
    }

    #[test]
    fn runs_corax_opcode_test_rom() {
        smoke(include_bytes!("../../roms/timendus/3-corax+.ch8"));
    }

    #[test]
    fn runs_flags_test_rom() {
        smoke(include_bytes!("../../roms/timendus/4-flags.ch8"));
    }

    #[test]
    fn runs_ibm_logo_rom() {
        smoke(include_bytes!("../../roms/timendus/2-ibm-logo.ch8"));
    }
}
