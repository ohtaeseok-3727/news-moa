use anyhow::{bail, Context, Result};
use reqwest::blocking::Client;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

const REPO: &str = "ohtaeseok-3727/news-moa";
const MAX_BYTES: u64 = 40 * 1024 * 1024;

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}
#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    size: u64,
    digest: Option<String>,
}

fn version(s: &str) -> Option<[u64; 3]> {
    let s = s.strip_prefix('v').unwrap_or(s);
    let p: Vec<_> = s.split('.').collect();
    if p.len() != 3
        || p.iter()
            .any(|x| x.is_empty() || !x.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    Some([p[0].parse().ok()?, p[1].parse().ok()?, p[2].parse().ok()?])
}

fn choose(release: &Release) -> Result<Option<&Asset>> {
    if release.draft || release.prerelease {
        return Ok(None);
    }
    let candidate = version(&release.tag_name).context("릴리스 버전 형식이 잘못되었습니다")?;
    if candidate <= version(env!("CARGO_PKG_VERSION")).unwrap() {
        return Ok(None);
    }
    let matches: Vec<_> = release
        .assets
        .iter()
        .filter(|a| a.name == "news_moa.exe")
        .collect();
    if matches.len() != 1 {
        bail!("릴리스에 실행 파일이 하나 있어야 합니다");
    }
    let asset = matches[0];
    let expected = format!(
        "https://github.com/{REPO}/releases/download/{}/news_moa.exe",
        release.tag_name
    );
    if asset.browser_download_url != expected || asset.size == 0 || asset.size > MAX_BYTES {
        bail!("업데이트 파일 주소 또는 크기가 올바르지 않습니다");
    }
    let digest = asset
        .digest
        .as_deref()
        .context("GitHub SHA-256 정보가 없습니다")?;
    if digest.len() != 71
        || !digest.starts_with("sha256:")
        || !digest[7..].bytes().all(|b| b.is_ascii_hexdigit())
    {
        bail!("SHA-256 정보가 올바르지 않습니다");
    }
    Ok(Some(asset))
}

fn check_and_stage(exe: &Path) -> Result<Option<PathBuf>> {
    let client = Client::builder()
        .user_agent("NewsMoa-Updater/1.3")
        .timeout(Duration::from_secs(120))
        .build()?;
    let response = client
        .get(format!(
            "https://api.github.com/repos/{REPO}/releases/latest"
        ))
        .send()?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let release: Release = response.error_for_status()?.json()?;
    let Some(asset) = choose(&release)? else {
        return Ok(None);
    };
    let mut response = client
        .get(&asset.browser_download_url)
        .send()?
        .error_for_status()?;
    let path = exe.with_file_name("news_moa.update.download");
    let result = (|| -> Result<()> {
        let mut file = std::fs::File::create(&path)?;
        let mut hash = Sha256::new();
        let mut count = 0u64;
        let mut buf = [0u8; 65536];
        loop {
            let n = response.read(&mut buf)?;
            if n == 0 {
                break;
            }
            count += n as u64;
            if count > asset.size || count > MAX_BYTES {
                bail!("업데이트 파일이 너무 큽니다");
            }
            hash.update(&buf[..n]);
            file.write_all(&buf[..n])?;
        }
        file.sync_all()?;
        if count != asset.size
            || format!("{:x}", hash.finalize())
                != asset.digest.as_ref().unwrap()[7..].to_ascii_lowercase()
        {
            bail!("업데이트 파일 검증에 실패했습니다");
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&path);
    }
    result?;
    Ok(Some(path))
}

pub fn watch(mut status: impl FnMut(String)) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    loop {
        status("업데이트 확인 중".into());
        match check_and_stage(&exe) {
            Ok(Some(staged)) => {
                let helper = exe.with_file_name("news_moa.updater.exe");
                let launch = (|| -> Result<()> {
                    std::fs::copy(&exe, &helper)?;
                    Command::new(&helper)
                        .arg("--apply-update")
                        .arg(&exe)
                        .arg(&staged)
                        .args(std::env::args_os().skip(1))
                        .spawn()?;
                    Ok(())
                })();
                match launch {
                    Ok(()) => {
                        status("새 버전 적용을 위해 재시작합니다".into());
                        eprintln!("새 버전을 적용하기 위해 다시 시작합니다.");
                        std::process::exit(0);
                    }
                    Err(e) => {
                        status(format!("업데이트 준비 실패: {e:#}"));
                        eprintln!("업데이트 준비 실패: {e:#}");
                    }
                }
            }
            Ok(None) => status("최신 버전이거나 새 릴리스가 없습니다".into()),
            Err(e) => {
                status(format!("업데이트 확인 실패: {e:#}"));
                eprintln!("업데이트 확인 실패 (다음에 다시 시도): {e:#}");
            }
        }
        std::thread::sleep(Duration::from_secs(6 * 3600));
    }
}

pub fn apply(args: &[std::ffi::OsString]) -> Result<()> {
    let target = PathBuf::from(args.get(2).context("업데이트 대상 누락")?);
    let staged = PathBuf::from(args.get(3).context("업데이트 파일 누락")?);
    let backup = target.with_file_name("news_moa.previous.exe");
    if staged != target.with_file_name("news_moa.update.download") {
        bail!("잘못된 업데이트 경로");
    }
    let _ = std::fs::remove_file(&backup);
    let mut moved = false;
    for _ in 0..60 {
        if std::fs::rename(&target, &backup).is_ok() {
            moved = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    if !moved {
        let _ = Command::new(&target)
            .args(&args[4..])
            .arg("--skip-update-once")
            .spawn();
        bail!("실행 중인 기존 프로그램을 교체할 수 없습니다");
    }
    if let Err(e) = std::fs::rename(&staged, &target) {
        let _ = std::fs::rename(&backup, &target);
        let _ = Command::new(&target)
            .args(&args[4..])
            .arg("--skip-update-once")
            .spawn();
        return Err(e.into());
    }
    let marker = target.with_file_name("news_moa.update.ready");
    let _ = std::fs::remove_file(&marker);
    let clean_args: Vec<_> = args[4..]
        .iter()
        .enumerate()
        .filter(|(i, a)| {
            *a != "--skip-update-once"
                && *a != "--update-health"
                && !(*i > 0 && args[4..][*i - 1] == "--update-health")
        })
        .map(|(_, a)| a)
        .collect();
    let start = Command::new(&target)
        .args(&clean_args)
        .arg("--update-health")
        .arg(&marker)
        .spawn();
    if let Err(e) = start {
        let _ = std::fs::remove_file(&target);
        let _ = std::fs::rename(&backup, &target);
        let _ = Command::new(&target)
            .args(&clean_args)
            .arg("--skip-update-once")
            .spawn();
        return Err(e.into());
    }
    let mut child = start.unwrap();
    let mut healthy = false;
    for _ in 0..120 {
        std::thread::sleep(Duration::from_millis(500));
        if child.try_wait()?.is_some() {
            break;
        }
        if marker.is_file() {
            healthy = true;
            break;
        }
    }
    let _ = std::fs::remove_file(&marker);
    if !healthy {
        let _ = child.kill();
        let _ = child.wait();
        std::fs::remove_file(&target)?;
        std::fs::rename(&backup, &target)?;
        Command::new(&target)
            .args(&clean_args)
            .arg("--skip-update-once")
            .spawn()?;
        bail!("새 버전이 정상 시작되지 않아 이전 버전으로 복구했습니다");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn versions_are_strict_and_numeric() {
        assert_eq!(version("v1.10.0"), Some([1, 10, 0]));
        assert!(version("v1.3.0-beta").is_none());
        assert!(version("1.2").is_none());
    }
    #[test]
    fn rejects_untrusted_assets_and_prereleases() {
        let mut r = Release {
            tag_name: "v999.0.0".into(),
            draft: false,
            prerelease: false,
            assets: vec![Asset {
                name: "news_moa.exe".into(),
                browser_download_url: "https://evil.example/news_moa.exe".into(),
                size: 10,
                digest: Some(format!("sha256:{}", "a".repeat(64))),
            }],
        };
        assert!(choose(&r).is_err());
        r.assets[0].browser_download_url =
            format!("https://github.com/{REPO}/releases/download/v999.0.0/news_moa.exe");
        assert!(choose(&r).unwrap().is_some());
        r.prerelease = true;
        assert!(choose(&r).unwrap().is_none());
    }
}
