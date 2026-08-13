import init, { Emulator } from "./pkg/iron_chip_web.js";

const INSTRUCTIONS_PER_FRAME = 11; // ~700 instructions/second at 60 Hz
const FRAME_MS = 1000 / 60;

const KEYMAP = {
  Digit1: 0x1, Digit2: 0x2, Digit3: 0x3, Digit4: 0xC,
  KeyQ: 0x4, KeyW: 0x5, KeyE: 0x6, KeyR: 0xD,
  KeyA: 0x7, KeyS: 0x8, KeyD: 0x9, KeyF: 0xE,
  KeyZ: 0xA, KeyX: 0x0, KeyC: 0xB, KeyV: 0xF,
};

const status = document.getElementById("status");
const romSelect = document.getElementById("rom-select");
const romFile = document.getElementById("rom-file");
const pauseButton = document.getElementById("pause");
const resetButton = document.getElementById("reset");

function say(message, isError = false) {
  status.textContent = message;
  status.classList.toggle("error", isError);
}

async function main() {
  await init();

  const canvas = document.getElementById("screen");
  const seed = (Math.random() * 0xffffffff) >>> 0;
  const emulator = new Emulator(canvas, seed);

  let paused = false;
  let crashed = false;
  let lastTime = 0;
  let accumulator = 0;

  // Beep: a square wave whose gain tracks the sound timer. The AudioContext
  // is created lazily on first interaction, per autoplay rules.
  let audio = null;
  let gain = null;
  function ensureAudio() {
    if (audio) return;
    audio = new (window.AudioContext || window.webkitAudioContext)();
    const oscillator = audio.createOscillator();
    oscillator.type = "square";
    oscillator.frequency.value = 440;
    gain = audio.createGain();
    gain.gain.value = 0;
    oscillator.connect(gain).connect(audio.destination);
    oscillator.start();
  }

  async function loadBundled(name) {
    try {
      const response = await fetch(`roms/${name}`);
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      const bytes = new Uint8Array(await response.arrayBuffer());
      emulator.load_rom(bytes);
      crashed = false;
      say(`Running ${name}`);
    } catch (error) {
      say(`Could not load ${name}: ${error.message}`, true);
    }
  }

  romSelect.addEventListener("change", () => loadBundled(romSelect.value));

  romFile.addEventListener("change", async () => {
    const file = romFile.files[0];
    if (!file) return;
    try {
      const bytes = new Uint8Array(await file.arrayBuffer());
      emulator.load_rom(bytes);
      crashed = false;
      say(`Running ${file.name}`);
    } catch (error) {
      say(`Could not load ${file.name}: ${error}`, true);
    }
  });

  pauseButton.addEventListener("click", () => {
    paused = !paused;
    pauseButton.textContent = paused ? "Resume" : "Pause";
  });

  resetButton.addEventListener("click", () => {
    emulator.reset();
    crashed = false;
    say("Reset");
  });

  window.addEventListener("keydown", (event) => {
    ensureAudio();
    const key = KEYMAP[event.code];
    if (key !== undefined) {
      event.preventDefault();
      emulator.key_down(key);
    }
  });

  window.addEventListener("keyup", (event) => {
    const key = KEYMAP[event.code];
    if (key !== undefined) {
      event.preventDefault();
      emulator.key_up(key);
    }
  });

  window.addEventListener("pointerdown", ensureAudio);

  function tick(time) {
    requestAnimationFrame(tick);
    if (paused || crashed) return;

    // Run 60 Hz frames regardless of the display's refresh rate.
    accumulator += time - lastTime;
    lastTime = time;
    accumulator = Math.min(accumulator, 3 * FRAME_MS); // don't spiral after a tab switch

    while (accumulator >= FRAME_MS) {
      accumulator -= FRAME_MS;
      try {
        emulator.frame(INSTRUCTIONS_PER_FRAME);
      } catch (error) {
        crashed = true;
        say(`Machine halted: ${error}`, true);
        break;
      }
    }

    if (gain) {
      gain.gain.value = emulator.beeping() && !paused && !crashed ? 0.04 : 0;
    }
  }

  await loadBundled(romSelect.value);
  requestAnimationFrame((time) => {
    lastTime = time;
    requestAnimationFrame(tick);
  });
}

main().catch((error) => say(`Failed to start: ${error}`, true));
