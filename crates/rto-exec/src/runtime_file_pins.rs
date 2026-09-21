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
pub const RUNTIME_FILES_VERSION: &str = "0.10.2";

/// Every pinned platform's extracted runtime files.
pub const RUNTIME_FILES: &[PinnedRuntimeFiles] = &[
    PinnedRuntimeFiles {
        target: "darwin-arm64",
        archive_sha256: "fbd3d7143f4f9e1217c07a58eceeef4755da5e9ee6aa844c2d11bee8036969d4",
        files: &[
            PinnedFile {
                name: "boxlite-guest",
                sha256: "c567d28631ac2cc4ef2c1c9f8158b0467d52f00550440b4b31fb165a924f18ca",
                bytes: 20_621_504,
            },
            PinnedFile {
                name: "boxlite-shim",
                sha256: "81d36d7792776297b4c0c67b40e37461c8c373e012bb3fff654ba389650cdae5",
                bytes: 23_401_824,
            },
            PinnedFile {
                name: "debugfs",
                sha256: "76e1261fc59922964abd056b709d5a180290e5ce8a00cabd9e4dd15acc7cf90b",
                bytes: 661_800,
            },
            PinnedFile {
                name: "guest-mke2fs",
                sha256: "64ac15a9dc85a05a34bc9dc74d127bcfb6b7aa05c0508657500e098eb1865bf4",
                bytes: 737_312,
            },
            PinnedFile {
                name: "guest-resize2fs",
                sha256: "0ec5291f557eff335dc6dc4f0edcd1adac21a5f44cc15a1193fba8c1612f3524",
                bytes: 563_872,
            },
            PinnedFile {
                name: "libkrunfw.5.dylib",
                sha256: "4735ad1eb68b8ae82222f0085fbab213ba25952d75f91f83fd7078c5c6913cd3",
                bytes: 23_762_768,
            },
            PinnedFile {
                name: "mke2fs",
                sha256: "024fa97e41bc79f8401856b61293ac20181b2676a66837fcc06d7e93970c1dea",
                bytes: 577_560,
            },
        ],
    },
    PinnedRuntimeFiles {
        target: "linux-arm64-gnu",
        archive_sha256: "0af74d10b4b211481b8c11b83260f0d9d0886f5b76bc48d73a016a3c811475b9",
        files: &[
            PinnedFile {
                name: "boxlite-guest",
                sha256: "b36ef7e43e0fd8e882bba7ef86b452e4c52b637278e118a60948c6a3e7f59ab7",
                bytes: 20_621_400,
            },
            PinnedFile {
                name: "boxlite-shim",
                sha256: "6fe2909aa89d9d04e9c5b16bb5f64ff3f4f2c6be66facc06f9c1cb14c811af01",
                bytes: 27_009_464,
            },
            PinnedFile {
                name: "bwrap",
                sha256: "a26628136ed40bb2f17d2bab201d7e1d4033bc9bf14a43e9ea8a815a17c5acb4",
                bytes: 307_120,
            },
            PinnedFile {
                name: "debugfs",
                sha256: "73a13fd88020dd85591491465b968c8c9efe5fe6f70cd1420c088d0a165b9703",
                bytes: 3_593_336,
            },
            PinnedFile {
                name: "guest-mke2fs",
                sha256: "d34d701f6d4c67f80297a00b6759a2d5ffbe7500eb36a14b21c3b021dae8f9d1",
                bytes: 645_720,
            },
            PinnedFile {
                name: "guest-resize2fs",
                sha256: "47e2659d5577bedc109de8f3427d42a26af1234669c563e50cc099961a9d0aca",
                bytes: 472_048,
            },
            PinnedFile {
                name: "libkrunfw.so.5",
                sha256: "a47fad6c557420899b7e079c63227b4660c365bbb1c7def0428bf6352786a321",
                bytes: 23_791_889,
            },
            PinnedFile {
                name: "mke2fs",
                sha256: "79f77566935b0a35a661aed455d3b86ca7caf6f17f1f7138184be57e89d2286c",
                bytes: 3_052_784,
            },
        ],
    },
    PinnedRuntimeFiles {
        target: "linux-x64-gnu",
        archive_sha256: "3977e350393502dcdcff6e2a232c3935bbbaa6ad5d02162a3f5588aaf83c6ec7",
        files: &[
            PinnedFile {
                name: "boxlite-guest",
                sha256: "59a2ba2261206b91fabe442b4da92f7f49239546467e2034e329c9372238d2f5",
                bytes: 21_277_568,
            },
            PinnedFile {
                name: "boxlite-shim",
                sha256: "b169c0bb50b17313cfd621656ce851e13353ad1375bcd0238798a2c51dc82afa",
                bytes: 29_798_520,
            },
            PinnedFile {
                name: "bwrap",
                sha256: "2a21b74a0359a6612a77db5f43de5251c79973fc14e577f16ba126005579e841",
                bytes: 187_376,
            },
            PinnedFile {
                name: "debugfs",
                sha256: "6bee03de45411bc927f5d254bebab7ee4f14ee34b5a4248c418d6f21cc7bfee2",
                bytes: 3_374_856,
            },
            PinnedFile {
                name: "guest-mke2fs",
                sha256: "9e580eae03106e92134085767da606e1e25b15803359e607f7bbbe8b7171c4bc",
                bytes: 560_048,
            },
            PinnedFile {
                name: "guest-resize2fs",
                sha256: "0a2c15cb20fa3e369a02e9712c611037a89236bc52ad109daf02c3d33a19ee97",
                bytes: 399_448,
            },
            PinnedFile {
                name: "libkrunfw.so.5",
                sha256: "953201c0c367070946a2f99695cb50dbb8980d97e416964bfa3fb1e9d1f15f69",
                bytes: 21_431_992,
            },
            PinnedFile {
                name: "mke2fs",
                sha256: "78f870fee7811faae4738c9888a9c1f2656e63532d220bd84a9ab02668348570",
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
