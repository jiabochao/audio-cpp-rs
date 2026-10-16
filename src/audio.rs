//! PCM audio: [`AudioRef`], a borrowed interleaved `f32` view used for both
//! request inputs and result outputs, and [`AudioBuffer`], its owned form.
//!
//! With the `wav` feature, [`AudioBuffer`] reads and writes WAV files; with
//! `decode`, it reads MP3, AAC/M4A, FLAC, Ogg Vorbis, AIFF, ALAC and CAF too;
//! with `resample`, it converts sample rates with a band-limited resampler.

use std::time::Duration;

use crate::error::{Error, Result};

/// Borrowed interleaved `f32` PCM: `frames() * channels` samples.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioRef<'a> {
    pub samples: &'a [f32],
    pub sample_rate: u32,
    pub channels: u32,
}

impl<'a> AudioRef<'a> {
    /// A view over `samples`, checked to hold whole frames at a positive rate.
    pub fn new(samples: &'a [f32], sample_rate: u32, channels: u32) -> Result<Self> {
        let audio = Self { samples, sample_rate, channels };
        audio.ffi_layout()?;
        Ok(audio)
    }

    /// A mono view.
    pub fn mono(samples: &'a [f32], sample_rate: u32) -> Result<Self> {
        Self::new(samples, sample_rate, 1)
    }

    /// Samples per channel.
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1) as usize
    }

    pub fn duration(&self) -> Duration {
        if self.sample_rate == 0 {
            return Duration::ZERO;
        }
        Duration::from_secs_f64(self.frames() as f64 / self.sample_rate as f64)
    }

    pub fn to_buffer(&self) -> AudioBuffer {
        AudioBuffer { samples: self.samples.to_vec(), sample_rate: self.sample_rate, channels: self.channels }
    }

    /// `(frames, sample_rate, channels)` as the C ABI takes them, validated.
    pub(crate) fn ffi_layout(&self) -> Result<(usize, i32, i32)> {
        let positive = |value: u32, what: &str| {
            i32::try_from(value)
                .ok()
                .filter(|&v| v > 0)
                .ok_or_else(|| Error::invalid_argument(format!("{what} must be in 1..={}", i32::MAX)))
        };
        let rate = positive(self.sample_rate, "sample_rate")?;
        let channels = positive(self.channels, "channels")?;
        if self.samples.len() % self.channels as usize != 0 {
            return Err(Error::invalid_argument(format!(
                "{} samples is not a whole number of {}-channel frames",
                self.samples.len(),
                self.channels
            )));
        }
        Ok((self.frames(), rate, channels))
    }
}

/// Owned interleaved `f32` PCM.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AudioBuffer {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u32,
}

impl AudioBuffer {
    pub fn new(samples: Vec<f32>, sample_rate: u32, channels: u32) -> Result<Self> {
        AudioRef::new(&samples, sample_rate, channels)?;
        Ok(Self { samples, sample_rate, channels })
    }

    /// `duration` of silence.
    pub fn silence(duration: Duration, sample_rate: u32, channels: u32) -> Self {
        let frames = (duration.as_secs_f64() * sample_rate as f64).round() as usize;
        Self { samples: vec![0.0; frames * channels as usize], sample_rate, channels }
    }

    pub fn as_ref(&self) -> AudioRef<'_> {
        AudioRef { samples: &self.samples, sample_rate: self.sample_rate, channels: self.channels }
    }

    pub fn frames(&self) -> usize {
        self.as_ref().frames()
    }

    pub fn duration(&self) -> Duration {
        self.as_ref().duration()
    }

    /// Averages channels down to mono. A mono buffer is returned as is.
    pub fn into_mono(self) -> Self {
        if self.channels <= 1 {
            return self;
        }
        let channels = self.channels as usize;
        let samples =
            self.samples.chunks_exact(channels).map(|frame| frame.iter().sum::<f32>() / channels as f32).collect();
        Self { samples, sample_rate: self.sample_rate, channels: 1 }
    }

    /// Appends `other`, which must share this buffer's rate and channel count.
    pub fn append(&mut self, other: AudioRef<'_>) -> Result<()> {
        if self.samples.is_empty() && self.sample_rate == 0 {
            self.sample_rate = other.sample_rate;
            self.channels = other.channels;
        }
        if other.sample_rate != self.sample_rate || other.channels != self.channels {
            return Err(Error::invalid_argument(format!(
                "cannot append {} Hz / {} ch audio to {} Hz / {} ch",
                other.sample_rate, other.channels, self.sample_rate, self.channels
            )));
        }
        self.samples.extend_from_slice(other.samples);
        Ok(())
    }

    /// Concatenates `parts`, inserting `gap` of silence between neighbours.
    pub fn concat<'a>(parts: impl IntoIterator<Item = AudioRef<'a>>, gap: Duration) -> Result<Self> {
        let mut out = Self::default();
        for (index, part) in parts.into_iter().enumerate() {
            if index > 0 && !gap.is_zero() {
                out.append(Self::silence(gap, part.sample_rate, part.channels).as_ref())?;
            }
            out.append(part)?;
        }
        Ok(out)
    }

    /// Largest absolute sample value.
    pub fn peak(&self) -> f32 {
        self.samples.iter().fold(0.0f32, |peak, s| peak.max(s.abs()))
    }
}

#[cfg(feature = "wav")]
mod wav {
    use std::io::{Read, Seek, Write};
    use std::path::Path;

    use super::AudioBuffer;
    use crate::error::{Error, Result};

    fn io_error(e: hound::Error) -> Error {
        Error::invalid_argument(format!("wav: {e}"))
    }

    impl AudioBuffer {
        /// Reads a WAV file: 8/16/24/32-bit integer or 32-bit float PCM.
        pub fn read_wav(path: impl AsRef<Path>) -> Result<Self> {
            Self::read_wav_from(std::io::BufReader::new(
                std::fs::File::open(path.as_ref())
                    .map_err(|e| Error::invalid_argument(format!("{}: {e}", path.as_ref().display())))?,
            ))
        }

        pub fn read_wav_from(reader: impl Read) -> Result<Self> {
            let mut reader = hound::WavReader::new(reader).map_err(io_error)?;
            let spec = reader.spec();
            let samples = match spec.sample_format {
                hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<Vec<_>, _>>(),
                hound::SampleFormat::Int => {
                    let scale = 1.0 / (1u64 << (spec.bits_per_sample.clamp(1, 32) - 1)) as f32;
                    reader.samples::<i32>().map(|s| s.map(|s| s as f32 * scale)).collect()
                }
            }
            .map_err(io_error)?;
            Self::new(samples, spec.sample_rate, u32::from(spec.channels))
        }

        /// Writes 32-bit float WAV, the lossless form of what models produce.
        pub fn write_wav(&self, path: impl AsRef<Path>) -> Result<()> {
            let file = std::fs::File::create(path.as_ref())
                .map_err(|e| Error::invalid_argument(format!("{}: {e}", path.as_ref().display())))?;
            self.write_wav_to(std::io::BufWriter::new(file))
        }

        pub fn write_wav_to(&self, writer: impl Write + Seek) -> Result<()> {
            let channels = u16::try_from(self.channels)
                .map_err(|_| Error::invalid_argument("wav supports at most 65535 channels"))?;
            let spec = hound::WavSpec {
                channels,
                sample_rate: self.sample_rate,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            };
            let mut writer = hound::WavWriter::new(writer, spec).map_err(io_error)?;
            for &sample in &self.samples {
                writer.write_sample(sample).map_err(io_error)?;
            }
            writer.finalize().map_err(io_error)
        }

        /// Writes 16-bit integer WAV, half the size, for sharing and editors.
        pub fn write_wav_i16(&self, path: impl AsRef<Path>) -> Result<()> {
            let channels = u16::try_from(self.channels)
                .map_err(|_| Error::invalid_argument("wav supports at most 65535 channels"))?;
            let spec = hound::WavSpec {
                channels,
                sample_rate: self.sample_rate,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut writer = hound::WavWriter::create(path.as_ref(), spec).map_err(io_error)?;
            for &sample in &self.samples {
                writer.write_sample((sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16).map_err(io_error)?;
            }
            writer.finalize().map_err(io_error)
        }
    }
}

#[cfg(feature = "decode")]
mod decode {
    use std::path::Path;

    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
    use symphonia::core::errors::Error as SymphoniaError;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    use super::AudioBuffer;
    use crate::error::{Error, Result};

    fn error(path: &Path, e: impl std::fmt::Display) -> Error {
        Error::invalid_argument(format!("{}: {e}", path.display()))
    }

    impl AudioBuffer {
        /// File extensions [`Self::decode_file`] understands.
        pub const DECODABLE_EXTENSIONS: &'static [&'static str] =
            &["wav", "mp3", "m4a", "mp4", "aac", "flac", "ogg", "oga", "aif", "aiff", "caf"];

        /// Decodes the first audio track of a file in any supported container.
        /// The format is detected from the content; the extension is a hint.
        pub fn decode_file(path: impl AsRef<Path>) -> Result<Self> {
            let path = path.as_ref();
            let file = std::fs::File::open(path).map_err(|e| error(path, e))?;
            let stream = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());
            let mut hint = Hint::new();
            if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
                hint.with_extension(extension);
            }
            let probed = symphonia::default::get_probe()
                .format(&hint, stream, &FormatOptions::default(), &MetadataOptions::default())
                .map_err(|e| error(path, e))?;
            let mut format = probed.format;
            let track = format
                .tracks()
                .iter()
                .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
                .ok_or_else(|| error(path, "no audio track"))?;
            let track_id = track.id;
            let mut decoder = symphonia::default::get_codecs()
                .make(&track.codec_params, &DecoderOptions::default())
                .map_err(|e| error(path, e))?;

            let (mut samples, mut sample_rate, mut channels) = (Vec::new(), 0u32, 0u32);
            let mut scratch: Option<SampleBuffer<f32>> = None;
            loop {
                let packet = match format.next_packet() {
                    Ok(packet) => packet,
                    Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                    Err(SymphoniaError::ResetRequired) => break,
                    Err(e) => return Err(error(path, e)),
                };
                if packet.track_id() != track_id {
                    continue;
                }
                let decoded = match decoder.decode(&packet) {
                    Ok(decoded) => decoded,
                    // A corrupt frame is skipped, as players do.
                    Err(SymphoniaError::DecodeError(_)) => continue,
                    Err(e) => return Err(error(path, e)),
                };
                let spec = *decoded.spec();
                if sample_rate == 0 {
                    sample_rate = spec.rate;
                    channels = spec.channels.count() as u32;
                }
                let buffer = scratch.get_or_insert_with(|| SampleBuffer::new(decoded.capacity() as u64, spec));
                if buffer.capacity() < decoded.capacity() * spec.channels.count() {
                    *buffer = SampleBuffer::new(decoded.capacity() as u64, spec);
                }
                buffer.copy_interleaved_ref(decoded);
                samples.extend_from_slice(buffer.samples());
            }
            if sample_rate == 0 || channels == 0 {
                return Err(error(path, "no decodable audio"));
            }
            Self::new(samples, sample_rate, channels)
        }
    }
}

#[cfg(feature = "resample")]
mod resample {
    use rubato::{FftFixedIn, Resampler};

    use super::AudioBuffer;
    use crate::error::{Error, Result};

    impl AudioBuffer {
        /// Converts to `sample_rate` with an FFT-based band-limited resampler.
        /// Returns `self` unchanged when the rate already matches.
        pub fn resample(self, sample_rate: u32) -> Result<Self> {
            if sample_rate == self.sample_rate || self.samples.is_empty() {
                return Ok(Self { sample_rate, ..self });
            }
            if sample_rate == 0 {
                return Err(Error::invalid_argument("target sample rate must be positive"));
            }
            let channels = self.channels.max(1) as usize;
            let frames = self.frames();
            const CHUNK: usize = 1024;
            let mut resampler =
                FftFixedIn::<f32>::new(self.sample_rate as usize, sample_rate as usize, CHUNK, 2, channels)
                    .map_err(|e| Error::invalid_argument(format!("resample: {e}")))?;

            // Deinterleave, padding the tail so every chunk is full; the
            // padding is trimmed from the output by the expected length.
            let padded = frames.div_ceil(CHUNK) * CHUNK + CHUNK;
            let mut planar: Vec<Vec<f32>> = (0..channels)
                .map(|c| {
                    let mut channel: Vec<f32> = self.samples.iter().skip(c).step_by(channels).copied().collect();
                    channel.resize(padded, 0.0);
                    channel
                })
                .collect();

            let mut output: Vec<Vec<f32>> = vec![Vec::new(); channels];
            for start in (0..padded).step_by(CHUNK) {
                let input: Vec<&[f32]> = planar.iter().map(|c| &c[start..start + CHUNK]).collect();
                let chunk =
                    resampler.process(&input, None).map_err(|e| Error::invalid_argument(format!("resample: {e}")))?;
                for (out, part) in output.iter_mut().zip(chunk) {
                    out.extend_from_slice(&part);
                }
            }
            planar.clear();

            // The resampler delays its output; drop the delay, then cut to length.
            let delay = resampler.output_delay();
            let expected = (frames as f64 * sample_rate as f64 / self.sample_rate as f64).round() as usize;
            let mut samples = Vec::with_capacity(expected * channels);
            for frame in delay..delay + expected {
                for channel in &output {
                    samples.push(channel.get(frame).copied().unwrap_or(0.0));
                }
            }
            Ok(Self { samples, sample_rate, channels: self.channels })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_frames_and_zero_rates_are_rejected() {
        assert!(AudioRef::new(&[0.0; 3], 16_000, 2).is_err());
        assert!(AudioRef::new(&[0.0; 4], 0, 2).is_err());
        assert!(AudioRef::new(&[0.0; 4], 16_000, 0).is_err());
        assert_eq!(AudioRef::new(&[0.0; 4], 16_000, 2).unwrap().frames(), 2);
    }

    #[test]
    fn concat_inserts_gaps_and_checks_formats() {
        let a = AudioBuffer::new(vec![1.0; 10], 10, 1).unwrap();
        let joined = AudioBuffer::concat([a.as_ref(), a.as_ref()], Duration::from_millis(500)).unwrap();
        assert_eq!(joined.samples.len(), 25);
        let stereo = AudioBuffer::new(vec![1.0; 10], 10, 2).unwrap();
        assert!(AudioBuffer::concat([a.as_ref(), stereo.as_ref()], Duration::ZERO).is_err());
    }

    #[test]
    fn mono_downmix_averages() {
        let stereo = AudioBuffer::new(vec![1.0, 0.0, 0.5, 0.5], 8, 2).unwrap().into_mono();
        assert_eq!(stereo.samples, vec![0.5, 0.5]);
    }

    #[cfg(feature = "wav")]
    #[test]
    fn wav_round_trip() {
        let audio = AudioBuffer::new(vec![0.0, 0.25, -0.5, 1.0], 22_050, 2).unwrap();
        let mut bytes = std::io::Cursor::new(Vec::new());
        audio.write_wav_to(&mut bytes).unwrap();
        bytes.set_position(0);
        assert_eq!(AudioBuffer::read_wav_from(bytes).unwrap(), audio);
    }

    #[cfg(all(feature = "decode", feature = "wav"))]
    #[test]
    fn decode_reads_wav_like_read_wav() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/sample_16k.wav");
        assert_eq!(AudioBuffer::decode_file(&path).unwrap(), AudioBuffer::read_wav(&path).unwrap());
    }

    #[cfg(feature = "resample")]
    #[test]
    fn resample_keeps_duration_and_tone() {
        let rate = 16_000u32;
        let tone: Vec<f32> =
            (0..rate).map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin()).collect();
        let out = AudioBuffer::new(tone, rate, 1).unwrap().resample(24_000).unwrap();
        assert_eq!(out.frames(), 24_000);
        // A clean 440 Hz tone keeps its amplitude away from the edges.
        let middle = &out.samples[6_000..18_000];
        let peak = middle.iter().fold(0.0f32, |p, s| p.max(s.abs()));
        assert!((0.95..1.05).contains(&peak), "peak {peak}");
    }
}
