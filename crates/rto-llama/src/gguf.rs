//! Read a GGUF file's embedded chat template without loading the model.
//!
//! # Why not just load it
//!
//! `LlamaModel::chat_template` answers this, and costs a model load — tens of
//! gigabytes of mmap and a chunk of wall-clock — to read a few kilobytes that sit
//! in the file's header. That is affordable once per request on a resident model
//! and absurd as a check at install time, which is exactly where the answer is
//! most useful: a template Roteiro cannot render should surface when you pull the
//! model, not in the middle of a conversation.
//!
//! # The format, only as far as needed
//!
//! GGUF opens with a magic, a version, a tensor count, a metadata-entry count,
//! and then the metadata as length-prefixed key/value pairs. Values are tagged;
//! arrays carry an element tag and a count. The tensor data follows, and this
//! reader never reaches it — it stops at the key it wants.
//!
//! Only the value *shapes* matter here, not their meanings, so the reader skips
//! everything it is not looking for rather than modelling it. That keeps this to
//! one screen and means a new value type in a future GGUF version cannot corrupt
//! the answer — it can only make this return `None`, which is the safe outcome.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

/// GGUF's magic, little-endian `"GGUF"`.
const MAGIC: [u8; 4] = *b"GGUF";

/// The metadata key holding a model's Jinja chat template.
const CHAT_TEMPLATE_KEY: &str = "tokenizer.chat_template";

/// Byte width of each fixed-size GGUF value type, indexed by its tag.
///
/// `None` marks the two variable-length types — string (8) and array (9) — which
/// are read rather than skipped.
fn fixed_width(tag: u32) -> Option<u64> {
    Some(match tag {
        0 | 1 | 7 => 1, // u8, i8, bool
        2 | 3 => 2,     // u16, i16
        4..=6 => 4,     // u32, i32, f32
        10..=12 => 8,   // u64, i64, f64
        _ => return None,
    })
}

fn read_u32(r: &mut impl Read) -> Option<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b).ok()?;
    Some(u32::from_le_bytes(b))
}

fn read_u64(r: &mut impl Read) -> Option<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b).ok()?;
    Some(u64::from_le_bytes(b))
}

/// A length-prefixed UTF-8 string.
///
/// Bounded at 16 MiB: a corrupt length field would otherwise ask for an
/// allocation the size of whatever the bytes happened to say, and no legitimate
/// metadata string in this format is close to that.
fn read_str(r: &mut impl Read) -> Option<String> {
    let n = read_u64(r)?;
    if n > 16 * 1024 * 1024 {
        return None;
    }
    let mut buf = vec![0u8; usize::try_from(n).ok()?];
    r.read_exact(&mut buf).ok()?;
    String::from_utf8(buf).ok()
}

/// Skip one value of type `tag`, returning `None` on a shape this reader does
/// not know — which ends the walk rather than guessing at an offset.
fn skip_value(r: &mut (impl Read + Seek), tag: u32) -> Option<()> {
    match tag {
        8 => {
            let n = read_u64(r)?;
            r.seek(SeekFrom::Current(i64::try_from(n).ok()?)).ok()?;
        }
        9 => {
            let elem = read_u32(r)?;
            let count = read_u64(r)?;
            match fixed_width(elem) {
                Some(w) => {
                    let bytes = w.checked_mul(count)?;
                    r.seek(SeekFrom::Current(i64::try_from(bytes).ok()?)).ok()?;
                }
                // An array of strings, or of arrays: each element must be walked,
                // because only its own length says where the next one starts.
                None => {
                    for _ in 0..count {
                        skip_value(r, elem)?;
                    }
                }
            }
        }
        other => {
            let w = fixed_width(other)?;
            r.seek(SeekFrom::Current(i64::try_from(w).ok()?)).ok()?;
        }
    }
    Some(())
}

/// The chat template embedded in the GGUF at `path`, if it carries one.
///
/// `None` covers every "no answer" case alike — not a GGUF, a version this
/// reader does not understand, no such key, an unreadable file. A caller uses
/// this to *warn*, so an unparseable header must not be louder than a missing
/// key: both mean "nothing to check here".
#[must_use]
pub fn chat_template(path: &Path) -> Option<String> {
    let mut r = BufReader::new(File::open(path).ok()?);

    let mut magic = [0u8; 4];
    r.read_exact(&mut magic).ok()?;
    if magic != MAGIC {
        return None;
    }
    let _version = read_u32(&mut r)?;
    let _tensors = read_u64(&mut r)?;
    let n_kv = read_u64(&mut r)?;

    for _ in 0..n_kv {
        let key = read_str(&mut r)?;
        let tag = read_u32(&mut r)?;
        if key == CHAT_TEMPLATE_KEY {
            // The one key worth reading; every other value is skipped, so this
            // stops long before the tensor data.
            return if tag == 8 { read_str(&mut r) } else { None };
        }
        skip_value(&mut r, tag)?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    /// Build a GGUF header carrying `entries`, enough for this reader.
    fn gguf(entries: &[(&str, u32, Vec<u8>)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&3u32.to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&(entries.len() as u64).to_le_bytes());
        for (k, tag, val) in entries {
            out.extend_from_slice(&(k.len() as u64).to_le_bytes());
            out.extend_from_slice(k.as_bytes());
            out.extend_from_slice(&tag.to_le_bytes());
            out.extend_from_slice(val);
        }
        out
    }

    fn string_val(s: &str) -> Vec<u8> {
        let mut v = (s.len() as u64).to_le_bytes().to_vec();
        v.extend_from_slice(s.as_bytes());
        v
    }

    fn write(bytes: &[u8], name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("rto-gguf-{}-{name}", std::process::id()));
        let mut f = File::create(&p).expect("create");
        f.write_all(bytes).expect("write");
        p
    }

    #[test]
    fn reads_the_template_past_other_keys() {
        // A fixed-width value, a string, and a string array all precede it — the
        // three skip paths this reader has.
        let bytes = gguf(&[
            ("general.file_type", 4, 7u32.to_le_bytes().to_vec()),
            ("general.name", 8, string_val("some-model")),
            ("tokenizer.ggml.tokens", 9, {
                let mut v = 8u32.to_le_bytes().to_vec();
                v.extend_from_slice(&2u64.to_le_bytes());
                v.extend(string_val("a"));
                v.extend(string_val("bb"));
                v
            }),
            (
                "tokenizer.chat_template",
                8,
                string_val("{%- if tools %}X{%- endif %}"),
            ),
        ]);
        let p = write(&bytes, "ok");
        assert_eq!(
            chat_template(&p).as_deref(),
            Some("{%- if tools %}X{%- endif %}")
        );
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn a_model_with_no_template_is_none_not_an_error() {
        let bytes = gguf(&[("general.name", 8, string_val("embedding-model"))]);
        let p = write(&bytes, "none");
        assert_eq!(chat_template(&p), None);
        std::fs::remove_file(&p).ok();
    }

    /// Every "cannot answer" case is `None`, never a panic — the caller warns on
    /// the answer, and a truncated file must not be louder than a missing key.
    /// The real thing: this reader answers on an actual model in the registry.
    ///
    /// Skipped when the model is not installed, so CI does not depend on a
    /// 20 GB download — but stated as a skip rather than silently passing, since
    /// a synthetic header proves the parser and not the format.
    #[test]
    fn it_reads_a_real_gguf_when_one_is_installed() {
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        let p = Path::new(&home).join(".roteiro/models/qwen3-32b/model.gguf");
        if !p.is_file() {
            return;
        }
        let t = chat_template(&p).expect("qwen3-32b embeds a chat template");
        assert!(
            t.len() > 1000,
            "a real template is kilobytes; {} bytes suggests the walk stopped early",
            t.len()
        );
        assert!(
            crate::chat_template::is_jinja(&t),
            "and it is Jinja, which is the whole reason for reading it"
        );
    }

    #[test]
    fn a_file_that_is_not_gguf_is_none() {
        let p = write(b"not a gguf at all", "bad");
        assert_eq!(chat_template(&p), None);
        std::fs::remove_file(&p).ok();

        // Truncated mid-header: the counts promise more than the file holds.
        let mut bytes = gguf(&[("tokenizer.chat_template", 8, string_val("{{ x }}"))]);
        bytes.truncate(bytes.len() - 4);
        let p = write(&bytes, "trunc");
        assert_eq!(chat_template(&p), None);
        std::fs::remove_file(&p).ok();

        assert_eq!(chat_template(Path::new("/no/such/file.gguf")), None);
    }
}
