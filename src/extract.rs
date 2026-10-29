use anyhow::{bail, Context, Result};
use reqwest::blocking::{Client, Response};
use scraper::{Html, Selector};
use std::io::Read;

pub fn article(client: &Client, url: &str) -> Result<String> {
    let mut target = reqwest::Url::parse(url)?;
    if target.host_str() == Some("news.ycombinator.com") {
        let id = target
            .query_pairs()
            .find(|(k, _)| k == "id")
            .map(|(_, v)| v.to_string())
            .context("토론 번호 없음")?;
        let id: u64 = id.parse()?;
        let item: serde_json::Value = client
            .get(format!(
                "https://hacker-news.firebaseio.com/v0/item/{id}.json"
            ))
            .send()?
            .error_for_status()?
            .json()?;
        if let Some(link) = item["url"].as_str() {
            target = reqwest::Url::parse(link)?;
        } else if let Some(text) = item["text"].as_str() {
            let text = crate::clean(text);
            if text.chars().count() >= 100 {
                return Ok(text);
            }
        } else {
            bail!("요약할 게시글 본문이 없습니다");
        }
    }
    if target.scheme() != "https" && target.scheme() != "http" {
        bail!("지원하지 않는 주소");
    }
    if target.host_str() == Some("news.google.com") {
        target = crate::google_news::resolve(client, target.as_str())
            .context("Google 원문 링크 연결 실패")?;
    }
    let response = crate::safe_fetch::get(target)?;
    let text = read_html(response)?;
    extract_html(&text)
}

fn read_html(response: Response) -> Result<String> {
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|s| s.to_str().ok())
        .unwrap_or("")
        .to_string();
    let mut raw = Vec::new();
    response.take(8_000_001).read_to_end(&mut raw)?;
    if raw.len() > 8_000_000 {
        bail!("본문 크기 제한 초과");
    }
    Ok(decode_html(&raw, &content_type))
}

fn decode_html(raw: &[u8], content_type: &str) -> String {
    let charset = regex::Regex::new(r#"(?i)charset\s*=\s*["']?\s*([a-z0-9_-]+)"#).unwrap();
    let head = String::from_utf8_lossy(&raw[..raw.len().min(8192)]);
    let label = charset
        .captures(content_type)
        .or_else(|| charset.captures(&head))
        .and_then(|c| c.get(1).map(|s| s.as_str().to_string()));
    let encoding = label
        .as_ref()
        .and_then(|s| encoding_rs::Encoding::for_label(s.as_bytes()))
        .unwrap_or(encoding_rs::UTF_8);
    encoding.decode(raw).0.into_owned()
}

pub fn extract_html(text: &str) -> Result<String> {
    let html = Html::parse_document(text);
    let paragraphs = Selector::parse("p").unwrap();
    for container in [
        "[itemprop=articleBody]",
        ".article-body",
        "#article-body",
        "#article-view-content-div",
        "#article-view-content",
        "#articleBody",
        "#dic_area",
        "#newsEndContents",
        "#article-view-content-div",
        "#article_body",
        "#article_txt",
        "#news_body_area",
        ".article_txt",
        ".article_view_content",
        ".news_end",
        "#articeBody",
        ".article_body",
        ".article_view",
        ".article-content",
        ".view_con",
        ".newsct_article",
        "article",
        "main",
        "[role=main]",
        "body",
    ] {
        if container == "body" {
            if let Some(body) = structured_article(&html, text)? {
                return Ok(body);
            }
        }
        let selector = Selector::parse(container).unwrap();
        let mut best = String::new();
        for parent in html.select(&selector) {
            let mut lines = Vec::new();
            for p in parent.select(&paragraphs) {
                if p.ancestors().any(|a| {
                    a.value()
                        .as_element()
                        .map(|e| {
                            matches!(
                                e.name(),
                                "nav" | "footer" | "header" | "aside" | "script" | "style"
                            )
                        })
                        .unwrap_or(false)
                }) {
                    continue;
                }
                let line = p
                    .text()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                if line.chars().count() >= 30 && !lines.contains(&line) {
                    lines.push(line);
                }
            }
            let joined = lines.join("\n\n");
            let joined = if joined.chars().count() < 200 && container != "body" {
                parent
                    .descendants()
                    .filter(|node| node.value().as_text().is_some())
                    .filter(|node| {
                        !node.ancestors().any(|a| {
                            a.value()
                                .as_element()
                                .map(|e| {
                                    matches!(
                                        e.name(),
                                        "script"
                                            | "style"
                                            | "nav"
                                            | "footer"
                                            | "header"
                                            | "aside"
                                            | "iframe"
                                    )
                                })
                                .unwrap_or(false)
                        })
                    })
                    .filter_map(|node| node.value().as_text())
                    .map(|s| s.text.as_ref())
                    .collect::<Vec<&str>>()
                    .join(" ")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            } else {
                joined
            };
            if joined.len() > best.len() {
                best = joined;
            }
        }
        if best.chars().count() >= 200 {
            return Ok(best);
        }
    }
    bail!("원문 본문 추출 실패: 로그인·차단 또는 지원하지 않는 페이지 구조")
}

fn structured_article(html: &Html, raw: &str) -> Result<Option<String>> {
    // Some public pages render their article from embedded JSON. Parse data only,
    // without executing JavaScript or fetching protected content.
    let assignment = regex::Regex::new(r"Fusion\.globalContent\s*=\s*").unwrap();
    if let Some(m) = assignment.find(raw) {
        let mut values =
            serde_json::Deserializer::from_str(&raw[m.end()..]).into_iter::<serde_json::Value>();
        if let Some(Ok(value)) = values.next() {
            if let Some(code) = value
                .pointer("/content_restrictions/content_code")
                .and_then(|v| v.as_str())
            {
                if !["0", "free", "open"].contains(&code) {
                    bail!("구독·접근 제한으로 표시된 기사입니다");
                }
            }
            if let Some(elements) = value.get("content_elements").and_then(|v| v.as_array()) {
                let body = elements
                    .iter()
                    .filter(|v| v.get("type").and_then(|v| v.as_str()) == Some("text"))
                    .filter_map(|v| v.get("content").and_then(|v| v.as_str()))
                    .map(crate::clean)
                    .collect::<Vec<_>>()
                    .join("\n\n");
                if body.chars().count() >= 200 {
                    return Ok(Some(body));
                }
            }
        }
    }
    let selector = Selector::parse("script[type='application/ld+json']").unwrap();
    for node in html.select(&selector) {
        if let Ok(value) =
            serde_json::from_str::<serde_json::Value>(&node.text().collect::<String>())
        {
            if let Some(body) = jsonld_body(&value) {
                if body.chars().count() >= 200 {
                    return Ok(Some(body));
                }
            }
        }
    }
    Ok(None)
}

fn jsonld_body(value: &serde_json::Value) -> Option<String> {
    if value.get("isAccessibleForFree").and_then(|v| v.as_bool()) == Some(false) {
        return None;
    }
    if let Some(body) = value.get("articleBody").and_then(|v| v.as_str()) {
        return Some(crate::clean(body));
    }
    if let Some(items) = value.as_array() {
        return items.iter().find_map(jsonld_body);
    }
    value
        .get("@graph")
        .and_then(|v| v.as_array())
        .and_then(|items| items.iter().find_map(jsonld_body))
}

#[cfg(test)]
mod tests {
    #[test]
    fn extracts_korean_news_body_from_common_container() {
        let line = "국내 개발사가 새 인공지능 모델을 공개했으며 이용자는 기존 서비스에서 변경된 기능을 사용할 수 있다. ";
        let html = format!(
            "<body><nav>메뉴</nav><div id='dic_area'>{}<br>{}</div></body>",
            line.repeat(3),
            line.repeat(3)
        );
        let body = super::extract_html(&html).unwrap();
        assert!(body.contains("인공지능 모델"));
        assert!(!body.contains("메뉴"));
    }
    #[test]
    fn extracts_public_hydration_data_but_rejects_restricted_payload() {
        let body =
            "공개된 한국어 기사 본문이며 인공지능과 게임 관련 발표를 자세히 설명하는 내용이다. "
                .repeat(8);
        let data = serde_json::json!({"content_restrictions":{"content_code":"0"},"content_elements":[{"type":"text","content":body}]});
        let html = format!(
            "<body><script>Fusion.globalContent={};Fusion.other=1;</script></body>",
            data
        );
        assert!(super::extract_html(&html).unwrap().contains("한국어 기사"));
        let restricted = html.replace("\"content_code\":\"0\"", "\"content_code\":\"1\"");
        assert!(super::extract_html(&restricted).is_err());
    }
    #[test]
    fn reads_korean_br_body_without_paragraph_tags() {
        let sentence="국내 게임 개발사가 새로운 협동 게임 업데이트를 발표했다. 이용자는 추가 비용 없이 콘텐츠를 이용할 수 있다. ";
        let html=format!("<body><nav>메뉴</nav><div id='article-view-content-div'>{}<br>{}<script>광고</script></div></body>",sentence.repeat(3),sentence.repeat(3));
        let text = super::extract_html(&html).unwrap();
        assert!(text.contains("협동 게임"));
        assert!(!text.contains("광고"));
        assert!(!text.contains("메뉴"));
    }
    #[test]
    fn decodes_euc_kr_article() {
        let html = "<html><meta charset=euc-kr><body>한국어 게임 업데이트 기사</body></html>";
        let (bytes, _, _) = encoding_rs::EUC_KR.encode(html);
        assert!(super::decode_html(&bytes, "text/html").contains("한국어 게임 업데이트 기사"));
    }
    #[test]
    fn ignores_navigation_and_scripts() {
        let body =
            "This is a detailed paragraph about a new game and its latest update. ".repeat(5);
        let html=format!("<html><body><nav><p>unrelated navigation {}</p></nav><article><script>bad</script><p>{}</p></article></body></html>","menu ".repeat(100),body);
        let result = super::extract_html(&html).unwrap();
        assert!(result.contains("new game"));
        assert!(!result.contains("navigation"));
    }
}
