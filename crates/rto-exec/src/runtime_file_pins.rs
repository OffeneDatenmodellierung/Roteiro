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
pub const RUNTIME_FILES_VERSION: &str = "0.10.4";

/// Every pinned platform's extracted runtime files.
pub const RUNTIME_FILES: &[PinnedRuntimeFiles] = &[
    PinnedRuntimeFiles {
        target: "darwin-arm64",
        archive_sha256: "1424114d7a9637c746b05429e4e05dd47b8b9e90a84ba709f0b515375a6db5d3",
        files: &[
            PinnedFile {
                name: "boxlite-guest",
                sha256: "8d4da49f293c4d0aefde3453a41c58c076d158d3e545f5f9739e2d0127b753b8",
                bytes: 20_940_688,
            },
            PinnedFile {
                name: "boxlite-shim",
                sha256: "0ec9b4c5d38ab9e7a8662fb801912b442abdc2a3ef25e6a0af30f2d335bb86fb",
                bytes: 23_402_288,
            },
            PinnedFile {
                name: "debugfs",
                sha256: "55e856734bbc74a552b0176271a09e975cb12052f8e7e3993254ceb24de3d6e9",
                bytes: 661_800,
            },
            PinnedFile {
                name: "guest-mke2fs",
                sha256: "c62fca2b94dd9a3793a208279017a9944df7a05bcc6b38cc27ccd19a5f5e8b7d",
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
                sha256: "1eb5e33265a19d4412d8913e7aaceaac3026e84d64c9f8f148907f933ede7dce",
                bytes: 577_560,
            },
        ],
    },
    PinnedRuntimeFiles {
        target: "linux-arm64-gnu",
        archive_sha256: "3d4876986676b80d5ad3b3fd726480925663dce47bd182b9729eb630bc4fc891",
        files: &[
            PinnedFile {
                name: "boxlite-guest",
                sha256: "c876b7fa743446e846e8f84237546660f93a8b1a4a84b2b30417e3873720d0f5",
                bytes: 20_940_952,
            },
            PinnedFile {
                name: "boxlite-shim",
                sha256: "20968f65b2b83c32becc5c3d8e8ff9b9bfa9462008d032bde8f25da2042a78c3",
                bytes: 27_010_192,
            },
            PinnedFile {
                name: "bwrap",
                sha256: "bb7274e457c1a17240639eb1fec81073238e59316ebc4fc93a65ffb3ae326864",
                bytes: 307_120,
            },
            PinnedFile {
                name: "debugfs",
                sha256: "c2118f52452831d520854266c79e34e70cf9e49dcd281aadd136cd3fa8f1f365",
                bytes: 3_593_336,
            },
            PinnedFile {
                name: "guest-mke2fs",
                sha256: "75fc8fb87ad53d7f1f57a856cf5a81112e213412e7b614c408323318aa222107",
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
                sha256: "fd5232b6498bfb6b91e21850e0f6fc05d49defe06d919a79f1b6e8a36bab5cf4",
                bytes: 3_052_784,
            },
        ],
    },
    PinnedRuntimeFiles {
        target: "linux-x64-gnu",
        archive_sha256: "ec31b15832e0b801d5b6f2e98883a6a16c08c3a0fa6a71c3a09cb7be16ee484c",
        files: &[
            PinnedFile {
                name: "boxlite-guest",
                sha256: "c906cac46f2300bc2f478a3032d021ba9eec42de9cbc1f49f023a7dc9cc2482f",
                bytes: 21_588_968,
            },
            PinnedFile {
                name: "boxlite-shim",
                sha256: "f1c74e134dfde352d17a5c019772e2ca0a68c930c9de61e1e3451744cac85944",
                bytes: 29_800_320,
            },
            PinnedFile {
                name: "bwrap",
                sha256: "6e39aacae4c93597ad2ba6daf6d6fd1efd7dbe8940467241bb22d970bc393eed",
                bytes: 187_376,
            },
            PinnedFile {
                name: "debugfs",
                sha256: "059248ba752c1b86e1274052988212d665a843fe598c8e75fc9170957482c5b1",
                bytes: 3_374_856,
            },
            PinnedFile {
                name: "guest-mke2fs",
                sha256: "f4ba4b24e032c3dacaec1520cc86179767867d430203e4ef7f8be4a5ba91b502",
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
                sha256: "66dccde1c32c27d82bf2a281b33512d75d47c7e378d9fb848b4d059f2a7b4fdc",
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
