//! Auto-detect the hub version a spoke deploys (ADR-0009 step 8b): read the
//! spoke's derived deploy-artifact nodes (`submodule` / `image_ref`, from the
//! Dockerfile / gitlink extractors) and match one to the hub, yielding the git rev
//! to resolve that spoke against. Submodules win — a sha resolves unambiguously; an
//! image tag is tried as a hub git ref (`<tag>`, then `v<tag>`), the common
//! release-tag == image-tag convention, needing no config.

use rto_graph::{NodeKind, Repo, Store};

/// A detected pin: the hub git rev the spoke deploys, and where it came from.
pub struct SpokePin {
    /// The hub git rev (a submodule sha, or an image tag resolved to a ref).
    pub rev: String,
    /// Human description of the pin source (e.g. `submodule vendor/app`).
    pub via: String,
}

/// Detect which hub version `spoke_store` pins. Matches the spoke's `submodule`
/// nodes (by URL → the hub's origin, or repo basename → hub name) and `image_ref`
/// nodes (by image basename → hub name, then tag → a hub git ref) to the hub.
/// Returns `None` when nothing pins the hub (the caller resolves against `HEAD`).
///
/// # Errors
/// Propagates store or git errors.
pub fn detect(
    spoke_store: &Store,
    hub_name: &str,
    hub_origin: Option<&str>,
    hub_repo: &Repo,
) -> anyhow::Result<Option<SpokePin>> {
    // A submodule pinning the hub → its exact commit sha (unambiguous).
    for n in spoke_store.nodes_by_kind(&NodeKind::Other("submodule".into()))? {
        let (Some(url), Some(sha)) = (meta_str(&n, "url"), meta_str(&n, "sha")) else {
            continue;
        };
        if url_matches_hub(url, hub_name, hub_origin) {
            let path = meta_str(&n, "path").unwrap_or("?");
            return Ok(Some(SpokePin {
                rev: sha.to_owned(),
                via: format!("submodule {path}"),
            }));
        }
    }
    // An image whose name matches the hub → resolve its tag as a hub git ref.
    for n in spoke_store.nodes_by_kind(&NodeKind::Other("image_ref".into()))? {
        let (Some(image), Some(tag)) = (meta_str(&n, "image"), meta_str(&n, "tag")) else {
            continue;
        };
        if image_basename(image) == hub_name {
            for candidate in [tag.to_owned(), format!("v{tag}")] {
                if hub_repo.blobs_at(&candidate).is_ok() {
                    return Ok(Some(SpokePin {
                        rev: candidate,
                        via: format!("image {image}:{tag}"),
                    }));
                }
            }
        }
    }
    Ok(None)
}

/// A node's `meta.<key>` as a string, if present.
fn meta_str<'a>(node: &'a rto_graph::Node, key: &str) -> Option<&'a str> {
    node.meta.get(key).and_then(serde_json::Value::as_str)
}

/// Whether a submodule URL points at the hub: its normalised form equals the hub's
/// origin, or its repo basename equals the hub project name (so a local test repo
/// with no remote still matches by directory name).
fn url_matches_hub(url: &str, hub_name: &str, hub_origin: Option<&str>) -> bool {
    hub_origin.is_some_and(|o| norm_url(o) == norm_url(url)) || repo_basename(url) == hub_name
}

/// Normalise a git URL for comparison: drop a trailing `/` and `.git`, lowercase.
fn norm_url(url: &str) -> String {
    url.trim_end_matches('/')
        .trim_end_matches(".git")
        .to_ascii_lowercase()
}

/// The repo name in a git URL (`git@host:acme/app.git` / `https://h/acme/app` → `app`).
fn repo_basename(url: &str) -> &str {
    url.trim_end_matches('/')
        .trim_end_matches(".git")
        .rsplit(['/', ':'])
        .next()
        .unwrap_or(url)
}

/// The final path segment of an image reference (`registry.io/acme/app` → `app`).
fn image_basename(image: &str) -> &str {
    image.rsplit('/').next().unwrap_or(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_matches_hub_by_origin_or_basename() {
        assert!(url_matches_hub(
            "https://github.com/acme/app.git",
            "anything",
            Some("https://github.com/acme/app")
        ));
        // No origin match, but the repo basename equals the hub project name.
        assert!(url_matches_hub("git@github.com:acme/app.git", "app", None));
        assert!(!url_matches_hub(
            "git@github.com:acme/other.git",
            "app",
            None
        ));
    }

    #[test]
    fn image_basename_strips_registry_and_org() {
        assert_eq!(image_basename("registry.io/acme/app"), "app");
        assert_eq!(image_basename("app"), "app");
    }
}
