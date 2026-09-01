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
pub const RUNTIME_FILES_VERSION: &str = "0.10.0";

/// Every pinned platform's extracted runtime files.
pub const RUNTIME_FILES: &[PinnedRuntimeFiles] = &[
    PinnedRuntimeFiles {
        target: "darwin-arm64",
        archive_sha256: "8867bb02687c02a8ab6975c1dd8ef85d549dba9e5e94087cb7fb61838b56d979",
        files: &[
            PinnedFile {
                name: "boxlite-guest",
                sha256: "b9d7905916ee8b46ef4a4e18b22623aca05ce9a6683e453bc2d5835d5106595f",
                bytes: 20_526_368,
            },
            PinnedFile {
                name: "boxlite-shim",
                sha256: "c1e8bfb8c81aa17bcc905fcf20e4c4df2935b32c5365b9a22ef92d8728f0876f",
                bytes: 23_338_128,
            },
            PinnedFile {
                name: "debugfs",
                sha256: "d65b062b98f7cc712e076cc9074897fa5bc808273fd3e7543b6067aefde42be2",
                bytes: 661_800,
            },
            PinnedFile {
                name: "guest-mke2fs",
                sha256: "f2db9033e783447cdb36aefb5e64d60cffa68d3eef155cce2d9b49b501e5a3b4",
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
                sha256: "c1e314367f92ec668ca02644a1d7d07bf738d48b5782f3422f0629c11295f47d",
                bytes: 577_560,
            },
        ],
    },
    PinnedRuntimeFiles {
        target: "linux-arm64-gnu",
        archive_sha256: "e67786ba493430bed70e992fcd7248f4a71e1eaf562ddbbb016f478d044ca4cf",
        files: &[
            PinnedFile {
                name: "boxlite-guest",
                sha256: "34982642a3a2afe7dad8f3a97dbcc951b4d94515c0dc8c35e854d2383a36a35d",
                bytes: 20_523_288,
            },
            PinnedFile {
                name: "boxlite-shim",
                sha256: "3bc4b2182c52d36089523d1d098cec3fe88a7dbf633fbc3ed8f470cb404f8550",
                bytes: 27_033_936,
            },
            PinnedFile {
                name: "bwrap",
                sha256: "e36661910bbb5933b42a1403b8409066471570bc99d129fb67f3399da01d03e1",
                bytes: 307_120,
            },
            PinnedFile {
                name: "debugfs",
                sha256: "f581b714e1e282481e6521b88108ac6562edad8940e0052d5b081c8aa77f6365",
                bytes: 3_593_336,
            },
            PinnedFile {
                name: "guest-mke2fs",
                sha256: "205fdbb6efb8234ba13967ece6c9a4098a74ca955fd784824cb004c87b02976c",
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
                sha256: "ac74dac5f83abc2bf0629e25ba39d56b8806a808b21e260ec25a527b5df55baf",
                bytes: 3_052_784,
            },
        ],
    },
    PinnedRuntimeFiles {
        target: "linux-x64-gnu",
        archive_sha256: "3de43b2ca1620f7d73b71630be7f9e26f13f28497a4a692617a663dde0c8400f",
        files: &[
            PinnedFile {
                name: "boxlite-guest",
                sha256: "7eb2088aa3a5ffe51186a16bae214568e513edb4684034da0180bbed4986a479",
                bytes: 21_145_160,
            },
            PinnedFile {
                name: "boxlite-shim",
                sha256: "27735820133cfae77056150b91c56fdcb912a62536383ba2fd1bc7025051c5d6",
                bytes: 29_756_096,
            },
            PinnedFile {
                name: "bwrap",
                sha256: "697ad1697342ecd415addbfc91d4dfee73a4d543239e03ed854e559f137d680c",
                bytes: 187_376,
            },
            PinnedFile {
                name: "debugfs",
                sha256: "35f189fd3a715e0bfa94e92ffb12c539a09c7b0a5b760fef902452e5e129ae57",
                bytes: 3_374_856,
            },
            PinnedFile {
                name: "guest-mke2fs",
                sha256: "a5dfe7496d2a56da6f5060557846f9f3752e07064fcdb0ecde3906f876011aca",
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
                sha256: "76bfb9a5de67275f5275f1b65fd99d8a30d8e02c58d3532d9b96d6c99634b867",
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
