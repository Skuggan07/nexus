//! Sound sharing: a paired viewer can hear what this PC is playing.
//!
//! The host records its speakers/headphones output ("loopback") and sends it as
//! 16-bit stereo PCM over an authenticated screen link (`{op:"audio"}`). The viewer
//! plays it on its own speakers. Both sides use cpal (WASAPI on Windows).

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::{
    collections::VecDeque,
    sync::{mpsc, Arc, Mutex},
};

/// Turns interleaved samples with `ch` channels into interleaved stereo i16.
fn to_stereo<T: Copy>(data: &[T], ch: usize, conv: impl Fn(T) -> i16) -> Vec<i16> {
    let ch = ch.max(1);
    let mut out = Vec::with_capacity(data.len() / ch * 2);
    for frame in data.chunks(ch) {
        let l = conv(frame[0]);
        let r = if frame.len() > 1 { conv(frame[1]) } else { l };
        out.push(l);
        out.push(r);
    }
    out
}

fn f32_to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * 32767.0) as i16
}

/// Starts recording what this PC plays. Returns the running recorder (keep it alive,
/// on this thread), its sample rate, and a channel of stereo chunks.
pub fn start_capture() -> Result<(cpal::Stream, u32, mpsc::Receiver<Vec<i16>>), String> {
    let host = cpal::default_host();
    let dev = host.default_output_device().ok_or("This PC has no speakers or headphones to listen to")?;
    let sup = dev.default_output_config().map_err(|e| format!("Could not read this PC's sound settings: {e}"))?;
    let rate = sup.sample_rate().0;
    let ch = sup.channels() as usize;
    let fmt = sup.sample_format();
    let cfg: cpal::StreamConfig = sup.into();
    let (tx, rx) = mpsc::channel::<Vec<i16>>();
    let on_err = |e: cpal::StreamError| eprintln!("sound capture: {e}");
    // On Windows, recording from an output device records what it plays (loopback).
    let stream = match fmt {
        cpal::SampleFormat::F32 => dev.build_input_stream(
            &cfg,
            move |d: &[f32], _: &cpal::InputCallbackInfo| {
                let _ = tx.send(to_stereo(d, ch, f32_to_i16));
            },
            on_err,
            None,
        ),
        cpal::SampleFormat::I16 => dev.build_input_stream(
            &cfg,
            move |d: &[i16], _: &cpal::InputCallbackInfo| {
                let _ = tx.send(to_stereo(d, ch, |s| s));
            },
            on_err,
            None,
        ),
        other => return Err(format!("This PC uses a sound format Nexus can't record yet ({other:?})")),
    }
    .map_err(|e| format!("Could not listen to this PC's sound: {e}"))?;
    stream.play().map_err(|e| format!("Could not listen to this PC's sound: {e}"))?;
    Ok((stream, rate, rx))
}

/// Plays sound that arrives from another PC on this PC's speakers.
pub struct Player {
    _stream: cpal::Stream,
    buf: Arc<Mutex<VecDeque<f32>>>,
    out_rate: u32,
    out_ch: usize,
}

impl Player {
    pub fn new() -> Result<Self, String> {
        let host = cpal::default_host();
        let dev = host.default_output_device().ok_or("This PC has no speakers or headphones")?;
        let sup = dev.default_output_config().map_err(|e| format!("Could not open the speakers: {e}"))?;
        let out_rate = sup.sample_rate().0;
        let out_ch = (sup.channels() as usize).max(1);
        let fmt = sup.sample_format();
        let cfg: cpal::StreamConfig = sup.into();
        let buf = Arc::new(Mutex::new(VecDeque::<f32>::new()));
        let feed = buf.clone();
        let on_err = |e: cpal::StreamError| eprintln!("sound playback: {e}");
        let stream = match fmt {
            cpal::SampleFormat::F32 => dev.build_output_stream(
                &cfg,
                move |d: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    let mut q = feed.lock().unwrap_or_else(|e| e.into_inner());
                    for s in d.iter_mut() {
                        *s = q.pop_front().unwrap_or(0.0);
                    }
                },
                on_err,
                None,
            ),
            cpal::SampleFormat::I16 => dev.build_output_stream(
                &cfg,
                move |d: &mut [i16], _: &cpal::OutputCallbackInfo| {
                    let mut q = feed.lock().unwrap_or_else(|e| e.into_inner());
                    for s in d.iter_mut() {
                        *s = f32_to_i16(q.pop_front().unwrap_or(0.0));
                    }
                },
                on_err,
                None,
            ),
            other => return Err(format!("These speakers use a sound format Nexus can't play yet ({other:?})")),
        }
        .map_err(|e| format!("Could not open the speakers: {e}"))?;
        stream.play().map_err(|e| format!("Could not open the speakers: {e}"))?;
        Ok(Player { _stream: stream, buf, out_rate, out_ch })
    }

    /// Queues interleaved stereo i16 samples recorded at `src_rate`.
    pub fn push(&self, src_rate: u32, stereo: &[i16]) {
        let frames = stereo.len() / 2;
        if frames == 0 || src_rate == 0 {
            return;
        }
        let out_frames = ((frames as u64 * self.out_rate as u64) / src_rate as u64).max(1) as usize;
        let step = src_rate as f64 / self.out_rate as f64;
        let sample = |i: usize, c: usize| stereo[i.min(frames - 1) * 2 + c] as f32 / 32768.0;
        let mut q = self.buf.lock().unwrap_or_else(|e| e.into_inner());
        for j in 0..out_frames {
            let t = j as f64 * step;
            let i = t as usize;
            let frac = (t - i as f64) as f32;
            let l = sample(i, 0) * (1.0 - frac) + sample(i + 1, 0) * frac;
            let r = sample(i, 1) * (1.0 - frac) + sample(i + 1, 1) * frac;
            for c in 0..self.out_ch {
                q.push_back(if c % 2 == 0 { l } else { r });
            }
        }
        // Keep the delay short: if more than 0.3 s is waiting, skip ahead to 0.1 s.
        let per_sec = self.out_rate as usize * self.out_ch;
        if q.len() > per_sec * 3 / 10 {
            let drop = q.len() - per_sec / 10;
            q.drain(..drop);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::to_stereo;

    #[test]
    fn mono_and_surround_become_stereo() {
        assert_eq!(to_stereo(&[1i16, 2], 1, |s| s), vec![1, 1, 2, 2]);
        assert_eq!(to_stereo(&[1i16, 2, 3, 4, 5, 6], 3, |s| s), vec![1, 2, 4, 5]);
    }
}
