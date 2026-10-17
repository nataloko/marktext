// Text encodings of Markdown files, standing in for
// packages/desktop/src/main/filesystem/encoding.ts (detection with ced) and the
// iconv-lite calls in main/filesystem/markdown.ts. Encoding names are
// iconv-lite's, as the renderer lists them in
// packages/desktop/src/common/encoding.ts ENCODING_NAME_MAP.
//
// Unlike iconv-lite, nothing is replaced silently: a file whose bytes do not
// fit the chosen encoding is not opened, and text that the file's encoding
// cannot represent is not saved, so a round trip never damages a file.

use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
use encoding_rs::Encoding;

const BOMS: [(&str, &[u8]); 3] = [
    ("utf8", &[0xef, 0xbb, 0xbf]),
    ("utf16be", &[0xfe, 0xff]),
    ("utf16le", &[0xff, 0xfe]),
];

// The renderer's names for the encodings encoding_rs handles; the first name
// for an encoding is the one a detected file reports. Other encoding_rs
// encodings go by their own label. UTF-16 and UTF-32 are handled here
// directly, because encoding_rs only decodes UTF-16 and has no UTF-32.
const LEGACY: &[(&str, &str)] = &[
    ("cp1252", "windows-1252"),
    ("ascii", "windows-1252"),
    ("latin3", "iso-8859-3"),
    ("iso885915", "iso-8859-15"),
    ("arabic", "iso-8859-6"),
    ("cp1256", "windows-1256"),
    ("latin4", "iso-8859-4"),
    ("cp1257", "windows-1257"),
    ("iso88592", "iso-8859-2"),
    ("windows1250", "windows-1250"),
    ("cp1250", "windows-1250"),
    ("cp866", "ibm866"),
    ("iso88595", "iso-8859-5"),
    ("koi8r", "koi8-r"),
    ("koi8u", "koi8-u"),
    ("cp1251", "windows-1251"),
    ("windows1251", "windows-1251"),
    ("iso885913", "iso-8859-13"),
    ("greek", "iso-8859-7"),
    ("cp1253", "windows-1253"),
    ("hebrew", "iso-8859-8"),
    ("cp1255", "windows-1255"),
    ("cp1254", "windows-1254"),
    ("latin5", "windows-1254"),
    ("gbk", "gbk"),
    ("gb2312", "gbk"),
    ("gb18030", "gb18030"),
    ("big5", "big5"),
    ("big5hkscs", "big5"),
    ("shiftjis", "shift_jis"),
    ("eucjp", "euc-jp"),
    ("euckr", "euc-kr"),
    ("latin6", "iso-8859-10"),
    ("macintosh", "macintosh"),
    ("windows1258", "windows-1258"),
    ("cp874", "windows-874"),
    ("latin2", "iso-8859-2"),
];

fn legacy_encoding(name: &str) -> Option<&'static Encoding> {
    let label = LEGACY.iter().find(|(iconv, _)| *iconv == name).map_or(name, |(_, label)| *label);
    Encoding::for_label(label.as_bytes())
}

fn iconv_name(encoding: &'static Encoding) -> String {
    let label = encoding.name().to_ascii_lowercase();
    LEGACY
        .iter()
        .find(|(_, other)| *other == label)
        .map_or(label.clone(), |(iconv, _)| iconv.to_string())
}

// A NUL byte means binary data or UTF-16 without a BOM, not UTF-8 text.
fn is_likely_utf8(bytes: &[u8]) -> bool {
    !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok()
}

// encoding.ts guessEncoding: a BOM decides; otherwise a valid UTF-8 file is
// UTF-8 whatever a detector might think (#3151), and only the rest is guessed.
pub fn guess(bytes: &[u8], auto_guess: bool) -> (String, bool) {
    if let Some((name, _)) = BOMS.iter().find(|(_, bom)| bytes.starts_with(bom)) {
        return (name.to_string(), true);
    }
    if !auto_guess || is_likely_utf8(bytes) {
        return ("utf8".into(), false);
    }
    let mut detector = EncodingDetector::new(Iso2022JpDetection::Deny);
    detector.feed(bytes, true);
    (iconv_name(detector.guess(None, Utf8Detection::Deny)), false)
}

fn unsupported(name: &str) -> String {
    format!("\"{name}\" encoding is not supported.")
}

fn decode_utf16(bytes: &[u8], little_endian: bool) -> Option<String> {
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let units = bytes.as_chunks::<2>().0.iter().map(|&pair| {
        if little_endian { u16::from_le_bytes(pair) } else { u16::from_be_bytes(pair) }
    });
    char::decode_utf16(units).collect::<Result<String, _>>().ok()
}

fn decode_utf32(bytes: &[u8], little_endian: bool) -> Option<String> {
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&quad| char::from_u32(if little_endian { u32::from_le_bytes(quad) } else { u32::from_be_bytes(quad) }))
        .collect()
}

pub fn decode(bytes: &[u8], name: &str, is_bom: bool) -> Result<String, String> {
    let bom_length = if is_bom {
        BOMS.iter().find(|(bom_name, _)| *bom_name == name).map_or(0, |(_, bom)| bom.len())
    } else {
        0
    };
    let body = &bytes[bom_length..];
    let text = match name {
        "utf8" => std::str::from_utf8(body).ok().map(str::to_string),
        "utf16le" => decode_utf16(body, true),
        "utf16be" => decode_utf16(body, false),
        "utf32le" => decode_utf32(body, true),
        "utf32be" => decode_utf32(body, false),
        _ => {
            let encoding = legacy_encoding(name).ok_or_else(|| unsupported(name))?;
            let (text, had_errors) = encoding.decode_without_bom_handling(body);
            (!had_errors).then(|| text.into_owned())
        }
    };
    text.ok_or_else(|| {
        format!("The file is not valid {name}. It was not opened, so saving cannot damage it.")
    })
}

pub fn encode(text: &str, name: &str, is_bom: bool) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::with_capacity(text.len() + 4);
    match name {
        "utf8" => {
            if is_bom {
                bytes.extend_from_slice(&[0xef, 0xbb, 0xbf]);
            }
            bytes.extend_from_slice(text.as_bytes());
        }
        "utf16le" | "utf16be" => {
            let little_endian = name == "utf16le";
            let units = (is_bom.then_some(0xfeff)).into_iter().chain(text.encode_utf16());
            for unit in units {
                bytes.extend(if little_endian { unit.to_le_bytes() } else { unit.to_be_bytes() });
            }
        }
        "utf32le" | "utf32be" => {
            let little_endian = name == "utf32le";
            let chars = (is_bom.then_some('\u{feff}')).into_iter().chain(text.chars());
            for ch in chars {
                let value = ch as u32;
                bytes.extend(if little_endian { value.to_le_bytes() } else { value.to_be_bytes() });
            }
        }
        _ => {
            let encoding = legacy_encoding(name).ok_or_else(|| unsupported(name))?;
            let mut encoder = encoding.new_encoder();
            let capacity = encoder
                .max_buffer_length_from_utf8_without_replacement(text.len())
                .ok_or_else(|| "The document is too large to encode.".to_string())?;
            bytes.resize(capacity, 0);
            let (result, _, written) = encoder.encode_from_utf8_without_replacement(text, &mut bytes, true);
            if let encoding_rs::EncoderResult::Unmappable(ch) = result {
                return Err(format!("\"{ch}\" cannot be saved in {name}. Change the file's encoding to save it."));
            }
            bytes.truncate(written);
        }
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boms_decide() {
        assert_eq!(guess(&[0xef, 0xbb, 0xbf, b'a'], false), ("utf8".into(), true));
        assert_eq!(guess(&[0xff, 0xfe, b'a', 0], true), ("utf16le".into(), true));
        assert_eq!(guess(&[0xfe, 0xff, 0, b'a'], true), ("utf16be".into(), true));
    }

    #[test]
    fn valid_utf8_stays_utf8() {
        assert_eq!(guess("µκα Grüße".as_bytes(), true), ("utf8".into(), false));
        assert_eq!(guess(&[0xe9], false), ("utf8".into(), false));
    }

    #[test]
    fn legacy_text_is_guessed() {
        let (gbk, _, _) = encoding_rs::GBK.encode("中文的文本，这是一个测试。我们需要足够的文字来猜测编码。");
        assert_eq!(guess(&gbk, true).0, "gbk");
        let (latin, _, _) = encoding_rs::WINDOWS_1252.encode("Ça déjà été très café, naïve façade.");
        assert_eq!(guess(&latin, true).0, "cp1252");
    }

    #[test]
    fn encodings_outside_the_table_use_their_label() {
        assert_eq!(iconv_name(encoding_rs::ISO_8859_8_I), "iso-8859-8-i");
        assert_eq!(decode(&[0xe0], "iso-8859-8-i", false).unwrap(), "א");
    }

    #[test]
    fn round_trips() {
        let text = "# Title\n\nGrüße, 中文, emoji 😀\n";
        for (name, bom) in [("utf8", false), ("utf8", true), ("utf16le", true), ("utf16be", true), ("utf32le", false)] {
            let bytes = encode(text, name, bom).unwrap();
            assert_eq!(decode(&bytes, name, bom).unwrap(), text, "{name} {bom}");
        }
        let bytes = encode("中文", "gbk", false).unwrap();
        assert_eq!(decode(&bytes, "gbk", false).unwrap(), "中文");
    }

    #[test]
    fn nothing_is_replaced() {
        assert!(decode(&[0xe9, b'a'], "utf8", false).is_err());
        assert!(encode("emoji 😀", "cp1252", false).is_err());
        assert!(decode(b"x", "nonsense", false).is_err());
    }
}
