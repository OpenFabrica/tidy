// Docker: images and build cache reported by `docker system df -v`, with
// last-use times from `docker image/container inspect`. Nothing
// here touches the filesystem; removal goes through the docker CLI itself so
// the daemon keeps its own consistency guarantees (an image that is still in
// use is refused by docker, not force-removed by us).

use crate::optimize::{run_bounded, CommandStatus};
use chrono::{DateTime, Datelike, Utc};
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

/// One image row from the daemon.
#[derive(Debug, Clone, Serialize)]
pub struct DockerImage {
    /// Full sha256 id (the removal key).
    pub id: String,
    pub repository: String,
    pub tag: String,
    pub size_kb: u64,
    /// Containers (any state) that use this image; 0 = unused.
    pub containers: u64,
    /// Days since the image was last used. Docker keeps no last-used stamp,
    /// so this is the newest of: image creation, last pull/build/tag
    /// (`Metadata.LastTagTime`), and create/start/stop of any container that
    /// still references it (a running container counts as used now).
    pub idle_days: Option<u64>,
    /// `<none>:<none>` leftovers from rebuilds.
    pub dangling: bool,
}

/// Everything the preview needs.
#[derive(Debug, Clone, Serialize, Default)]
pub struct DockerCatalog {
    pub images: Vec<DockerImage>,
    /// Build-cache entries not in use by a running build, summed.
    pub build_cache_unused_kb: u64,
}

/// Locate the docker CLI: the app's PATH is minimal, so check PATH first and
/// then the usual install locations.
pub fn find_docker(home: &str) -> Option<PathBuf> {
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':') {
            let p = PathBuf::from(dir).join("docker");
            if p.is_file() {
                return Some(p);
            }
        }
    }
    [
        "/usr/local/bin/docker".to_string(),
        "/opt/homebrew/bin/docker".to_string(),
        format!("{home}/.docker/bin/docker"),
        "/Applications/Docker.app/Contents/Resources/bin/docker".to_string(),
        "/Applications/OrbStack.app/Contents/MacOS/xbin/docker".to_string(),
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
}

/// Parse docker's human sizes ("280MB", "1.2GB", "383kB", "-1.025e+08B").
fn parse_size_kb(text: &str) -> u64 {
    let text = text.trim();
    let split = text
        .find(|c: char| c.is_ascii_alphabetic())
        .unwrap_or(text.len());
    let (num, unit) = text.split_at(split);
    let value: f64 = num.trim().parse().unwrap_or(0.0);
    if value <= 0.0 {
        return 0;
    }
    let mult = match unit.trim().to_ascii_lowercase().as_str() {
        "b" => 1.0,
        "kb" | "kib" => 1e3,
        "mb" | "mib" => 1e6,
        "gb" | "gib" => 1e9,
        "tb" | "tib" => 1e12,
        _ => 1.0,
    };
    (value * mult / 1024.0) as u64
}

/// Parse a docker timestamp: RFC 3339 from `inspect`
/// ("2026-10-28T04:41:56.15Z") or the `df` form ("2026-08-29 19:44:49 +0800 +08").
fn parse_stamp(text: &str) -> Option<DateTime<Utc>> {
    let t = match DateTime::parse_from_rfc3339(text.trim()) {
        Ok(t) => t,
        Err(_) => {
            // Keep "YYYY-MM-DD HH:MM:SS +ZZZZ", drop fractions and the zone name.
            let parts: Vec<&str> = text.split_whitespace().collect();
            if parts.len() < 3 {
                return None;
            }
            let stamp = format!("{} {} {}", parts[0], parts[1].split('.').next()?, parts[2]);
            DateTime::parse_from_str(&stamp, "%Y-%m-%d %H:%M:%S %z").ok()?
        }
    };
    // Docker writes Go's zero time ("0001-01-01T00:00:00Z") for "never".
    (t.year() >= 2000).then(|| t.with_timezone(&Utc))
}

/// Whole days from `t` to now (0 for future stamps / clock skew).
fn days_since(t: DateTime<Utc>) -> u64 {
    (Utc::now() - t).num_seconds().max(0) as u64 / 86_400
}

/// Newest usage stamp per image id, from `docker image inspect` and
/// `docker container inspect` JSON arrays. Either input may be empty or
/// garbage; missing data just means fewer signals, never an error.
fn usage_stamps(images_json: &str, containers_json: &str) -> HashMap<String, DateTime<Utc>> {
    let mut latest: HashMap<String, DateTime<Utc>> = HashMap::new();
    let mut bump = |id: &str, t: Option<DateTime<Utc>>| {
        if let Some(t) = t {
            let slot = latest.entry(id.to_string()).or_insert(t);
            if t > *slot {
                *slot = t;
            }
        }
    };
    let stamp_at =
        |v: &serde_json::Value, ptr: &str| v.pointer(ptr)?.as_str().and_then(parse_stamp);

    for img in first_json_array(images_json) {
        let Some(id) = img.get("Id").and_then(|v| v.as_str()) else {
            continue;
        };
        bump(id, stamp_at(&img, "/Created"));
        bump(id, stamp_at(&img, "/Metadata/LastTagTime"));
    }
    for c in first_json_array(containers_json) {
        let Some(id) = c.get("Image").and_then(|v| v.as_str()) else {
            continue;
        };
        // A running container is using the image right now; StartedAt alone
        // would make a long-running service look idle.
        if c.pointer("/State/Running").and_then(|v| v.as_bool()) == Some(true) {
            bump(id, Some(Utc::now()));
        }
        for ptr in ["/Created", "/State/StartedAt", "/State/FinishedAt"] {
            bump(id, stamp_at(&c, ptr));
        }
    }
    latest
}

/// First JSON value in `text` as an array. `run_bounded` appends stderr after
/// stdout, so a trailing warning must not spoil the parse.
fn first_json_array(text: &str) -> Vec<serde_json::Value> {
    serde_json::Deserializer::from_str(text)
        .into_iter::<serde_json::Value>()
        .next()
        .and_then(Result::ok)
        .and_then(|v| match v {
            serde_json::Value::Array(items) => Some(items),
            _ => None,
        })
        .unwrap_or_default()
}

/// Lower each image's idle_days with the usage stamps (newest signal wins).
fn apply_usage(catalog: &mut DockerCatalog, latest: &HashMap<String, DateTime<Utc>>) {
    for img in &mut catalog.images {
        if let Some(t) = latest.get(&img.id) {
            let days = days_since(*t);
            img.idle_days = Some(img.idle_days.map_or(days, |d| d.min(days)));
        }
    }
}

/// Turn `docker system df -v` JSON into a catalog.
fn parse_df(json: &str) -> Option<DockerCatalog> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let str_of = |o: &serde_json::Value, k: &str| o.get(k)?.as_str().map(str::to_string);
    let mut catalog = DockerCatalog::default();
    for img in v.get("Images")?.as_array()? {
        let Some(id) = str_of(img, "ID") else {
            continue;
        };
        let repository = str_of(img, "Repository").unwrap_or_default();
        let tag = str_of(img, "Tag").unwrap_or_default();
        catalog.images.push(DockerImage {
            dangling: repository == "<none>" || tag == "<none>",
            size_kb: parse_size_kb(&str_of(img, "Size").unwrap_or_default()),
            containers: str_of(img, "Containers")
                .and_then(|c| c.parse().ok())
                .unwrap_or(0),
            // Creation time is only the floor; scan() refines it with usage.
            idle_days: str_of(img, "CreatedAt")
                .and_then(|c| parse_stamp(&c))
                .map(days_since),
            id,
            repository,
            tag,
        });
    }
    if let Some(cache) = v.get("BuildCache").and_then(|c| c.as_array()) {
        for entry in cache {
            if str_of(entry, "InUse").as_deref() == Some("true") {
                continue;
            }
            catalog.build_cache_unused_kb +=
                parse_size_kb(&str_of(entry, "Size").unwrap_or_default());
        }
    }
    Some(catalog)
}

/// Query the daemon. None when docker is absent, not running, or the reply
/// is not understood — the preview simply has no Docker section then.
pub fn scan(home: &str) -> Option<DockerCatalog> {
    let docker = find_docker(home)?;
    let argv = [
        docker.to_string_lossy().into_owned(),
        "system".into(),
        "df".into(),
        "-v".into(),
        "--format".into(),
        "{{json .}}".into(),
    ];
    let result = run_bounded(&argv, Duration::from_secs(20));
    if result.status != CommandStatus::Success {
        return None;
    }
    let mut catalog = parse_df(&result.output)?;
    let latest = usage_stamps(
        &inspect_images(&docker, &catalog),
        &inspect_containers(&docker),
    );
    apply_usage(&mut catalog, &latest);
    Some(catalog)
}

/// `docker image inspect` for every catalog image. Output is used even on a
/// non-zero exit: an image removed mid-scan fails only its own entry.
fn inspect_images(docker: &std::path::Path, catalog: &DockerCatalog) -> String {
    if catalog.images.is_empty() {
        return String::new();
    }
    let mut argv = vec![
        docker.to_string_lossy().into_owned(),
        "image".into(),
        "inspect".into(),
    ];
    argv.extend(catalog.images.iter().map(|i| i.id.clone()));
    run_bounded(&argv, Duration::from_secs(20)).output
}

/// `docker container inspect` for every container (any state).
fn inspect_containers(docker: &std::path::Path) -> String {
    let bin = docker.to_string_lossy().into_owned();
    let list = run_bounded(
        &[bin.clone(), "ps".into(), "-aq".into()],
        Duration::from_secs(20),
    );
    if !list.success() {
        return String::new();
    }
    let ids: Vec<String> = list
        .output
        .split_whitespace()
        .filter(|id| id.chars().all(|c| c.is_ascii_hexdigit()))
        .map(str::to_string)
        .collect();
    if ids.is_empty() {
        return String::new();
    }
    let mut argv = vec![bin, "container".into(), "inspect".into()];
    argv.extend(ids);
    run_bounded(&argv, Duration::from_secs(20)).output
}

/// Remove one image by id (no --force: in-use images are refused by docker).
pub fn remove_image(docker: &std::path::Path, id: &str) -> Result<(), String> {
    let argv = [
        docker.to_string_lossy().into_owned(),
        "image".into(),
        "rm".into(),
        id.into(),
    ];
    let result = run_bounded(&argv, Duration::from_secs(60));
    if result.success() {
        Ok(())
    } else {
        Err(result.output)
    }
}

/// Drop every build-cache entry not in use.
pub fn prune_build_cache(docker: &std::path::Path) -> Result<(), String> {
    let argv = [
        docker.to_string_lossy().into_owned(),
        "builder".into(),
        "prune".into(),
        "--force".into(),
        "--all".into(),
    ];
    let result = run_bounded(&argv, Duration::from_secs(300));
    if result.success() {
        Ok(())
    } else {
        Err(result.output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_parse_like_docker_prints_them() {
        assert_eq!(parse_size_kb("280MB"), 273_437);
        assert_eq!(parse_size_kb("1.5GB"), 1_464_843);
        assert_eq!(parse_size_kb("383kB"), 374);
        assert_eq!(parse_size_kb("-1.025e+08B"), 0);
        assert_eq!(parse_size_kb("0B"), 0);
    }

    #[test]
    fn df_json_becomes_catalog() {
        let json = r#"{"Images":[
          {"ID":"sha256:aa","Repository":"app","Tag":"dev","Size":"536MB","Containers":"1","CreatedAt":"2020-01-01 00:00:00 +0000 UTC"},
          {"ID":"sha256:bb","Repository":"<none>","Tag":"<none>","Size":"10MB","Containers":"0","CreatedAt":"bogus"}],
          "BuildCache":[{"InUse":"false","Size":"136MB"},{"InUse":"true","Size":"1GB"}]}"#;
        let c = parse_df(json).unwrap();
        assert_eq!(c.images.len(), 2);
        assert_eq!(c.images[0].containers, 1);
        assert!(c.images[0].idle_days.unwrap() > 365 * 5);
        assert!(c.images[1].dangling);
        assert_eq!(c.images[1].idle_days, None);
        assert_eq!(c.build_cache_unused_kb, parse_size_kb("136MB"));
    }

    #[test]
    fn stamps_parse_both_formats_and_reject_zero_time() {
        assert!(parse_stamp("2026-10-28T04:41:56.150695704Z").is_some());
        assert!(parse_stamp("2026-10-28T11:54:53.768782414+08:00").is_some());
        assert!(parse_stamp("2026-08-29 19:44:49 +0800 +08").is_some());
        assert_eq!(parse_stamp("0001-01-01T00:00:00Z"), None);
        assert_eq!(parse_stamp("bogus"), None);
    }

    #[test]
    fn idle_days_follow_last_use_not_creation() {
        // Old upstream image pulled recently; another created long ago but
        // a container ran it recently; a third with no newer signal.
        let df = r#"{"Images":[
          {"ID":"sha256:pulled","Repository":"rust","Tag":"1","Size":"1GB","Containers":"0","CreatedAt":"2020-01-01 00:00:00 +0000 UTC"},
          {"ID":"sha256:ran","Repository":"app","Tag":"dev","Size":"1GB","Containers":"0","CreatedAt":"2020-01-01 00:00:00 +0000 UTC"},
          {"ID":"sha256:old","Repository":"old","Tag":"x","Size":"1GB","Containers":"0","CreatedAt":"2020-01-01 00:00:00 +0000 UTC"}]}"#;
        let recent = (Utc::now() - chrono::Duration::days(3)).to_rfc3339();
        let images = format!(
            r#"[{{"Id":"sha256:pulled","Created":"2020-01-01T00:00:00Z","Metadata":{{"LastTagTime":"{recent}"}}}},
               {{"Id":"sha256:ran","Created":"2020-01-01T00:00:00Z","Metadata":{{"LastTagTime":"0001-01-01T00:00:00Z"}}}},
               {{"Id":"sha256:old","Created":"2020-01-01T00:00:00Z","Metadata":{{}}}}]
            Error: No such image: sha256:gone"#
        );
        let containers = format!(
            r#"[{{"Image":"sha256:ran","Created":"2020-01-01T00:00:00Z","State":{{"Running":false,"StartedAt":"2020-01-02T00:00:00Z","FinishedAt":"{recent}"}}}}]"#
        );
        let mut c = parse_df(df).unwrap();
        apply_usage(&mut c, &usage_stamps(&images, &containers));
        assert_eq!(c.images[0].idle_days, Some(3));
        assert_eq!(c.images[1].idle_days, Some(3));
        assert!(c.images[2].idle_days.unwrap() > 365 * 5);
    }

    #[test]
    fn running_container_means_used_now() {
        let df = r#"{"Images":[{"ID":"sha256:svc","Repository":"svc","Tag":"1","Size":"1GB","Containers":"1","CreatedAt":"2020-01-01 00:00:00 +0000 UTC"}]}"#;
        let containers = r#"[{"Image":"sha256:svc","Created":"2020-01-01T00:00:00Z","State":{"Running":true,"StartedAt":"2020-01-02T00:00:00Z","FinishedAt":"0001-01-01T00:00:00Z"}}]"#;
        let mut c = parse_df(df).unwrap();
        apply_usage(&mut c, &usage_stamps("", containers));
        assert_eq!(c.images[0].idle_days, Some(0));
    }

    #[test]
    fn garbage_inspect_output_keeps_creation_floor() {
        let df = r#"{"Images":[{"ID":"sha256:a","Repository":"a","Tag":"1","Size":"1GB","Containers":"0","CreatedAt":"2020-01-01 00:00:00 +0000 UTC"}]}"#;
        let mut c = parse_df(df).unwrap();
        apply_usage(&mut c, &usage_stamps("docker: not running", "{}"));
        assert!(c.images[0].idle_days.unwrap() > 365 * 5);
    }
}

#[cfg(test)]
mod real_machine {
    /// Manual: cargo test -p mole-ops real_docker_scan -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_docker_scan() {
        let home = std::env::var("HOME").unwrap();
        let Some(c) = super::scan(&home) else {
            eprintln!("docker unavailable");
            return;
        };
        let unused = c.images.iter().filter(|i| i.containers == 0).count();
        eprintln!(
            "{} images ({unused} unused), build cache unused {} KB",
            c.images.len(),
            c.build_cache_unused_kb
        );
        for i in c.images.iter().take(10) {
            eprintln!("{:?}", i);
        }
    }
}
