pub const FILE_ANALYSIS_BYTES: usize = 1024;

#[derive(Debug, PartialEq)]
pub enum ByteContent {
    Utf16Le,
    Utf16Be,
    Binary,
    Unknown,
}

// Heuristic check using null byte distribution plus a generic text-likeness
// heuristic. This prefers UTF-16 when many bytes are NUL and otherwise
// distinguishes between text-like and binary-like content.
pub fn analyze_byte_content(bytes: &[u8]) -> ByteContent {
    if bytes.len() < 2 {
        return ByteContent::Unknown;
    }

    if is_known_binary_header(bytes) {
        return ByteContent::Binary;
    }

    let limit = bytes.len().min(FILE_ANALYSIS_BYTES);
    let mut even_null_count = 0usize;
    let mut odd_null_count = 0usize;
    let mut non_text_like_count = 0usize;

    for (i, &byte) in bytes[..limit].iter().enumerate() {
        if byte == 0 {
            if i % 2 == 0 {
                even_null_count += 1;
            } else {
                odd_null_count += 1;
            }
            non_text_like_count += 1;
            continue;
        }

        let is_text_like = match byte {
            b'\t' | b'\n' | b'\r' | 0x0C => true,
            0x20..=0x7E => true,
            // Treat bytes that are likely part of UTF-8 or single-byte encodings as text-like.
            0x80..=0xBF | 0xC2..=0xF4 => true,
            _ => false,
        };

        if !is_text_like {
            non_text_like_count += 1;
        }
    }

    let total_null_count = even_null_count + odd_null_count;

    // If there are no NUL bytes at all, this is overwhelmingly likely to be text.
    if total_null_count == 0 {
        return ByteContent::Unknown;
    }

    let has_significant_nulls = total_null_count >= limit / 16;
    let nulls_skew_to_even = even_null_count > odd_null_count * 4;
    let nulls_skew_to_odd = odd_null_count > even_null_count * 4;

    if has_significant_nulls {
        let sample = &bytes[..limit];

        // UTF-16BE ASCII: [0x00, char] — nulls at even positions (high byte first)
        // UTF-16LE ASCII: [char, 0x00] — nulls at odd positions (low byte first)

        if nulls_skew_to_even && is_plausible_utf16_text(sample, false) {
            return ByteContent::Utf16Be;
        }

        if nulls_skew_to_odd && is_plausible_utf16_text(sample, true) {
            return ByteContent::Utf16Le;
        }

        return ByteContent::Binary;
    }

    if non_text_like_count * 100 < limit * 8 {
        ByteContent::Unknown
    } else {
        ByteContent::Binary
    }
}

fn is_known_binary_header(bytes: &[u8]) -> bool {
    bytes.starts_with(b"%PDF-") // PDF
        || bytes.starts_with(b"PK\x03\x04") // ZIP local header
        || bytes.starts_with(b"PK\x05\x06") // ZIP end of central directory
        || bytes.starts_with(b"PK\x07\x08") // ZIP spanning/splitting
        || bytes.starts_with(b"\x89PNG\r\n\x1a\n") // PNG
        || bytes.starts_with(b"\xFF\xD8\xFF") // JPEG
        || bytes.starts_with(b"GIF87a") // GIF87a
        || bytes.starts_with(b"GIF89a") // GIF89a
        || bytes.starts_with(b"IWAD") // Doom IWAD archive
        || bytes.starts_with(b"PWAD") // Doom PWAD archive
        || bytes.starts_with(b"RIFF") // WAV, AVI, WebP
        || bytes.starts_with(b"OggS") // OGG (Vorbis, Opus, FLAC)
        || bytes.starts_with(b"fLaC") // FLAC
        || bytes.starts_with(b"ID3") // MP3 with ID3v2 tag
        || bytes.starts_with(b"\xFF\xFB") // MP3 frame sync (MPEG1 Layer3)
        || bytes.starts_with(b"\xFF\xFA") // MP3 frame sync (MPEG1 Layer3)
        || bytes.starts_with(b"\xFF\xF3") // MP3 frame sync (MPEG2 Layer3)
        || bytes.starts_with(b"\xFF\xF2") // MP3 frame sync (MPEG2 Layer3)
}

// Null byte skew alone is not enough to identify UTF-16 -- binary formats with
// small 16-bit values (like PCM audio) produce the same pattern. Decode the
// bytes as UTF-16 and reject if too many code units land in control character
// ranges or form unpaired surrogates, which real text almost never contains.
fn is_plausible_utf16_text(bytes: &[u8], little_endian: bool) -> bool {
    let mut suspicious_count = 0usize;
    let mut word_like_count = 0usize;
    let mut total = 0usize;

    let mut i = 0;
    while let Some(code_unit) = read_u16(bytes, i, little_endian) {
        total += 1;

        match code_unit {
            0x0009 | 0x000A | 0x000C | 0x000D => {}
            0x0020 | 0x0030..=0x0039 | 0x0041..=0x005A | 0x0061..=0x007A => {
                word_like_count += 1;
            }
            // C0/C1 control characters and non-characters
            0x0000..=0x001F | 0x007F..=0x009F | 0xFFFE | 0xFFFF => suspicious_count += 1,
            0xD800..=0xDBFF => {
                let next_offset = i + 2;
                let has_low_surrogate = read_u16(bytes, next_offset, little_endian)
                    .is_some_and(|next| (0xDC00..=0xDFFF).contains(&next));
                if has_low_surrogate {
                    total += 1;
                    word_like_count += 2;
                    i += 2;
                } else {
                    suspicious_count += 1;
                }
            }
            // Lone low surrogate without a preceding high surrogate
            0xDC00..=0xDFFF => suspicious_count += 1,
            0x0100.. => word_like_count += 1,
            _ => {}
        }

        i += 2;
    }

    if total == 0 {
        return false;
    }

    // Real UTF-16 text has near-zero control characters; binary data with
    // small 16-bit values typically exceeds 5%. 2% provides a safe margin.
    let low_control_ratio = suspicious_count * 100 < total * 2;

    // Binary formats that interleave short ASCII fragments with small
    // length/type fields (e.g. game asset formats) can dodge the control
    // character check above while barely containing any real words: their
    // code units land on ASCII punctuation and Latin-1 symbol values rather
    // than letters, digits, or spaces. Real text is overwhelmingly made of
    // word characters, so require a minimum share of them. Code units above
    // the Latin-1 range (and surrogate pairs) also count as word-like so
    // that scripts such as Cyrillic or Greek, whose letters are non-ASCII,
    // are still recognized -- tag bytes paired with a zero byte can never
    // land there.
    let enough_word_chars = word_like_count * 100 >= total * 30;

    low_control_ratio && enough_word_chars
}

fn read_u16(bytes: &[u8], offset: usize, little_endian: bool) -> Option<u16> {
    let pair = [*bytes.get(offset)?, *bytes.get(offset + 1)?];
    if little_endian {
        return Some(u16::from_le_bytes(pair));
    }
    Some(u16::from_be_bytes(pair))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// reproduction of issue #50785
    fn build_pcm16_wav_bytes() -> Vec<u8> {
        let header: Vec<u8> = vec![
            /*  RIFF header  */
            0x52, 0x49, 0x46, 0x46, // "RIFF"
            0xc6, 0xcf, 0x00, 0x00, // file size: 8
            0x57, 0x41, 0x56, 0x45, // "WAVE"
            /*  fmt chunk  */
            0x66, 0x6d, 0x74, 0x20, // "fmt "
            0x10, 0x00, 0x00, 0x00, // chunk size: 16
            0x01, 0x00, // format: PCM (1)
            0x01, 0x00, // channels: 1 (mono)
            0x80, 0x3e, 0x00, 0x00, // sample rate: 16000
            0x00, 0x7d, 0x00, 0x00, // byte rate: 32000
            0x02, 0x00, // block align: 2
            0x10, 0x00, // bits per sample: 16
            /*  LIST chunk  */
            0x4c, 0x49, 0x53, 0x54, // "LIST"
            0x1a, 0x00, 0x00, 0x00, // chunk size: 26
            0x49, 0x4e, 0x46, 0x4f, // "INFO"
            0x49, 0x53, 0x46, 0x54, // "ISFT"
            0x0d, 0x00, 0x00, 0x00, // sub-chunk size: 13
            0x4c, 0x61, 0x76, 0x66, 0x36, 0x32, 0x2e, 0x33, // "Lavf62.3"
            0x2e, 0x31, 0x30, 0x30, 0x00, // ".100\0"
            /* padding byte for word alignment */
            0x00, // data chunk header
            0x64, 0x61, 0x74, 0x61, // "data"
            0x80, 0xcf, 0x00, 0x00, // chunk size
        ];

        let mut bytes = header;

        // fill remaining space up to `FILE_ANALYSIS_BYTES` with synthetic PCM
        let audio_bytes_needed = FILE_ANALYSIS_BYTES - bytes.len();
        for i in 0..(audio_bytes_needed / 2) {
            let sample = (i & 0xFF) as u8;
            bytes.push(sample); // low byte: varies
            bytes.push(0x00); // high byte: zero for small values
        }

        bytes
    }

    #[test]
    fn test_pcm16_wav_detected_as_binary() {
        let wav_bytes = build_pcm16_wav_bytes();
        assert_eq!(wav_bytes.len(), FILE_ANALYSIS_BYTES);

        let result = analyze_byte_content(&wav_bytes);
        assert_eq!(
            result,
            ByteContent::Binary,
            "PCM 16-bit WAV should be detected as Binary via RIFF header"
        );
    }

    #[test]
    fn test_le16_binary_not_misdetected_as_utf16le() {
        let mut bytes = b"FAKE".to_vec();
        while bytes.len() < FILE_ANALYSIS_BYTES {
            let sample = (bytes.len() & 0xFF) as u8;
            bytes.push(sample);
            bytes.push(0x00);
        }
        bytes.truncate(FILE_ANALYSIS_BYTES);

        let result = analyze_byte_content(&bytes);
        assert_eq!(
            result,
            ByteContent::Binary,
            "LE 16-bit binary with control characters should be detected as Binary"
        );
    }

    #[test]
    fn test_be16_binary_not_misdetected_as_utf16be() {
        let mut bytes = b"FAKE".to_vec();
        while bytes.len() < FILE_ANALYSIS_BYTES {
            bytes.push(0x00);
            let sample = (bytes.len() & 0xFF) as u8;
            bytes.push(sample);
        }
        bytes.truncate(FILE_ANALYSIS_BYTES);

        let result = analyze_byte_content(&bytes);
        assert_eq!(
            result,
            ByteContent::Binary,
            "BE 16-bit binary with control characters should be detected as Binary"
        );
    }

    // Mimics binary formats that interleave short ASCII fragments with small
    // length/type fields (as seen in some game/asset binary formats, e.g.
    // Tibia-style OTBM maps): most high bytes are zero, matching UTF-16LE's
    // null-byte pattern for ASCII, but the low bytes are mostly non-word
    // "tag" values rather than real letters/digits/spaces.
    fn build_tag_interleaved_binary_bytes() -> Vec<u8> {
        let mut bytes = Vec::new();
        let tags: [u8; 6] = [0xFE, 0xFF, 0x25, 0x2B, 0xA3, 0xC5];
        let mut i = 0;
        while bytes.len() < FILE_ANALYSIS_BYTES {
            bytes.push(tags[i % tags.len()]);
            bytes.push(0x00);
            i += 1;
        }
        bytes.truncate(FILE_ANALYSIS_BYTES);
        bytes
    }

    #[test]
    fn test_tag_interleaved_binary_not_misdetected_as_utf16le() {
        let bytes = build_tag_interleaved_binary_bytes();
        assert_eq!(bytes.len(), FILE_ANALYSIS_BYTES);

        let result = analyze_byte_content(&bytes);
        assert_eq!(
            result,
            ByteContent::Binary,
            "binary data with sparse non-word low bytes and null high bytes \
             should not be misdetected as UTF-16LE text"
        );
    }

    #[test]
    fn test_utf16le_text_detected_as_utf16le() {
        let text = "Hello, world! This is a UTF-16 test string. ";
        let mut bytes = Vec::new();
        while bytes.len() < FILE_ANALYSIS_BYTES {
            bytes.extend(text.encode_utf16().flat_map(|u| u.to_le_bytes()));
        }
        bytes.truncate(FILE_ANALYSIS_BYTES);

        assert_eq!(analyze_byte_content(&bytes), ByteContent::Utf16Le);
    }

    #[test]
    fn test_utf16be_text_detected_as_utf16be() {
        let text = "Hello, world! This is a UTF-16 test string. ";
        let mut bytes = Vec::new();
        while bytes.len() < FILE_ANALYSIS_BYTES {
            bytes.extend(text.encode_utf16().flat_map(|u| u.to_be_bytes()));
        }
        bytes.truncate(FILE_ANALYSIS_BYTES);

        assert_eq!(analyze_byte_content(&bytes), ByteContent::Utf16Be);
    }

    #[test]
    fn test_utf16le_cyrillic_text_detected_as_utf16le() {
        let text = "Привет, мир! Это тестовая строка в UTF-16. ";
        let mut bytes = Vec::new();
        while bytes.len() < FILE_ANALYSIS_BYTES {
            bytes.extend(text.encode_utf16().flat_map(|u| u.to_le_bytes()));
        }
        bytes.truncate(FILE_ANALYSIS_BYTES);

        assert_eq!(analyze_byte_content(&bytes), ByteContent::Utf16Le);
    }

    #[test]
    fn test_utf16be_greek_text_detected_as_utf16be() {
        let text = "Γεια σου κόσμε! Αυτή είναι μια δοκιμαστική συμβολοσειρά. ";
        let mut bytes = Vec::new();
        while bytes.len() < FILE_ANALYSIS_BYTES {
            bytes.extend(text.encode_utf16().flat_map(|u| u.to_be_bytes()));
        }
        bytes.truncate(FILE_ANALYSIS_BYTES);

        assert_eq!(analyze_byte_content(&bytes), ByteContent::Utf16Be);
    }

    #[test]
    fn test_known_binary_headers() {
        let cases: &[(&[u8], &str)] = &[
            (b"RIFF\x00\x00\x00\x00WAVE", "WAV"),
            (b"RIFF\x00\x00\x00\x00AVI ", "AVI"),
            (b"OggS\x00\x02", "OGG"),
            (b"fLaC\x00\x00", "FLAC"),
            (b"ID3\x03\x00", "MP3 ID3v2"),
            (b"\xFF\xFB\x90\x00", "MP3 MPEG1 Layer3"),
            (b"\xFF\xF3\x90\x00", "MP3 MPEG2 Layer3"),
        ];

        for (header, label) in cases {
            let mut bytes = header.to_vec();
            bytes.resize(FILE_ANALYSIS_BYTES, 0x41); // pad with 'A'
            assert_eq!(
                analyze_byte_content(&bytes),
                ByteContent::Binary,
                "{label} should be detected as Binary"
            );
        }
    }
}
