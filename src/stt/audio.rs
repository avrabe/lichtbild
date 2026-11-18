// Adapted from candle-examples 0.11.1, src/audio.rs
// (https://github.com/huggingface/candle, MIT OR Apache-2.0).

/// Decode the first audio track to mono (channel 0) samples, stopping
/// after `max_seconds`. Returns the samples, the sample rate, and whether
/// the audio was cut.
pub fn pcm_decode(src: std::fs::File, max_seconds: f64) -> anyhow::Result<(Vec<f32>, u32, bool)> {
    use symphonia::core::audio::conv::FromSample;
    use symphonia::core::audio::sample::Sample;
    use symphonia::core::audio::{Audio, AudioBuffer, GenericAudioBufferRef};
    use symphonia::core::codecs::audio::AudioDecoderOptions;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::formats::TrackType;

    fn conv<T>(samples: &mut Vec<f32>, data: &AudioBuffer<T>)
    where
        T: Sample,
        f32: FromSample<T>,
    {
        if let Some(plane) = data.plane(0) {
            samples.extend(plane.iter().map(|v| f32::from_sample(*v)))
        }
    }

    // Create the media source stream.
    let mss = symphonia::core::io::MediaSourceStream::new(Box::new(src), Default::default());

    // Create a probe hint using the file's extension. [Optional]
    let hint = Hint::new();

    // Use the default options for metadata and format readers.
    let meta_opts: symphonia::core::meta::MetadataOptions = Default::default();
    let fmt_opts: symphonia::core::formats::FormatOptions = Default::default();

    // Probe the media source and get the instantiated format reader.
    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, fmt_opts, meta_opts)
        .map_err(anyhow::Error::msg)?;

    // Find the first audio track with a known (decodable) codec, and create a decoder for it.
    // The borrow on `format` is scoped so that the decode loop below can borrow it mutably.
    let (track_id, sample_rate, mut decoder) = {
        let track = format
            .first_track_known_codec(TrackType::Audio)
            .ok_or_else(|| anyhow::anyhow!("no supported audio tracks".to_string()))?;
        let codec_params = track
            .codec_params
            .as_ref()
            .and_then(|params| params.audio())
            .ok_or_else(|| anyhow::anyhow!("no supported audio tracks".to_string()))?;

        // Use the default options for the decoder.
        let dec_opts: AudioDecoderOptions = Default::default();

        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(codec_params, &dec_opts)
            .map_err(|_| anyhow::anyhow!("unsupported codec".to_string()))?;
        (track.id, codec_params.sample_rate.unwrap_or(0), decoder)
    };

    let mut pcm_data = Vec::new();
    let cap = (f64::from(sample_rate.max(1)) * max_seconds) as usize;
    let mut cut = false;
    // The decode loop.
    while let Ok(Some(packet)) = format.next_packet() {
        // Consume any new metadata that has been read since the last packet.
        while !format.metadata().is_latest() {
            format.metadata().pop();
        }

        // If the packet does not belong to the selected track, skip over it.
        if packet.track_id != track_id {
            continue;
        }
        if pcm_data.len() >= cap {
            cut = true;
            break;
        }
        match decoder.decode(&packet).map_err(anyhow::Error::msg)? {
            GenericAudioBufferRef::F32(buf) => {
                if let Some(plane) = buf.plane(0) {
                    pcm_data.extend(plane)
                }
            }
            GenericAudioBufferRef::U8(data) => conv(&mut pcm_data, data),
            GenericAudioBufferRef::U16(data) => conv(&mut pcm_data, data),
            GenericAudioBufferRef::U24(data) => conv(&mut pcm_data, data),
            GenericAudioBufferRef::U32(data) => conv(&mut pcm_data, data),
            GenericAudioBufferRef::S8(data) => conv(&mut pcm_data, data),
            GenericAudioBufferRef::S16(data) => conv(&mut pcm_data, data),
            GenericAudioBufferRef::S24(data) => conv(&mut pcm_data, data),
            GenericAudioBufferRef::S32(data) => conv(&mut pcm_data, data),
            GenericAudioBufferRef::F64(data) => conv(&mut pcm_data, data),
        }
    }
    pcm_data.truncate(cap);
    Ok((pcm_data, sample_rate, cut))
}

pub fn resample(pcm_in: &[f32], sr_in: u32, sr_out: u32) -> anyhow::Result<Vec<f32>> {
    use rubato::audioadapter_buffers::direct::SequentialSlice;
    use rubato::Resampler;

    let mut resampler = rubato::Fft::<f32>::new(
        sr_in as usize,
        sr_out as usize,
        1024,
        1,
        rubato::FixedSync::Both,
    )
    .map_err(anyhow::Error::msg)?;
    // Mono, so the interleaved and sequential layouts are the same.
    let buffer_in = SequentialSlice::new(pcm_in, 1, pcm_in.len()).map_err(anyhow::Error::msg)?;
    let pcm_out = resampler
        .process_all(&buffer_in, pcm_in.len(), None)
        .map_err(anyhow::Error::msg)?;
    Ok(pcm_out.take_data())
}
