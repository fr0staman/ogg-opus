use crate::Error;

use audiopus::SampleRate;

// We use this to check whether a file is ogg opus or not inside the client
pub(crate) const OGG_OPUS_SPS: u32 = 48000;
pub(crate) const MAX_NUM_CHANNELS: u8 = 2;
pub(crate) const OPUS_MAGIC_HEADER: [u8; 8] = [b'O', b'p', b'u', b's', b'H', b'e', b'a', b'd'];
pub(crate) const MAX_FRAME_SAMPLES: usize = 5760; // According to opus_decode docs
pub(crate) const MAX_FRAME_SIZE: usize = MAX_FRAME_SAMPLES * (MAX_NUM_CHANNELS as usize); // Our buffer will be i16 so, don't convert to bytes
pub(crate) const FRAME_TIME_MS: u32 = 20;
pub(crate) const MAX_PACKET: usize = 4000; // Maximum theorical recommended by Opus
pub(crate) const MIN_FRAME_MICROS: u32 = 25;
pub(crate) const VENDOR_STR: &str = concat!("ogg-opus", " ", std::env!("CARGO_PKG_VERSION"));

pub(crate) const fn calc_sr(val: u16, org_sr: u32, dest_sr: u32) -> u16 {
    ((val as u32 * dest_sr) / org_sr) as u16
}
pub(crate) const fn calc_sr_u64(val: u64, org_sr: u32, dest_sr: u32) -> u64 {
    (val * dest_sr as u64) / (org_sr as u64)
}

pub(crate) const fn s_ps_to_audiopus(s_ps: u32) -> Result<SampleRate, Error> {
    match s_ps {
        8000 => Ok(SampleRate::Hz8000),
        12000 => Ok(SampleRate::Hz12000),
        16000 => Ok(SampleRate::Hz16000),
        24000 => Ok(SampleRate::Hz24000),
        48000 => Ok(SampleRate::Hz48000),
        _ => Err(Error::InvalidSps),
    }
}
