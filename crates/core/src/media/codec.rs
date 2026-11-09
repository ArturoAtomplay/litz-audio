use crate::pcm::RATE;
use crate::protocol::{Config, FRAME, HEADER, Header, MAX_DATAGRAM, Stream};
use opus::{Application, Bitrate, Channels};

fn channels(stream: Stream) -> Channels {
    match stream {
        Stream::Mic => Channels::Mono,
        Stream::Playback => Channels::Stereo,
    }
}

pub struct Encoder {
    encoder: opus::Encoder,
    config: Config,
    sequence: u32,
    timestamp: u64,
    bitrate: i32,
}
impl Encoder {
    pub fn try_new(config: Config) -> Result<Self, opus::Error> {
        // CELT-only: these bitrates never select SILK, and the other
        // applications add 4 ms of encoder lookahead for mode switching.
        let bitrate = config.stream.bitrates().0;
        let mut encoder = opus::Encoder::new(RATE, channels(config.stream), Application::LowDelay)?;
        encoder.set_bitrate(Bitrate::Bits(bitrate))?;
        encoder.set_vbr(true)?;
        encoder.set_vbr_constraint(true)?;
        encoder.set_dtx(false)?;
        encoder.set_complexity(7)?;
        Ok(Self {
            encoder,
            config,
            sequence: 0,
            timestamp: 0,
            bitrate,
        })
    }
    pub fn reduce_bitrate(&mut self) -> Result<(), opus::Error> {
        self.bitrate = (self.bitrate * 4 / 5).max(self.config.stream.bitrates().1);
        self.encoder.set_bitrate(Bitrate::Bits(self.bitrate))
    }
    pub fn recover_bitrate(&mut self) -> Result<(), opus::Error> {
        self.bitrate = (self.bitrate * 5 / 4).min(self.config.stream.bitrates().0);
        self.encoder.set_bitrate(Bitrate::Bits(self.bitrate))
    }
    pub fn encode(
        &mut self,
        pcm: &[f32],
        packet: &mut [u8],
        path_limit: usize,
    ) -> Result<usize, &'static str> {
        let limit = packet.len().min(path_limit).min(MAX_DATAGRAM);
        if limit < HEADER + self.bitrate as usize / 800
            || pcm.len() != FRAME * self.config.channels()
        {
            return Err("codec frame/limit");
        }
        Header {
            config: self.config,
            sequence: self.sequence,
            timestamp: self.timestamp,
        }
        .write(packet)?;
        let size = self
            .encoder
            .encode_float(pcm, &mut packet[HEADER..limit])
            .map_err(|_| "Opus encode")?;
        self.sequence = self.sequence.wrapping_add(1);
        self.timestamp = self.timestamp.wrapping_add(FRAME as u64);
        Ok(HEADER + size)
    }
}

pub struct Decoder {
    decoder: opus::Decoder,
    channels: usize,
}
impl Decoder {
    pub fn reset(&mut self) {
        let _ = self.decoder.reset_state();
    }
    pub fn try_new(config: Config) -> Result<Self, opus::Error> {
        Ok(Self {
            decoder: opus::Decoder::new(RATE, channels(config.stream))?,
            channels: config.channels(),
        })
    }
    pub fn decode(&mut self, packet: &[u8], out: &mut [f32]) -> Result<(), &'static str> {
        if out.len() != FRAME * self.channels {
            return Err("decode buffer");
        }
        if !packet.is_empty()
            && opus::packet::get_nb_samples(packet, RATE).map_err(|_| "Opus packet")? != FRAME
        {
            return Err("Opus duration");
        }
        let frames = self
            .decoder
            .decode_float(packet, out, false)
            .map_err(|_| "Opus decode")?;
        if frames != FRAME {
            return Err("Opus duration");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn congestion_bitrate_recovers_to_original_ceiling() {
        for stream in Stream::ALL {
            let mut encoder = Encoder::try_new(Config { stream, epoch: 1 }).unwrap();
            let original = encoder.bitrate;
            for _ in 0..8 {
                encoder.reduce_bitrate().unwrap();
            }
            assert!(encoder.bitrate < original);
            for _ in 0..16 {
                encoder.recover_bitrate().unwrap();
            }
            assert_eq!(encoder.bitrate, original);
        }
    }
    #[test]
    fn balanced_packet_bounds() {
        for stream in Stream::ALL {
            let config = Config { stream, epoch: 1 };
            let mut encoder = Encoder::try_new(config).unwrap();
            let mut decoder = Decoder::try_new(config).unwrap();
            let pcm = [0.1; FRAME * 2];
            let mut decoded = [0.0; FRAME * 2];
            let mut packet = [0; MAX_DATAGRAM];
            for limit in [MAX_DATAGRAM, 576, 576, 576, 576, 576] {
                let len = encoder
                    .encode(&pcm[..FRAME * config.channels()], &mut packet, limit)
                    .unwrap();
                assert!(len <= limit);
                let (_, payload) = Header::parse(&packet[..len], config, limit).unwrap();
                decoder
                    .decode(payload, &mut decoded[..FRAME * config.channels()])
                    .unwrap();
                encoder.reduce_bitrate().unwrap();
            }
            assert!(
                encoder
                    .encode(&pcm[..FRAME * config.channels()], &mut packet, HEADER)
                    .is_err()
            );
        }
    }

    use proptest::prelude::*;

    proptest! {
        #[test]
        fn opus_encode_decode_proptest(
            samples in prop::collection::vec(-1.0f32..1.0f32, FRAME)
        ) {
            let config = Config { stream: Stream::Mic, epoch: 1 };
            let mut encoder = Encoder::try_new(config).unwrap();
            let mut decoder = Decoder::try_new(config).unwrap();
            let mut packet = [0u8; MAX_DATAGRAM];
            let len = encoder.encode(&samples, &mut packet, MAX_DATAGRAM).unwrap();
            prop_assert!(len <= MAX_DATAGRAM);
            prop_assert!(len > HEADER);

            let (header, payload) = Header::parse(&packet[..len], config, MAX_DATAGRAM).unwrap();
            prop_assert_eq!(header.config, config);

            let mut output = [0.0f32; FRAME];
            decoder.decode(payload, &mut output).unwrap();
            prop_assert!(output.iter().all(|s| s.is_finite()));
        }
    }
}
