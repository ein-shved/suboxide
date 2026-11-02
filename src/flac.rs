//! Native FLAC frame indexing. Compressed subframes are parsed, never decoded.

use std::fs::{File, Metadata};
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::Path;
use std::time::UNIX_EPOCH;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// The exact file version to which a stored frame index belongs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    pub size: u64,
    pub modified_ns: u128,
}

impl Fingerprint {
    pub fn from_metadata(metadata: &Metadata) -> io::Result<Self> {
        Ok(Self {
            size: metadata.len(),
            modified_ns: metadata
                .modified()?
                .duration_since(UNIX_EPOCH)
                .map_err(io::Error::other)?
                .as_nanos(),
        })
    }
}

/// One complete frame, including its original header and CRC footer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub offset: u64,
    pub length: u64,
    pub sample_start: u64,
    pub samples: u32,
    pub header: Vec<u8>,
    pub checksum: u16,
}

/// Shared index for one FLAC image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameIndex {
    pub streaminfo: Vec<u8>,
    pub sample_rate: u32,
    pub channels: u8,
    pub bit_depth: u8,
    pub total_samples: u64,
    pub frames: Vec<Frame>,
}

/// Scan-local pair; its per-frame index is never persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueSource {
    pub image_path: String,
    pub cue_content: String,
    pub fingerprint: Fingerprint,
    pub index: FrameIndex,
}

impl FrameIndex {
    fn read_metadata<R: Read>(reader: &mut Bits<R>) -> io::Result<(Self, u64)> {
        let mut signature = [0; 4];
        reader.inner.read_exact(&mut signature)?;
        reader.position = 4;
        if &signature != b"fLaC" {
            return Err(invalid("not a native FLAC stream"));
        }
        let mut streaminfo = Vec::new();
        loop {
            let kind = reader.byte()?;
            let length = (u32::from(reader.byte()?) << 16)
                | (u32::from(reader.byte()?) << 8)
                | u32::from(reader.byte()?);
            if streaminfo.is_empty() {
                if kind & 0x7f != 0 || length != 34 {
                    return Err(invalid("FLAC must start with STREAMINFO"));
                }
                for _ in 0..length {
                    streaminfo.push(reader.byte()?);
                }
            } else {
                if kind.trailing_zeros() >= 7 || kind & 0x7f == 127 {
                    return Err(invalid("invalid FLAC metadata block"));
                }
                reader.skip(u64::from(length) * 8)?;
            }
            if kind & 0x80 != 0 {
                break;
            }
        }
        let packed = u64::from_be_bytes(streaminfo[10..18].try_into().map_err(io::Error::other)?);
        let sample_rate = u32::try_from(packed >> 44).map_err(io::Error::other)?;
        let channels = u8::try_from((packed >> 41) & 7).map_err(io::Error::other)? + 1;
        let bit_depth = u8::try_from((packed >> 36) & 31).map_err(io::Error::other)? + 1;
        if sample_rate == 0 || bit_depth < 4 {
            return Err(invalid("invalid STREAMINFO audio properties"));
        }
        Ok((
            Self {
                streaminfo,
                sample_rate,
                channels,
                bit_depth,
                total_samples: 0,
                frames: Vec::new(),
            },
            packed & ((1 << 36) - 1),
        ))
    }
    /// Parse actual subframe lengths and validate both CRCs; sync-like payload
    /// bytes cannot become accidental frame boundaries.
    pub fn read(path: &Path) -> io::Result<(Self, Fingerprint)> {
        let file = File::open(path)?;
        let fingerprint = Fingerprint::from_metadata(&file.metadata()?)?;
        let mut reader = Bits::new(BufReader::new(file));
        let (mut index, declared_samples) = Self::read_metadata(&mut reader)?;
        index.read_frames(&mut reader, fingerprint.size, None)?;
        if declared_samples != 0 && declared_samples != index.total_samples {
            return Err(invalid("FLAC sample count mismatch"));
        }
        if Fingerprint::from_metadata(&reader.inner.get_ref().metadata()?)? != fingerprint {
            return Err(invalid("FLAC source changed while indexing"));
        }
        Ok((index, fingerprint))
    }

    /// Parse just the selected byte interval, without reading preceding audio
    /// or original metadata. Song columns supply the STREAMINFO parameters.
    pub fn read_interval(
        file: &mut File,
        track: &crate::models::music::CueTrack,
        song: &crate::models::music::Song,
    ) -> io::Result<Self> {
        let start = u64::try_from(track.byte_start).map_err(io::Error::other)?;
        let end = u64::try_from(track.byte_end).map_err(io::Error::other)?;
        let first = u64::try_from(track.start_frame).map_err(io::Error::other)?;
        let count = u64::try_from(track.end_frame - track.start_frame).map_err(io::Error::other)?;
        let sample_rate = song
            .sampling_rate
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 0)
            .ok_or_else(|| invalid("missing FLAC sampling rate"))?;
        let channels = song
            .channel_count
            .and_then(|n| u8::try_from(n).ok())
            .filter(|n| (1..=8).contains(n))
            .ok_or_else(|| invalid("missing FLAC channels"))?;
        let bit_depth = song
            .bit_depth
            .and_then(|n| u8::try_from(n).ok())
            .filter(|n| (4..=32).contains(n))
            .ok_or_else(|| invalid("missing FLAC bit depth"))?;
        if start >= end || count == 0 || end > file.metadata()?.len() {
            return Err(invalid("invalid CUE byte/frame interval"));
        }
        file.seek(SeekFrom::Start(start))?;
        let mut reader = Bits::new(BufReader::new(file.take(end - start)));
        reader.position = start;
        let mut index = Self {
            streaminfo: vec![0; 34],
            sample_rate,
            channels,
            bit_depth,
            total_samples: 0,
            frames: Vec::new(),
        };
        index.read_frames(&mut reader, end, Some(first))?;
        if index.frames.len() as u64 != count {
            return Err(invalid("CUE frame count mismatch"));
        }
        Ok(index)
    }

    fn read_frames<R: Read>(
        &mut self,
        reader: &mut Bits<R>,
        end: u64,
        first_ordinal: Option<u64>,
    ) -> io::Result<()> {
        let mut strategy = None;
        let mut first_sample = 0;
        while reader.position < end {
            let offset = reader.position;
            reader.crc = 0;
            let header = read_header(reader, self)?;
            let variable = header.bytes[1] & 1 != 0;
            if strategy.is_some_and(|old| old != variable) {
                return Err(invalid("FLAC blocking strategy changed"));
            }
            strategy = Some(variable);
            if self.frames.is_empty() && first_ordinal.is_some() {
                first_sample = header.number;
            }
            let expected = if variable {
                first_sample + self.total_samples
            } else {
                first_ordinal.unwrap_or(0) + self.frames.len() as u64
            };
            if header.number != expected {
                return Err(invalid("noncontiguous FLAC frame numbering"));
            }
            if !variable
                && self
                    .frames
                    .last()
                    .is_some_and(|frame| frame.samples != self.frames[0].samples)
            {
                return Err(invalid("short fixed block before final frame"));
            }
            if !variable
                && self
                    .frames
                    .first()
                    .is_some_and(|first| header.samples > first.samples)
            {
                return Err(invalid("fixed block size increased"));
            }
            let assignment = header.bytes[3] >> 4;
            for channel in 0..self.channels {
                let side = matches!((assignment, channel), (8 | 10, 1) | (9, 0));
                skip_subframe(reader, header.samples, self.bit_depth + u8::from(side))?;
            }
            if reader.remaining > 0 && reader.read(reader.remaining)? != 0 {
                return Err(invalid("nonzero FLAC frame padding"));
            }
            let checksum = reader.crc;
            reader.byte()?;
            reader.byte()?;
            if reader.crc != 0 {
                return Err(invalid("FLAC frame CRC mismatch"));
            }
            if header.samples < 16 && reader.position < end {
                return Err(invalid("short block before final frame"));
            }
            self.frames.push(Frame {
                offset,
                length: reader.position - offset,
                sample_start: self.total_samples,
                samples: header.samples,
                header: header.bytes,
                checksum,
            });
            self.total_samples += u64::from(header.samples);
        }
        if self.frames.is_empty() || reader.position != end {
            return Err(invalid("empty or truncated FLAC interval"));
        }
        Ok(())
    }

    /// Ceiling cuts compare rational CUE ticks without floating point rounding.
    pub fn intervals(&self, ticks: &[u64]) -> io::Result<Vec<Range<usize>>> {
        let cuts = ticks
            .iter()
            .map(|&tick| {
                if u128::from(tick) * u128::from(self.sample_rate)
                    >= u128::from(self.total_samples) * 75
                {
                    return Err(invalid("CUE boundary outside source"));
                }
                Ok(self.frames.partition_point(|frame| {
                    u128::from(frame.sample_start) * 75
                        < u128::from(tick) * u128::from(self.sample_rate)
                }))
            })
            .collect::<io::Result<Vec<_>>>()?;
        cuts.iter()
            .enumerate()
            .map(|(i, &start)| {
                let end = cuts.get(i + 1).copied().unwrap_or(self.frames.len());
                if start >= end {
                    Err(invalid("empty rounded CUE interval"))
                } else {
                    Ok(start..end)
                }
            })
            .collect()
    }
}

struct Header {
    bytes: Vec<u8>,
    samples: u32,
    number: u64,
}

fn read_header<R: Read>(reader: &mut Bits<R>, index: &FrameIndex) -> io::Result<Header> {
    let mut bytes = Vec::with_capacity(16);
    for _ in 0..4 {
        bytes.push(reader.byte()?);
    }
    if bytes[0] != 0xff || bytes[1] & 0xfe != 0xf8 || bytes[3] & 1 != 0 {
        return Err(invalid("invalid FLAC frame sync or reserved bit"));
    }
    let number = read_number(reader, &mut bytes)?;
    let block = bytes[2] >> 4;
    let samples = match block {
        1 => 192,
        2..=5 => 576 << (block - 2),
        6 => {
            let n = reader.byte()?;
            bytes.push(n);
            u32::from(n) + 1
        }
        7 => {
            let hi = reader.byte()?;
            let lo = reader.byte()?;
            bytes.extend([hi, lo]);
            u32::from(u16::from_be_bytes([hi, lo])) + 1
        }
        8..=15 => 256 << (block - 8),
        _ => return Err(invalid("reserved FLAC block size")),
    };
    let rate = match bytes[2] & 15 {
        0 => index.sample_rate,
        n @ 1..=11 => [
            0, 88200, 176_400, 192_000, 8000, 16000, 22050, 24000, 32000, 44100, 48000, 96000,
        ][usize::from(n)],
        12 => {
            let n = reader.byte()?;
            bytes.push(n);
            u32::from(n) * 1000
        }
        n @ (13 | 14) => {
            let hi = reader.byte()?;
            let lo = reader.byte()?;
            bytes.extend([hi, lo]);
            u32::from(u16::from_be_bytes([hi, lo])) * if n == 14 { 10 } else { 1 }
        }
        _ => return Err(invalid("reserved FLAC sample rate")),
    };
    let depth = match (bytes[3] >> 1) & 7 {
        0 => index.bit_depth,
        1 => 8,
        2 => 12,
        4 => 16,
        5 => 20,
        6 => 24,
        7 => 32,
        _ => return Err(invalid("reserved FLAC bit depth")),
    };
    let channels = match bytes[3] >> 4 {
        n @ 0..=7 => n + 1,
        8..=10 => 2,
        _ => return Err(invalid("reserved FLAC channel assignment")),
    };
    if samples > 65535
        || rate != index.sample_rate
        || depth != index.bit_depth
        || channels != index.channels
    {
        return Err(invalid("inconsistent FLAC frame properties"));
    }
    let crc = reader.byte()?;
    if crc8(&bytes) != crc {
        return Err(invalid("FLAC header CRC mismatch"));
    }
    bytes.push(crc);
    Ok(Header {
        bytes,
        samples,
        number,
    })
}

fn read_number<R: Read>(reader: &mut Bits<R>, bytes: &mut Vec<u8>) -> io::Result<u64> {
    let first = reader.byte()?;
    bytes.push(first);
    let count = first.leading_ones();
    let mut number = match count {
        0 => u64::from(first),
        2..=7 => u64::from(first & (0x7f >> count)),
        _ => return Err(invalid("invalid FLAC coded number")),
    };
    for _ in 1..count {
        let next = reader.byte()?;
        if next & 0xc0 != 0x80 {
            return Err(invalid("invalid FLAC coded number continuation"));
        }
        bytes.push(next);
        number = (number << 6) | u64::from(next & 0x3f);
    }
    let min_number = match count {
        2 => 0x80,
        3 => 0x800,
        4 => 0x1_0000,
        5 => 0x20_0000,
        6 => 0x400_0000,
        7 => 0x8000_0000,
        _ => 0,
    };
    if number < min_number || number >= (1 << if bytes[1] & 1 == 0 { 31 } else { 36 }) {
        return Err(invalid("noncanonical FLAC coded number"));
    }
    Ok(number)
}

fn skip_subframe<R: Read>(reader: &mut Bits<R>, samples: u32, depth: u8) -> io::Result<()> {
    let header = reader.read(8)?;
    if header & 0x80 != 0 {
        return Err(invalid("reserved FLAC subframe bit"));
    }
    let wasted = if header & 1 == 0 {
        0
    } else {
        reader.unary()? + 1
    };
    let depth = u64::from(depth)
        .checked_sub(wasted)
        .filter(|&n| n > 0)
        .ok_or_else(|| invalid("invalid wasted bits"))?;
    let kind = (header >> 1) & 63;
    let order = match kind {
        0 => return reader.skip(depth),
        1 => return reader.skip(u64::from(samples) * depth),
        8..=12 => kind - 8,
        32..=63 => kind - 31,
        _ => return Err(invalid("reserved FLAC subframe type")),
    };
    if order > u64::from(samples) {
        return Err(invalid("predictor order exceeds block size"));
    }
    reader.skip(order * depth)?;
    if kind >= 32 {
        let precision = reader.read(4)? + 1;
        if precision == 16 || reader.read(5)? & 16 != 0 {
            return Err(invalid("invalid FLAC LPC precision or shift"));
        }
        reader.skip(order * precision)?;
    }
    let parameter_bits = match reader.read(2)? {
        0 => 4,
        1 => 5,
        _ => return Err(invalid("reserved FLAC residual method")),
    };
    let partitions = 1_u32 << reader.read(4)?;
    if !samples.is_multiple_of(partitions) || u64::from(samples / partitions) <= order {
        return Err(invalid("invalid FLAC residual partitions"));
    }
    for partition in 0..partitions {
        let parameter = reader.read(parameter_bits)?;
        let count = u64::from(samples / partitions) - if partition == 0 { order } else { 0 };
        if parameter == (1 << parameter_bits) - 1 {
            let raw_bits = reader.read(5)?;
            reader.skip(count * raw_bits)?;
        } else {
            for _ in 0..count {
                reader.unary()?;
                reader.skip(parameter)?;
            }
        }
    }
    Ok(())
}

struct Bits<R> {
    inner: R,
    position: u64,
    byte: u8,
    remaining: u8,
    crc: u16,
}

impl<R: Read> Bits<R> {
    const fn new(inner: R) -> Self {
        Self {
            inner,
            position: 0,
            byte: 0,
            remaining: 0,
            crc: 0,
        }
    }
    fn byte(&mut self) -> io::Result<u8> {
        let mut data = [0];
        self.inner.read_exact(&mut data)?;
        self.position += 1;
        self.crc = crc16_byte(self.crc, data[0]);
        Ok(data[0])
    }
    fn read(&mut self, count: u8) -> io::Result<u64> {
        let mut value = 0;
        for _ in 0..count {
            if self.remaining == 0 {
                self.byte = self.byte()?;
                self.remaining = 8;
            }
            self.remaining -= 1;
            value = (value << 1) | u64::from((self.byte >> self.remaining) & 1);
        }
        Ok(value)
    }
    fn skip(&mut self, mut count: u64) -> io::Result<()> {
        let buffered = count.min(u64::from(self.remaining));
        self.remaining -= u8::try_from(buffered).map_err(io::Error::other)?;
        count -= buffered;
        while count >= 8 {
            self.byte()?;
            count -= 8;
        }
        self.read(u8::try_from(count).map_err(io::Error::other)?)?;
        Ok(())
    }
    fn unary(&mut self) -> io::Result<u64> {
        let mut zeros = 0;
        loop {
            if self.remaining == 0 {
                self.byte = self.byte()?;
                self.remaining = 8;
            }
            let leading = (self.byte << (8 - self.remaining)).leading_zeros();
            let available = u8::try_from(leading)
                .map_err(io::Error::other)?
                .min(self.remaining);
            self.remaining -= available;
            zeros += u64::from(available);
            if self.remaining > 0 {
                self.remaining -= 1;
                return Ok(zeros);
            }
        }
    }
}

fn crc8(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0, |mut crc, &byte| {
        crc ^= byte;
        for _ in 0..8 {
            crc = (crc << 1) ^ if crc & 0x80 == 0 { 0 } else { 7 };
        }
        crc
    })
}

const fn crc16_byte(mut crc: u16, byte: u8) -> u16 {
    crc ^= (byte as u16) << 8;
    let mut bit = 0;
    while bit < 8 {
        crc = (crc << 1) ^ if crc & 0x8000 == 0 { 0 } else { 0x8005 };
        bit += 1;
    }
    crc
}

fn crc16(bytes: &[u8]) -> u16 {
    bytes.iter().fold(0, |crc, &byte| crc16_byte(crc, byte))
}

const fn apply_crc_matrix(matrix: &[u16; 16], mut value: u16) -> u16 {
    let mut result = 0;
    let mut bit = 0;
    while value != 0 {
        if value & 1 != 0 {
            result ^= matrix[bit];
        }
        value >>= 1;
        bit += 1;
    }
    result
}

const CRC_SHIFT: [[u16; 16]; 64] = {
    let mut powers = [[0; 16]; 64];
    let mut bit = 0;
    while bit < 16 {
        powers[0][bit] = crc16_byte(1 << bit, 0);
        bit += 1;
    }
    let mut power = 1;
    while power < 64 {
        bit = 0;
        while bit < 16 {
            powers[power][bit] = apply_crc_matrix(&powers[power - 1], powers[power - 1][bit]);
            bit += 1;
        }
        power += 1;
    }
    powers
};

fn shifted_crc(mut crc: u16, mut bytes: u64) -> u16 {
    for matrix in &CRC_SHIFT {
        if bytes & 1 != 0 {
            crc = apply_crc_matrix(matrix, crc);
        }
        bytes >>= 1;
        if bytes == 0 {
            break;
        }
    }
    crc
}

fn coded_number(mut number: u64) -> io::Result<Vec<u8>> {
    let length = [7, 11, 16, 21, 26, 31, 36]
        .iter()
        .position(|&bits| number < (1_u64 << bits))
        .ok_or_else(|| invalid("FLAC coded number overflow"))?
        + 1;
    let mut bytes = vec![0; length];
    for byte in bytes.iter_mut().skip(1).rev() {
        *byte = 0x80 | u8::try_from(number & 63).map_err(io::Error::other)?;
        number >>= 6;
    }
    bytes[0] = u8::try_from(number).map_err(io::Error::other)?
        | if length == 1 {
            0
        } else {
            u8::MAX << (8 - length)
        };
    Ok(bytes)
}

fn rebased_header(frame: &Frame, number: u64) -> io::Result<Vec<u8>> {
    let mut header = frame
        .header
        .get(..4)
        .ok_or_else(|| invalid("stored FLAC header"))?
        .to_vec();
    let number_length = frame
        .header
        .get(4)
        .ok_or_else(|| invalid("stored FLAC number"))?
        .leading_ones()
        .max(1);
    let tail = frame
        .header
        .get(4 + usize::try_from(number_length).map_err(io::Error::other)?..frame.header.len() - 1)
        .ok_or_else(|| invalid("stored FLAC header extension"))?;
    header.extend(coded_number(number)?);
    header.extend(tail);
    header.push(crc8(&header));
    Ok(header)
}

fn metadata_block(output: &mut Vec<u8>, kind: u8, data: &[u8]) -> io::Result<()> {
    let length = u32::try_from(data.len()).map_err(io::Error::other)?;
    if length > 0xff_ffff {
        return Err(invalid("FLAC metadata block too large"));
    }
    output.push(kind);
    output.extend(&length.to_be_bytes()[1..]);
    output.extend(data);
    Ok(())
}

fn track_comments(
    song: &crate::models::music::Song,
    album_artist: Option<&str>,
) -> io::Result<Vec<u8>> {
    let fields = [
        ("TITLE", Some(song.title.as_str())),
        ("ARTIST", song.artist_name.as_deref()),
        ("ALBUM", song.album_name.as_deref()),
        ("ALBUMARTIST", album_artist),
        ("GENRE", song.genre.as_deref()),
        ("MUSICBRAINZ_TRACKID", song.musicbrainz_id.as_deref()),
    ];
    let mut fields: Vec<_> = fields
        .into_iter()
        .filter_map(|(key, value)| value.map(|value| format!("{key}={value}")))
        .collect();
    fields.extend(
        [
            song.track_number
                .map(|number| format!("TRACKNUMBER={number}")),
            song.disc_number
                .map(|number| format!("DISCNUMBER={number}")),
            song.year.map(|year| format!("DATE={year}")),
        ]
        .into_iter()
        .flatten(),
    );
    let vendor = b"suboxide";
    let mut data = u32::try_from(vendor.len())
        .map_err(io::Error::other)?
        .to_le_bytes()
        .to_vec();
    data.extend(vendor);
    data.extend(
        u32::try_from(fields.len())
            .map_err(io::Error::other)?
            .to_le_bytes(),
    );
    for field in fields {
        data.extend(
            u32::try_from(field.len())
                .map_err(io::Error::other)?
                .to_le_bytes(),
        );
        data.extend(field.as_bytes());
    }
    Ok(data)
}

#[derive(Debug)]
enum SegmentData {
    Bytes(Vec<u8>),
    Source { offset: u64, length: u64 },
}

impl SegmentData {
    const fn length(&self) -> u64 {
        match self {
            Self::Bytes(bytes) => bytes.len() as u64,
            Self::Source { length, .. } => *length,
        }
    }
}

#[derive(Debug)]
struct Segment {
    start: u64,
    data: SegmentData,
}

/// A seekable standalone FLAC assembled from metadata and original compressed
/// payload slices. Construction and seeking read no source audio bytes.
#[derive(Debug)]
pub struct VirtualTrack {
    file: File,
    segments: Vec<Segment>,
    length: u64,
    position: u64,
}

impl VirtualTrack {
    pub fn new(
        file: File,
        index: &FrameIndex,
        song: &crate::models::music::Song,
        album_artist: Option<&str>,
    ) -> io::Result<Self> {
        let frames = Some(index.frames.as_slice())
            .filter(|frames| !frames.is_empty())
            .ok_or_else(|| invalid("invalid stored CUE frame interval"))?;
        let mut segments = Vec::with_capacity(1 + frames.len() * 3);
        let mut samples = 0_u64;
        let mut min_frame = u64::MAX;
        let mut max_frame = 0;
        for (ordinal, frame) in frames.iter().enumerate() {
            let number = if frame.header.get(1).is_some_and(|flags| flags & 1 != 0) {
                samples
            } else {
                ordinal as u64
            };
            let header = rebased_header(frame, number)?;
            let payload_length = frame
                .length
                .checked_sub(frame.header.len() as u64 + 2)
                .ok_or_else(|| invalid("invalid stored FLAC frame length"))?;
            let checksum =
                frame.checksum ^ shifted_crc(crc16(&frame.header) ^ crc16(&header), payload_length);
            let length = header.len() as u64 + payload_length + 2;
            min_frame = min_frame.min(length);
            max_frame = max_frame.max(length);
            segments.push(SegmentData::Bytes(header));
            segments.push(SegmentData::Source {
                offset: frame.offset + frame.header.len() as u64,
                length: payload_length,
            });
            segments.push(SegmentData::Bytes(checksum.to_be_bytes().to_vec()));
            samples += u64::from(frame.samples);
        }
        let metadata = Self::metadata(
            index,
            frames,
            samples,
            min_frame,
            max_frame,
            song,
            album_artist,
        )?;
        let mut position = metadata.len() as u64;
        let mut positioned = vec![Segment {
            start: 0,
            data: SegmentData::Bytes(metadata),
        }];
        for data in segments {
            let length = data.length();
            positioned.push(Segment {
                start: position,
                data,
            });
            position += length;
        }
        Ok(Self {
            file,
            segments: positioned,
            length: position,
            position: 0,
        })
    }

    fn metadata(
        index: &FrameIndex,
        frames: &[Frame],
        samples: u64,
        min_frame: u64,
        max_frame: u64,
        song: &crate::models::music::Song,
        album_artist: Option<&str>,
    ) -> io::Result<Vec<u8>> {
        let mut info = index.streaminfo.clone();
        if info.len() != 34
            || samples >= 1 << 36
            || !(1..=8).contains(&index.channels)
            || !(4..=32).contains(&index.bit_depth)
            || index.sample_rate == 0
        {
            return Err(invalid("invalid stored FLAC STREAMINFO"));
        }
        let max_block = frames
            .iter()
            .map(|frame| frame.samples)
            .max()
            .ok_or_else(|| invalid("empty interval"))?
            .max(16);
        let min_block = if frames[0].header[1] & 1 == 0 {
            max_block
        } else {
            frames
                .iter()
                .map(|frame| frame.samples)
                .min()
                .ok_or_else(|| invalid("empty interval"))?
                .max(16)
        };
        info[..2].copy_from_slice(
            &u16::try_from(min_block)
                .map_err(io::Error::other)?
                .to_be_bytes(),
        );
        info[2..4].copy_from_slice(
            &u16::try_from(max_block)
                .map_err(io::Error::other)?
                .to_be_bytes(),
        );
        info[4..7].copy_from_slice(
            &u32::try_from(min_frame)
                .map_err(io::Error::other)?
                .to_be_bytes()[1..],
        );
        info[7..10].copy_from_slice(
            &u32::try_from(max_frame)
                .map_err(io::Error::other)?
                .to_be_bytes()[1..],
        );
        let packed = (u64::from(index.sample_rate) << 44)
            | (u64::from(index.channels - 1) << 41)
            | (u64::from(index.bit_depth - 1) << 36)
            | samples;
        info[10..18].copy_from_slice(&packed.to_be_bytes());
        info[18..34].fill(0);
        let mut output = b"fLaC".to_vec();
        metadata_block(&mut output, 0, &info)?;
        metadata_block(&mut output, 0x84, &track_comments(song, album_artist)?)?;
        Ok(output)
    }

    #[must_use]
    pub const fn length(&self) -> u64 {
        self.length
    }
}

impl Read for VirtualTrack {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() || self.position >= self.length {
            return Ok(0);
        }
        let segment = &self.segments[self
            .segments
            .partition_point(|segment| segment.start <= self.position)
            - 1];
        let within = self.position - segment.start;
        let limit = usize::try_from((segment.data.length() - within).min(output.len() as u64))
            .map_err(io::Error::other)?;
        let read = match &segment.data {
            SegmentData::Bytes(bytes) => {
                let start = usize::try_from(within).map_err(io::Error::other)?;
                output[..limit].copy_from_slice(&bytes[start..start + limit]);
                limit
            }
            SegmentData::Source { offset, .. } => {
                self.file.seek(SeekFrom::Start(offset + within))?;
                let read = self.file.read(&mut output[..limit])?;
                if read == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "FLAC source truncated during playback",
                    ));
                }
                read
            }
        };
        self.position += read as u64;
        Ok(read)
    }
}

impl Seek for VirtualTrack {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.position = match from {
            SeekFrom::Start(position) => position,
            SeekFrom::End(offset) => self
                .length
                .checked_add_signed(offset)
                .ok_or_else(|| invalid("invalid virtual seek"))?,
            SeekFrom::Current(offset) => self
                .position
                .checked_add_signed(offset)
                .ok_or_else(|| invalid("invalid virtual seek"))?,
        };
        Ok(self.position)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn residual_partition_must_exceed_predictor_order() {
        for (partition_order, partitions, expected_valid) in
            [("0011", 8, true), ("0100", 16, false)]
        {
            // Fixed predictor order 1, 16 samples, zero warm-up and Rice(0)
            // residuals. Equality would leave the first partition empty.
            let mut encoded = format!("00010010{}00{partition_order}", "0".repeat(16));
            for partition in 0..partitions {
                encoded.push_str("0000");
                encoded.push_str(&"1".repeat(16 / partitions - usize::from(partition == 0)));
            }
            let bytes = encoded
                .as_bytes()
                .chunks(8)
                .map(|chunk| {
                    chunk
                        .iter()
                        .fold(0_u8, |byte, &bit| (byte << 1) | u8::from(bit == b'1'))
                        << (8 - chunk.len())
                })
                .collect::<Vec<_>>();
            let result = skip_subframe(&mut Bits::new(bytes.as_slice()), 16, 16);
            assert_eq!(result.is_ok(), expected_valid);
            if let Err(error) = result {
                assert_eq!(error.to_string(), "invalid FLAC residual partitions");
            }
        }
    }

    pub fn image(variable: bool) -> Vec<u8> {
        image_blocks(variable, &[1024, 1024, 517])
    }

    fn image_blocks(variable: bool, blocks: &[u16]) -> Vec<u8> {
        let mut info = [0; 34];
        info[0..2].copy_from_slice(&blocks.iter().copied().min().expect("blocks").to_be_bytes());
        info[2..4].copy_from_slice(&blocks.iter().copied().max().expect("blocks").to_be_bytes());
        let total: u64 = blocks.iter().map(|&samples| u64::from(samples)).sum();
        let properties = (44101_u64 << 44) | (15 << 36) | total;
        info[10..18].copy_from_slice(&properties.to_be_bytes());
        let mut bytes = b"fLaC\x80\0\0\x22".to_vec();
        bytes.extend(info);
        let mut start = 0_u64;
        for (i, &samples) in blocks.iter().enumerate() {
            let mut frame = vec![0xff, 0xf8 | u8::from(variable), 0x70, 0x08];
            let number = if variable { start } else { i as u64 };
            if number < 128 {
                frame.push(u8::try_from(number).expect("small number"));
            } else if number < 2048 {
                frame.extend([
                    0xc0 | u8::try_from(number >> 6).expect("number fits"),
                    0x80 | u8::try_from(number & 63).expect("six bits"),
                ]);
            } else {
                frame.extend([
                    0xe0 | u8::try_from(number >> 12).expect("number fits"),
                    0x80 | u8::try_from((number >> 6) & 63).expect("six bits"),
                    0x80 | u8::try_from(number & 63).expect("six bits"),
                ]);
            }
            frame.extend((samples - 1).to_be_bytes());
            frame.push(crc8(&frame));
            frame.extend([0, 0x12, 0x34]); // constant subframe
            frame.extend(
                frame
                    .iter()
                    .fold(0, |crc, &byte| crc16_byte(crc, byte))
                    .to_be_bytes(),
            );
            bytes.extend(frame);
            start += u64::from(samples);
        }
        bytes
    }

    fn song() -> crate::models::music::Song {
        crate::models::music::Song {
            id: 1,
            title: "Virtual track".to_owned(),
            sort_name: None,
            album_id: None,
            artist_id: None,
            artist_name: Some("Track artist".to_owned()),
            album_name: Some("Album".to_owned()),
            music_folder_id: 1,
            path: "/music/image.flac".to_owned(),
            parent_path: "/music".to_owned(),
            cue_track_id: Some(1),
            file_size: 0,
            content_type: "audio/flac".to_owned(),
            suffix: "flac".to_owned(),
            duration: 0,
            bit_rate: None,
            bit_depth: Some(16),
            sampling_rate: Some(44101),
            channel_count: Some(1),
            track_number: Some(2),
            disc_number: None,
            year: None,
            genre: None,
            cover_art: None,
            musicbrainz_id: None,
            play_count: 0,
            created_at: chrono::DateTime::UNIX_EPOCH.naive_utc(),
            updated_at: chrono::DateTime::UNIX_EPOCH.naive_utc(),
        }
    }

    #[test]
    fn virtual_tracks_preserve_payload_and_rebase_crc_lengths_and_ranges() {
        for variable in [false, true] {
            let mut source = image_blocks(variable, &[16; 132]);
            source[4] = 0;
            let mut seektable = vec![0x83, 0, 0, 18];
            seektable.extend([0; 16]);
            seektable.extend(16_u16.to_be_bytes());
            source.splice(42..42, seektable);
            let path = std::env::temp_dir().join(format!(
                "suboxide-virtual-{}-{}.flac",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::write(&path, &source).expect("source");
            let (mut index, _) = FrameIndex::read(&path).expect("source index");
            index.frames = index.frames[128..132].to_vec();
            let mut track = VirtualTrack::new(
                File::open(&path).expect("open"),
                &index,
                &song(),
                Some("Album artist"),
            )
            .expect("virtual track");
            let mut output = Vec::new();
            track.read_to_end(&mut output).expect("virtual output");
            assert_eq!(track.length(), output.len() as u64);
            let result = read_bytes(&output).expect("valid CRCs and rebased numbering");
            assert_eq!(result.total_samples, 64);
            assert_eq!(&output[26..42], &[0; 16]);
            assert_eq!(
                output[42], 0x84,
                "only regenerated track tags follow STREAMINFO"
            );
            assert!(
                output
                    .windows(b"ALBUMARTIST=Album artist".len())
                    .any(|bytes| bytes == b"ALBUMARTIST=Album artist")
            );
            for (original, emitted) in index.frames.iter().zip(&result.frames) {
                let old = usize::try_from(original.offset).expect("offset");
                let new = usize::try_from(emitted.offset).expect("offset");
                assert_eq!(
                    &source[old + original.header.len()
                        ..old + usize::try_from(original.length).expect("length") - 2],
                    &output[new + emitted.header.len()
                        ..new + usize::try_from(emitted.length).expect("length") - 2]
                );
                assert!(emitted.header.len() < original.header.len());
            }
            for start in 0..output.len() {
                track.seek(SeekFrom::Start(start as u64)).expect("seek");
                let mut range = Vec::new();
                (&mut track)
                    .take(17)
                    .read_to_end(&mut range)
                    .expect("range");
                assert_eq!(range, output[start..(start + 17).min(output.len())]);
            }
            std::fs::remove_file(path).expect("cleanup");
        }
    }

    fn read_bytes(bytes: &[u8]) -> io::Result<FrameIndex> {
        let path = std::env::temp_dir().join(format!(
            "suboxide-flac-{}-{}.flac",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, bytes)?;
        let result = FrameIndex::read(&path).map(|(index, _)| index);
        std::fs::remove_file(path)?;
        result
    }

    #[test]
    fn indexes_fixed_and_variable_frames_and_ceiling_cuts() {
        for variable in [false, true] {
            let index = read_bytes(&image(variable)).expect("valid image");
            assert_eq!(index.total_samples, 2565);
            assert_eq!(
                index
                    .frames
                    .iter()
                    .map(|f| f.sample_start)
                    .collect::<Vec<_>>(),
                [0, 1024, 2048]
            );
            assert_eq!(
                index.intervals(&[0, 1, 2]).expect("three tracks"),
                [0..1, 1..2, 2..3]
            );
            assert!(index.intervals(&[0, 1, 1]).is_err());
            assert!(index.intervals(&[0, 4]).is_err()); // ceiling lies at EOF
            assert!(index.intervals(&[0, 5]).is_err()); // outside source
        }
    }

    #[test]
    fn rejects_truncated_and_corrupt_images() {
        let mut bytes = image(true);
        bytes[50] ^= 1;
        assert!(read_bytes(&bytes).is_err());
        let bytes = image(false);
        for length in [0, 4, 20, bytes.len() - 1] {
            assert!(read_bytes(&bytes[..length]).is_err());
        }
    }
}
