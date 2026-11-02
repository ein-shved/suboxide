//! External, single-image CUE sheets.

use std::io;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueTrack {
    pub number: u32,
    pub title: Option<String>,
    pub performer: Option<String>,
    pub ticks: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueSheet {
    pub image: PathBuf,
    pub title: Option<String>,
    pub performer: Option<String>,
    pub tracks: Vec<CueTrack>,
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn text(value: &str) -> io::Result<&str> {
    let value = value.trim();
    if value.starts_with('"') {
        value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .ok_or_else(|| invalid("unclosed CUE quote"))
    } else {
        Ok(value)
    }
}

fn parse_ticks(time: &str) -> io::Result<u64> {
    let parts = time
        .trim()
        .split(':')
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(io::Error::other)?;
    let [minutes, seconds, frames] = parts.as_slice() else {
        return Err(invalid("invalid CUE time"));
    };
    if *seconds >= 60 || *frames >= 75 {
        return Err(invalid("invalid CUE time units"));
    }
    minutes
        .checked_mul(60)
        .and_then(|n| n.checked_add(*seconds))
        .and_then(|n| n.checked_mul(75))
        .and_then(|n| n.checked_add(*frames))
        .ok_or_else(|| invalid("CUE time overflow"))
}

fn parse_image(value: &str) -> io::Result<PathBuf> {
    let (name, kind) = value
        .rsplit_once(char::is_whitespace)
        .ok_or_else(|| invalid("invalid CUE FILE"))?;
    let path = Path::new(text(name)?);
    if !kind.eq_ignore_ascii_case("WAVE")
        || path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
        || name.contains('\\')
        || !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("flac"))
    {
        return Err(invalid("CUE image must be a same-directory FLAC"));
    }
    Ok(path.to_path_buf())
}

impl CueSheet {
    pub fn parse(content: &str) -> io::Result<Self> {
        let mut sheet = Self {
            image: PathBuf::new(),
            title: None,
            performer: None,
            tracks: Vec::new(),
        };
        let mut track = None;
        let mut index_seen = false;
        for line in content
            .trim_start_matches('\u{feff}')
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            let (command, value) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
            let value = value.trim();
            match command.to_ascii_uppercase().as_str() {
                "FILE" => {
                    if !sheet.image.as_os_str().is_empty() || track.is_some() {
                        return Err(invalid("CUE must reference exactly one image"));
                    }
                    sheet.image = parse_image(value)?;
                }
                "TRACK" => {
                    if sheet.image.as_os_str().is_empty() {
                        return Err(invalid("CUE TRACK before FILE"));
                    }
                    if track.is_some() && !index_seen {
                        return Err(invalid("CUE track missing INDEX 01"));
                    }
                    let (number, kind) = value
                        .split_once(char::is_whitespace)
                        .ok_or_else(|| invalid("invalid CUE TRACK"))?;
                    let number = number.parse::<u32>().map_err(io::Error::other)?;
                    if number == 0
                        || !kind.trim().eq_ignore_ascii_case("AUDIO")
                        || sheet.tracks.last().is_some_and(|old| old.number >= number)
                    {
                        return Err(invalid("CUE requires increasing AUDIO track numbers"));
                    }
                    sheet.tracks.push(CueTrack {
                        number,
                        title: None,
                        performer: None,
                        ticks: 0,
                    });
                    track = Some(sheet.tracks.len() - 1);
                    index_seen = false;
                }
                "TITLE" | "PERFORMER" => {
                    let target = match (
                        command.eq_ignore_ascii_case("TITLE"),
                        track.map(|i| &mut sheet.tracks[i]),
                    ) {
                        (true, Some(track)) => &mut track.title,
                        (false, Some(track)) => &mut track.performer,
                        (true, None) => &mut sheet.title,
                        (false, None) => &mut sheet.performer,
                    };
                    *target = Some(text(value)?.to_owned());
                }
                "INDEX" => {
                    let (index, time) = value
                        .split_once(char::is_whitespace)
                        .ok_or_else(|| invalid("invalid CUE INDEX"))?;
                    let ticks = parse_ticks(time)?;
                    if index == "01" {
                        if index_seen {
                            return Err(invalid("duplicate CUE INDEX 01"));
                        }
                        track
                            .map(|i| &mut sheet.tracks[i])
                            .ok_or_else(|| invalid("CUE INDEX before TRACK"))?
                            .ticks = ticks;
                        index_seen = true;
                    }
                }
                "REM" | "CATALOG" | "ISRC" | "FLAGS" | "PREGAP" | "POSTGAP" | "SONGWRITER"
                | "CDTEXTFILE" => {}
                _ => return Err(invalid("unsupported CUE directive")),
            }
        }
        if sheet.tracks.is_empty()
            || !index_seen
            || sheet
                .tracks
                .windows(2)
                .any(|pair| pair[0].ticks >= pair[1].ticks)
        {
            return Err(invalid("CUE needs tracks with increasing INDEX 01 times"));
        }
        Ok(sheet)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_image_tags_and_rational_ticks() {
        let cue = CueSheet::parse("\u{feff}PERFORMER \"Album Artist\"\nTITLE \"Album\"\nFILE \"album image.flac\" WAVE\n TRACK 01 AUDIO\n TITLE \"First\"\n INDEX 00 00:00:00\n INDEX 01 00:00:01\n TRACK 03 AUDIO\n PERFORMER \"Guest\"\n INDEX 01 01:02:03\n").expect("valid sheet");
        assert_eq!(cue.image, Path::new("album image.flac"));
        assert_eq!(cue.title.as_deref(), Some("Album"));
        assert_eq!(cue.performer.as_deref(), Some("Album Artist"));
        assert_eq!(cue.tracks[0].ticks, 1);
        assert_eq!(cue.tracks[1].ticks, 4653);
        assert_eq!(cue.tracks[1].number, 3);
        assert_eq!(cue.tracks[1].performer.as_deref(), Some("Guest"));
        assert_eq!(cue.tracks[1].title, None);
    }

    #[test]
    fn rejects_invalid_pair_and_track_structure() {
        for sheet in [
            "",
            "FILE \"../image.flac\" WAVE\nTRACK 01 AUDIO\nINDEX 01 00:00:00",
            "FILE \"/image.flac\" WAVE\nTRACK 01 AUDIO\nINDEX 01 00:00:00",
            "FILE \"image.mp3\" WAVE\nTRACK 01 AUDIO\nINDEX 01 00:00:00",
            "FILE \"image.flac\" WAVE\nFILE \"other.flac\" WAVE",
            "FILE \"image.flac\" WAVE\nTRACK 01 MODE1/2352\nINDEX 01 00:00:00",
            "FILE \"image.flac\" WAVE\nTRACK 01 AUDIO",
            "FILE \"image.flac\" WAVE\nTRACK 01 AUDIO\nINDEX 01 00:60:00",
            "FILE \"image.flac\" WAVE\nTRACK 01 AUDIO\nINDEX 01 00:00:75",
            "FILE \"image.flac\" WAVE\nTRACK 01 AUDIO\nINDEX 01 00:00:00\nTRACK 02 AUDIO\nINDEX 01 00:00:00",
            "FILE \"image.flac\" WAVE\nTRACK 01 AUDIO\nINDEX 01 00:00:00\nTRACK 01 AUDIO\nINDEX 01 00:01:00",
            "FILE \"image.flac\" WAVE\nTRACK 01 AUDIO\nINDEX 01 00:00:00\nINDEX 01 00:00:01",
        ] {
            assert!(CueSheet::parse(sheet).is_err(), "accepted {sheet}");
        }
    }
}
