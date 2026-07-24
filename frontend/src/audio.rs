//! Audio output: a lock-free SPSC ring of 32 kHz stereo frames feeding a cpal
//! output stream. The stream callback linearly resamples 32 kHz -> the device
//! rate with dynamic rate control (the resample ratio is nudged +/-0.5% in
//! proportion to the ring-buffer fill's deviation from 50%), so emulator/host
//! clock drift becomes an inaudible pitch wobble instead of pops/gaps.
//!
//! The producer runs on the emulator thread (`AudioOutput::push`); the consumer
//! runs on cpal's audio thread. Device init is fully guarded: `AudioOutput::new`
//! returns `None` (never panics) when no device/stream is available, so a
//! headless or device-less host stays silent rather than crashing.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};

/// S-DSP output rate (matches `snes_core` `Apu::sample_rate`).
pub const DSP_SAMPLE_RATE: u32 = 32_000;

/// Ring capacity in stereo frames (power of two). ~256 ms at 32 kHz, which
/// comfortably absorbs one video frame of jitter (~533 frames at 60 Hz) plus
/// OS scheduling slop. One slot is reserved to disambiguate full from empty.
const RING_CAPACITY: usize = 8192;
const RING_MASK: usize = RING_CAPACITY - 1;

/// Maximum resample-ratio trim applied by the rate controller (+/-0.5%).
const MAX_RATE_TRIM: f64 = 0.005;

/// SPSC ring of stereo `f32` frames. The producer owns `write`, the consumer
/// owns `read`; the acquire/release pairing on those indices publishes slot
/// writes across the thread boundary. `slots` is only ever touched by the side
/// that owns the corresponding index, so the `UnsafeCell` aliasing is sound.
struct Ring {
    slots: Box<[UnsafeCell<[f32; 2]>]>,
    write: AtomicUsize,
    read: AtomicUsize,
}

// Only the producer writes a slot before publishing `write`, and only the
// consumer reads it after observing that publication; no slot is ever accessed
// concurrently.
unsafe impl Sync for Ring {}
unsafe impl Send for Ring {}

impl Ring {
    fn new() -> Arc<Ring> {
        let mut slots = Vec::with_capacity(RING_CAPACITY);
        slots.resize_with(RING_CAPACITY, || UnsafeCell::new([0.0f32; 2]));
        Arc::new(Ring {
            slots: slots.into_boxed_slice(),
            write: AtomicUsize::new(0),
            read: AtomicUsize::new(0),
        })
    }
}

struct Producer {
    ring: Arc<Ring>,
}

impl Producer {
    /// Push one frame; returns false (dropping the frame) when the ring is full.
    fn push(&self, frame: [f32; 2]) -> bool {
        let w = self.ring.write.load(Ordering::Relaxed);
        let next = (w + 1) & RING_MASK;
        if next == self.ring.read.load(Ordering::Acquire) {
            return false; // full
        }
        unsafe { *self.ring.slots[w].get() = frame };
        self.ring.write.store(next, Ordering::Release);
        true
    }
}

struct Consumer {
    ring: Arc<Ring>,
}

impl Consumer {
    fn pop(&self) -> Option<[f32; 2]> {
        let r = self.ring.read.load(Ordering::Relaxed);
        if r == self.ring.write.load(Ordering::Acquire) {
            return None; // empty
        }
        let frame = unsafe { *self.ring.slots[r].get() };
        self.ring.read.store((r + 1) & RING_MASK, Ordering::Release);
        Some(frame)
    }

    /// Current occupancy in frames (0..RING_CAPACITY-1).
    fn fill(&self) -> usize {
        let w = self.ring.write.load(Ordering::Acquire);
        let r = self.ring.read.load(Ordering::Acquire);
        w.wrapping_sub(r) & RING_MASK
    }
}

/// Clamp to [-1, 1] and flush NaN/inf/denormals to zero so a bad synth sample
/// can never emit a click or a subnormal-induced CPU stall in the callback.
#[inline]
fn clean(x: f32) -> f32 {
    if !x.is_finite() {
        0.0
    } else if x.abs() < 1.0e-20 {
        0.0
    } else {
        x.clamp(-1.0, 1.0)
    }
}

/// Linear resampler + rate controller living on the cpal audio thread.
struct Resampler {
    consumer: Consumer,
    /// Nominal input frames consumed per output frame (src_rate / device_rate).
    base_ratio: f64,
    /// Target ring occupancy the controller steers toward (half full).
    target_fill: f64,
    cur: [f32; 2],
    next: [f32; 2],
    /// Fractional read position between `cur` and `next`, in input frames.
    pos: f64,
}

impl Resampler {
    fn new(consumer: Consumer, device_rate: u32) -> Resampler {
        Resampler {
            consumer,
            base_ratio: DSP_SAMPLE_RATE as f64 / device_rate as f64,
            target_fill: RING_CAPACITY as f64 * 0.5,
            cur: [0.0; 2],
            next: [0.0; 2],
            // pos>=1 forces the priming pass to load `next` from the first ring
            // frame before the first sample is emitted. `cur` is still the
            // initial silence, so exactly one silent output frame is produced at
            // stream start (inaudible), after which output tracks real data.
            pos: 1.0,
        }
    }

    /// Resample ratio for this callback: buffer fuller than target -> consume
    /// faster (drain it); emptier -> consume slower (refill). Trimmed +/-0.5%
    /// in proportion to the fill's deviation from `target_fill`.
    fn current_ratio(&self) -> f64 {
        let fill = self.consumer.fill() as f64;
        let dev = (fill - self.target_fill) / self.target_fill;
        let trim = (dev * MAX_RATE_TRIM).clamp(-MAX_RATE_TRIM, MAX_RATE_TRIM);
        self.base_ratio * (1.0 + trim)
    }

    fn process<T>(&mut self, data: &mut [T], channels: usize)
    where
        T: SizedSample + FromSample<f32>,
    {
        // `chunks_mut(0)` would panic in the real-time callback; a real device
        // always advertises >=1 channel, so this only guards a latent edge case.
        if channels == 0 {
            return;
        }
        // One rate decision per callback.
        let ratio = self.current_ratio();

        for frame in data.chunks_mut(channels) {
            while self.pos >= 1.0 {
                self.cur = self.next;
                // Underrun: repeat the last frame instead of gapping.
                self.next = self.consumer.pop().unwrap_or(self.cur);
                self.pos -= 1.0;
            }
            let t = self.pos as f32;
            let l = clean(self.cur[0] + (self.next[0] - self.cur[0]) * t);
            let r = clean(self.cur[1] + (self.next[1] - self.cur[1]) * t);
            self.pos += ratio;

            match channels {
                1 => frame[0] = T::from_sample((l + r) * 0.5),
                _ => {
                    frame[0] = T::from_sample(l);
                    frame[1] = T::from_sample(r);
                    for s in &mut frame[2..] {
                        *s = T::from_sample(0.0f32);
                    }
                }
            }
        }
    }
}

/// Owns the cpal stream (keeping it alive) and the ring producer. Lives on the
/// emulator thread. Dropping it stops audio.
pub struct AudioOutput {
    _stream: cpal::Stream,
    producer: Producer,
    /// Linear amplitude applied to every frame on its way into the ring (see
    /// `gain_for`). Muting is a gain of 0, never a stream stop: the APU keeps
    /// running, so unmuting resumes mid-note instead of restarting the song.
    gain: f32,
}

impl AudioOutput {
    /// Open the default output device and start a stream. Returns `None` and
    /// logs a warning on any failure (no device, no config, unsupported format,
    /// stream build/play error) so the caller can continue silently.
    pub fn new() -> Option<AudioOutput> {
        let host = cpal::default_host();
        let device = match host.default_output_device() {
            Some(d) => d,
            None => {
                eprintln!("audio: no default output device; continuing silent");
                return None;
            }
        };
        let supported = match device.default_output_config() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("audio: no default output config ({e}); continuing silent");
                return None;
            }
        };
        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let device_rate = config.sample_rate.0;
        let channels = config.channels as usize;

        let ring = Ring::new();
        let producer = Producer { ring: Arc::clone(&ring) };
        let mut resampler = Resampler::new(Consumer { ring }, device_rate);

        let err_fn = |e| eprintln!("audio: stream error: {e}");
        let build = |device: &cpal::Device| -> Result<cpal::Stream, cpal::BuildStreamError> {
            match sample_format {
                cpal::SampleFormat::F32 => device.build_output_stream(
                    &config,
                    move |data: &mut [f32], _| resampler.process(data, channels),
                    err_fn,
                    None,
                ),
                cpal::SampleFormat::I16 => device.build_output_stream(
                    &config,
                    move |data: &mut [i16], _| resampler.process(data, channels),
                    err_fn,
                    None,
                ),
                cpal::SampleFormat::U16 => device.build_output_stream(
                    &config,
                    move |data: &mut [u16], _| resampler.process(data, channels),
                    err_fn,
                    None,
                ),
                other => {
                    eprintln!("audio: unsupported sample format {other:?}; continuing silent");
                    Err(cpal::BuildStreamError::StreamConfigNotSupported)
                }
            }
        };

        let stream = match build(&device) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("audio: build stream failed ({e}); continuing silent");
                return None;
            }
        };
        if let Err(e) = stream.play() {
            eprintln!("audio: stream play failed ({e}); continuing silent");
            return None;
        }
        eprintln!("audio: {device_rate} Hz, {channels} ch, {sample_format:?}");
        Some(AudioOutput { _stream: stream, producer, gain: 1.0 })
    }

    /// Set the output gain (see `gain_for`). Out-of-range or non-finite values
    /// are clamped to `0.0..=1.0`.
    pub fn set_gain(&mut self, gain: f32) {
        self.gain = clamp_gain(gain);
    }

    /// Push a run of 32 kHz stereo frames into the ring, scaled by the current
    /// gain. Overflowing frames are dropped (the rate controller will have
    /// slowed the consumer to prevent this in steady state).
    pub fn push(&mut self, frames: &[(i16, i16)]) {
        let gain = self.gain;
        for &(l, r) in frames {
            let f = [l as f32 / 32768.0 * gain, r as f32 / 32768.0 * gain];
            self.producer.push(f);
        }
    }
}

/// Clamp a requested gain into `0.0..=1.0`; a non-finite request (which would
/// poison every sample it multiplies) becomes silence.
fn clamp_gain(gain: f32) -> f32 {
    if gain.is_finite() {
        gain.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Linear amplitude for a mute flag + 0..=100 volume setting. Muting wins over
/// any volume; the mapping is linear in amplitude (100 % = unity gain, i.e. the
/// unmodified S-DSP output), so 0 % is exact silence and the setting can never
/// amplify past the DSP's own full scale.
pub fn gain_for(mute: bool, volume: u8) -> f32 {
    if mute {
        return 0.0;
    }
    volume.min(100) as f32 / 100.0
}

/// One step of the volume control: 10-percentage-point increments, snapped to a
/// multiple of 10 and clamped to 0..=100. A value already off-grid (hand-edited
/// preferences file) snaps to the next grid point in the requested direction.
pub fn step_volume(volume: u8, up: bool) -> u8 {
    let v = volume.min(100);
    if up {
        (v / 10 * 10 + 10).min(100)
    } else {
        // Round down to the grid first, so 42 -> 40 rather than 32.
        let floor = v / 10 * 10;
        if floor == v {
            v.saturating_sub(10)
        } else {
            floor
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_push_pop_roundtrip() {
        let ring = Ring::new();
        let p = Producer { ring: Arc::clone(&ring) };
        let c = Consumer { ring };
        assert_eq!(c.fill(), 0);
        assert!(p.push([0.25, -0.5]));
        assert_eq!(c.fill(), 1);
        assert_eq!(c.pop(), Some([0.25, -0.5]));
        assert_eq!(c.pop(), None);
    }

    #[test]
    fn ring_reports_full() {
        let ring = Ring::new();
        let p = Producer { ring: Arc::clone(&ring) };
        let c = Consumer { ring };
        // Usable capacity is RING_CAPACITY - 1 (one reserved slot).
        for i in 0..RING_CAPACITY - 1 {
            assert!(p.push([i as f32, 0.0]), "push {i} should fit");
        }
        assert!(!p.push([0.0, 0.0]), "ring must report full");
        assert_eq!(c.fill(), RING_CAPACITY - 1);
    }

    #[test]
    fn gain_for_maps_mute_and_volume() {
        assert_eq!(gain_for(false, 100), 1.0);
        assert_eq!(gain_for(false, 0), 0.0);
        assert_eq!(gain_for(true, 100), 0.0, "mute overrides volume");
        assert!((gain_for(false, 50) - 0.5).abs() < 1e-6);
        // A hand-edited file could hold >100; never amplify past unity.
        assert_eq!(gain_for(false, 250), 1.0);
    }

    #[test]
    fn step_volume_walks_the_ten_percent_grid() {
        assert_eq!(step_volume(100, true), 100, "clamped at the top");
        assert_eq!(step_volume(90, true), 100);
        assert_eq!(step_volume(0, false), 0, "clamped at the bottom");
        assert_eq!(step_volume(10, false), 0);
        assert_eq!(step_volume(50, true), 60);
        assert_eq!(step_volume(50, false), 40);
        // Off-grid values snap toward the requested direction.
        assert_eq!(step_volume(42, true), 50);
        assert_eq!(step_volume(42, false), 40);
        assert_eq!(step_volume(250, false), 90);
    }

    #[test]
    fn clamp_gain_rejects_out_of_range_and_non_finite() {
        assert_eq!(clamp_gain(0.5), 0.5);
        assert_eq!(clamp_gain(2.0), 1.0);
        assert_eq!(clamp_gain(-1.0), 0.0);
        assert_eq!(clamp_gain(f32::NAN), 0.0);
        assert_eq!(clamp_gain(f32::INFINITY), 0.0);
    }

    #[test]
    fn clean_flushes_bad_values() {
        assert_eq!(clean(f32::NAN), 0.0);
        assert_eq!(clean(f32::INFINITY), 0.0);
        assert_eq!(clean(1e-30), 0.0);
        assert_eq!(clean(2.0), 1.0);
        assert_eq!(clean(-2.0), -1.0);
        assert_eq!(clean(0.5), 0.5);
    }

    #[test]
    fn resampler_underrun_repeats_last() {
        let ring = Ring::new();
        let p = Producer { ring: Arc::clone(&ring) };
        let mut rs = Resampler::new(Consumer { ring }, 32_000); // ratio ~1.0
        p.push([0.5, 0.5]);
        p.push([0.5, 0.5]);
        // More output frames than input frames -> must not gap to silence.
        let mut buf = [0.0f32; 16]; // 8 stereo frames
        rs.process(&mut buf, 2);
        // Once primed, the tail repeats the last real sample (0.5), never 0.
        assert!(buf[14] > 0.4 && buf[15] > 0.4, "underrun should hold last sample");
    }

    #[test]
    fn resampler_ratio_at_48khz() {
        let base = DSP_SAMPLE_RATE as f64 / 48_000.0; // 2/3
        // Empty ring: fill far below target -> ratio trimmed down to base*0.995.
        let ring = Ring::new();
        let rs = Resampler::new(Consumer { ring }, 48_000);
        assert!((rs.base_ratio - base).abs() < 1e-12);
        assert!((rs.current_ratio() - base * 0.995).abs() < 1e-9);
        // Full ring: fill far above target -> ratio trimmed up to base*1.005.
        let ring = Ring::new();
        let p = Producer { ring: Arc::clone(&ring) };
        let rs = Resampler::new(Consumer { ring }, 48_000);
        for i in 0..RING_CAPACITY - 1 {
            assert!(p.push([i as f32, 0.0]));
        }
        // One slot is reserved, so the max fill can't quite reach dev==1; the
        // trim saturates just below +0.5%.
        let full = rs.current_ratio();
        assert!(full <= base * 1.005 && full > base * 1.00499, "full ratio {full}");
    }

    #[test]
    fn resampler_interpolates_at_48khz() {
        // device_rate 48000 -> base_ratio 2/3. Neutralize rate control by
        // pinning target_fill to the current occupancy (dev=0 -> trim=0), so the
        // ratio is exactly 2/3 and the linear interpolation is deterministic.
        let ring = Ring::new();
        let p = Producer { ring: Arc::clone(&ring) };
        let mut rs = Resampler::new(Consumer { ring }, 48_000);
        for i in 0..6 {
            assert!(p.push([0.3 * i as f32, 0.3 * i as f32]));
        }
        rs.target_fill = rs.consumer.fill() as f64;
        let mut buf = [0.0f32; 16]; // 8 stereo frames
        rs.process(&mut buf, 2);
        // The priming pass consumes one silent frame, so the first two outputs
        // read the [0,0] pair, then interpolation settles into a clean 0.2-step
        // ramp (0.1, 0.3, 0.5, 0.7, 0.9) at ratio 2/3 over the 0.3-step input.
        let expected = [0.0, 0.0, 0.1, 0.3, 0.5, 0.7, 0.9];
        for (k, e) in expected.iter().enumerate() {
            assert!((buf[k * 2] - e).abs() < 2e-3, "frame {k}: {} != {e}", buf[k * 2]);
        }
    }
}
