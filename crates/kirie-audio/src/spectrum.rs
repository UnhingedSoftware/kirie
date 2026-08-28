use crate::dsp::{BANDS_16, BANDS_32, BANDS_64, Smoother};

#[derive(Clone, Debug, PartialEq)]
pub struct AudioSpectrum {
    pub audio16: [f32; BANDS_16],
    pub audio32: [f32; BANDS_32],
    pub audio64: [f32; BANDS_64],
}

impl AudioSpectrum {
    #[must_use]
    pub const fn silent() -> Self {
        Self {
            audio16: [0.0; BANDS_16],
            audio32: [0.0; BANDS_32],
            audio64: [0.0; BANDS_64],
        }
    }

    #[must_use]
    pub fn bands(&self, resolution: usize) -> &[f32] {
        match resolution {
            16 => &self.audio16,
            32 => &self.audio32,
            _ => &self.audio64,
        }
    }
}

impl Default for AudioSpectrum {
    fn default() -> Self {
        Self::silent()
    }
}

impl From<&Smoother> for AudioSpectrum {
    fn from(s: &Smoother) -> Self {
        Self {
            audio16: s.b16,
            audio32: s.b32,
            audio64: s.b64,
        }
    }
}
