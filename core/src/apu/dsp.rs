//! S-DSP: 8 voices with ADSR/GAIN envelopes, Gaussian-interpolated BRR
//! playback, pitch modulation, a shared noise LFSR and an 8-tap FIR echo unit.
//! Produces one 32 kHz stereo sample per `tick` (called every 32 SPC cycles).
//!
//! The 128-byte register array ($00-$7F) is written/read by the SPC700 through
//! $F2/$F3. The DSP writes back ENVX/OUTX/ENDX status into that array so the
//! sound driver can poll voice state. All constants (rate table, Gaussian
//! table, filter forms, echo pipeline) come from the APU reference.

use super::brr;

/// Envelope/noise period in samples per rate index (0 = never fires).
const RATE_PERIOD: [i32; 32] = [
    0, 2048, 1536, 1280, 1024, 768, 640, 512, 384, 320, 256, 192, 160, 128, 96, 80, 64, 48, 40, 32,
    24, 20, 16, 12, 10, 8, 6, 5, 4, 3, 2, 1,
];

/// A global counter decrements once per sample, wrapping to $77FF below 0; an
/// envelope/noise event at `rate` fires when `(counter + offset) % period == 0`.
/// Column offsets: rate ≡ 1 (mod 3) → 0, ≡ 2 → 1040, ≡ 0 → 536.
fn rate_fires(rate: u8, counter: i32) -> bool {
    if rate == 0 {
        return false;
    }
    let period = RATE_PERIOD[rate as usize];
    let offset = match rate % 3 {
        1 => 0,
        2 => 1040,
        _ => 536,
    };
    (counter + offset) % period == 0
}

fn clamp16(v: i32) -> i32 {
    v.clamp(-32768, 32767)
}

fn read16(ram: &[u8; 0x10000], addr: usize) -> i32 {
    let lo = ram[addr & 0xFFFF] as u16;
    let hi = ram[(addr + 1) & 0xFFFF] as u16;
    (lo | (hi << 8)) as i16 as i32
}

fn write16(ram: &mut [u8; 0x10000], addr: usize, val: i32) {
    let v = val as u16;
    ram[addr & 0xFFFF] = v as u8;
    ram[(addr + 1) & 0xFFFF] = (v >> 8) as u8;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Attack,
    Decay,
    Sustain,
    Release,
}

struct Voice {
    active: bool,
    /// Samples remaining in the 5-sample post-key-on warm-up.
    kon_delay: u8,
    /// 11-bit envelope level, 0..$7FF.
    env: i32,
    phase: Phase,
    /// 16-bit pitch accumulator; only the low 12 bits (the interpolation
    /// fraction) are retained after consuming whole source samples.
    counter: u16,
    /// Four most recent decoded 15-bit samples (hist[3] = newest).
    hist: [i32; 4],
    block_addr: u16,
    loop_addr: u16,
    decoded: [i32; 16],
    decode_pos: usize,
    p1: i32,
    p2: i32,
    cur_end: bool,
    cur_loop: bool,
    first_block: bool,
    /// This voice's post-envelope 15-bit output (feeds the next voice's PMON).
    out: i32,
}

impl Voice {
    fn new() -> Self {
        Voice {
            active: false,
            kon_delay: 0,
            env: 0,
            phase: Phase::Release,
            counter: 0,
            hist: [0; 4],
            block_addr: 0,
            loop_addr: 0,
            decoded: [0; 16],
            decode_pos: 16,
            p1: 0,
            p2: 0,
            cur_end: false,
            cur_loop: false,
            first_block: false,
            out: 0,
        }
    }
}

pub struct Dsp {
    regs: [u8; 128],
    voices: [Voice; 8],
    /// Shared 15-bit noise LFSR (raw bits; -$4000 at reset).
    noise: u16,
    /// Global envelope/noise rate counter.
    rate_counter: i32,
    /// Toggles each sample; KON/KOFF are polled on even samples (every 2nd).
    sample_ctr: u32,
    /// Echo ring position (in 4-byte frames).
    echo_index: usize,
    /// Latched echo length in frames (re-latched from EDL when the ring wraps).
    echo_len: usize,
    /// Per-channel 8-entry FIR history ring.
    echo_hist: [[i32; 8]; 2],
    fir_pos: usize,
}

impl Dsp {
    pub fn new() -> Self {
        // FLG ($6C) reset value $E0: soft-reset + mute + echo-write disable.
        let mut regs = [0u8; 128];
        regs[0x6C] = 0xE0;
        Dsp {
            regs,
            voices: [(); 8].map(|_| Voice::new()),
            noise: 0x4000,
            rate_counter: 0,
            sample_ctr: 0,
            echo_index: 0,
            echo_len: 1,
            echo_hist: [[0; 8]; 2],
            fir_pos: 0,
        }
    }

    pub fn read(&self, addr: u8) -> u8 {
        self.regs[(addr & 0x7F) as usize]
    }

    pub fn write(&mut self, addr: u8, val: u8) {
        let a = (addr & 0x7F) as usize;
        self.regs[a] = val;
        // Any write to ENDX ($7C) clears all its bits.
        if a == 0x7C {
            self.regs[0x7C] = 0;
        }
    }

    fn reg(&self, voice: usize, off: usize) -> u8 {
        self.regs[voice * 0x10 + off]
    }

    fn key_on(&mut self, x: usize, ram: &[u8; 0x10000]) {
        let srcn = self.reg(x, 4) as usize;
        let base = (self.regs[0x5D] as usize) << 8;
        let e = base + srcn * 4;
        let start = ram[e & 0xFFFF] as u16 | ((ram[(e + 1) & 0xFFFF] as u16) << 8);
        let loop_addr = ram[(e + 2) & 0xFFFF] as u16 | ((ram[(e + 3) & 0xFFFF] as u16) << 8);
        let v = &mut self.voices[x];
        v.block_addr = start;
        v.loop_addr = loop_addr;
        v.decode_pos = 16;
        v.first_block = true;
        v.p1 = 0;
        v.p2 = 0;
        v.hist = [0; 4];
        v.counter = 0;
        v.env = 0;
        v.phase = Phase::Attack;
        v.kon_delay = 5;
        v.active = true;
        v.cur_end = false;
        v.cur_loop = false;
        v.out = 0;
        // Key-on clears this voice's ENDX bit.
        self.regs[0x7C] &= !(1 << x);
    }

    /// Advance a voice's BRR stream by `n` source samples, decoding new blocks
    /// on demand and maintaining the 4-sample Gaussian history.
    fn brr_advance(&mut self, x: usize, ram: &[u8; 0x10000], n: u32) {
        for _ in 0..n {
            if self.voices[x].decode_pos >= 16 {
                if self.voices[x].first_block {
                    self.voices[x].first_block = false;
                } else if self.voices[x].cur_end {
                    self.voices[x].block_addr = self.voices[x].loop_addr;
                    // End+mute (end set, loop clear): force Release with env 0.
                    if !self.voices[x].cur_loop {
                        self.voices[x].env = 0;
                        self.voices[x].phase = Phase::Release;
                    }
                } else {
                    let a = self.voices[x].block_addr.wrapping_add(9);
                    self.voices[x].block_addr = a;
                }
                let addr = self.voices[x].block_addr as usize;
                let mut bytes = [0u8; 9];
                for (i, b) in bytes.iter_mut().enumerate() {
                    *b = ram[(addr + i) & 0xFFFF];
                }
                let (dec, hdr, p1, p2) =
                    brr::decode_block(&bytes, self.voices[x].p1, self.voices[x].p2);
                self.voices[x].decoded = dec;
                self.voices[x].p1 = p1;
                self.voices[x].p2 = p2;
                self.voices[x].cur_end = hdr.end_flag;
                self.voices[x].cur_loop = hdr.loop_flag;
                self.voices[x].decode_pos = 0;
                // ENDX is set at the start of decoding a block whose end bit is set.
                if hdr.end_flag {
                    self.regs[0x7C] |= 1 << x;
                }
            }
            let s = self.voices[x].decoded[self.voices[x].decode_pos];
            let v = &mut self.voices[x];
            v.decode_pos += 1;
            v.hist[0] = v.hist[1];
            v.hist[1] = v.hist[2];
            v.hist[2] = v.hist[3];
            v.hist[3] = s;
        }
    }

    /// Advance one voice's envelope by one sample (rate-gated per its ADSR/GAIN
    /// configuration). Assumes the voice is not in its key-on warm-up.
    fn update_env(&mut self, x: usize) {
        let adsr1 = self.reg(x, 5);
        let adsr2 = self.reg(x, 6);
        let gain = self.reg(x, 7);
        let counter = self.rate_counter;
        let v = &mut self.voices[x];
        if v.phase == Phase::Release {
            v.env -= 8;
            if v.env < 0 {
                v.env = 0;
            }
            return;
        }
        if adsr1 & 0x80 != 0 {
            match v.phase {
                Phase::Attack => {
                    let a = adsr1 & 0x0F;
                    let rate = (a << 1) + 1;
                    if rate_fires(rate, counter) {
                        v.env += if a == 0x0F { 0x400 } else { 0x20 };
                    }
                    if v.env >= 0x7E0 {
                        if v.env > 0x7FF {
                            v.env = 0x7FF;
                        }
                        v.phase = Phase::Decay;
                    }
                }
                Phase::Decay => {
                    let rate = (((adsr1 >> 4) & 0x07) * 2) + 16;
                    if rate_fires(rate, counter) {
                        v.env -= ((v.env - 1) >> 8) + 1;
                    }
                    let sl = (adsr2 >> 5) as i32;
                    if v.env <= (sl + 1) * 0x100 {
                        v.phase = Phase::Sustain;
                    }
                }
                Phase::Sustain => {
                    let rate = adsr2 & 0x1F;
                    if rate_fires(rate, counter) {
                        v.env -= ((v.env - 1) >> 8) + 1;
                    }
                }
                Phase::Release => unreachable!(),
            }
        } else if gain & 0x80 == 0 {
            // Direct gain: env = V*16, set every sample.
            v.env = ((gain & 0x7F) as i32) << 4;
        } else {
            let rate = gain & 0x1F;
            if rate_fires(rate, counter) {
                match (gain >> 5) & 0x03 {
                    0 => v.env -= 0x20,
                    1 => v.env -= ((v.env - 1) >> 8) + 1,
                    2 => v.env += 0x20,
                    _ => v.env += if v.env < 0x600 { 0x20 } else { 0x08 },
                }
            }
        }
        v.env = v.env.clamp(0, 0x7FF);
    }

    fn noise_signed(&self) -> i32 {
        (((self.noise as i32) & 0x7FFF) ^ 0x4000) - 0x4000
    }

    /// Interpolate the voice's current 15-bit output using the 4-tap Gaussian
    /// filter and the pitch counter's fractional index.
    fn gaussian(&self, x: usize) -> i32 {
        let i = ((self.voices[x].counter >> 4) & 0xFF) as usize;
        let h = &self.voices[x].hist;
        let oldest = h[0];
        let older = h[1];
        let old = h[2];
        let new = h[3];
        let mut out = (GAUSS[0x0FF - i] * oldest) >> 10;
        out += (GAUSS[0x1FF - i] * older) >> 10;
        out = out as i16 as i32;
        out += (GAUSS[0x100 + i] * old) >> 10;
        out = clamp16(out);
        out += (GAUSS[0x000 + i] * new) >> 10;
        out = clamp16(out);
        out >> 1
    }

    /// Emit one 32 kHz stereo sample, reading/writing ARAM for the sample
    /// directory, BRR blocks and the echo ring buffer.
    pub fn tick(&mut self, ram: &mut [u8; 0x10000], out: &mut Vec<(i16, i16)>) {
        let flg = self.regs[0x6C];
        let soft_reset = flg & 0x80 != 0;
        let mute = flg & 0x40 != 0;
        let echo_write_disable = flg & 0x20 != 0;

        if soft_reset {
            for v in self.voices.iter_mut() {
                v.phase = Phase::Release;
                v.env = 0;
            }
        }

        // KON/KOFF are polled every 2nd sample; internal KON bits clear on poll.
        if self.sample_ctr & 1 == 0 {
            let kon = self.regs[0x4C];
            let koff = self.regs[0x5C];
            for x in 0..8 {
                if (koff >> x) & 1 != 0 {
                    self.voices[x].phase = Phase::Release;
                }
                if (kon >> x) & 1 != 0 {
                    self.key_on(x, ram);
                }
            }
            self.regs[0x4C] = 0;
        }

        // Clock the shared noise LFSR at the FLG-selected rate.
        if rate_fires(flg & 0x1F, self.rate_counter) {
            let feedback = (self.noise ^ (self.noise >> 1)) & 1;
            self.noise = ((self.noise >> 1) & 0x3FFF) | (feedback << 14);
        }

        let pmon = self.regs[0x2D];
        let non = self.regs[0x3D];
        let eon = self.regs[0x4D];

        let mut main_l = 0i32;
        let mut main_r = 0i32;
        let mut echo_in_l = 0i32;
        let mut echo_in_r = 0i32;
        let mut prev_out = 0i32;

        for x in 0..8 {
            if !self.voices[x].active {
                self.regs[x * 0x10 + 8] = 0;
                self.regs[x * 0x10 + 9] = 0;
                self.voices[x].out = 0;
                prev_out = 0;
                continue;
            }
            if self.voices[x].kon_delay > 0 {
                self.voices[x].kon_delay -= 1;
                self.regs[x * 0x10 + 8] = 0;
                self.regs[x * 0x10 + 9] = 0;
                self.voices[x].out = 0;
                prev_out = 0;
                continue;
            }

            self.update_env(x);

            // Voice sample: BRR+Gaussian, or the shared noise generator.
            let raw = if (non >> x) & 1 != 0 {
                self.noise_signed()
            } else {
                self.gaussian(x)
            };
            let env = self.voices[x].env;
            let sample = (raw * env) >> 11;
            self.voices[x].out = sample;

            self.regs[x * 0x10 + 8] = ((env >> 4) & 0x7F) as u8;
            self.regs[x * 0x10 + 9] = (sample >> 7) as u8;

            let voll = self.reg(x, 0) as i8 as i32;
            let volr = self.reg(x, 1) as i8 as i32;
            main_l = clamp16(main_l + ((sample * voll) >> 6));
            main_r = clamp16(main_r + ((sample * volr) >> 6));
            if (eon >> x) & 1 != 0 {
                echo_in_l = clamp16(echo_in_l + ((sample * voll) >> 6));
                echo_in_r = clamp16(echo_in_r + ((sample * volr) >> 6));
            }

            // Advance the pitch counter; PMON scales the step by the previous
            // voice's 15-bit output.
            let pitch = (self.reg(x, 2) as i32 | ((self.reg(x, 3) as i32 & 0x3F) << 8)) & 0x3FFF;
            let mut step = pitch;
            if x > 0 && (pmon >> x) & 1 != 0 {
                let factor = (prev_out >> 4) + 0x400;
                step = (step * factor) >> 10;
            }
            step = step.clamp(0, 0x3FFF);
            let sum = self.voices[x].counter as u32 + step as u32;
            let advance = sum >> 12;
            self.voices[x].counter = (sum & 0x0FFF) as u16;
            self.brr_advance(x, ram, advance);

            prev_out = sample;

            // Voice turns off once its release envelope reaches 0.
            if self.voices[x].phase == Phase::Release && self.voices[x].env == 0 {
                self.voices[x].active = false;
            }
        }

        // --- Echo ---
        let esa = self.regs[0x6D] as usize;
        let addr = ((esa << 8) + self.echo_index * 4) & 0xFFFF;
        let in_l = read16(ram, addr) >> 1;
        let in_r = read16(ram, addr + 2) >> 1;
        self.echo_hist[0][self.fir_pos] = in_l;
        self.echo_hist[1][self.fir_pos] = in_r;
        let fir: [i32; 8] = core::array::from_fn(|k| self.regs[0x0F + k * 0x10] as i8 as i32);
        let fir_l = apply_fir(&self.echo_hist[0], self.fir_pos, &fir);
        let fir_r = apply_fir(&self.echo_hist[1], self.fir_pos, &fir);

        let evoll = self.regs[0x2C] as i8 as i32;
        let evolr = self.regs[0x3C] as i8 as i32;
        let efb = self.regs[0x0D] as i8 as i32;
        let echo_out_l = (fir_l * evoll) >> 7;
        let echo_out_r = (fir_r * evolr) >> 7;

        // Feedback: voices routed to echo plus filtered echo; forced 15-bit.
        let echo_write_l = clamp16(echo_in_l + ((fir_l * efb) >> 7)) & !1;
        let echo_write_r = clamp16(echo_in_r + ((fir_r * efb) >> 7)) & !1;
        if !echo_write_disable {
            write16(ram, addr, echo_write_l);
            write16(ram, addr + 2, echo_write_r);
        }

        self.fir_pos = (self.fir_pos + 1) & 7;
        self.echo_index += 1;
        if self.echo_index >= self.echo_len {
            self.echo_index = 0;
            let edl = (self.regs[0x7D] & 0x0F) as usize;
            self.echo_len = if edl == 0 { 1 } else { edl * 512 };
        }

        // --- Master mix ---
        let mvoll = self.regs[0x0C] as i8 as i32;
        let mvolr = self.regs[0x1C] as i8 as i32;
        let mut out_l = clamp16((main_l * mvoll) >> 7);
        let mut out_r = clamp16((main_r * mvolr) >> 7);
        out_l = clamp16(out_l + echo_out_l);
        out_r = clamp16(out_r + echo_out_r);
        if mute {
            out_l = 0;
            out_r = 0;
        }
        out.push((out_l as i16, out_r as i16));

        self.rate_counter = if self.rate_counter == 0 { 0x77FF } else { self.rate_counter - 1 };
        self.sample_ctr = self.sample_ctr.wrapping_add(1);
    }
}

/// 8-tap FIR: the 7 oldest taps accumulate with 16-bit wrap (hardware bug), the
/// newest tap (FIR7) is saturated to signed 16-bit.
fn apply_fir(hist: &[i32; 8], pos: usize, fir: &[i32; 8]) -> i32 {
    let mut sum = 0i32;
    for (k, &coeff) in fir.iter().enumerate().take(7) {
        sum += (hist[(pos + 1 + k) & 7] * coeff) >> 6;
    }
    let sum = sum as i16 as i32;
    clamp16(sum + ((hist[pos] * fir[7]) >> 6))
}

impl Default for Dsp {
    fn default() -> Self {
        Self::new()
    }
}

/// 512-entry Gaussian interpolation table (fullsnes, verified monotonic).
#[rustfmt::skip]
const GAUSS: [i32; 512] = [
    0x000,0x000,0x000,0x000,0x000,0x000,0x000,0x000,0x000,0x000,0x000,0x000,0x000,0x000,0x000,0x000,
    0x001,0x001,0x001,0x001,0x001,0x001,0x001,0x001,0x001,0x001,0x001,0x002,0x002,0x002,0x002,0x002,
    0x002,0x002,0x003,0x003,0x003,0x003,0x003,0x004,0x004,0x004,0x004,0x004,0x005,0x005,0x005,0x005,
    0x006,0x006,0x006,0x006,0x007,0x007,0x007,0x008,0x008,0x008,0x009,0x009,0x009,0x00A,0x00A,0x00A,
    0x00B,0x00B,0x00B,0x00C,0x00C,0x00D,0x00D,0x00E,0x00E,0x00F,0x00F,0x00F,0x010,0x010,0x011,0x011,
    0x012,0x013,0x013,0x014,0x014,0x015,0x015,0x016,0x017,0x017,0x018,0x018,0x019,0x01A,0x01B,0x01B,
    0x01C,0x01D,0x01D,0x01E,0x01F,0x020,0x020,0x021,0x022,0x023,0x024,0x024,0x025,0x026,0x027,0x028,
    0x029,0x02A,0x02B,0x02C,0x02D,0x02E,0x02F,0x030,0x031,0x032,0x033,0x034,0x035,0x036,0x037,0x038,
    0x03A,0x03B,0x03C,0x03D,0x03E,0x040,0x041,0x042,0x043,0x045,0x046,0x047,0x049,0x04A,0x04C,0x04D,
    0x04E,0x050,0x051,0x053,0x054,0x056,0x057,0x059,0x05A,0x05C,0x05E,0x05F,0x061,0x063,0x064,0x066,
    0x068,0x06A,0x06B,0x06D,0x06F,0x071,0x073,0x075,0x076,0x078,0x07A,0x07C,0x07E,0x080,0x082,0x084,
    0x086,0x089,0x08B,0x08D,0x08F,0x091,0x093,0x096,0x098,0x09A,0x09C,0x09F,0x0A1,0x0A3,0x0A6,0x0A8,
    0x0AB,0x0AD,0x0AF,0x0B2,0x0B4,0x0B7,0x0BA,0x0BC,0x0BF,0x0C1,0x0C4,0x0C7,0x0C9,0x0CC,0x0CF,0x0D2,
    0x0D4,0x0D7,0x0DA,0x0DD,0x0E0,0x0E3,0x0E6,0x0E9,0x0EC,0x0EF,0x0F2,0x0F5,0x0F8,0x0FB,0x0FE,0x101,
    0x104,0x107,0x10B,0x10E,0x111,0x114,0x118,0x11B,0x11E,0x122,0x125,0x129,0x12C,0x130,0x133,0x137,
    0x13A,0x13E,0x141,0x145,0x148,0x14C,0x150,0x153,0x157,0x15B,0x15F,0x162,0x166,0x16A,0x16E,0x172,
    0x176,0x17A,0x17D,0x181,0x185,0x189,0x18D,0x191,0x195,0x19A,0x19E,0x1A2,0x1A6,0x1AA,0x1AE,0x1B2,
    0x1B7,0x1BB,0x1BF,0x1C3,0x1C8,0x1CC,0x1D0,0x1D5,0x1D9,0x1DD,0x1E2,0x1E6,0x1EB,0x1EF,0x1F3,0x1F8,
    0x1FC,0x201,0x205,0x20A,0x20F,0x213,0x218,0x21C,0x221,0x226,0x22A,0x22F,0x233,0x238,0x23D,0x241,
    0x246,0x24B,0x250,0x254,0x259,0x25E,0x263,0x267,0x26C,0x271,0x276,0x27B,0x280,0x284,0x289,0x28E,
    0x293,0x298,0x29D,0x2A2,0x2A6,0x2AB,0x2B0,0x2B5,0x2BA,0x2BF,0x2C4,0x2C9,0x2CE,0x2D3,0x2D8,0x2DC,
    0x2E1,0x2E6,0x2EB,0x2F0,0x2F5,0x2FA,0x2FF,0x304,0x309,0x30E,0x313,0x318,0x31D,0x322,0x326,0x32B,
    0x330,0x335,0x33A,0x33F,0x344,0x349,0x34E,0x353,0x357,0x35C,0x361,0x366,0x36B,0x370,0x374,0x379,
    0x37E,0x383,0x388,0x38C,0x391,0x396,0x39B,0x39F,0x3A4,0x3A9,0x3AD,0x3B2,0x3B7,0x3BB,0x3C0,0x3C5,
    0x3C9,0x3CE,0x3D2,0x3D7,0x3DC,0x3E0,0x3E5,0x3E9,0x3ED,0x3F2,0x3F6,0x3FB,0x3FF,0x403,0x408,0x40C,
    0x410,0x415,0x419,0x41D,0x421,0x425,0x42A,0x42E,0x432,0x436,0x43A,0x43E,0x442,0x446,0x44A,0x44E,
    0x452,0x455,0x459,0x45D,0x461,0x465,0x468,0x46C,0x470,0x473,0x477,0x47A,0x47E,0x481,0x485,0x488,
    0x48C,0x48F,0x492,0x496,0x499,0x49C,0x49F,0x4A2,0x4A6,0x4A9,0x4AC,0x4AF,0x4B2,0x4B5,0x4B7,0x4BA,
    0x4BD,0x4C0,0x4C3,0x4C5,0x4C8,0x4CB,0x4CD,0x4D0,0x4D2,0x4D5,0x4D7,0x4D9,0x4DC,0x4DE,0x4E0,0x4E3,
    0x4E5,0x4E7,0x4E9,0x4EB,0x4ED,0x4EF,0x4F1,0x4F3,0x4F5,0x4F6,0x4F8,0x4FA,0x4FB,0x4FD,0x4FF,0x500,
    0x502,0x503,0x504,0x506,0x507,0x508,0x50A,0x50B,0x50C,0x50D,0x50E,0x50F,0x510,0x511,0x511,0x512,
    0x513,0x514,0x514,0x515,0x516,0x516,0x517,0x517,0x517,0x518,0x518,0x518,0x518,0x518,0x519,0x519,
];

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_ram() -> Box<[u8; 0x10000]> {
        vec![0u8; 0x10000].into_boxed_slice().try_into().unwrap()
    }

    /// Install a looping BRR sample and a directory entry; DIR page is $20.
    fn install_sample(ram: &mut [u8; 0x10000], srcn: usize, start: u16, header: u8, data: u8) {
        let entry = 0x2000usize + srcn * 4;
        ram[entry] = start as u8;
        ram[entry + 1] = (start >> 8) as u8;
        ram[entry + 2] = start as u8; // loop back to the same block
        ram[entry + 3] = (start >> 8) as u8;
        ram[start as usize] = header;
        for i in 0..8 {
            ram[start as usize + 1 + i] = data;
        }
    }

    #[test]
    fn full_volume_voice_produces_output() {
        let mut ram = empty_ram();
        // Looping block, shift 8, filter 0, end+loop; nibble 7 -> +896 samples.
        install_sample(&mut ram, 0, 0x0400, 0x83, 0x77);
        let mut dsp = Dsp::new();
        dsp.write(0x5D, 0x20); // DIR page $20
        dsp.write(0x00, 0x7F); // V0VOLL
        dsp.write(0x01, 0x7F); // V0VOLR
        dsp.write(0x02, 0x00); // pitch low
        dsp.write(0x03, 0x10); // pitch high -> $1000 (1:1)
        dsp.write(0x04, 0x00); // SRCN 0
        dsp.write(0x05, 0x00); // ADSR1: E=0 -> GAIN mode
        dsp.write(0x07, 0x7F); // GAIN direct, env = $7F0
        dsp.write(0x0C, 0x7F); // MVOLL
        dsp.write(0x1C, 0x7F); // MVOLR
        dsp.write(0x6C, 0x20); // FLG: no reset, no mute, echo-write disabled
        dsp.write(0x4C, 0x01); // KON voice 0

        let mut out = Vec::new();
        for _ in 0..64 {
            dsp.tick(&mut ram, &mut out);
        }
        assert!(out.iter().any(|&(l, _)| l.abs() > 100), "voice produced silence");
    }

    #[test]
    fn adsr_attack_decay_sustain_progression() {
        let mut ram = empty_ram();
        install_sample(&mut ram, 0, 0x0400, 0x83, 0x77);
        let mut dsp = Dsp::new();
        dsp.write(0x5D, 0x20);
        dsp.write(0x03, 0x10);
        // ADSR: attack rate $F (fast), decay rate $7, sustain level 4, SR 0
        // (sustain holds; a non-zero SR would keep decaying).
        dsp.write(0x05, 0x80 | (0x7 << 4) | 0x0F);
        dsp.write(0x06, 0x4 << 5);
        dsp.write(0x6C, 0x20);
        dsp.write(0x4C, 0x01);

        let mut out = Vec::new();
        // Warm-up + attack: envelope must climb toward full scale.
        let mut peak = 0u8;
        for _ in 0..12 {
            dsp.tick(&mut ram, &mut out);
            peak = peak.max(dsp.regs[0x08]);
        }
        assert!(peak > 0x70, "attack should approach full scale, got {peak:#x}");
        // Decay drives the level down to the sustain plateau (SL=4 -> $500,
        // ENVX ~$50), where SR=0 holds it. Run enough samples to settle.
        for _ in 0..4000 {
            dsp.tick(&mut ram, &mut out);
        }
        let sustained = dsp.regs[0x08];
        assert!(sustained < peak, "decay must lower the envelope");
        assert!((0x4E..=0x51).contains(&sustained), "sustain plateau ~$50, got {sustained:#x}");
    }

    #[test]
    fn endx_set_on_end_block() {
        let mut ram = empty_ram();
        // Single end+loop block; ENDX bit 0 set when its end flag is decoded.
        install_sample(&mut ram, 0, 0x0400, 0x83, 0x77);
        let mut dsp = Dsp::new();
        dsp.write(0x5D, 0x20);
        dsp.write(0x03, 0x20); // pitch $2000 -> 2 samples/tick, reach block end fast
        dsp.write(0x05, 0x00);
        dsp.write(0x07, 0x7F);
        dsp.write(0x6C, 0x20);
        dsp.write(0x4C, 0x01);
        let mut out = Vec::new();
        for _ in 0..64 {
            dsp.tick(&mut ram, &mut out);
        }
        assert_ne!(dsp.regs[0x7C] & 0x01, 0, "ENDX bit 0 must be set after looping");
    }

    #[test]
    fn echo_buffer_index_wraps() {
        let mut ram = empty_ram();
        let mut dsp = Dsp::new();
        dsp.write(0x6D, 0x40); // ESA page $40 -> $4000
        dsp.write(0x7D, 0x01); // EDL 1 -> 512-frame buffer
        dsp.echo_len = 512;
        dsp.write(0x6C, 0x00); // echo writes enabled, no mute/reset
        let mut out = Vec::new();
        let mut max_index = 0usize;
        for _ in 0..520 {
            dsp.tick(&mut ram, &mut out);
            max_index = max_index.max(dsp.echo_index);
        }
        assert!(max_index < 512, "echo index must stay within the buffer");
        assert_eq!(dsp.echo_index, 520 - 512, "index must wrap past the buffer end");
    }

    #[test]
    fn dsp_register_roundtrip() {
        let mut dsp = Dsp::new();
        dsp.write(0x4C, 0x55);
        assert_eq!(dsp.read(0x4C), 0x55);
        // Writing ENDX clears all its bits.
        dsp.regs[0x7C] = 0xFF;
        dsp.write(0x7C, 0x12);
        assert_eq!(dsp.read(0x7C), 0x00);
    }
}
