use std::borrow::Cow;
use std::cmp::min;
use std::process;

use crate::common::*;
use crate::Error;

use audiopus::{
    coder::{Encoder as OpusEnc, GenericCtl},
    Bitrate,
};
use byteorder::{ByteOrder, LittleEndian};
use ogg::PacketWriter;
use rand::Rng;

//--- Final range  things ------------------------------------------------------

#[cfg(test)]
use std::cell::RefCell;

#[cfg(test)]
thread_local! {
    static LAST_FINAL_RANGE: RefCell<u32> = RefCell::new(0);
}

#[cfg(test)]
fn set_final_range(r: u32) {
    LAST_FINAL_RANGE.with(|f| *f.borrow_mut() = r);
}

// Just here so that it can be used in the function
#[cfg(not(test))]
fn set_final_range(_: u32) {}

#[cfg(test)]
pub(crate) fn get_final_range() -> u32 {
    LAST_FINAL_RANGE.with(|f| *f.borrow())
}

//--- Code ---------------------------------------------------------------------
const fn to_samples<const S_PS: u32>(ms: u32) -> usize {
    ((S_PS * ms) / 1000) as usize
}

// In microseconds
const fn calc_fr_size(us: u32, channels: u8, sps: u32) -> usize {
    let samps_ms = sps * us;
    const US_TO_MS: u32 = 10;
    ((samps_ms * channels as u32) / (1000 * US_TO_MS)) as usize
}

const fn opus_channels(val: u8) -> audiopus::Channels {
    if val == 1 || val == 0 {
        audiopus::Channels::Mono
    } else {
        audiopus::Channels::Stereo
    }
}

// Calc the biggest frame buffer that still is either smaller or the
// same size as the input
fn calc_biggest_spills<T: PartialOrd + Copy>(val: T, possibles: &[T]) -> Option<T> {
    for container in possibles.iter().rev() {
        if *container <= val {
            return Some(*container);
        }
    }
    None
}

const fn is_end_of_stream(pos: usize, max: usize) -> ogg::PacketWriteEndInfo {
    if pos == max {
        ogg::PacketWriteEndInfo::EndStream
    } else {
        ogg::PacketWriteEndInfo::NormalPacket
    }
}

const fn granule<const S_PS: u32>(val: usize) -> u64 {
    calc_sr_u64(val as u64, S_PS, OGG_OPUS_SPS)
}

pub fn encode<const S_PS: u32, const NUM_CHANNELS: u8>(audio: &[i16]) -> Result<Vec<u8>, Error> {
    //NOTE: In the future the S_PS const generic will let us use const on a lot
    // of things, until then we need to use variables

    // This should have a bitrate of 24 Kb/s, exactly what IBM recommends

    // More frame time, sligtly less overhead more problematic packet loses,
    // a frame time of 20ms is considered good enough for most applications

    // Data
    let frame_samples: usize = to_samples::<S_PS>(FRAME_TIME_MS);
    let frame_size: usize = frame_samples * (NUM_CHANNELS as usize);

    // Generate the serial which is nothing but a value to identify a stream, we
    // will also use the process id so that two programs don't use
    // the same serial even if getting one at the same time
    let mut rnd = rand::thread_rng();
    let serial = rnd.gen::<u32>() ^ process::id();

    let opus_sr = s_ps_to_audiopus(S_PS)?;

    let mut opus_encoder = OpusEnc::new(
        opus_sr,
        opus_channels(NUM_CHANNELS),
        audiopus::Application::Audio,
    )?;
    // Balance with quality, speed and size, especially for Telegram
    opus_encoder.set_bitrate(Bitrate::BitsPerSecond(24000))?;

    let skip = opus_encoder.lookahead()? as u16;
    let inner_encoder = InnerEncoder {
        encoder: opus_encoder,
    };
    let skip_us = skip as usize;
    let tot_samples = audio.len() + skip_us;
    let skip_48 = calc_sr(skip, S_PS, OGG_OPUS_SPS);

    let max = (tot_samples as f32 / frame_size as f32).floor() as usize;

    let calc = |counter| -> usize { counter * frame_size };
    let calc_samples = |counter| -> usize { counter * frame_samples };
    let mut buffer: Vec<u8> = Vec::with_capacity(calc(max));
    let mut packet_writer = PacketWriter::new(&mut buffer);

    let opus_head: [u8; 19] = [
        OPUS_MAGIC_HEADER[0],
        OPUS_MAGIC_HEADER[1],
        OPUS_MAGIC_HEADER[2],
        OPUS_MAGIC_HEADER[3],
        OPUS_MAGIC_HEADER[4],
        OPUS_MAGIC_HEADER[5],
        OPUS_MAGIC_HEADER[6],
        OPUS_MAGIC_HEADER[7],
        // Magic header
        1,            // Version number, always 1
        NUM_CHANNELS, // Channels
        0,
        0, //Pre-skip
        0,
        0,
        0,
        0, // Original Hz (informational)
        0,
        0, // Output gain
        0, // Channel map family
           // If Channel map != 0, here should go channel mapping table
    ];

    let mut head = opus_head;
    LittleEndian::write_u16(&mut head[10..12], skip_48 as u16); // Write pre-skip
    LittleEndian::write_u32(&mut head[12..16], S_PS); // Write Samples per second

    let mut opus_tags: Vec<u8> = Vec::with_capacity(60);
    opus_tags.extend(b"OpusTags");
    let mut len_bf = [0u8; 4];
    LittleEndian::write_u32(&mut len_bf, VENDOR_STR.len() as u32);
    opus_tags.extend(&len_bf);
    opus_tags.extend(VENDOR_STR.bytes());
    opus_tags.extend(&[0u8; 4]); // No user comments

    packet_writer.write_packet(&head[..], serial, ogg::PacketWriteEndInfo::EndPage, 0)?;
    packet_writer.write_packet(opus_tags, serial, ogg::PacketWriteEndInfo::EndPage, 0)?;

    // Do all frames
    for counter in 0..max {
        // Last value of counter is max - 1
        let pos_a = calc(counter);
        let pos_b = calc(counter + 1);
        assert!((pos_b - pos_a) <= frame_size);

        let new_buffer = inner_encoder.encode_with_skip(audio, pos_a, pos_b, skip_us)?;

        packet_writer.write_packet(
            new_buffer,
            serial,
            is_end_of_stream(pos_b, tot_samples),
            granule::<S_PS>(skip_us + calc_samples(counter + 1)),
        )?;
    }

    // Try to add as less of empty audio as possible, first everything into
    // small frames, and on the last one, if needed fill with 0, since the
    // last one is going to be smaller this should be much less of a problem
    let mut last_sample = calc(max);
    assert!(last_sample <= audio.len() + skip_us);
    let frame_sizes: [usize; 4] = [
        calc_fr_size(MIN_FRAME_MICROS, NUM_CHANNELS, S_PS),
        calc_fr_size(50, NUM_CHANNELS, S_PS),
        calc_fr_size(100, NUM_CHANNELS, S_PS),
        calc_fr_size(200, NUM_CHANNELS, S_PS),
    ];

    while last_sample < tot_samples {
        let rem_samples = tot_samples - last_sample;
        let last_audio_s = last_sample - min(last_sample, skip_us);

        match calc_biggest_spills(rem_samples, &frame_sizes) {
            Some(frame_size) => {
                let enc = if last_sample >= skip_us {
                    inner_encoder.encode_no_skip(audio, last_audio_s, frame_size)?
                } else {
                    inner_encoder.encode_with_skip(
                        audio,
                        last_sample,
                        last_sample + frame_size,
                        skip_us,
                    )?
                };
                last_sample += frame_size;
                packet_writer.write_packet(
                    enc,
                    serial,
                    is_end_of_stream(last_sample, tot_samples),
                    granule::<S_PS>(last_sample / (NUM_CHANNELS as usize)),
                )?;
            }
            None => {
                // Maximum size for a 2.5 ms frame
                const MAX_25_SIZE: usize =
                    calc_fr_size(MIN_FRAME_MICROS, MAX_NUM_CHANNELS, OGG_OPUS_SPS);
                let mut in_buffer = [0i16; MAX_25_SIZE];
                let rem_skip = skip_us - min(last_sample, skip_us);
                in_buffer[rem_skip..rem_samples].copy_from_slice(&audio[last_audio_s..]);

                last_sample = tot_samples; // We end this here

                let enc = inner_encoder.encode_no_skip(&in_buffer, 0, frame_sizes[0])?;
                packet_writer.write_packet(
                    enc,
                    serial,
                    ogg::PacketWriteEndInfo::EndStream,
                    granule::<S_PS>((skip_us + audio.len()) / (NUM_CHANNELS as usize)),
                )?;
            }
        }
    }

    if cfg!(test) {
        set_final_range(inner_encoder.encoder.final_range().unwrap())
    }

    Ok(buffer)
}

struct InnerEncoder {
    encoder: OpusEnc,
}

impl InnerEncoder {
    fn encode_vec(&self, audio: &[i16]) -> Result<Cow<'_, [u8]>, Error> {
        let mut output: Vec<u8> = vec![0; MAX_PACKET];
        let result = self.encoder.encode(audio, &mut output)?;
        output.truncate(result);
        Ok(output.into())
    }

    fn encode_with_skip(
        &self,
        audio: &[i16],
        pos_a: usize,
        pos_b: usize,
        skip_us: usize,
    ) -> Result<Cow<'_, [u8]>, Error> {
        if pos_a > skip_us {
            self.encode_vec(&audio[pos_a - skip_us..pos_b - skip_us])
        } else {
            let mut buf = vec![0; pos_b - pos_a];
            if pos_b > skip_us {
                buf[skip_us - pos_a..].copy_from_slice(&audio[..pos_b - skip_us]);
            }
            self.encode_vec(&buf)
        }
    }

    fn encode_no_skip(
        &self,
        audio: &[i16],
        start: usize,
        frame_size: usize,
    ) -> Result<Cow<'_, [u8]>, Error> {
        self.encode_vec(&audio[start..start + frame_size])
    }
}
