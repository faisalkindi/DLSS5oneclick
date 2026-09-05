//! HTTP helpers: GitHub JSON, streamed downloads with progress, zip member extraction.

use anyhow::{anyhow, bail, Context, Result};
use reqwest::blocking::Client;
use std::fs;
use std::io::{Read, Seek, Write};
use std::path::Path;
use std::time::Duration;

pub type Progress<'a> = &'a (dyn Fn(u8, &str) + Sync);

pub fn client() -> Result<Client> {
    Client::builder()
        .user_agent(concat!("DLSS5oneclick/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(120))
        .build()
        .context("cannot build HTTP client")
}

/// GitHub API calls are capped at 60/hour per IP without a token. Honour
/// `GITHUB_TOKEN` when a user sets one; otherwise callers fall back to the
/// HTML release pages, which have no such cap.
pub fn get_json_github(client: &Client, url: &str) -> Result<serde_json::Value> {
    let mut req = client
        .get(url)
        .header("Accept", "application/vnd.github+json");
    if let Ok(tok) = std::env::var("GITHUB_TOKEN") {
        if !tok.trim().is_empty() {
            req = req.bearer_auth(tok.trim());
        }
    }
    if std::env::var("DLSS5ONECLICK_NO_API").is_ok() {
        bail!("API disabled by DLSS5ONECLICK_NO_API");
    }
    let resp = req
        .send()
        .with_context(|| format!("request failed: {url}"))?;
    if !resp.status().is_success() {
        bail!("{url}: HTTP {}", resp.status());
    }
    resp.json().with_context(|| format!("bad JSON from {url}"))
}

/// Release tags of `owner/repo` starting with `prefix`, read from the HTML
/// releases pages (newest first, up to `pages` pages of 10). No API, no cap.
pub fn github_release_tags_html(
    client: &Client,
    repo: &str,
    prefix: &str,
    pages: usize,
) -> Result<Vec<String>> {
    let re = regex::Regex::new(&format!(
        r#"/{}/releases/tag/({}[A-Za-z0-9._-]*)"#,
        regex::escape(repo),
        regex::escape(prefix)
    ))
    .unwrap();
    let mut tags: Vec<String> = Vec::new();
    for page in 1..=pages {
        let html = get_text(
            client,
            &format!("https://github.com/{repo}/releases?page={page}"),
        )?;
        let mut found_any = false;
        for c in re.captures_iter(&html) {
            let t = c[1].to_string();
            if !tags.contains(&t) {
                tags.push(t);
            }
            found_any = true;
        }
        // Stop once a page had matches and the next one would be older releases,
        // or when the page lists nothing at all.
        if found_any || !html.contains("/releases/tag/") {
            if found_any && page >= 1 {
                // one more page catches prefixes split across the boundary
                if page == pages {
                    break;
                }
                let html2 = get_text(
                    client,
                    &format!("https://github.com/{repo}/releases?page={}", page + 1),
                )?;
                for c in re.captures_iter(&html2) {
                    let t = c[1].to_string();
                    if !tags.contains(&t) {
                        tags.push(t);
                    }
                }
            }
            break;
        }
    }
    Ok(tags)
}

/// Download URL of the first asset of `tag` whose file name matches `name_re`,
/// read from GitHub's expanded-assets HTML fragment. No API.
pub fn github_asset_url_html(
    client: &Client,
    repo: &str,
    tag: &str,
    name_re: &str,
) -> Result<String> {
    let html = get_text(
        client,
        &format!("https://github.com/{repo}/releases/expanded_assets/{tag}"),
    )?;
    let re = regex::Regex::new(&format!(
        r#"/{}/releases/download/{}/({})"#,
        regex::escape(repo),
        regex::escape(tag),
        name_re
    ))
    .unwrap();
    let m = re
        .captures(&html)
        .ok_or_else(|| anyhow!("no asset matching {name_re} in {repo} {tag}"))?;
    Ok(format!(
        "https://github.com/{repo}/releases/download/{tag}/{}",
        &m[1]
    ))
}

/// Small GET (release pages, JSON, wiki markdown), with the same connection-level
/// retry as `download`: github.com's edge sometimes answers one request with a
/// broken TLS record (`received corrupt message of type InvalidContentType`).
pub fn get_text(client: &Client, url: &str) -> Result<String> {
    with_retry(&|_, _| {}, url, || {
        let resp = client
            .get(url)
            .send()
            .with_context(|| format!("request failed: {url}"))?;
        if !resp.status().is_success() {
            bail!("{url}: HTTP {}", resp.status());
        }
        resp.text().with_context(|| format!("bad body from {url}"))
    })
}

/// Tag of a repo's newest *non-prerelease* release: `releases/latest` redirects
/// to `/releases/tag/<tag>`. The releases page lists betas first, so scraping it
/// would hand out pre-releases (DLSS5-Feeder 0.12.1-beta.1).
pub fn latest_tag(client: &Client, repo: &str) -> Result<String> {
    with_retry(&|_, _| {}, repo, || {
        let resp = client
            .get(format!("https://github.com/{repo}/releases/latest"))
            .send()
            .with_context(|| format!("request failed: {repo} releases/latest"))?;
        if !resp.status().is_success() {
            bail!("{repo} releases/latest: HTTP {}", resp.status());
        }
        let url = resp.url().as_str().to_owned();
        url.rsplit("/tag/")
            .next()
            .filter(|t| !t.is_empty() && !t.contains('/'))
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("{repo}: releases/latest did not land on a tag ({url})"))
    })
}

/// Content-Length of `url` after redirects (GitHub's `latest/download` → CDN),
/// or `None` when the server does not say.
pub fn remote_len(client: &Client, url: &str) -> Result<Option<u64>> {
    with_retry(&|_, _| {}, url, || {
        let resp = client
            .head(url)
            .send()
            .with_context(|| format!("request failed: {url}"))?;
        if !resp.status().is_success() {
            bail!("{url}: HTTP {}", resp.status());
        }
        Ok(resp.content_length())
    })
}

/// Run `f` up to four times when it fails on a connection-level error.
fn with_retry<T>(progress: Progress, label: &str, f: impl Fn() -> Result<T>) -> Result<T> {
    let mut attempt = 1u32;
    loop {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) => {
                let msg = format!("{e:#}");
                let retryable = msg.contains("error sending request")
                    || msg.contains("Connect")
                    || msg.contains("corrupt message")
                    || msg.contains("connection")
                    || msg.contains("timed out")
                    || msg.contains("reset");
                if !retryable || attempt == 4 {
                    return Err(e);
                }
                progress(0, &format!("{label}: retrying ({attempt}/3)"));
                std::thread::sleep(std::time::Duration::from_secs(2 * attempt as u64));
                attempt += 1;
            }
        }
    }
}

fn fmt_bytes(n: u64) -> String {
    if n >= 1 << 20 {
        format!("{:.1} MB", n as f64 / (1u64 << 20) as f64)
    } else {
        format!("{} KB", n / 1024)
    }
}

/// Stream `url` to `dest`, retrying connection-level failures (GitHub's CDN edge
/// occasionally answers with a broken TLS record; the next attempt succeeds).
pub fn download(
    client: &Client,
    url: &str,
    dest: &Path,
    label: &str,
    progress: Progress,
) -> Result<()> {
    // A release asset URL names its tag, so the bytes behind it never change:
    // the 165 MB model and the OptiScaler zip were being fetched again for
    // every game somebody set up (#42). Reuse what is already on disk.
    if let Some(cached) = cache_path(url) {
        if cached.is_file() {
            if let Some(p) = dest.parent() {
                fs::create_dir_all(p)?;
            }
            if fs::copy(&cached, dest).is_ok() {
                progress(100, &format!("{label} (cached)"));
                return Ok(());
            }
            // A cache entry that cannot be copied is not worth keeping.
            let _ = fs::remove_file(&cached);
        }
    }
    with_retry(progress, label, || {
        download_once(client, url, dest, label, progress)
    })?;
    if let Some(cached) = cache_path(url) {
        if let Some(p) = cached.parent() {
            let _ = fs::create_dir_all(p);
            // Copying first and renaming keeps a half-written entry from ever
            // being read as a complete one.
            let tmp = cached.with_extension("part");
            if fs::copy(dest, &tmp).is_ok() && fs::rename(&tmp, &cached).is_err() {
                let _ = fs::remove_file(&tmp);
            }
            prune_cache(p);
        }
    }
    Ok(())
}

/// Where a downloaded file is kept for next time.
pub fn cache_dir() -> std::path::PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("dlss5oneclick")
        .join("downloads")
}

/// The cache file for a URL, or `None` for a URL whose contents can change
/// under the same address -- `/releases/latest/download/...` is a moving
/// target, and caching it would pin everybody to whatever shipped first.
fn cache_path(url: &str) -> Option<std::path::PathBuf> {
    if !url.starts_with("https://") || url.contains("/latest/download/") {
        return None;
    }
    let name = url.rsplit('/').next().filter(|n| !n.is_empty())?;
    if name.len() > 80 || name.contains(|c: char| !c.is_ascii_graphic()) {
        return None;
    }
    // FNV-1a over the whole URL: the file name alone is not unique across
    // tags, and this needs no dependency.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in url.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    Some(cache_dir().join(format!("{h:016x}-{safe}")))
}

/// Cap the cache so a year of upstream releases cannot fill a disk: oldest
/// entries go first, by last-modified time.
const CACHE_LIMIT: u64 = 2 * 1024 * 1024 * 1024;

fn prune_cache(dir: &Path) {
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, u64, std::path::PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            if !m.is_file() {
                return None;
            }
            Some((m.modified().ok()?, m.len(), e.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|(_, len, _)| *len).sum();
    if total <= CACHE_LIMIT {
        return;
    }
    files.sort_by_key(|(when, _, _)| *when);
    for (_, len, path) in files {
        if total <= CACHE_LIMIT {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(len);
        }
    }
}

/// One attempt: stream to a `.part` file, reporting percent + KB/MB downloaded.
fn download_once(
    client: &Client,
    url: &str,
    dest: &Path,
    label: &str,
    progress: Progress,
) -> Result<()> {
    let mut resp = client
        .get(url)
        .send()
        .with_context(|| format!("download failed: {label}"))?;
    if !resp.status().is_success() {
        bail!("{label}: HTTP {}", resp.status());
    }
    if let Some(p) = dest.parent() {
        fs::create_dir_all(p)?;
    }
    let part = dest.with_extension(format!(
        "{}.part",
        dest.extension().and_then(|e| e.to_str()).unwrap_or("")
    ));
    let total = resp.content_length().unwrap_or(0);
    let mut done: u64 = 0;
    let mut buf = vec![0u8; 1 << 18];
    let result: Result<()> = (|| {
        let mut out = fs::File::create(&part)?;
        loop {
            let n = resp.read(&mut buf)?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
            done += n as u64;
            if total > 0 {
                let pct = (done as f64 / total as f64 * 100.0).min(99.0) as u8;
                progress(
                    pct,
                    &format!("{label}: {} / {}", fmt_bytes(done), fmt_bytes(total)),
                );
            } else {
                progress(0, &format!("{label}: {}", fmt_bytes(done)));
            }
        }
        out.flush()?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&part);
        return Err(e).with_context(|| format!("download failed: {label}"));
    }
    fs::rename(&part, dest)?;
    Ok(())
}

/// Copy one zip member to an exact destination (the zip's own path is never used).
pub fn extract_member<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
    member: &str,
    dest: &Path,
) -> Result<()> {
    if let Some(p) = dest.parent() {
        fs::create_dir_all(p)?;
    }
    let mut f = zip
        .by_name(member)
        .with_context(|| format!("zip member missing: {member}"))?;
    let mut out = fs::File::create(dest)?;
    std::io::copy(&mut f, &mut out)?;
    Ok(())
}

/// File members whose name matches `re`.
pub fn members_matching<R: Read + Seek>(
    zip: &zip::ZipArchive<R>,
    re: &regex::Regex,
) -> Vec<String> {
    zip.file_names()
        .filter(|n| !n.ends_with('/') && re.is_match(n))
        .map(str::to_owned)
        .collect()
}

pub fn file_name(member: &str) -> &str {
    member.rsplit('/').next().unwrap_or(member)
}

#[cfg(test)]
mod tests {

    /// The cache key has to separate two releases of the same file name, and
    /// has to refuse a URL whose contents move under it (#42).
    #[test]
    fn cache_path_is_per_url_and_skips_moving_targets() {
        let a = super::cache_path("https://github.com/o/r/releases/download/v1.4.10/x.addon64");
        let b = super::cache_path("https://github.com/o/r/releases/download/v1.4.11/x.addon64");
        assert!(a.is_some() && b.is_some());
        assert_ne!(a, b, "two tags must not share one cache entry");
        assert!(a
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with("-x.addon64"));

        // Moves whenever upstream publishes: never cached.
        assert!(
            super::cache_path("https://github.com/o/r/releases/latest/download/x.zip").is_none()
        );
        // Not https, or no file name at the end.
        assert!(super::cache_path("http://example.com/x.zip").is_none());
        assert!(super::cache_path("https://example.com/dir/").is_none());
    }

    use super::*;
    use std::cell::Cell;

    #[test]
    fn with_retry_retries_connection_errors_only() {
        let n = Cell::new(0);
        let r: Result<u8> = with_retry(&|_, _| {}, "x", || {
            n.set(n.get() + 1);
            if n.get() < 3 {
                bail!("request failed: received corrupt message of type InvalidContentType")
            }
            Ok(7)
        });
        assert_eq!(r.unwrap(), 7);
        assert_eq!(n.get(), 3);
        let m = Cell::new(0);
        let r: Result<u8> = with_retry(&|_, _| {}, "x", || {
            m.set(m.get() + 1);
            bail!("HTTP 404 Not Found")
        });
        assert!(r.is_err());
        assert_eq!(m.get(), 1);
    }
}
