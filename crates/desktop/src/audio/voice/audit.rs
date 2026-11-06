//! Reproducible offline quality/performance audit. Run in release, serially.
use super::suppression;
use super::worker::Pipeline;
use litz_core::protocol::{FRAME, Suppression, VoiceSettings};
use std::fs;
use std::path::Path;
use std::time::Instant;

pub(super) fn speech() -> Vec<f32> {
    include_bytes!("../../../tests/fixtures/speech.pcm")
        .chunks_exact(2)
        .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0)
        .collect()
}
fn wav(path: &Path, samples: &[f32]) {
    let bytes = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(bytes as usize + 44);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(bytes + 36).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt \x10\0\0\0\x01\0\x01\0");
    out.extend_from_slice(&48000u32.to_le_bytes());
    out.extend_from_slice(&96000u32.to_le_bytes());
    out.extend_from_slice(b"\x02\0\x10\0data");
    out.extend_from_slice(&bytes.to_le_bytes());
    for x in samples {
        out.extend_from_slice(&((x.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes());
    }
    fs::write(path, out).unwrap();
}
fn score(clean: &[f32], output: &[f32], delay: usize) -> f64 {
    let n = clean.len().min(output.len().saturating_sub(delay));
    let (mut signal, mut error) = (0.0f64, 0.0f64);
    for (x, y) in clean[..n].iter().zip(&output[delay..delay + n]) {
        signal += f64::from(*x).powi(2);
        error += f64::from(*x - *y).powi(2);
    }
    10.0 * (signal.max(1e-20) / error.max(1e-20)).log10()
}

#[test]
#[ignore = "offline WAV and timing audit; run --release --ignored --nocapture"]
fn speech_quality_matrix() {
    let directory = std::env::var_os("LITZ_AUDIT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("../../target/audio-audit"));
    fs::create_dir_all(&directory).unwrap();
    let original = speech();
    let mut report = Vec::new();
    for scenario in ["clean", "noise", "quiet", "room", "impulses"] {
        if std::env::var("LITZ_AUDIT_SCENARIO").is_ok_and(|selected| selected != scenario) {
            continue;
        }
        let mut clean = vec![0.0; 48000];
        clean.extend(
            original
                .iter()
                .map(|x| x * if scenario == "quiet" { 0.1 } else { 1.0 }),
        );
        clean.resize((clean.len() / FRAME + 101) * FRAME, 0.0);
        let mut input = clean.clone();
        let mut seed = 73u32;
        for (i, x) in input.iter_mut().enumerate() {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            if scenario == "noise" || scenario == "quiet" {
                *x += seed as i32 as f32 / i32::MAX as f32 * 0.025;
            }
            if scenario == "impulses" && i % 12000 < 48 {
                *x += seed as i32 as f32 / i32::MAX as f32 * 0.2;
            }
            if scenario == "room" && i >= 2400 {
                *x += clean[i - 2400] * 0.4;
            }
        }
        wav(&directory.join(format!("{scenario}-input.wav")), &input);
        wav(&directory.join(format!("{scenario}-clean.wav")), &clean);
        for algorithm in [
            Suppression::None,
            Suppression::Rnnoise,
            Suppression::DeepFilterNet,
        ] {
            for level in [false, true] {
                let settings = VoiceSettings {
                    suppression: algorithm,
                    auto_level: level,
                    ..Default::default()
                };
                let mut pipeline = Pipeline::try_new(settings).unwrap();
                let mut output = Vec::with_capacity(input.len());
                let mut times = Vec::new();
                for block in input.as_chunks::<FRAME>().0 {
                    let mut frame = *block;
                    let start = Instant::now();
                    pipeline.process(&mut frame).unwrap();
                    times.push(start.elapsed().as_secs_f64() * 1000.0);
                    assert!(frame.iter().all(|x| x.is_finite() && x.abs() <= 1.0));
                    output.extend_from_slice(&frame);
                }
                times.sort_by(f64::total_cmp);
                let delay = match algorithm {
                    Suppression::None => 0,
                    Suppression::Rnnoise => FRAME,
                    Suppression::DeepFilterNet => 3 * FRAME,
                };
                let label = format!("{scenario}-{algorithm:?}-level{level}");
                wav(&directory.join(format!("{label}.wav")), &output);
                let snr = score(&clean, &output, delay);
                let row = serde_json::json!({"case": label, "delay_samples": delay, "input_snr_db": score(&clean, &input, 0), "output_snr_db": snr, "p50_ms": times[times.len()/2], "p99_ms": times[times.len()*99/100], "max_ms": times.last(), "over_10ms": times.iter().filter(|t| **t > 10.0).count()});
                eprintln!("{row}");
                report.push(row);
            }
        }
    }
    fs::write(
        directory.join("report.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
}

#[test]
fn every_voice_combination_accepts_real_speech_and_resets_cleanly() {
    let speech = speech();
    for algorithm in [Suppression::None, Suppression::Rnnoise] {
        for level in [false, true] {
            let settings = VoiceSettings {
                suppression: algorithm,
                auto_level: level,
                ..Default::default()
            };
            let mut pipeline = Pipeline::try_new(settings).unwrap();
            for block in speech.as_chunks::<FRAME>().0.iter().take(100) {
                let mut frame = *block;
                pipeline.process(&mut frame).unwrap();
                assert!(frame.iter().all(|x| x.is_finite() && x.abs() <= 1.0));
            }
            pipeline.reset(settings).unwrap();
            let mut silent = [0.0; FRAME];
            pipeline.process(&mut silent).unwrap();
            assert!(silent.iter().all(|x| x.abs() < 1e-6), "{settings:?}");
        }
    }
}

#[test]
fn deep_filter_net_accepts_real_speech_and_resets_cleanly() {
    let speech = speech();
    for level in [false, true] {
        let settings = VoiceSettings {
            suppression: Suppression::DeepFilterNet,
            auto_level: level,
            ..Default::default()
        };
        let Some(mut pipeline) = Pipeline::try_new(settings).ok() else {
            return;
        };
        for block in speech.as_chunks::<FRAME>().0.iter().take(100) {
            let mut frame = *block;
            pipeline.process(&mut frame).unwrap();
            assert!(frame.iter().all(|x| x.is_finite() && x.abs() <= 1.0));
        }
        pipeline.reset(settings).unwrap();
        let mut silent = [0.0; FRAME];
        pipeline.process(&mut silent).unwrap();
        assert!(silent.iter().all(|x| x.abs() < 1e-6), "{settings:?}");
    }
}

#[test]
#[ignore = "parameter comparison; run explicitly with --ignored --nocapture"]
fn compare_deepfilter_attenuation_on_speech() {
    let speech = speech();
    for noise in [0.0, 0.025, 0.08] {
        for attenuation in [24.0, 100.0] {
            let mut processor =
                suppression::Processor::try_new(Suppression::DeepFilterNet).unwrap();
            if let suppression::Processor::DeepFilterNet { active, .. } = &mut processor {
                active.set_atten_lim(attenuation);
            }
            let mut clean = vec![0.0; 48000];
            clean.extend_from_slice(&speech);
            clean.resize((clean.len() / FRAME + 101) * FRAME, 0.0);
            let mut output = Vec::new();
            let mut seed = 73u32;
            for block in clean.as_chunks::<FRAME>().0 {
                let mut frame = std::array::from_fn(|i| {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    block[i] + seed as i32 as f32 / i32::MAX as f32 * noise
                });
                processor.process(&mut frame).unwrap();
                output.extend_from_slice(&frame);
            }
            eprintln!(
                "DFN noise={noise} attenuation={attenuation} aligned_snr={:.3}",
                score(&clean, &output, 3 * FRAME)
            );
        }
    }
}
