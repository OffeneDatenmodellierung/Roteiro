//! The **sandbox image store**: what it is holding, and dropping it safely.
//!
//! `roteiro security prefetch` obtains and `roteiro security status` reports.
//! Nothing removed, so the store only grew — 2.9 GB when issue #433 was filed,
//! 8.7 GB on the machine this module was written on, 12 GB after one afternoon
//! of trying candidate builder images. ADR-0014 v1.6 gives provisioning its third
//! verb, and this module is the half of it that knows what is on disk.
//!
//! # The store has no names in it, so deletion starts at the index
//!
//! Everything under `<asset-root>/boxlite-home/images/` is keyed by digest —
//! `layers/`, `configs/`, `manifests/`, `extracted/`, `disk-images/` — and not one
//! path carries an image's name. The name → digest mapping lives only in
//! `db/boxlite.db`'s `image_index` table. **There is no filesystem-only route**:
//! a walk can tell you the store is 8.5 GB and cannot tell you which image any of
//! it belongs to.
//!
//! So [`status`] and [`clear`] both start by reading that table, and every path
//! this module touches is derived from a row in it. A file it cannot derive is
//! never deleted on a per-image request and is reported by name — see
//! [`SandboxStatus::unattributed`].
//!
//! # Deletion is a set difference, never a walk
//!
//! Blobs are shared. Two images can share a base layer, and one image's layer
//! list can name the same digest twice — `cimg/rust`'s does, in the store this
//! was written against. Dropping image A must therefore remove only the digests
//! **no surviving image references**, which is a set difference over the whole
//! index rather than a walk of A's own object list.
//!
//! Getting this wrong is invisible until it isn't. The first pair of images with
//! a common layer turns a naive per-image delete into a *broken surviving image*
//! — not an error, not a warning, just a `security run` that fails much later
//! with a missing blob. [`plan`] is the only place the difference is computed and
//! [`clear`] cannot delete anything [`plan`] did not put in the doomed set.
//!
//! # The derived artifacts, and the association that was thought to be missing
//!
//! `images/disk-images/*.ext4` is the largest thing in the store — 3.7 GB of 8.7
//! on this machine — and issue #433's hand-clearing notes record that nothing
//! mapped an image to the disk image built from it, so they were separated by
//! **mtime** and the note says a shipped `clear` must not do that.
//!
//! It does not have to. `boxlite`'s `ImageObject::compute_image_digest` keys a
//! disk image by the SHA-256 of its layer digest strings concatenated in manifest
//! order, and `ImageDiskManager::disk_path` writes it as `{digest}.ext4` with the
//! `:` turned into `-`. That is [`image_digest`], it is computed from
//! `image_index.layers` alone, and it reproduces all three filenames in the live
//! store exactly. The base rootfs in `bases/` is keyed off the same value:
//! `base_disk.name` is `{image_digest[..12]}-{guest_binary_hash[..12]}`.
//!
//! **It is a re-derivation of another crate's private function, so it is checked
//! rather than trusted.** [`Attribution`] records whether every disk image on
//! disk was claimed by some indexed image; when one is not, it becomes
//! `unattributed` and a per-image `clear` leaves it alone. A `boxlite` upgrade
//! that changed the key would show up as unattributed bytes in `status`, which is
//! a visible wrong number rather than a silent wrong deletion.
//!
//! # What may be dropped, and the two things here that may not
//!
//! ADR-0014 v1.6's permission and its limit are one property: everything under
//! the asset cache is re-obtainable from a pinned digest, so clearing costs time
//! and never information — **and the verb may therefore never reach anything that
//! is not**. Under this store root, two things are not:
//!
//! - **A `base_disk` row of kind `snapshot` or `clone_base`.** A `rootfs` base is
//!   rebuilt from the image; a snapshot is the state of a box somebody ran, and no
//!   digest re-obtains it. They are never deleted, and they are reported —
//!   [`ClearReport::preserved`].
//! - **Anything under the store root this module does not recognise.** A new
//!   `boxlite` layout directory is not known to be re-obtainable just because it
//!   turned up in a cache, so [`clear`] refuses rather than guesses:
//!   [`StoreError::UnrecognisedEntry`].
//!
//! Nothing outside `<asset-root>/boxlite-home` is reachable from here at all. The
//! findings layers, the memory records and `graph.db` live in the repository's
//! store, which this module has no path to and does not link against.
//!
//! # Two prefixes that look like one
//!
//! The index stores `sha256:abc…`; the filesystem writes `sha256-abc…`. Issue
//! #433's first hand-clearing pass matched nothing because of it. [`blob_name`]
//! is the single translation, and every path in this module goes through it.
//!
//! @rto:0014
//! @rto:0013

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::sha256_hex;

/// Schema tag for the sandbox-store status document.
pub const SANDBOX_STATUS_SCHEMA: &str = "roteiro.sandbox.status/v1";

/// Schema tag for the sandbox-store clear document.
pub const SANDBOX_CLEAR_SCHEMA: &str = "roteiro.sandbox.clear/v1";

/// The sandbox store's directory under the asset cache root.
///
/// `boxlite.rs` spells the same literal where it builds a runtime's `home_dir`,
/// and cannot share this constant without `exec-boxlite` becoming a condition of
/// being able to *clear* a store a previous build filled. The two are held
/// together by `the_store_directory_is_the_one_boxlite_is_pointed_at` instead.
pub const SANDBOX_STORE_DIR: &str = "boxlite-home";

/// The index `boxlite` keeps its name → digest mapping in, under the store root.
const INDEX_DB: &str = "db/boxlite.db";

/// Top-level entries [`clear`] knows the disposition of.
///
/// Anything else under the store root stops [`clear`] with
/// [`StoreError::UnrecognisedEntry`], because "it appeared in a cache" is not
/// evidence that a digest re-obtains it (ADR-0014 v1.6).
const KNOWN_ENTRIES: &[&str] = &[".lock", "bases", "boxes", "db", "images", "locks", "tmp"];

/// The digest-keyed object directories under `images/`.
const IMAGE_DIRS: &[&str] = &["configs", "disk-images", "extracted", "layers", "manifests"];

/// Turn an index digest (`sha256:abc…`) into the name the filesystem uses
/// (`sha256-abc…`).
///
/// The single place that translation happens. It is one character and it is the
/// reason issue #433's first hand-clearing pass matched nothing at all.
#[must_use]
pub fn blob_name(digest: &str) -> String {
    digest.replace(':', "-")
}

/// The cache key `boxlite` derives a disk image and a base rootfs from: the
/// SHA-256 of the layer digest strings, concatenated in manifest order.
///
/// A re-derivation of `boxlite`'s `ImageObject::compute_image_digest`, which is
/// private to that crate. It is what removes the mtime heuristic issue #433's
/// notes fell back to, and [`Attribution`] is what keeps it honest if `boxlite`
/// ever changes it.
///
/// Duplicates are **not** removed and order is **not** normalised: this hashes the
/// layer list as the manifest wrote it, because that is what the other side does.
/// `cimg/rust` names one digest twice and still resolves to the filename on disk.
#[must_use]
pub fn image_digest(layers: &[String]) -> String {
    let joined: String = layers.concat();
    format!("sha256:{}", sha256_hex(joined.as_bytes()))
}

/// What went wrong, in terms of what to do about it.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// The index could not be read. Without it nothing in the store has a name,
    /// so neither verb can proceed — see this module's documentation.
    #[error("cannot read the sandbox image index at {path}: {message}")]
    Index {
        /// The database that could not be read.
        path: String,
        /// What the database layer said.
        message: String,
    },
    /// A filesystem operation failed.
    #[error("{action} {path}: {message}")]
    Io {
        /// What was being attempted, as a verb phrase.
        action: &'static str,
        /// The path it was attempted on.
        path: String,
        /// What the operating system said.
        message: String,
    },
    /// A per-image request named an image the store is not holding.
    ///
    /// Carries what it *is* holding, because the likely cause is a tag written
    /// where the index has a digest reference, and a listing is the way forward.
    #[error("the sandbox store is not holding `{reference}`; it is holding: {known}")]
    UnknownImage {
        /// What was asked for.
        reference: String,
        /// The references the index does have, comma-separated.
        known: String,
    },
    /// A box is registered in the store, so something may be using these bytes.
    ///
    /// `boxlite` takes an exclusive `flock` on `<store>/.lock` for the lifetime of
    /// a runtime, which this crate cannot take back: `unsafe_code = "forbid"`
    /// rules out the `libc::flock` call that acquires it. So the guard is the
    /// evidence a *lock* would have protected — a registered box — and it is
    /// checked rather than assumed absent.
    #[error(
        "the sandbox store has {boxes} registered box(es); \
         stop them before clearing, or the bytes a running box is reading go away underneath it"
    )]
    LiveBoxes {
        /// How many boxes are registered.
        boxes: usize,
    },
    /// Something under the store root that this module does not recognise.
    ///
    /// ADR-0014 v1.6's limit, enforced rather than trusted: `clear` may drop what
    /// a pinned digest re-obtains and may drop nothing else, and an unknown entry
    /// is not known to be re-obtainable.
    #[error(
        "the sandbox store holds `{entry}`, which this version of Roteiro does not recognise; \
         it will not be cleared, and nothing else was cleared either — \
         report it on issue #433, because an entry a digest does not re-obtain does not belong here"
    )]
    UnrecognisedEntry {
        /// The entry's name under the store root.
        entry: String,
    },
    /// A `base_disk` row points outside the store root.
    ///
    /// `base_path` is an absolute path recorded when the base was built, so it is
    /// data rather than a derivation, and data can name anywhere. This is the
    /// check that a row cannot aim deletion at a path outside the asset cache.
    #[error("the sandbox index has a base disk at {path}, which is outside the store root {root}")]
    BaseOutsideStore {
        /// Where the row pointed.
        path: String,
        /// The root it had to be under.
        root: String,
    },
}

/// Which images a [`clear`] is being asked for.
///
/// Two variants rather than an `Option<String>`, because ADR-0014 v1.6 requires
/// that "clear this image" and "clear everything" be **different arguments** — a
/// caller asking for one must not be able to receive the other by supplying
/// nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Every cached image, and the unattributed bytes alongside them.
    Everything,
    /// One image, by the reference the index holds it under. Anything a surviving
    /// image still references stays.
    Image(String),
}

impl Scope {
    /// The token this serialises as, for a report that names what was asked for.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Everything => "everything",
            Self::Image(reference) => reference,
        }
    }
}

/// Whether the store's derived artifacts were all claimed by an indexed image.
///
/// [`image_digest`] re-derives a key that is private to `boxlite`, so this is the
/// evidence that the re-derivation still matches. `Complete` means every
/// `disk-images/*.ext4` in the store was claimed; `Partial` names how many were
/// not, and those bytes are reported as unattributed rather than deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Attribution {
    /// Every derived artifact belongs to an indexed image.
    Complete,
    /// Some do not. A per-image `clear` will not touch them.
    Partial,
}

/// One image's byte accounting.
///
/// `total` is everything the image references; `exclusive` is what dropping this
/// image *alone* would free. They differ exactly when another cached image shares
/// a blob, which is the case the set difference exists for — and reporting only
/// `total` would promise bytes back that a shared layer keeps.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct ImageBytes {
    /// The manifest and config JSON.
    pub metadata: u64,
    /// The compressed layer tarballs under `images/layers/`.
    pub layers: u64,
    /// The unpacked layer trees under `images/extracted/`.
    pub extracted: u64,
    /// The ext4 disk image built from the layer stack, if one has been built.
    pub disk_image: u64,
    /// The guest rootfs base built from that disk image, if one has been built.
    pub base_disk: u64,
    /// Everything above.
    pub total: u64,
    /// What dropping this image alone would actually free — `total` minus every
    /// byte another cached image also references.
    pub exclusive: u64,
}

/// How much of an image's **pulled** content is on disk.
///
/// Manifest, config and one entry per unique layer digest — the objects a pull
/// produces and a run consumes. Deliberately *not* counting the extracted trees,
/// the disk image or the base rootfs: those are built lazily on first run and are
/// a cache below this cache, so an image that has only ever been pulled is
/// complete without them.
///
/// The unit matches what issue #433's hand verification counted: 15/15 for
/// `semgrep` (one manifest, one config, thirteen layers) and 3/3 for `debian`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Objects {
    /// How many objects the index says this image has.
    pub expected: usize,
    /// How many of them are on disk.
    pub present: usize,
}

impl Objects {
    /// Whether every object the index names is on disk.
    #[must_use]
    pub fn complete(self) -> bool {
        self.expected == self.present
    }
}

/// One image in the store.
#[derive(Debug, Clone, Serialize)]
pub struct CachedImage {
    /// The reference the index holds it under — a tag, or a digest-pinned name.
    pub reference: String,
    /// The manifest digest, as the index spells it (`sha256:…`).
    pub manifest_digest: String,
    /// The config digest, as the index spells it.
    pub config_digest: String,
    /// The derived key its disk image and base rootfs are stored under
    /// ([`image_digest`]).
    pub image_digest: String,
    /// When the pull completed, as the index recorded it.
    pub cached_at: String,
    /// Whether the index considers the pull finished. A `false` here is a partial
    /// pull, not a corrupt one, and `security prefetch` completes it.
    pub pull_complete: bool,
    /// How many **distinct** layer digests it has. Below the manifest's layer
    /// count when a layer is named twice.
    pub layers: usize,
    /// Its byte accounting.
    pub bytes: ImageBytes,
    /// How much of its pulled content is on disk.
    pub objects: Objects,
    /// Whether a disk image has been built from it.
    pub disk_image_built: bool,
    /// Whether a guest rootfs base has been built from it.
    pub base_disk_built: bool,
}

/// Bytes under the store root that no indexed image claims.
#[derive(Debug, Clone, Serialize)]
pub struct Unattributed {
    /// Where it is, relative to the store root.
    pub path: String,
    /// Its size in bytes.
    pub bytes: u64,
}

/// Something under the store root that [`clear`] deliberately leaves alone, and
/// why.
#[derive(Debug, Clone, Serialize)]
pub struct Preserved {
    /// Where it is, relative to the store root.
    pub path: String,
    /// Why it is not re-obtainable from a pinned digest, in one sentence.
    pub reason: String,
}

/// What the sandbox store is holding.
///
/// # Why `scope` is a field and not a sentence in a doc comment
///
/// The same reason [`crate::tool_security::MachineScope`] carries one: an asset
/// root is a property of the **machine**, and a caller who selected a project
/// (ADR-0008) will otherwise read a store size as that project's. There is one
/// sandbox store per asset root and every repository on the host shares it, so
/// the document says so in a field that travels with any half of it that gets
/// quoted.
#[derive(Debug, Clone, Serialize)]
pub struct SandboxStatus {
    /// Stable schema tag ([`SANDBOX_STATUS_SCHEMA`]).
    pub schema: &'static str,
    /// Always `"machine"`. The store is shared by every repository on this host.
    pub scope: &'static str,
    /// The store root these numbers describe.
    pub store: String,
    /// Whether there is a store there at all. `false` means nothing is cached,
    /// which is a different fact from an empty index.
    pub present: bool,
    /// Every image the index is holding, largest first.
    pub images: Vec<CachedImage>,
    /// Whether every derived artifact was claimed by one of them.
    pub attribution: Attribution,
    /// Bytes no indexed image claims. A per-image `clear` never touches these.
    pub unattributed: Vec<Unattributed>,
    /// State a digest does not re-obtain, which `clear` will not remove.
    pub preserved: Vec<Preserved>,
    /// How many boxes are registered. Non-zero blocks a `clear`.
    pub live_boxes: usize,
    /// Every byte under the store root.
    pub total_bytes: u64,
}

/// One image [`clear`] removed, and what removing it freed.
#[derive(Debug, Clone, Serialize)]
pub struct RemovedImage {
    /// The reference it was held under.
    pub reference: String,
    /// Bytes freed by removing it — its exclusive bytes, never its total.
    pub freed_bytes: u64,
    /// How many objects were removed from the filesystem.
    pub objects_removed: usize,
}

/// A surviving image, re-checked against the filesystem after the deletion.
///
/// The assertion that matters. A set-difference bug does not present as an error;
/// it presents as an image whose blobs are gone, discovered on the next run. So
/// `clear` re-resolves every survivor's manifest, config and layers against the
/// filesystem *after* deleting, and says so in its report — issue #433's "a
/// `clear` that cannot demonstrate the surviving images are still complete is one
/// nobody trusts twice".
#[derive(Debug, Clone, Serialize)]
pub struct VerifiedImage {
    /// The reference it is held under.
    pub reference: String,
    /// Its object tally after the deletion.
    pub objects: Objects,
    /// Whether every object is still there.
    pub complete: bool,
}

/// What a [`clear`] would do, or did.
#[derive(Debug, Clone, Serialize)]
pub struct ClearReport {
    /// Stable schema tag ([`SANDBOX_CLEAR_SCHEMA`]).
    pub schema: &'static str,
    /// Always `"machine"`, for the reason [`SandboxStatus::scope`] gives.
    pub scope: &'static str,
    /// The store root that was cleared.
    pub store: String,
    /// What was asked for — a reference, or `everything`.
    pub requested: String,
    /// Whether this is a plan or a completed removal.
    pub applied: bool,
    /// The images removed.
    pub removed: Vec<RemovedImage>,
    /// The unattributed bytes removed. Only ever populated for
    /// [`Scope::Everything`].
    pub removed_unattributed: Vec<Unattributed>,
    /// Bytes accounted for by the objects removed.
    pub freed_bytes: u64,
    /// Every byte under the store root before the deletion.
    pub store_bytes_before: u64,
    /// Every byte under the store root after it. Equal to `store_bytes_before` on
    /// a plan.
    pub store_bytes_after: u64,
    /// The survivors, re-checked blob by blob after the deletion.
    pub retained: Vec<VerifiedImage>,
    /// State a digest does not re-obtain, left alone and named.
    pub preserved: Vec<Preserved>,
}

impl ClearReport {
    /// The bytes the filesystem actually gave back.
    ///
    /// Reported alongside [`ClearReport::freed_bytes`] rather than instead of it:
    /// the accounted figure is what this module believes it removed, the measured
    /// one is what the store shrank by, and the two are checkable against each
    /// other by anyone holding a `du`.
    ///
    /// They are not identical, and the gap is one thing. **`SQLite` does not shrink
    /// a file when rows are deleted** — it frees pages inside it, and a `DELETE`
    /// under a write transaction can add one. So the index is the only part of the
    /// store that can move these two figures apart, it moves them by kilobytes
    /// against a clear measured in gigabytes, and
    /// `the_accounted_bytes_and_the_measured_bytes_differ_only_by_the_index` is
    /// what keeps that claim true. Anything larger is a defect, which is why both
    /// numbers are reported rather than one.
    #[must_use]
    pub fn measured_freed_bytes(&self) -> u64 {
        self.store_bytes_before
            .saturating_sub(self.store_bytes_after)
    }

    /// Whether every surviving image is still complete.
    #[must_use]
    pub fn survivors_intact(&self) -> bool {
        self.retained.iter().all(|image| image.complete)
    }
}

// ---------------------------------------------------------------------------
// The index
// ---------------------------------------------------------------------------

/// One `image_index` row, with its layer list parsed.
#[derive(Debug, Clone)]
struct IndexRow {
    reference: String,
    manifest_digest: String,
    config_digest: String,
    /// In manifest order and **with duplicates**, because [`image_digest`] hashes
    /// the list as the manifest wrote it.
    layers: Vec<String>,
    cached_at: String,
    complete: bool,
}

impl IndexRow {
    /// The distinct layer digests, for the object lists that are keyed by digest.
    fn unique_layers(&self) -> BTreeSet<String> {
        self.layers.iter().cloned().collect()
    }
}

/// One `base_disk` row.
#[derive(Debug, Clone)]
struct BaseRow {
    name: String,
    kind: String,
    path: PathBuf,
}

/// What the index says, read in one pass.
#[derive(Debug, Default)]
struct Index {
    images: Vec<IndexRow>,
    bases: Vec<BaseRow>,
    boxes: usize,
}

/// Read `image_index`, `base_disk` and the box count.
///
/// Opened read-only: [`status`] must not be able to write to a store it is only
/// describing, and [`clear`] does its own writing through a separate connection
/// once it has decided what to do.
fn read_index(store: &Path) -> Result<Index, StoreError> {
    let path = store.join(INDEX_DB);
    if !path.exists() {
        return Ok(Index::default());
    }
    let fail = |message: String| StoreError::Index {
        path: path.display().to_string(),
        message,
    };
    let db = rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|error| fail(error.to_string()))?;

    let mut images = Vec::new();
    {
        let mut statement = db
            .prepare(
                "SELECT reference, manifest_digest, config_digest, layers, cached_at, complete \
                 FROM image_index ORDER BY reference",
            )
            .map_err(|error| fail(error.to_string()))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .map_err(|error| fail(error.to_string()))?;
        for row in rows {
            let (reference, manifest_digest, config_digest, layers, cached_at, complete) =
                row.map_err(|error| fail(error.to_string()))?;
            // A layer list that will not parse is a row this module cannot derive
            // paths from, and inventing an empty one would make its blobs look
            // unreferenced — which is how a set difference deletes a live image.
            // Refusing is the only safe reading.
            let layers: Vec<String> = serde_json::from_str(&layers).map_err(|error| {
                fail(format!(
                    "image_index row `{reference}` has an unreadable layer list: {error}"
                ))
            })?;
            images.push(IndexRow {
                reference,
                manifest_digest,
                config_digest,
                layers,
                cached_at,
                complete: complete != 0,
            });
        }
    }

    let mut bases = Vec::new();
    {
        let mut statement = db
            .prepare("SELECT name, kind, base_path FROM base_disk ORDER BY id")
            .map_err(|error| fail(error.to_string()))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|error| fail(error.to_string()))?;
        for row in rows {
            let (name, kind, path) = row.map_err(|error| fail(error.to_string()))?;
            bases.push(BaseRow {
                name: name.unwrap_or_default(),
                kind,
                path: PathBuf::from(path),
            });
        }
    }

    let boxes = db
        .query_row("SELECT COUNT(*) FROM box_config", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|error| fail(error.to_string()))?;

    Ok(Index {
        images,
        bases,
        boxes: usize::try_from(boxes).unwrap_or(usize::MAX),
    })
}

// ---------------------------------------------------------------------------
// Object lists
// ---------------------------------------------------------------------------

/// The `bases/` prefix a `rootfs` base disk belonging to `digest` is named with.
///
/// `boxlite`'s `GuestRootfsManager::version_key` is
/// `{image_digest[..12]}-{guest_binary_hash[..12]}`, so the image half is a
/// prefix match and the guest half — which changes when the runtime does — is not
/// something this crate has to know.
fn base_name_prefix(image_digest: &str) -> String {
    let bare = image_digest.strip_prefix("sha256:").unwrap_or(image_digest);
    format!("{}-", &bare[..12.min(bare.len())])
}

/// Every path one image references, split into the pulled objects and the
/// derived ones.
#[derive(Debug, Default)]
struct ImageObjects {
    /// Manifest, config and layer tarballs — what a pull produces. The unit
    /// [`Objects`] counts.
    pulled: Vec<PathBuf>,
    /// Extracted trees, the disk image and the base rootfs — built lazily on
    /// first run, and a cache below this cache.
    derived: Vec<PathBuf>,
    /// How many `rootfs` base disks the index attributes to this image.
    bases: usize,
}

impl ImageObjects {
    fn all(&self) -> impl Iterator<Item = &PathBuf> {
        self.pulled.iter().chain(self.derived.iter())
    }
}

/// Resolve one index row to the paths it references.
///
/// The index manifest — the `application/vnd.oci.image.index.v1+json` a tag or a
/// digest reference resolves through — is *not* here. It is claimed separately by
/// [`index_manifests_for`], because a tag reference does not record which index it
/// came from and the association has to be read out of the files themselves.
fn objects_for(store: &Path, row: &IndexRow, bases: &[BaseRow]) -> ImageObjects {
    let images = store.join("images");
    let mut objects = ImageObjects::default();

    objects.pulled.push(
        images
            .join("manifests")
            .join(format!("{}.json", blob_name(&row.manifest_digest))),
    );
    objects.pulled.push(
        images
            .join("configs")
            .join(format!("{}.json", blob_name(&row.config_digest))),
    );
    for layer in row.unique_layers() {
        let name = blob_name(&layer);
        objects
            .pulled
            .push(images.join("layers").join(format!("{name}.tar.gz")));
        objects.derived.push(images.join("extracted").join(name));
    }

    let digest = image_digest(&row.layers);
    objects.derived.push(
        images
            .join("disk-images")
            .join(format!("{}.ext4", blob_name(&digest))),
    );
    let prefix = base_name_prefix(&digest);
    for base in bases {
        if base.kind == "rootfs" && base.name.starts_with(&prefix) {
            objects.derived.push(base.path.clone());
            objects.bases += 1;
        }
    }
    objects
}

/// The index manifests that resolve to any of `retained` platform manifests.
///
/// `docker.io/library/debian:bookworm-slim` is held under a **tag**, so its index
/// digest appears nowhere in `image_index` — only the platform manifest it
/// resolved to does. Deleting every manifest file the index does not name would
/// therefore take the index file of a surviving image with it. Reading the files
/// and keeping any that lists a retained manifest is the association the database
/// does not carry.
fn index_manifests_for(store: &Path, retained: &BTreeSet<String>) -> BTreeSet<PathBuf> {
    let dir = store.join("images").join("manifests");
    let mut keep = BTreeSet::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return keep;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(document) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        let Some(children) = document.get("manifests").and_then(|value| value.as_array()) else {
            continue;
        };
        if children
            .iter()
            .filter_map(|child| child.get("digest").and_then(|value| value.as_str()))
            .any(|digest| retained.contains(digest))
        {
            keep.insert(path);
        }
    }
    keep
}

// ---------------------------------------------------------------------------
// Sizes
// ---------------------------------------------------------------------------

/// Apparent bytes at `path`: the file's length, or the sum of the lengths of
/// every regular file beneath it.
///
/// Apparent rather than allocated. `du` and this agree on the store measured for
/// issue #433 because nothing in it is sparse or hard-linked, and an apparent
/// figure is the one that predicts what a re-pull will cost — which is what the
/// number is for.
fn size_of(path: &Path) -> u64 {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !metadata.is_dir() {
        return metadata.len();
    }
    let mut total = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                stack.push(entry.path());
            } else {
                total += metadata.len();
            }
        }
    }
    total
}

/// Measure every path once, so a digest shared by two images is not weighed twice.
fn measure(paths: impl IntoIterator<Item = PathBuf>) -> BTreeMap<PathBuf, u64> {
    let mut sizes = BTreeMap::new();
    for path in paths {
        sizes.entry(path).or_insert_with_key(|path| size_of(path));
    }
    sizes
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

/// Where the sandbox store lives under an asset cache root.
#[must_use]
pub fn store_root(asset_root: &Path) -> PathBuf {
    asset_root.join(SANDBOX_STORE_DIR)
}

/// Report what the sandbox store is holding, per image, with sizes.
///
/// Enough to decide what to drop, which is ADR-0014 v1.6's third rule: a
/// destructive verb with no way to see what it will destroy is invoked blind.
///
/// # Errors
///
/// [`StoreError::Index`] when the index exists and cannot be read. An **absent**
/// store is not an error — it is `present: false`, because "nothing is cached" is
/// an answer rather than a failure.
pub fn status(asset_root: &Path) -> Result<SandboxStatus, StoreError> {
    let store = store_root(asset_root);
    let mut report = SandboxStatus {
        schema: SANDBOX_STATUS_SCHEMA,
        scope: "machine",
        store: store.display().to_string(),
        present: store.is_dir(),
        images: Vec::new(),
        attribution: Attribution::Complete,
        unattributed: Vec::new(),
        preserved: Vec::new(),
        live_boxes: 0,
        total_bytes: 0,
    };
    if !report.present {
        return Ok(report);
    }

    let index = read_index(&store)?;
    report.live_boxes = index.boxes;
    report.total_bytes = size_of(&store);

    let objects: Vec<(usize, ImageObjects)> = index
        .images
        .iter()
        .enumerate()
        .map(|(at, row)| (at, objects_for(&store, row, &index.bases)))
        .collect();

    // Every path any image references, measured once. `references` counts how
    // many images claim each path, which is what turns a total into an exclusive
    // figure without measuring anything twice.
    let sizes = measure(
        objects
            .iter()
            .flat_map(|(_, object)| object.all().cloned())
            .collect::<Vec<_>>(),
    );
    let mut references: BTreeMap<&PathBuf, usize> = BTreeMap::new();
    for (_, object) in &objects {
        for path in object.all() {
            *references.entry(path).or_default() += 1;
        }
    }

    for (at, object) in &objects {
        let row = &index.images[*at];
        report
            .images
            .push(cached_image(&store, row, object, &sizes, &references));
    }
    report
        .images
        .sort_by_key(|image| std::cmp::Reverse(image.bytes.total));

    let claimed: BTreeSet<PathBuf> = objects
        .iter()
        .flat_map(|(_, object)| object.all().cloned())
        .chain(index_manifests_for(
            &store,
            &index
                .images
                .iter()
                .map(|row| row.manifest_digest.clone())
                .collect(),
        ))
        .chain(preserved_paths(&index))
        .collect();
    report.unattributed = unattributed(&store, &claimed);
    if !report.unattributed.is_empty() {
        report.attribution = Attribution::Partial;
    }
    report.preserved = preserved(&index);
    Ok(report)
}

/// Build one image's row from its object list and the shared size table.
fn cached_image(
    store: &Path,
    row: &IndexRow,
    object: &ImageObjects,
    sizes: &BTreeMap<PathBuf, u64>,
    references: &BTreeMap<&PathBuf, usize>,
) -> CachedImage {
    let images = store.join("images");
    let mut bytes = ImageBytes::default();
    for path in object.all() {
        let size = sizes.get(path).copied().unwrap_or_default();
        bytes.total += size;
        if references.get(path).copied().unwrap_or(1) == 1 {
            bytes.exclusive += size;
        }
        if path.starts_with(images.join("layers")) {
            bytes.layers += size;
        } else if path.starts_with(images.join("extracted")) {
            bytes.extracted += size;
        } else if path.starts_with(images.join("disk-images")) {
            bytes.disk_image += size;
        } else if path.starts_with(images.join("manifests"))
            || path.starts_with(images.join("configs"))
        {
            bytes.metadata += size;
        } else {
            bytes.base_disk += size;
        }
    }

    let digest = image_digest(&row.layers);
    let disk = images
        .join("disk-images")
        .join(format!("{}.ext4", blob_name(&digest)));
    CachedImage {
        reference: row.reference.clone(),
        manifest_digest: row.manifest_digest.clone(),
        config_digest: row.config_digest.clone(),
        image_digest: digest,
        cached_at: row.cached_at.clone(),
        pull_complete: row.complete,
        layers: row.unique_layers().len(),
        bytes,
        objects: Objects {
            expected: object.pulled.len(),
            present: object.pulled.iter().filter(|path| path.exists()).count(),
        },
        disk_image_built: disk.exists(),
        base_disk_built: object.bases > 0,
    }
}

/// Digest-keyed objects that no indexed image claims.
///
/// One entry per top-level object rather than a single total, because "146 MB of
/// extracted layer nobody references" is actionable and "146 MB unaccounted" is
/// not. The live store had exactly one — an extracted tree whose layer tarball
/// and index row are both gone.
///
/// `claimed` must already carry the [`preserved`] paths as well as the objects
/// every indexed image references: a snapshot's base disk is claimed by nobody
/// and is not therefore spare.
fn unattributed(store: &Path, claimed: &BTreeSet<PathBuf>) -> Vec<Unattributed> {
    let images = store.join("images");
    let mut scanned: Vec<PathBuf> = IMAGE_DIRS.iter().map(|dir| images.join(dir)).collect();
    scanned.push(store.join("bases"));
    let mut found = Vec::new();
    for dir in scanned {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if claimed.contains(&path) {
                continue;
            }
            found.push(Unattributed {
                path: path
                    .strip_prefix(store)
                    .unwrap_or(&path)
                    .display()
                    .to_string(),
                bytes: size_of(&path),
            });
        }
    }
    found.sort_by_key(|entry| std::cmp::Reverse(entry.bytes));
    found
}

/// State under the store root that no pinned digest re-obtains.
///
/// A `rootfs` base is rebuilt from its image; a `snapshot` or a `clone_base` is
/// the state of a box somebody ran. ADR-0014 v1.6's permission does not extend to
/// it, so it is listed rather than cleared.
fn preserved(index: &Index) -> Vec<Preserved> {
    index
        .bases
        .iter()
        .filter(|base| base.kind != "rootfs")
        .map(|base| Preserved {
            path: base.path.display().to_string(),
            reason: format!(
                "a `{}` base disk is the state of a box that ran, which no digest re-obtains",
                base.kind
            ),
        })
        .collect()
}

/// The paths [`preserved`] names, as a set.
///
/// Folded into `claimed` before [`unattributed`] runs, so that state a digest
/// does not re-obtain is never reported as spare and never reaches the doomed
/// set. Nobody references a snapshot's base disk, which is exactly why an
/// unclaimed-means-spare rule would delete it.
fn preserved_paths(index: &Index) -> BTreeSet<PathBuf> {
    index
        .bases
        .iter()
        .filter(|base| base.kind != "rootfs")
        .map(|base| base.path.clone())
        .collect()
}

// ---------------------------------------------------------------------------
// clear
// ---------------------------------------------------------------------------

/// Split the index into the rows a scope drops and the rows it keeps.
///
/// Returns them in that order — doomed first — because that is the order the two
/// are used in, and a pair whose halves can be swapped by a careless edit is a
/// set difference computed the wrong way round.
fn select<'a>(
    index: &'a Index,
    scope: &Scope,
) -> Result<(Vec<&'a IndexRow>, Vec<&'a IndexRow>), StoreError> {
    match scope {
        Scope::Everything => Ok((index.images.iter().collect(), Vec::new())),
        Scope::Image(reference) => {
            if !index.images.iter().any(|row| &row.reference == reference) {
                return Err(StoreError::UnknownImage {
                    reference: reference.clone(),
                    known: index
                        .images
                        .iter()
                        .map(|row| row.reference.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                });
            }
            Ok(index
                .images
                .iter()
                .partition(|row| &row.reference == reference))
        }
    }
}

/// Every path a set of index rows references, including the index manifests they
/// resolve through.
///
/// The other half of the set difference, and the reason it is a named function:
/// applied to the **surviving** rows it is the retained set, and anything not in
/// it is what may go. Computing it from the doomed rows instead is the walk this
/// module's documentation says a deletion must never be.
fn paths_for(store: &Path, rows: &[&IndexRow], bases: &[BaseRow]) -> BTreeSet<PathBuf> {
    rows.iter()
        .flat_map(|row| {
            objects_for(store, row, bases)
                .all()
                .cloned()
                .collect::<Vec<_>>()
        })
        .chain(index_manifests_for(
            store,
            &rows.iter().map(|row| row.manifest_digest.clone()).collect(),
        ))
        .collect()
}

/// What a [`clear`] would remove, without removing it.
///
/// Separate from [`clear`] so the set difference has one implementation and the
/// tests can assert on the doomed set directly rather than inferring it from what
/// survived a deletion.
///
/// # Errors
///
/// [`StoreError::UnknownImage`] when a per-image scope names a reference the index
/// does not hold, [`StoreError::LiveBoxes`] when a box is registered,
/// [`StoreError::UnrecognisedEntry`] when the store root holds something this
/// module cannot classify, [`StoreError::BaseOutsideStore`] when a base-disk row
/// points outside the store, and [`StoreError::Index`] when the index cannot be
/// read.
pub fn plan(asset_root: &Path, scope: &Scope) -> Result<(ClearReport, Vec<PathBuf>), StoreError> {
    let store = store_root(asset_root);
    let mut report = ClearReport {
        schema: SANDBOX_CLEAR_SCHEMA,
        scope: "machine",
        store: store.display().to_string(),
        requested: scope.as_str().to_owned(),
        applied: false,
        removed: Vec::new(),
        removed_unattributed: Vec::new(),
        freed_bytes: 0,
        store_bytes_before: 0,
        store_bytes_after: 0,
        retained: Vec::new(),
        preserved: Vec::new(),
    };
    if !store.is_dir() {
        return Ok((report, Vec::new()));
    }

    let index = read_index(&store)?;
    if index.boxes > 0 {
        return Err(StoreError::LiveBoxes { boxes: index.boxes });
    }
    guard_entries(&store)?;
    guard_bases(&store, &index)?;

    let (doomed_rows, surviving_rows) = select(&index, scope)?;
    let retained = paths_for(&store, &surviving_rows, &index.bases);

    report.store_bytes_before = size_of(&store);
    report.preserved = preserved(&index);

    let mut doomed: Vec<PathBuf> = Vec::new();
    for row in &doomed_rows {
        let objects = objects_for(&store, row, &index.bases);
        let mine: Vec<PathBuf> = objects
            .all()
            .filter(|path| !retained.contains(*path))
            .filter(|path| path.exists())
            .cloned()
            .collect();
        let freed = mine.iter().map(|path| size_of(path)).sum();
        report.removed.push(RemovedImage {
            reference: row.reference.clone(),
            freed_bytes: freed,
            objects_removed: mine.len(),
        });
        report.freed_bytes += freed;
        doomed.extend(mine);
    }

    // The index manifest a doomed image resolved through goes with it, unless a
    // survivor resolves through the same one.
    for path in index_manifests_for(
        &store,
        &doomed_rows
            .iter()
            .map(|row| row.manifest_digest.clone())
            .collect(),
    ) {
        if !retained.contains(&path) && path.exists() {
            report.freed_bytes += size_of(&path);
            doomed.push(path);
        }
    }

    // Unattributed bytes are only ever in scope for `everything`. A per-image
    // request has no evidence they belong to the image it named, and this module
    // does not delete on a guess.
    if matches!(scope, Scope::Everything) {
        let claimed: BTreeSet<PathBuf> = doomed
            .iter()
            .cloned()
            .chain(preserved_paths(&index))
            .collect();
        report.removed_unattributed = unattributed(&store, &claimed);
        for entry in &report.removed_unattributed {
            report.freed_bytes += entry.bytes;
            doomed.push(store.join(&entry.path));
        }
    }

    doomed.sort();
    doomed.dedup();
    report.store_bytes_after = report.store_bytes_before;
    Ok((report, doomed))
}

/// Remove what [`plan`] named, then re-check every surviving image against the
/// filesystem.
///
/// # Errors
///
/// Everything [`plan`] can return, plus [`StoreError::Io`] if a removal or the
/// index update fails.
pub fn clear(asset_root: &Path, scope: &Scope) -> Result<ClearReport, StoreError> {
    let store = store_root(asset_root);
    let (mut report, doomed) = plan(asset_root, scope)?;
    if !store.is_dir() {
        report.applied = true;
        return Ok(report);
    }

    for path in &doomed {
        remove(path)?;
    }
    // The rows go with the blobs. Leaving them behind makes `status` report images
    // whose bytes are gone, which is issue #433's fourth trap and reads as a
    // corrupt store rather than a cleared one.
    forget(&store, &report.removed)?;

    report.applied = true;
    report.store_bytes_after = size_of(&store);
    report.retained = verify(&store)?;
    Ok(report)
}

/// Delete a file or a directory tree.
fn remove(path: &Path) -> Result<(), StoreError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(StoreError::Io {
                action: "inspecting",
                path: path.display().to_string(),
                message: error.to_string(),
            });
        }
    };
    let outcome = if metadata.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    outcome.map_err(|error| StoreError::Io {
        action: "removing",
        path: path.display().to_string(),
        message: error.to_string(),
    })
}

/// Drop the index rows for what was removed.
///
/// Two different rules, because the two tables are keyed differently. An
/// `image_index` row is named by the request, so it goes by reference. A
/// `base_disk` row is named by nothing the request knows, so it goes **by whether
/// its file is still there** — which means the set difference that spared a base
/// disk a surviving image shares also spares its row, with no second chance to get
/// the difference wrong.
fn forget(store: &Path, removed: &[RemovedImage]) -> Result<(), StoreError> {
    let path = store.join(INDEX_DB);
    if !path.exists() {
        return Ok(());
    }
    let fail = |message: String| StoreError::Index {
        path: path.display().to_string(),
        message,
    };
    let db = rusqlite::Connection::open(&path).map_err(|error| fail(error.to_string()))?;
    // `IMMEDIATE` takes the write lock up front, so a concurrent `boxlite` pull
    // fails to start rather than interleaving with this. It is not the `flock`
    // that crate holds — see `StoreError::LiveBoxes` for why this crate cannot
    // take that one — and it is the strongest guard available without `unsafe`.
    db.execute_batch("BEGIN IMMEDIATE")
        .map_err(|error| fail(error.to_string()))?;
    for image in removed {
        db.execute(
            "DELETE FROM image_index WHERE reference = ?1",
            [&image.reference],
        )
        .map_err(|error| fail(error.to_string()))?;
    }
    let orphaned: Vec<String> = {
        let mut statement = db
            .prepare("SELECT base_path FROM base_disk WHERE kind = 'rootfs'")
            .map_err(|error| fail(error.to_string()))?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| fail(error.to_string()))?;
        rows.filter_map(Result::ok)
            .filter(|base_path| !Path::new(base_path).exists())
            .collect()
    };
    for base_path in &orphaned {
        db.execute(
            "DELETE FROM base_disk WHERE kind = 'rootfs' AND base_path = ?1",
            [base_path],
        )
        .map_err(|error| fail(error.to_string()))?;
    }
    db.execute_batch("COMMIT")
        .map_err(|error| fail(error.to_string()))?;
    Ok(())
}

/// Re-resolve every image the index still holds against the filesystem.
fn verify(store: &Path) -> Result<Vec<VerifiedImage>, StoreError> {
    let index = read_index(store)?;
    Ok(index
        .images
        .iter()
        .map(|row| {
            let objects = objects_for(store, row, &index.bases);
            let tally = Objects {
                expected: objects.pulled.len(),
                present: objects.pulled.iter().filter(|path| path.exists()).count(),
            };
            VerifiedImage {
                reference: row.reference.clone(),
                objects: tally,
                complete: tally.complete(),
            }
        })
        .collect())
}

/// Refuse if the store root holds something this module cannot classify.
fn guard_entries(store: &Path) -> Result<(), StoreError> {
    let entries = std::fs::read_dir(store).map_err(|error| StoreError::Io {
        action: "reading",
        path: store.display().to_string(),
        message: error.to_string(),
    })?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !KNOWN_ENTRIES.contains(&name.as_str()) {
            return Err(StoreError::UnrecognisedEntry { entry: name });
        }
    }
    let images = store.join("images");
    if !images.is_dir() {
        return Ok(());
    }
    let entries = std::fs::read_dir(&images).map_err(|error| StoreError::Io {
        action: "reading",
        path: images.display().to_string(),
        message: error.to_string(),
    })?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !IMAGE_DIRS.contains(&name.as_str()) {
            return Err(StoreError::UnrecognisedEntry {
                entry: format!("images/{name}"),
            });
        }
    }
    Ok(())
}

/// Refuse if any base-disk row points outside the store root.
fn guard_bases(store: &Path, index: &Index) -> Result<(), StoreError> {
    for base in &index.bases {
        if !base.path.starts_with(store) {
            return Err(StoreError::BaseOutsideStore {
                path: base.path.display().to_string(),
                root: store.display().to_string(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        Attribution, IMAGE_DIRS, INDEX_DB, SANDBOX_STORE_DIR, Scope, StoreError, blob_name, clear,
        image_digest, plan, status,
    };
    use std::path::PathBuf;

    /// The three `boxlite` 0.9.7 tables this module reads, declared as that crate
    /// declares them.
    ///
    /// A restatement, and deliberately a literal one: issue #433's hand-clearing
    /// notes describe an `images` table and a `disk-images/` directory, and the
    /// live schema is `image_index` / `base_disk` / `base_disk_ref` / `snapshot`.
    /// A procedure written against a remembered schema goes stale silently, so
    /// what the fixture builds is what was read off the store rather than what
    /// anyone recalled about it.
    const SCHEMA: &str = "
        CREATE TABLE image_index (
            reference TEXT PRIMARY KEY NOT NULL,
            manifest_digest TEXT NOT NULL,
            config_digest TEXT NOT NULL,
            layers TEXT NOT NULL,
            cached_at TEXT NOT NULL,
            complete INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE base_disk (
            id TEXT PRIMARY KEY NOT NULL,
            source_box_id TEXT NOT NULL,
            name TEXT,
            kind TEXT NOT NULL CHECK(kind IN ('snapshot', 'clone_base', 'rootfs')),
            base_path TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            json TEXT NOT NULL,
            UNIQUE(source_box_id, name)
        );
        CREATE TABLE box_config (
            id TEXT PRIMARY KEY NOT NULL,
            name TEXT UNIQUE,
            created_at INTEGER NOT NULL,
            json TEXT NOT NULL
        );
    ";

    /// A plausible digest for a seed word, so fixtures read like the real store.
    fn digest(seed: &str) -> String {
        format!("sha256:{}", crate::sha256_hex(seed.as_bytes()))
    }

    /// An asset root holding a sandbox store, built object by object.
    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir()
                .join(format!("rto-exec-sandbox-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let store = root.join(SANDBOX_STORE_DIR);
            for dir in IMAGE_DIRS {
                std::fs::create_dir_all(store.join("images").join(dir)).expect("image dir");
            }
            std::fs::create_dir_all(store.join("bases")).expect("bases dir");
            std::fs::create_dir_all(store.join("db")).expect("db dir");
            std::fs::write(store.join(".lock"), []).expect("lock file");
            let db = rusqlite::Connection::open(store.join(INDEX_DB)).expect("open index");
            db.execute_batch(SCHEMA).expect("index schema");
            Self { root }
        }

        fn store(&self) -> PathBuf {
            self.root.join(SANDBOX_STORE_DIR)
        }

        fn db(&self) -> rusqlite::Connection {
            rusqlite::Connection::open(self.store().join(INDEX_DB)).expect("open index")
        }

        fn write(&self, relative: &str, bytes: usize) -> PathBuf {
            let path = self.store().join(relative);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("parent dir");
            std::fs::write(&path, vec![b'x'; bytes]).expect("write object");
            path
        }

        /// Add an image: its index row, its pulled objects, its extracted trees
        /// and the disk image derived from its layer list.
        fn image(&self, reference: &str, layers: &[&str], layer_bytes: usize) {
            let manifest = digest(&format!("{reference} manifest"));
            let config = digest(&format!("{reference} config"));
            let digests: Vec<String> = layers.iter().map(|seed| digest(seed)).collect();
            self.db()
                .execute(
                    "INSERT INTO image_index
                     (reference, manifest_digest, config_digest, layers, cached_at, complete)
                     VALUES (?1, ?2, ?3, ?4, ?5, 1)",
                    rusqlite::params![
                        reference,
                        manifest,
                        config,
                        serde_json::to_string(&digests).expect("layer list"),
                        "2026-08-19T00:00:00+00:00",
                    ],
                )
                .expect("insert image row");
            self.write(
                &format!("images/manifests/{}.json", blob_name(&manifest)),
                64,
            );
            self.write(&format!("images/configs/{}.json", blob_name(&config)), 32);
            for layer in &digests {
                self.write(
                    &format!("images/layers/{}.tar.gz", blob_name(layer)),
                    layer_bytes,
                );
                self.write(
                    &format!("images/extracted/{}/rootfs", blob_name(layer)),
                    layer_bytes,
                );
            }
            self.write(
                &format!(
                    "images/disk-images/{}.ext4",
                    blob_name(&image_digest(&digests))
                ),
                layer_bytes * 4,
            );
        }

        /// The layer tarball a seed word resolves to.
        fn layer(&self, seed: &str) -> PathBuf {
            self.store()
                .join("images/layers")
                .join(format!("{}.tar.gz", blob_name(&digest(seed))))
        }

        /// Add a `base_disk` row and the file it points at.
        fn base(&self, id: &str, kind: &str, name: &str) -> PathBuf {
            let path = self.write(&format!("bases/{id}.ext4"), 512);
            self.db()
                .execute(
                    "INSERT INTO base_disk
                     (id, source_box_id, name, kind, base_path, created_at, json)
                     VALUES (?1, '__global__', ?2, ?3, ?4, 0, '{}')",
                    rusqlite::params![id, name, kind, path.display().to_string()],
                )
                .expect("insert base row");
            path
        }
    }

    /// The disk-image filename is **derived**, and the vector is the live store.
    ///
    /// `boxlite` keys `images/disk-images/*.ext4` by the SHA-256 of the layer
    /// digest strings concatenated in manifest order. Issue #433's hand-clearing
    /// notes record that no such association existed and fell back to **mtime**,
    /// with the instruction that a shipped `clear` must not. This is the
    /// assertion that it does not have to: both vectors are read off the store
    /// measured for that issue, and `cimg/rust` is the one that names a digest
    /// twice — so a de-duplicating or re-ordering derivation fails here rather
    /// than in a wrong deletion.
    #[test]
    fn the_disk_image_filename_is_derived_from_the_layer_list() {
        let debian = [
            "sha256:0f5d7465a5bb9d419f60c93d126a161286c73a1ede4a8b2e46bd5e7ad5782cc7".to_owned(),
        ];
        assert_eq!(
            image_digest(&debian),
            "sha256:2674b856eab71e6d70f5d8ad573394d1b90f40da02593e3af3a31c17b8de1d97",
            "the derivation no longer reproduces the disk image the live store holds for \
             docker.io/library/debian:bookworm-slim"
        );

        let cimg: Vec<String> = [
            "c36472b3458398be28ecbfebbaac44143c040eae73411baded48a22060d3055b",
            "fee4d731b9208f65a65b57345c4945de0d8eccf9a9f8729e796be8911bd3131c",
            "fec6be0b4b4a6668684b8cc97d59c44998ed49004a5e18954caa3f58986549a6",
            "943b99e461484cf70776207df97a17783fc424cfeafd5e1bdffab309f42fe84f",
            "4dc664574997cda0756a223b9e39e9c4cac313e72dbd59adef0fa723ac8ffc5f",
            "cd344ce4edc31b84f799cfd1ff435b61345534535f474fcb8a7f2e9d2ddb209d",
            "ab2260fc0eee2ac435fe045b63e2fc28c19cf56cb707101e8eb77601bcf7cdb3",
            "4f4fb700ef54461cfa02571ae0db9a0dc1e0cdb5577484a6d75e68dc38e8acc1",
            "5b96641132bf37840e483d28ac60942c3b7b26c2382322c8fa94e62e83b86523",
            "4f4fb700ef54461cfa02571ae0db9a0dc1e0cdb5577484a6d75e68dc38e8acc1",
        ]
        .iter()
        .map(|hex| format!("sha256:{hex}"))
        .collect();
        assert_eq!(
            image_digest(&cimg),
            "sha256:b038ce43bcc84d230823ec62558e9a5926f9057206e3eb5ec704da92677fcc0d",
            "the derivation no longer reproduces the disk image the live store holds for \
             docker.io/cimg/rust, whose layer list names one digest twice"
        );
    }

    /// The index writes `sha256:`; the filesystem writes `sha256-`.
    ///
    /// One character, and it is why issue #433's first hand-clearing pass matched
    /// nothing at all.
    #[test]
    fn the_on_disk_name_is_not_the_index_digest() {
        assert_eq!(blob_name("sha256:abc123"), "sha256-abc123");
    }

    /// Dropping one image must not take a layer another image still references.
    ///
    /// The correctness bug this module exists to prevent, and it does not present
    /// as an error: it presents as a **surviving image whose blobs are gone**,
    /// discovered on some later run. So the assertion is made twice — the shared
    /// layer is absent from the doomed set, and the survivor is verified blob by
    /// blob after the deletion actually happened.
    #[test]
    fn a_layer_two_images_share_survives_dropping_one_of_them() {
        let fixture = Fixture::new("shared-layer");
        fixture.image("registry/a:1", &["common", "only-a"], 4096);
        fixture.image("registry/b:1", &["common", "only-b"], 4096);

        let scope = Scope::Image("registry/a:1".to_owned());
        let (_, doomed) = plan(&fixture.root, &scope).expect("plan");
        assert!(
            !doomed.contains(&fixture.layer("common")),
            "a layer `registry/b:1` still references was put in the doomed set"
        );
        assert!(
            doomed.contains(&fixture.layer("only-a")),
            "the layer only the dropped image references was not in the doomed set"
        );

        let report = clear(&fixture.root, &scope).expect("clear");
        assert!(
            fixture.layer("common").exists(),
            "the shared layer was deleted, so `registry/b:1` is now broken"
        );
        assert!(!fixture.layer("only-a").exists());
        assert_eq!(report.retained.len(), 1);
        assert!(
            report.survivors_intact(),
            "a surviving image is incomplete after the clear: {:?}",
            report.retained
        );
        assert_eq!(report.retained[0].reference, "registry/b:1");
        assert_eq!(report.retained[0].objects.expected, 4);
        assert_eq!(report.retained[0].objects.present, 4);
    }

    /// A shared blob is reported as shared, rather than promised back.
    ///
    /// `total` is what the image references and `exclusive` is what dropping it
    /// alone would free. Reporting only the total would offer bytes that a
    /// surviving image keeps, which is a `clear` whose number nobody can check.
    #[test]
    fn the_status_row_separates_what_an_image_references_from_what_it_would_free() {
        let fixture = Fixture::new("exclusive-bytes");
        fixture.image("registry/a:1", &["common", "only-a"], 4096);
        fixture.image("registry/b:1", &["common", "only-b"], 4096);

        let report = status(&fixture.root).expect("status");
        let image = report
            .images
            .iter()
            .find(|image| image.reference == "registry/a:1")
            .expect("the image is listed");
        assert!(
            image.bytes.exclusive < image.bytes.total,
            "a shared layer was counted as reclaimable: {:?}",
            image.bytes
        );
        assert_eq!(image.objects.expected, 4, "manifest, config and two layers");
        assert_eq!(image.objects.present, 4);
    }

    /// `everything` empties the store and the index together, and the two byte
    /// figures agree to within the index itself.
    ///
    /// The rows go with the blobs. Leaving a row behind reports an image whose
    /// bytes are gone, which reads as a corrupt store rather than a cleared one —
    /// issue #433's fourth trap.
    ///
    /// It also holds [`super::ClearReport::measured_freed_bytes`]'s claim: the
    /// only thing here that can change size without having been removed is the
    /// `SQLite` index, which frees pages inside its file rather than shrinking it.
    #[test]
    fn the_accounted_bytes_and_the_measured_bytes_differ_only_by_the_index() {
        let fixture = Fixture::new("everything");
        fixture.image("registry/a:1", &["common", "only-a"], 4096);
        fixture.image("registry/b:1", &["common", "only-b"], 4096);

        let before = status(&fixture.root).expect("status").total_bytes;
        let report = clear(&fixture.root, &Scope::Everything).expect("clear");
        assert_eq!(report.removed.len(), 2);
        assert!(report.retained.is_empty());
        assert_eq!(report.store_bytes_before, before);
        assert!(
            report.store_bytes_after < report.store_bytes_before,
            "the store did not shrink"
        );
        let index = super::size_of(&fixture.store().join("db"));
        assert!(
            report.freed_bytes >= report.measured_freed_bytes(),
            "the store shrank by more than the objects that were removed"
        );
        assert!(
            report.freed_bytes - report.measured_freed_bytes() <= index,
            "the accounted bytes ({}) and the bytes the filesystem gave back ({}) differ by \
             more than the index ({index}), which is the only thing in the store that can \
             change size without having been removed",
            report.freed_bytes,
            report.measured_freed_bytes()
        );

        let after = status(&fixture.root).expect("status");
        assert!(
            after.images.is_empty(),
            "the index still reports images whose blobs are gone: {:?}",
            after.images
        );
    }

    /// An index manifest a survivor resolves through is kept.
    ///
    /// A tag reference — `debian:bookworm-slim` in the live store — records only
    /// the platform manifest it resolved to, never the index digest above it. A
    /// rule of "delete every manifest file no row names" would therefore take a
    /// surviving image's index with it, so the association is read out of the
    /// files themselves.
    #[test]
    fn an_index_manifest_a_survivor_resolves_through_is_kept() {
        let fixture = Fixture::new("index-manifest");
        fixture.image("registry/a:1", &["only-a"], 4096);
        fixture.image("registry/b:1", &["only-b"], 4096);

        let survivor = digest("registry/b:1 manifest");
        let dropped = digest("registry/a:1 manifest");
        let keep = fixture.write("images/manifests/sha256-keep.json", 0);
        std::fs::write(
            &keep,
            serde_json::json!({ "manifests": [{ "digest": survivor }] }).to_string(),
        )
        .expect("write index manifest");
        let go = fixture.write("images/manifests/sha256-go.json", 0);
        std::fs::write(
            &go,
            serde_json::json!({ "manifests": [{ "digest": dropped }] }).to_string(),
        )
        .expect("write index manifest");

        clear(&fixture.root, &Scope::Image("registry/a:1".to_owned())).expect("clear");
        assert!(
            keep.exists(),
            "the index manifest `registry/b:1` resolves through was deleted"
        );
        assert!(!go.exists(), "the dropped image's index manifest was kept");
    }

    /// Something under the store root this module cannot classify stops the clear.
    ///
    /// ADR-0014 v1.6's limit, enforced rather than trusted: the verb may drop what
    /// a pinned digest re-obtains and nothing else, and turning up in a cache is
    /// not evidence of being re-obtainable. Nothing else is cleared either — a
    /// partial clear alongside a refusal is the worst of both.
    #[test]
    fn an_unrecognised_entry_stops_the_clear_without_removing_anything() {
        let fixture = Fixture::new("unrecognised");
        fixture.image("registry/a:1", &["only-a"], 4096);
        std::fs::create_dir_all(fixture.store().join("provenance")).expect("mystery dir");

        let error = clear(&fixture.root, &Scope::Everything).expect_err("a refusal");
        assert!(
            matches!(&error, StoreError::UnrecognisedEntry { entry } if entry == "provenance"),
            "expected the unrecognised entry to be named, got: {error}"
        );
        assert!(
            fixture.layer("only-a").exists(),
            "the refusal removed objects on its way out"
        );
    }

    /// A `base_disk` row cannot aim a deletion outside the store root.
    ///
    /// `base_path` is an absolute path recorded when the base was built, so it is
    /// data rather than a derivation — and data can name anywhere, including the
    /// repository store this verb must never reach.
    #[test]
    fn a_base_disk_row_pointing_outside_the_store_is_refused() {
        let fixture = Fixture::new("escape");
        fixture.image("registry/a:1", &["only-a"], 4096);
        let outside = fixture.root.join("graph.db");
        std::fs::write(&outside, b"not re-obtainable").expect("write");
        fixture
            .db()
            .execute(
                "INSERT INTO base_disk
                 (id, source_box_id, name, kind, base_path, created_at, json)
                 VALUES ('esc', '__global__', 'escapee', 'rootfs', ?1, 0, '{}')",
                rusqlite::params![outside.display().to_string()],
            )
            .expect("insert base row");

        let error = clear(&fixture.root, &Scope::Everything).expect_err("a refusal");
        assert!(
            matches!(&error, StoreError::BaseOutsideStore { path, .. } if path.contains("graph.db")),
            "expected the escaping path to be named, got: {error}"
        );
        assert!(outside.exists(), "the clear reached outside the store root");
    }

    /// A registered box blocks the clear.
    ///
    /// `boxlite` holds an exclusive `flock` on `<store>/.lock` for a runtime's
    /// lifetime, which this crate cannot take back — `unsafe_code = "forbid"`
    /// rules out the call that acquires it. So the guard is the evidence such a
    /// lock would have protected, and it is checked rather than assumed absent.
    #[test]
    fn a_registered_box_blocks_the_clear() {
        let fixture = Fixture::new("live-box");
        fixture.image("registry/a:1", &["only-a"], 4096);
        fixture
            .db()
            .execute(
                "INSERT INTO box_config (id, name, created_at, json)
                 VALUES ('box1', 'running', 0, '{}')",
                [],
            )
            .expect("insert box row");

        let error = clear(&fixture.root, &Scope::Everything).expect_err("a refusal");
        assert!(
            matches!(error, StoreError::LiveBoxes { boxes: 1 }),
            "expected a live-box refusal, got: {error}"
        );
        assert!(fixture.layer("only-a").exists());
    }

    /// A snapshot base disk is preserved, named, and never counted as spare.
    ///
    /// A `rootfs` base is rebuilt from its image; a `snapshot` is the state of a
    /// box somebody ran, and no digest re-obtains it. It is referenced by no
    /// image, which is exactly why an unclaimed-means-spare rule would delete it.
    #[test]
    fn a_snapshot_base_disk_is_preserved_rather_than_treated_as_spare() {
        let fixture = Fixture::new("snapshot");
        fixture.image("registry/a:1", &["only-a"], 4096);
        let snapshot = fixture.base("snap1", "snapshot", "a-snapshot");

        let before = status(&fixture.root).expect("status");
        assert!(
            !before
                .unattributed
                .iter()
                .any(|entry| snapshot.ends_with(&entry.path)),
            "a snapshot base disk was reported as unattributed bytes"
        );
        assert_eq!(before.preserved.len(), 1);

        let report = clear(&fixture.root, &Scope::Everything).expect("clear");
        assert!(
            snapshot.exists(),
            "the clear removed a snapshot, which no digest re-obtains"
        );
        assert_eq!(report.preserved.len(), 1);
    }

    /// A per-image request that names nothing in the store says what is in it.
    ///
    /// The likely cause is a tag typed where the index holds a digest reference,
    /// so the listing is the way forward rather than a courtesy.
    #[test]
    fn an_unknown_reference_names_what_the_store_is_holding() {
        let fixture = Fixture::new("unknown-image");
        fixture.image("registry/a:1", &["only-a"], 4096);

        let error =
            clear(&fixture.root, &Scope::Image("registry/a:2".to_owned())).expect_err("a refusal");
        assert!(
            matches!(&error, StoreError::UnknownImage { known, .. } if known == "registry/a:1"),
            "expected the cached references to be named, got: {error}"
        );
    }

    /// Bytes no index row claims survive a per-image clear and go with everything.
    ///
    /// The live store had one: a 146 MB extracted layer tree whose tarball and
    /// index row are both gone. A per-image request has no evidence it belongs to
    /// the image it named, so it is reported rather than deleted on a guess.
    #[test]
    fn unattributed_bytes_survive_a_per_image_clear_and_go_with_everything() {
        let fixture = Fixture::new("unattributed");
        fixture.image("registry/a:1", &["only-a"], 4096);
        fixture.image("registry/b:1", &["only-b"], 4096);
        let orphan = fixture.write("images/extracted/sha256-orphan/rootfs", 8192);

        let before = status(&fixture.root).expect("status");
        assert_eq!(before.attribution, Attribution::Partial);
        assert_eq!(before.unattributed.len(), 1);
        assert_eq!(before.unattributed[0].bytes, 8192);

        clear(&fixture.root, &Scope::Image("registry/a:1".to_owned())).expect("clear");
        assert!(
            orphan.exists(),
            "a per-image clear deleted bytes it could not attribute to that image"
        );

        let report = clear(&fixture.root, &Scope::Everything).expect("clear");
        assert!(
            !orphan.exists(),
            "`everything` left unattributed bytes behind"
        );
        assert_eq!(report.removed_unattributed.len(), 1);
    }

    /// An absent store is an answer, not a failure.
    #[test]
    fn an_absent_store_is_reported_rather_than_failing() {
        let root =
            std::env::temp_dir().join(format!("rto-exec-sandbox-absent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let report = status(&root).expect("status");
        assert!(!report.present);
        assert!(report.images.is_empty());
        assert_eq!(report.total_bytes, 0);

        let cleared = clear(&root, &Scope::Everything).expect("clear");
        assert_eq!(cleared.freed_bytes, 0);
        assert!(cleared.applied);
    }

    /// This module and `boxlite.rs` name the same directory.
    ///
    /// They cannot share the constant: `boxlite.rs` is behind `exec-boxlite`, and
    /// making that feature a condition of *clearing* a store some earlier build
    /// filled is the shape of the bootstrap problem that moved provisioning off
    /// the backend features in the first place. So the two literals are checked
    /// against each other instead, by reading the source — the only way to compare
    /// a constant with a `join` argument.
    #[test]
    fn the_store_directory_is_the_one_boxlite_is_pointed_at() {
        let source = include_str!("boxlite.rs");
        let marker = "home_dir: assets_root.join(\"";
        let named: Vec<&str> = source
            .match_indices(marker)
            .map(|(at, _)| {
                source[at + marker.len()..]
                    .split_once('"')
                    .expect("a join argument is closed on the same line")
                    .0
            })
            .collect();
        assert!(
            !named.is_empty(),
            "no `home_dir: assets_root.join(..)` was found to check against"
        );
        for directory in named {
            assert_eq!(
                directory, SANDBOX_STORE_DIR,
                "boxlite.rs points a runtime at `{directory}` and this module clears \
                 `{SANDBOX_STORE_DIR}`"
            );
        }
    }

    /// The document says whose store it is describing.
    ///
    /// One sandbox store per asset root, shared by every repository on the host —
    /// the same hazard `MachineScope` carries a `scope` field for, and the same
    /// remedy: a caller who selected a project must not read a machine-global
    /// figure as that project's.
    #[test]
    fn the_status_document_labels_its_scope_as_the_machine() {
        let fixture = Fixture::new("scope");
        fixture.image("registry/a:1", &["only-a"], 16);
        let document =
            serde_json::to_value(status(&fixture.root).expect("status")).expect("serialise");
        assert_eq!(document["scope"], "machine");
        assert_eq!(document["schema"], super::SANDBOX_STATUS_SCHEMA);
        assert!(
            document["store"]
                .as_str()
                .expect("a store path")
                .ends_with(SANDBOX_STORE_DIR)
        );
    }
}
