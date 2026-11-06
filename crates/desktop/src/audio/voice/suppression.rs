use super::worker::Result;
use litz_core::protocol::{FRAME, Suppression};
use nnnoiseless::DenoiseState;
use std::path::PathBuf;

const DEEP_FILTER_MODEL: &str = "DeepFilterNet3_onnx.tar.gz";

pub fn deep_filter_model() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok();
    let exe_dir = exe.as_deref().and_then(std::path::Path::parent);
    let user = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .map(|data| data.join("litz"));
    [
        std::env::var_os("LITZ_DATA_DIR").map(PathBuf::from),
        exe_dir.map(|dir| dir.join("../share/litz")),
        exe_dir.map(PathBuf::from),
        user,
    ]
    .into_iter()
    .flatten()
    .map(|dir| dir.join(DEEP_FILTER_MODEL))
    .find(|path| path.is_file())
}

pub(super) enum Processor {
    None,
    Rnnoise(Box<DenoiseState<'static>>),
    DeepFilterNet {
        active: Box<df::tract::DfTract>,
        pristine: Box<df::tract::DfTract>,
    },
}
impl Processor {
    pub fn try_new(algorithm: Suppression) -> Result<Self> {
        Ok(match algorithm {
            Suppression::None => Self::None,
            Suppression::Rnnoise => Self::Rnnoise(DenoiseState::new()),
            Suppression::DeepFilterNet => {
                let path = deep_filter_model().ok_or("DeepFilterNet model not installed")?;
                let params = df::tract::DfParams::new(path)
                    .map_err(|e| format!("DeepFilterNet model: {e:#}"))?;
                let model = df::tract::DfTract::new(
                    params,
                    // Limit attenuation and relax SNR thresholds for quiet speech.
                    &df::tract::RuntimeParams::default_with_ch(1)
                        .with_atten_lim(24.0)
                        .with_thresholds(-15.0, 35.0, 35.0),
                )
                .map_err(|e| format!("DeepFilterNet initialization: {e:#}"))?;
                if model.hop_size != FRAME {
                    return Err("Unsupported DeepFilterNet frame size".into());
                }
                Self::DeepFilterNet {
                    active: Box::new(model.clone()),
                    pristine: Box::new(model),
                }
            }
        })
    }
    pub fn process(&mut self, samples: &mut [f32; FRAME]) -> Result<()> {
        match self {
            Self::None => {}
            Self::Rnnoise(processor) => {
                // RNNoise uses signed 16-bit amplitude, represented as floats.
                let input = samples.map(|sample| sample * 32768.0);
                processor.process_frame(samples, &input);
                for sample in samples {
                    *sample /= 32768.0;
                }
            }
            Self::DeepFilterNet { active: p, .. } => {
                let input = *samples;
                let input = ndarray::ArrayView2::from_shape((1, FRAME), &input)
                    .map_err(|e| e.to_string())?;
                let output = ndarray::ArrayViewMut2::from_shape((1, FRAME), samples)
                    .map_err(|e| e.to_string())?;
                p.process(input, output)
                    .map_err(|e| format!("DeepFilterNet processing: {e}"))?;
            }
        }
        Ok(())
    }
    pub fn reset(&mut self, algorithm: Suppression) -> Result<()> {
        if let Self::DeepFilterNet { active, pristine } = self {
            *active = pristine.clone();
        } else {
            *self = Self::try_new(algorithm)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_algorithms_produce_finite_bounded_audio() {
        for algorithm in [Suppression::None, Suppression::Rnnoise] {
            let mut processor = Processor::try_new(algorithm).unwrap();
            for n in 0..20 {
                let mut frame =
                    std::array::from_fn(|i| (((i + n * FRAME) as f32 * 0.04).sin()) * 0.2);
                processor.process(&mut frame).unwrap();
                assert!(
                    frame.iter().all(|v| v.is_finite() && v.abs() < 2.0),
                    "{algorithm:?}"
                );
            }
        }
    }

    #[test]
    fn deep_filter_net_produces_finite_bounded_audio() {
        let Some(mut processor) = Processor::try_new(Suppression::DeepFilterNet).ok() else {
            return;
        };
        for n in 0..20 {
            let mut frame = std::array::from_fn(|i| (((i + n * FRAME) as f32 * 0.04).sin()) * 0.2);
            processor.process(&mut frame).unwrap();
            assert!(
                frame.iter().all(|v| v.is_finite() && v.abs() < 2.0),
                "DeepFilterNet"
            );
        }
    }
    #[test]
    fn denoisers_reduce_stationary_noise_after_warmup() {
        let mut seed = 7u32;
        let (mut input_energy, mut output_energy) = (0.0f64, 0.0f64);
        let mut processor = Processor::try_new(Suppression::Rnnoise).unwrap();
        for block in 0..300 {
            let mut frame = std::array::from_fn(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                seed as i32 as f32 / i32::MAX as f32 * 0.05
            });
            if block >= 200 {
                input_energy += frame.iter().map(|v| f64::from(v * v)).sum::<f64>();
            }
            processor.process(&mut frame).unwrap();
            if block >= 200 {
                output_energy += frame.iter().map(|v| f64::from(v * v)).sum::<f64>();
            }
        }
        eprintln!(
            "Rnnoise: noise reduction {:.1} dB",
            10.0 * (input_energy / output_energy.max(1e-20)).log10()
        );
        assert!(
            output_energy < input_energy * 0.5,
            "Rnnoise did not suppress stationary noise"
        );
    }

    #[test]
    fn deep_filter_net_reduces_stationary_noise() {
        let Some(mut processor) = Processor::try_new(Suppression::DeepFilterNet).ok() else {
            return;
        };
        let mut seed = 7u32;
        let (mut input_energy, mut output_energy) = (0.0f64, 0.0f64);
        for block in 0..300 {
            let mut frame = std::array::from_fn(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                seed as i32 as f32 / i32::MAX as f32 * 0.05
            });
            if block >= 200 {
                input_energy += frame.iter().map(|v| f64::from(v * v)).sum::<f64>();
            }
            processor.process(&mut frame).unwrap();
            if block >= 200 {
                output_energy += frame.iter().map(|v| f64::from(v * v)).sum::<f64>();
            }
        }
        eprintln!(
            "DeepFilterNet: noise reduction {:.1} dB",
            10.0 * (input_energy / output_energy.max(1e-20)).log10()
        );
        assert!(
            output_energy < input_energy * 0.5,
            "DeepFilterNet did not suppress stationary noise"
        );
        assert!(
            output_energy > input_energy * 0.001,
            "DeepFilterNet exceeded its live attenuation limit"
        );
    }
    #[test]
    fn none_is_an_exact_bypass() {
        let original = std::array::from_fn(|i| i as f32 / FRAME as f32);
        let mut frame = original;
        Processor::try_new(Suppression::None)
            .unwrap()
            .process(&mut frame)
            .unwrap();
        assert_eq!(frame, original);
    }
}
