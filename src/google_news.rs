//! Resolve public Google News redirect links. No login or access restriction is bypassed.
use anyhow::{bail, Context, Result};
use base64::Engine;
use reqwest::{blocking::Client, Url};
use scraper::{Html, Selector};
use serde_json::{json, Value};
use std::{io::Read, time::Duration};

pub fn resolve(_client: &Client, url: &str) -> Result<Url> {
    let input = Url::parse(url)?;
    if input.host_str() != Some("news.google.com") {
        return Ok(input);
    }
    if input.scheme() != "https" || input.port().is_some() || !input.username().is_empty() {
        bail!("잘못된 Google 뉴스 주소");
    }
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(25))
        .user_agent("Mozilla/5.0 NewsMoa/1.4")
        .build()?;
    let id = input
        .path_segments()
        .and_then(|mut p| p.next_back())
        .filter(|s| !s.is_empty())
        .context("Google 뉴스 글 번호 없음")?;
    if let Some(url) = legacy_url(id) {
        return Ok(url);
    }
    std::thread::sleep(Duration::from_millis(750));
    let mut current = input.clone();
    let mut response = None;
    for _ in 0..=5 {
        let result = client
            .get(current.clone())
            .header("Accept-Language", "ko-KR,ko;q=0.9,en;q=0.7")
            .send()?;
        if !result.status().is_redirection() {
            response = Some(result);
            break;
        }
        let location = result
            .headers()
            .get(reqwest::header::LOCATION)
            .context("Google 뉴스 이동 주소 없음")?
            .to_str()?;
        let next = current.join(location)?;
        if next.host_str() != Some("news.google.com") {
            return public_url(next.as_str());
        }
        if next.scheme() != "https" || next.port().is_some() || !next.username().is_empty() {
            bail!("잘못된 Google 뉴스 이동 주소");
        }
        current = next;
    }
    let response = response.context("Google 뉴스 이동 횟수 초과")?;
    let response = response.error_for_status()?;
    if response.url().host_str() != Some("news.google.com") {
        let actual = response.url().clone();
        if actual.host_str() == Some("consent.google.com") {
            bail!("Google 뉴스에서 동의 화면을 반환해 원문 주소를 확인하지 못했습니다");
        }
        return public_url(actual.as_str());
    }
    let mut raw = String::new();
    response.take(3_000_001).read_to_string(&mut raw)?;
    if raw.len() > 3_000_000 {
        bail!("Google 뉴스 연결 페이지가 너무 큽니다");
    }
    let (signature, timestamp) = parameters(&raw)?;
    let inner = json!([
        "garturlreq",
        [
            [
                "X",
                "X",
                ["X", "X"],
                null,
                null,
                1,
                1,
                "US:en",
                null,
                1,
                null,
                null,
                null,
                null,
                null,
                0,
                1
            ],
            "X",
            "X",
            1,
            [1, 1, 1],
            1,
            1,
            null,
            0,
            0,
            null,
            0
        ],
        id,
        timestamp,
        signature
    ]);
    let request = json!([[["Fbv4je", serde_json::to_string(&inner)?, null, "generic"]]]);
    let response = client
        .post("https://news.google.com/_/DotsSplashUi/data/batchexecute")
        .header("Origin", "https://news.google.com")
        .header("Referer", "https://news.google.com/")
        .header("X-Same-Domain", "1")
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded;charset=UTF-8",
        )
        .form(&[("f.req", serde_json::to_string(&request)?)])
        .send()?
        .error_for_status()?;
    let mut raw = String::new();
    response.take(1_000_001).read_to_string(&mut raw)?;
    if raw.len() > 1_000_000 {
        bail!("Google 뉴스 응답이 너무 큽니다");
    }
    parse_response(&raw)
}

fn parameters(page: &str) -> Result<(String, u64)> {
    let html = Html::parse_document(page);
    let selector = Selector::parse("[data-n-a-sg][data-n-a-ts]").unwrap();
    let node=html.select(&selector).next().context("Google 뉴스 원문 연결 정보를 찾지 못했습니다. 동의·차단 화면 또는 페이지 변경일 수 있습니다")?;
    Ok((
        node.value().attr("data-n-a-sg").unwrap().into(),
        node.value().attr("data-n-a-ts").unwrap().parse()?,
    ))
}

fn parse_response(raw: &str) -> Result<Url> {
    for line in raw.lines().map(str::trim).filter(|l| l.starts_with('[')) {
        if let Ok(value) = serde_json::from_str::<Value>(line) {
            if let Some(url) = find_url(&value) {
                return public_url(&url);
            }
        }
    }
    bail!("Google 뉴스에서 원문 주소를 반환하지 않았습니다. 나중에 다시 시도하세요")
}

fn find_url(value: &Value) -> Option<String> {
    let array = value.as_array()?;
    if array.first()?.as_str() == Some("wrb.fr") && array.get(1)?.as_str() == Some("Fbv4je") {
        let payload: Value = serde_json::from_str(array.get(2)?.as_str()?).ok()?;
        if payload.get(0)?.as_str() == Some("garturlres") {
            return payload.get(1)?.as_str().map(str::to_string);
        }
    }
    array.iter().find_map(find_url)
}

fn public_url(text: &str) -> Result<Url> {
    let url = Url::parse(text)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || url.host_str() == Some("news.google.com")
    {
        bail!("유효한 언론사 원문 주소가 아닙니다");
    }
    Ok(url)
}

fn legacy_url(id: &str) -> Option<Url> {
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(id.trim_end_matches('='))
        .ok()?;
    if !decoded.starts_with(&[0x08, 0x13, 0x22]) {
        return None;
    }
    let mut pos = 3;
    let mut len = 0usize;
    let mut shift = 0;
    loop {
        let b = *decoded.get(pos)?;
        pos += 1;
        len |= ((b & 0x7f) as usize) << shift;
        if b & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift > 28 {
            return None;
        }
    }
    public_url(std::str::from_utf8(decoded.get(pos..pos.checked_add(len)?)?).ok()?).ok()
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_signed_redirect_and_nested_response() {
        let (sig, ts) =
            super::parameters("<div data-n-a-sg='abc&amp;def' data-n-a-ts='123'></div>").unwrap();
        assert_eq!(sig, "abc&def");
        assert_eq!(ts, 123);
        let inner = serde_json::json!(["garturlres", "https://example.org/news?a=1&b=2"]);
        let frames = serde_json::json!([
            ["di", 1],
            ["wrb.fr", "Fbv4je", inner.to_string(), null],
            ["af.httprm", 1]
        ]);
        let raw = format!(")]}}'\n\n{}\n", frames);
        assert_eq!(
            super::parse_response(&raw).unwrap().as_str(),
            "https://example.org/news?a=1&b=2"
        );
        assert!(super::parse_response(")]}'\n\n[]").is_err());
    }
}
