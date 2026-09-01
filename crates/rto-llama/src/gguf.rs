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

/// How deeply an array may nest before this reader gives up.
///
/// Nothing legitimate approaches it: a GGUF metadata array holds scalars or
/// strings, and an array *of arrays* is already unusual. It exists because
/// `skip_value` recurses, and recursion driven by a file's own bytes is a stack
/// the file gets to choose the depth of.
const MAX_ARRAY_DEPTH: u32 = 64;

/// Skip one value of type `tag`, returning `None` on a shape this reader does
/// not know — which ends the walk rather than guessing at an offset.
///
/// `depth` bounds nesting. Without it a crafted header aborts the process: each
/// `[array, array]` pair costs 12 bytes in the file and one stack frame here, so
/// 2.4 MB of them overflows the stack — measured, `fatal runtime error: stack
/// overflow`. That is not a parse failure this function can report, and every
/// other malformed input it meets comes back as `None`; a reader whose contract
/// is "unreadable means `None`" must not have an input that kills the process
/// instead.
///
/// The unbounded *counts* are deliberately left alone. `n_kv` and an array's
/// element count are read straight from the file and used as loop bounds, which
/// reads like the same hazard and is not: every iteration reads from the file, so
/// the walk ends at EOF whatever the count claimed. Measured with both set to
/// `u64::MAX` — 24 µs and 21 µs, both `None`. Capping them would add a bound that
/// never binds, and would state a guarantee the EOF already gives.
fn skip_value(r: &mut (impl Read + Seek), tag: u32, depth: u32) -> Option<()> {
    if depth > MAX_ARRAY_DEPTH {
        return None;
    }
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
                        skip_value(r, elem, depth + 1)?;
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
        skip_value(&mut r, tag, 0)?;
    }
    None
}

#[cfg(test)]
mod tests {

    /// A header that nests arrays without limit is refused, not fatal.
    ///
    /// The distinction this pins is the whole contract of this module: every
    /// malformed input comes back as `None`, so the caller can say "this model's
    /// metadata is unreadable" and carry on. A stack overflow is not `None` — it
    /// aborts the process, and `roteiro model pull` dies with `fatal runtime
    /// error` instead of a sentence naming the model.
    ///
    /// Measured before the bound existed: this exact 2.4 MB input aborted with
    /// `fatal runtime error: stack overflow` (SIGABRT). The file is small because
    /// each nesting level costs 12 bytes and one stack frame, which is the point
    /// — the attacker spends bytes far more cheaply than the reader spends stack.
    #[test]
    fn a_header_that_nests_arrays_without_end_is_refused_rather_than_fatal() {
        let mut b = Vec::new();
        b.extend_from_slice(&MAGIC);
        b.extend_from_slice(&3u32.to_le_bytes());
        b.extend_from_slice(&0u64.to_le_bytes());
        b.extend_from_slice(&1u64.to_le_bytes());
        b.extend_from_slice(&1u64.to_le_bytes());
        b.push(b'x');
        b.extend_from_slice(&9u32.to_le_bytes());
        // 200_000 levels of "an array whose elements are arrays".
        for _ in 0..200_000u32 {
            b.extend_from_slice(&9u32.to_le_bytes());
            b.extend_from_slice(&1u64.to_le_bytes());
        }
        let dir = std::env::temp_dir().join("rto-gguf-nested");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let p = dir.join("nested.gguf");
        std::fs::write(&p, &b).expect("write");

        assert_eq!(
            chat_template(&p),
            None,
            "a nested header must be unreadable, not fatal"
        );
        let _ = std::fs::remove_file(&p);
    }

    /// The counts are *not* bounded, and that is the right call.
    ///
    /// `n_kv` and an array's element count are read from the file and used as
    /// loop bounds, which looks like the same hazard as the recursion above. It
    /// is not: each iteration reads, so the walk ends at EOF regardless of what
    /// the count claimed.
    ///
    /// This **documents** that; it does not guard it. Adding
    /// `read_u64(..)?.min(4096)` leaves it green — verified — because a
    /// redundant cap and no cap produce the same `None`. What it would catch is
    /// the outcome changing: a dishonest count that hangs, panics, or returns a
    /// template. Said plainly because a test named for a property it cannot fail
    /// on is worth less than one that admits its reach.
    #[test]
    fn a_dishonest_count_ends_at_the_end_of_the_file() {
        let header = |tail: &[u8], n_kv: u64| {
            let mut b = Vec::new();
            b.extend_from_slice(&MAGIC);
            b.extend_from_slice(&3u32.to_le_bytes());
            b.extend_from_slice(&0u64.to_le_bytes());
            b.extend_from_slice(&n_kv.to_le_bytes());
            b.extend_from_slice(tail);
            b
        };
        let dir = std::env::temp_dir().join("rto-gguf-counts");
        std::fs::create_dir_all(&dir).expect("temp dir");

        // `n_kv` claims 2^64-1 entries and the file holds none.
        let a = dir.join("nkv.gguf");
        std::fs::write(&a, header(&[], u64::MAX)).expect("write");
        assert_eq!(chat_template(&a), None);

        // One entry whose array claims 2^64-1 string elements.
        let mut tail = Vec::new();
        tail.extend_from_slice(&1u64.to_le_bytes());
        tail.push(b'x');
        tail.extend_from_slice(&9u32.to_le_bytes());
        tail.extend_from_slice(&8u32.to_le_bytes());
        tail.extend_from_slice(&u64::MAX.to_le_bytes());
        let c = dir.join("count.gguf");
        std::fs::write(&c, header(&tail, 1)).expect("write");
        assert_eq!(chat_template(&c), None);

        let _ = std::fs::remove_file(&a);
        let _ = std::fs::remove_file(&c);
    }
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
        // Said out loud, both times. A test that returns early in silence is
        // indistinguishable from one that ran and passed, and this one asserts
        // nothing at all when it takes either branch — which is its state on CI,
        // where the model is not installed. The doc above promises a stated skip;
        // this is what makes that true rather than aspirational.
        let Some(home) = std::env::var_os("HOME") else {
            eprintln!("SKIP it_reads_a_real_gguf_when_one_is_installed: no HOME");
            return;
        };
        let p = Path::new(&home).join(".roteiro/models/qwen3-32b/model.gguf");
        if !p.is_file() {
            eprintln!(
                "SKIP it_reads_a_real_gguf_when_one_is_installed: {} is not installed \
                 (`roteiro model pull qwen3-32b`); the synthetic-header test still \
                 proves the parser, but nothing here has proved the format",
                p.display()
            );
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
