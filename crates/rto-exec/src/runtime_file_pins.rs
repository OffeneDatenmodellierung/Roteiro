// GENERATED FILE — do not edit by hand.
//
// Regenerate with:
//
//     scripts/derive-runtime-file-pins.py
//
// # What these are, and why they are derived rather than written
//
// `boxlite` downloads the runtime archive, extracts it into its own OUT_DIR and
// `include_bytes!`s **the extracted files** into the rlib. Those files are what
// ends up in the binary, so those are what `build.rs` verifies — one digest per
// file per platform, checked after extraction and before anything is linked.
//
// The archive pins in `runtime_pins.rs` remain the source of truth. This file is
// a mechanical function of them: the generator verifies each archive against its
// own pin before opening it, then hashes every member. A `boxlite` bump is
// therefore `runtime_pins.rs` + re-run the generator + review the diff, never
// fifteen hand-typed hex strings.
//
// Standalone on purpose — **no `use`, no `crate::` paths** — because `build.rs`
// pulls it in with `include!`, exactly as it does `runtime_pins.rs`.

/// One file as it must appear in `boxlite`'s extracted runtime directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinnedFile {
    /// Its name in the runtime directory — the archive member with the
    /// leading `boxlite-runtime/` component stripped, as
    /// `tar --strip-components=1` leaves it.
    pub name: &'static str,
    /// Lowercase hex SHA-256 of its contents.
    pub sha256: &'static str,
    /// Its exact size in bytes.
    pub bytes: u64,
}

/// One platform's extracted runtime files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinnedRuntimeFiles {
    /// The platform, as the upstream release names it.
    pub target: &'static str,
    /// The archive these were derived from, so a bumped archive pin that
    /// nobody re-derived is a test failure rather than a silent mismatch.
    pub archive_sha256: &'static str,
    /// Every file the archive contributes, sorted by name.
    pub files: &'static [PinnedFile],
}

/// The `boxlite` release these were derived from.
pub const RUNTIME_FILES_VERSION: &str = "0.9.7";

/// Every pinned platform's extracted runtime files.
pub const RUNTIME_FILES: &[PinnedRuntimeFiles] = &[
    PinnedRuntimeFiles {
        target: "darwin-arm64",
        archive_sha256: "7f64529978cd2af420411ddfd4cc3b5799ca20234d90346c887cb596d52f8d4e",
        files: &[
            PinnedFile {
                name: "boxlite-guest",
                sha256: "f09abc03fd2d233b1e6fa31327cb30c14698d7c60ee21be241096620767ae261",
                bytes: 14_074_296,
            },
            PinnedFile {
                name: "boxlite-shim",
                sha256: "741ced072011ee62ef3861908e4573bd2bb13c21bc0c40bca42eabea3de65a93",
                bytes: 22_991_696,
            },
            PinnedFile {
                name: "debugfs",
                sha256: "6bf2f08e5cb6ce2d2c9c445ed4d4b2ac12721d61ad2592b0e958c4db0e2d6f87",
                bytes: 661_800,
            },
            PinnedFile {
                name: "libkrunfw.5.dylib",
                sha256: "454efb5b04045c1b89eaa4d28e90afe92bad1b125b059168f164971a7189cf18",
                bytes: 22_970_192,
            },
            PinnedFile {
                name: "mke2fs",
                sha256: "d82d5b85dd86ee8fc8d6d1e72b59b331cd4bd288e485f9c9fb0da81170eb5854",
                bytes: 577_560,
            },
        ],
    },
    PinnedRuntimeFiles {
        target: "linux-arm64-gnu",
        archive_sha256: "78e978d6398d5a78dc76d675941cb05287e8c70b1b647e98a479058a9652be28",
        files: &[
            PinnedFile {
                name: "boxlite-guest",
                sha256: "e641807883c0c2d427e93d1fd18313fdd806593b89dbb1fafff48293e3bf8aa6",
                bytes: 14_079_928,
            },
            PinnedFile {
                name: "boxlite-shim",
                sha256: "2ce896d88569c9164a33b4b4c36221988076d78aab02a3c97d100fef4f49c87a",
                bytes: 26_652_536,
            },
            PinnedFile {
                name: "bwrap",
                sha256: "3e599fc36c39f220d9f0b05956c5c57720b6a83c819cc3114e3ed7e6175dbefa",
                bytes: 307_120,
            },
            PinnedFile {
                name: "debugfs",
                sha256: "ae190ef38146ffe20cdc12a419a86d9fd7fd55316356ffd6a995e9d9391acef4",
                bytes: 3_593_336,
            },
            PinnedFile {
                name: "libkrunfw.so.5",
                sha256: "f30112748a09cefccb9b3d98098fe2b770e785debfafea5dc9e0523f17b8d74a",
                bytes: 22_939_240,
            },
            PinnedFile {
                name: "mke2fs",
                sha256: "c5b92faf507b95db98b78234c7c12580e7a9fd141d4ad5bdeb212e0b4b2537d0",
                bytes: 3_052_784,
            },
        ],
    },
    PinnedRuntimeFiles {
        target: "linux-x64-gnu",
        archive_sha256: "9ae495f55d363e6af04640ab55025ac80b4bf4762e38fa0b8ac80c7604e3148c",
        files: &[
            PinnedFile {
                name: "boxlite-guest",
                sha256: "8d090705ae2fe424a5d2a501733029a9cadb58f158691957abecc823c638ae40",
                bytes: 14_480_904,
            },
            PinnedFile {
                name: "boxlite-shim",
                sha256: "ee1572b212f9041b7de208540c68e6bee608386064eedf2aabbc9a1f7b058677",
                bytes: 29_320_000,
            },
            PinnedFile {
                name: "bwrap",
                sha256: "3312ccace553fd083a00f4706c5658880113ed79f3fc1ec7bb486ec0797a7523",
                bytes: 187_376,
            },
            PinnedFile {
                name: "debugfs",
                sha256: "cf3e94ba478311991c4dd461a983365529b612293ff82a76ed4b1c1d6adfaf89",
                bytes: 3_374_856,
            },
            PinnedFile {
                name: "libkrunfw.so.5",
                sha256: "c29492267947a7f40218a9a181fe5aa3e9bcb96efbd0e3fab3d1839f5a0a9eff",
                bytes: 19_203_768,
            },
            PinnedFile {
                name: "mke2fs",
                sha256: "c3bedf42c320212abeb1c6325bd2663d7507a46d0ba06c2a76af1a9222c8ebb1",
                bytes: 2_831_648,
            },
        ],
    },
];

/// The extracted-file pins for an upstream target name.
#[must_use]
pub fn runtime_files_for(target: &str) -> Option<&'static PinnedRuntimeFiles> {
    let mut index = 0;
    // A plain loop rather than an iterator, matching `runtime_pins.rs`: this file
    // is `include!`d into a build script, where keeping to the language core is
    // the point.
    while index < RUNTIME_FILES.len() {
        if RUNTIME_FILES[index].target.as_bytes() == target.as_bytes() {
            return Some(&RUNTIME_FILES[index]);
        }
        index += 1;
    }
    None
}
