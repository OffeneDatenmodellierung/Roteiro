// A `file://` URL and the local path it names — both halves in one file,
// because they are one round-trip and halves of a round-trip kept apart is the
// defect this file exists to close.
//
// # Why this file is `include!`d rather than imported
//
// The same reason `src/asset_paths.rs` is: `build.rs` needs it, and a build
// script cannot depend on the crate it builds. That constrains what may appear
// here — **no `use`, no `crate::` paths, no references to anything outside this
// file** — so every type is spelled out in full.
//
// # Why anything is encoded at all
//
// The URL `build.rs` prints is not decoration. It is the copy-pasteable value
// for `BOXLITE_RUNTIME_URL`, and what reads that variable is `boxlite`'s own
// build script, which shells out to a bare `curl -fsSL`. `curl` rejects a
// `file://` URL carrying a raw space outright (curl 8.7.1):
//
//     $ curl -fsSL -o /dev/null "file:///tmp/polly space test/x.tar.gz"
//     curl: (3) URL rejected: Malformed input to a URL function
//     $ curl -fsSL -o /dev/null "file:///tmp/polly%20space%20test/x.tar.gz"
//     (ok)
//
// So an operator whose asset cache sits under a path with a space in it — a
// custom `ROTEIRO_SECURITY_ASSETS`, or simply a home directory with one — was
// handed a command that cannot work, and it failed inside a *dependency's*
// build script, where nothing on the output names the cause.
//
// # Why `%` is encoded too, and not spaces alone
//
// Because `curl` percent-decodes generally, so an encoder that stopped at
// spaces would leave the two sides disagreeing about which file a URL names.
// Measured against a directory whose name is literally `pct%20dir`:
//
//     $ curl -fsSL -o /dev/null "file:///tmp/pct%20dir/x.tar.gz"
//     curl: (37) Couldn't open file /tmp/pct%20dir/x.tar.gz   <- decoded to "pct dir"
//     $ curl -fsSL -o /dev/null "file:///tmp/pct%2520dir/x.tar.gz"
//     (ok)
//
// `build.rs` verifies the bytes at the path *it* resolves, and `curl` fetches
// the path *it* resolves; a URL on which those two answers differ is one
// archive verified and a different one embedded. Encoding `%` is also what
// makes the encoding reversible at all — with spaces alone, `/tmp/pct%20dir`
// and `/tmp/pct dir` emit the same URL, and no decoder can undo that.
//
// # Why exactly these two characters
//
// Because the decoder below understands exactly these two. Widening the encoder
// without widening the decoder in the same breath is this defect again with the
// arrow reversed, and the project has spent a day on that class already. Any
// other character passes through as itself, on both sides, and the failure —
// should `curl` object to one — is a URL that resolves to nothing rather than
// one that quietly resolves elsewhere.
//
// The same two-character map is spelled out once more, in Python, in
// `scripts/provision-sandbox-runtime.py`, which is what sets this variable in
// CI. Two languages is why it is stated twice and nowhere else.

/// The `file://` URL naming `path`, in the form both [`file_url_path`] and
/// `curl` read back as `path`.
///
/// Every Rust emitter of `BOXLITE_RUNTIME_URL=` goes through here — `build.rs`
/// has three, and three call sites each interpolating a raw path is exactly how
/// the fourth gets added raw too. The fourth already existed, in Python.
#[must_use]
pub fn file_url(path: &std::path::Path) -> String {
    // `%` first, and it has to be: it is the escape character, so encoding it
    // after the space would re-encode the `%` this very pass introduced.
    let encoded = path
        .to_string_lossy()
        .replace('%', "%25")
        .replace(' ', "%20");
    format!("file://{encoded}")
}

/// The local path a `file://` URL names, or `None` for any other scheme.
///
/// Deliberately minimal: `file:///abs/path` and the `file://localhost/abs/path`
/// form, and the exact inverse of [`file_url`]'s encoding. A relative or
/// scheme-less URL is refused rather than guessed at.
#[must_use]
pub fn file_url_path(url: &str) -> Option<std::path::PathBuf> {
    let rest = url
        .strip_prefix("file://localhost")
        .or_else(|| url.strip_prefix("file://"))?;
    if !rest.starts_with('/') {
        return None;
    }
    // The inverse order, for the mirror-image reason: `%20` before `%25`, so
    // that `%2520` — an encoded literal `%20` — comes back as `%20` and does
    // not collapse into a space.
    Some(std::path::PathBuf::from(
        rest.replace("%20", " ").replace("%25", "%"),
    ))
}

#[cfg(test)]
mod file_url_tests {
    use super::{file_url, file_url_path};
    use std::path::{Path, PathBuf};

    /// A space survives emit-then-parse byte for byte.
    ///
    /// This is the assertion that keeps the two halves honest: it fails if the
    /// emitter stops encoding, and it fails if the decoder stops decoding.
    #[test]
    fn a_path_with_a_space_round_trips() {
        let path = Path::new("/tmp/polly space test/boxlite-runtime.tar.gz");
        let url = file_url(path);

        assert_eq!(
            url, "file:///tmp/polly%20space%20test/boxlite-runtime.tar.gz",
            "the emitted URL is what an operator pastes into BOXLITE_RUNTIME_URL"
        );
        assert!(
            !url.contains(' '),
            "curl rejects a file:// URL with a raw space (exit 3, \
             \"URL rejected: Malformed input to a URL function\"): {url}"
        );
        assert_eq!(
            file_url_path(&url).as_deref(),
            Some(path),
            "what build.rs verifies has to be the file the URL was built from"
        );
    }

    /// And so does a literal `%`, which is what makes the encoding reversible.
    ///
    /// `pct%20dir` and `pct dir` are different directories. Without the `%25`
    /// pair they would emit the same URL, and `curl` — which decodes — would
    /// fetch the second while this crate verified the first.
    #[test]
    fn a_path_with_a_literal_percent_round_trips_too() {
        for path in [
            Path::new("/tmp/pct%20dir/boxlite-runtime.tar.gz"),
            Path::new("/tmp/100% done/boxlite-runtime.tar.gz"),
            Path::new("/tmp/%2520/boxlite-runtime.tar.gz"),
        ] {
            let url = file_url(path);
            assert_eq!(
                file_url_path(&url).as_deref(),
                Some(path),
                "{url} did not decode back to the path it was built from"
            );
        }
        assert_eq!(
            file_url(Path::new("/tmp/pct%20dir/x.tar.gz")),
            "file:///tmp/pct%2520dir/x.tar.gz"
        );
    }

    /// The cheap end of the real question: the URL resolves to the file.
    ///
    /// The round-trip above is a string identity. This one writes bytes under a
    /// directory whose name actually contains a space and reads them back
    /// through the emitted URL, so the encoding is checked against a filesystem
    /// rather than against another `replace` call.
    #[test]
    fn the_emitted_url_resolves_to_the_file_it_was_built_from() {
        const BODY: &[u8] = b"not really an archive";

        let dir = std::env::temp_dir().join(format!("rto-exec file url {}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp directory with a space in its name");
        let file = dir.join("boxlite-runtime.tar.gz");
        std::fs::write(&file, BODY).expect("write the stand-in archive");

        let url = file_url(&file);
        assert!(
            !url.contains(' ') && url.contains("%20"),
            "the directory really does have a space in its name, so the URL really does have \
             to carry an encoded one — that is the shape curl accepts: {url}"
        );

        let resolved = file_url_path(&url).expect("the emitter produces URLs the parser accepts");
        assert_eq!(resolved, file);
        assert_eq!(
            std::fs::read(&resolved).expect("the resolved path is readable"),
            BODY
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The parser's contract, unchanged: `file://` only, and absolute only.
    #[test]
    fn only_absolute_file_urls_are_accepted() {
        assert_eq!(
            file_url_path("file://localhost/tmp/a%20b/x.tar.gz").as_deref(),
            Some(Path::new("/tmp/a b/x.tar.gz"))
        );
        assert_eq!(
            file_url_path("file:///tmp/x.tar.gz"),
            Some(PathBuf::from("/tmp/x.tar.gz"))
        );
        assert_eq!(file_url_path("https://example.invalid/x.tar.gz"), None);
        assert_eq!(file_url_path("/tmp/x.tar.gz"), None);
        assert_eq!(file_url_path("file://relative/x.tar.gz"), None);
    }
}
