//! Integer PCM encoding for the normalized f32 decoder pipeline.
//! Scale by the signed range's power of two; positive full scale saturates.

pub(crate) fn s16(sample: f32) -> i16 {
    (sample * 32_768.0) as i16
}

pub(crate) fn s24(sample: f32) -> i32 {
    (sample * 8_388_608.0).clamp(-8_388_608.0, 8_388_607.0) as i32
}

pub(crate) fn s24_packed(sample: f32) -> [u8; 3] {
    let bytes = s24(sample).to_le_bytes();
    [bytes[0], bytes[1], bytes[2]]
}

#[cfg(test)]
mod tests {
    use super::*;
    use symphonia::core::{audio::SampleBuffer, conv::FromSample, sample::i24};

    #[test]
    fn every_signed_16_and_24_bit_value_round_trips() {
        for value in i16::MIN..=i16::MAX {
            assert_eq!(
                s16(f32::from_sample(value)).to_le_bytes(),
                value.to_le_bytes()
            );
        }
        for value in -8_388_608i32..=8_388_607 {
            let decoded = f32::from_sample(i24::from(value));
            assert_eq!(s24(decoded).to_le_bytes(), value.to_le_bytes());
            assert_eq!(s24_packed(decoded), value.to_le_bytes()[..3]);
        }
    }

    #[test]
    fn nonfinite_and_out_of_range_samples_saturate_without_wrapping() {
        for sample in [f32::NAN, 0.0, -0.0] {
            assert_eq!(s16(sample), 0);
            assert_eq!(s24(sample), 0);
        }
        for sample in [1.0, 2.0, f32::INFINITY] {
            assert_eq!(s16(sample), i16::MAX);
            assert_eq!(s24(sample), 8_388_607);
        }
        for sample in [-1.0, -2.0, f32::NEG_INFINITY] {
            assert_eq!(s16(sample), i16::MIN);
            assert_eq!(s24(sample), -8_388_608);
        }
    }

    fn wav(values: &[i32], bits: u16) -> Vec<u8> {
        let width = usize::from(bits / 8);
        let size = (values.len() * width) as u32;
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&(36 + size).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
        bytes.extend_from_slice(&2u16.to_le_bytes()); // stereo
        bytes.extend_from_slice(&192_000u32.to_le_bytes());
        bytes.extend_from_slice(&(192_000u32 * 2 * width as u32).to_le_bytes());
        bytes.extend_from_slice(&(2u16 * width as u16).to_le_bytes());
        bytes.extend_from_slice(&bits.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&size.to_le_bytes());
        for value in values {
            bytes.extend_from_slice(&value.to_le_bytes()[..width]);
        }
        bytes
    }

    #[test]
    fn real_decoder_preserves_192khz_stereo_pcm_bytes() {
        use symphonia::core::{errors::Error, io::MediaSourceStream, probe::Hint};
        for bits in [16, 24] {
            let values = if bits == 16 {
                (i16::MIN..=i16::MAX).map(i32::from).collect::<Vec<_>>()
            } else {
                let mut values = vec![-8_388_608, -8_388_607, -1, 0, 1, 8_388_606, 8_388_607, 0];
                let mut state = 0x5261_984du32;
                for _ in 0..65_536 {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    values.push((state as i32) >> 8);
                }
                values
            };
            let source = MediaSourceStream::new(
                Box::new(std::io::Cursor::new(wav(&values, bits))),
                Default::default(),
            );
            let mut format = symphonia::default::get_probe()
                .format(
                    &Hint::new(),
                    source,
                    &Default::default(),
                    &Default::default(),
                )
                .unwrap()
                .format;
            let params = format.default_track().unwrap().codec_params.clone();
            assert_eq!(params.sample_rate, Some(192_000));
            assert_eq!(params.channels.unwrap().count(), 2);
            let mut decoder = symphonia::default::get_codecs()
                .make(&params, &Default::default())
                .unwrap();
            let mut offset = 0;
            loop {
                let packet = match format.next_packet() {
                    Ok(packet) => packet,
                    Err(Error::IoError(error))
                        if error.kind() == std::io::ErrorKind::UnexpectedEof =>
                    {
                        break
                    }
                    Err(error) => panic!("unexpected decoder error: {error}"),
                };
                let decoded = decoder.decode(&packet).unwrap();
                let mut samples =
                    SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());
                samples.copy_interleaved_ref(decoded);
                for sample in samples.samples() {
                    let expected = values[offset];
                    if bits == 16 {
                        assert_eq!(s16(*sample).to_le_bytes(), (expected as i16).to_le_bytes());
                    } else {
                        assert_eq!(s24(*sample).to_le_bytes(), expected.to_le_bytes());
                        assert_eq!(s24_packed(*sample), expected.to_le_bytes()[..3]);
                    }
                    offset += 1;
                }
            }
            assert_eq!(offset, values.len());
        }
    }
}
