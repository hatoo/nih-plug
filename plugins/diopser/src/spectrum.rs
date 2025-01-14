// Diopser: a phase rotation plugin
// Copyright (C) 2021-2022 Robbert van der Helm
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

use fftw::array::AlignedVec;
use fftw::plan::{R2CPlan, R2CPlan32};
use fftw::types::{c32, Flag};
use nih_plug::prelude::*;
use std::f32;
use triple_buffer::TripleBuffer;

pub const SPECTRUM_WINDOW_SIZE: usize = 2048;
// Don't need that much precision here
const SPECTRUM_WINDOW_OVERLAP: usize = 2;

/// The amplitudes of all frequency bins in a windowed FFT of the input, minus the DC offset bin.
pub type Spectrum = [f32; SPECTRUM_WINDOW_SIZE / 2];
/// A receiver for a spectrum computed by [`SpectrumInput`].
pub type SpectrumOutput = triple_buffer::Output<Spectrum>;

/// Continuously compute spectrums and send them to the connected [`SpectrumOutput`].
pub struct SpectrumInput {
    /// A helper to do most of the STFT process.
    stft: util::StftHelper,
    /// The number of channels we're working on.
    num_channels: usize,

    /// A way to send data to the corresponding [`SpectrumOutput`]. `spectrum_result_buffer` gets
    /// copied into this buffer every time a new spectrum is available.
    triple_buffer_input: triple_buffer::Input<Spectrum>,
    /// A scratch buffer to compute the resulting power amplitude spectrum.
    spectrum_result_buffer: Spectrum,

    /// The algorithm for the FFT operation.
    plan: Plan,
    /// A Hann window window, passed to the STFT helper. The gain compensation is already part of
    /// this window to save a multiplication step.
    compensated_window_function: Vec<f32>,
    /// Scratch buffers for computing our FFT. The [`StftHelper`] already contains a buffer for the
    /// real values.
    complex_fft_scratch_buffer: AlignedVec<c32>,
}

/// FFTW uses raw pointers which aren't Send+Sync, so we'll wrap this in a separate struct.
struct Plan {
    r2c_plan: R2CPlan32,
}

unsafe impl Send for Plan {}
unsafe impl Sync for Plan {}

impl SpectrumInput {
    /// Create a new spectrum input and output pair. The output should be moved to the editor.
    pub fn new(num_channels: usize) -> (SpectrumInput, SpectrumOutput) {
        let (triple_buffer_input, triple_buffer_output) =
            TripleBuffer::new(&[0.0; SPECTRUM_WINDOW_SIZE / 2]).split();

        let input = Self {
            stft: util::StftHelper::new(num_channels, SPECTRUM_WINDOW_SIZE),
            num_channels,

            triple_buffer_input,
            spectrum_result_buffer: [0.0; SPECTRUM_WINDOW_SIZE / 2],

            plan: Plan {
                r2c_plan: R2CPlan32::aligned(
                    &[SPECTRUM_WINDOW_SIZE],
                    Flag::MEASURE | Flag::DESTROYINPUT,
                )
                .unwrap(),
            },
            compensated_window_function: util::window::hann(SPECTRUM_WINDOW_SIZE)
                .into_iter()
                // Include the gain compensation in the window function to save some multiplications
                .map(|x| x / SPECTRUM_WINDOW_SIZE as f32)
                .collect(),
            complex_fft_scratch_buffer: AlignedVec::new(SPECTRUM_WINDOW_SIZE / 2 + 1),
        };

        (input, triple_buffer_output)
    }

    /// Compute the spectrum for a buffer and send it to the corresponding output pair.
    pub fn compute(&mut self, buffer: &Buffer) {
        self.stft.process_analyze_only(
            buffer,
            &self.compensated_window_function,
            SPECTRUM_WINDOW_OVERLAP,
            |channel_idx, real_fft_scratch_buffer| {
                // Forward FFT, the helper has already applied window function
                self.plan
                    .r2c_plan
                    .r2c(
                        real_fft_scratch_buffer,
                        &mut self.complex_fft_scratch_buffer,
                    )
                    .unwrap();

                // To be able to reuse `real_fft_scratch_buffer` this function is called per
                // channel, so we need to use the channel index to do any pre- or post-processing.
                // Gain compensation has already been baked into the window function.
                if channel_idx == 0 {
                    for (bin, spectrum_result) in self
                        .complex_fft_scratch_buffer
                        .iter()
                        // We don't care about the DC bin
                        .skip(1)
                        .zip(&mut self.spectrum_result_buffer)
                    {
                        *spectrum_result = bin.norm();
                    }
                } else {
                    for (bin, spectrum_result) in self
                        .complex_fft_scratch_buffer
                        .iter()
                        .skip(1)
                        .zip(&mut self.spectrum_result_buffer)
                    {
                        *spectrum_result += bin.norm();
                    }
                }

                let num_channels_recip = (self.num_channels as f32).recip();
                if channel_idx == self.num_channels - 1 {
                    for bin in &mut self.spectrum_result_buffer {
                        *bin *= num_channels_recip;
                    }
                }

                self.triple_buffer_input.write(self.spectrum_result_buffer);
            },
        );
    }
}
