use anyhow::Context;
use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, Request, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::Engine as _;
use chrono::{DateTime, Utc};
use regex::Regex;
use reqwest::blocking::Client;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
mod ai;
mod ai_updates;
mod extract;
mod google_news;
mod safe_fetch;
mod spm;
mod update;

const FEEDS: &[(&str, &str, &str)] = &[
    ("AI", "Google 뉴스 · AI 업데이트", "https://news.google.com/rss/search"),
    ("AI", "Google 뉴스 · AI 공식 발표", "https://news.google.com/rss/search"),
    ("게임", "Google 뉴스 · 게임", "https://news.google.com/rss/search?q=%EA%B2%8C%EC%9E%84+%EC%B6%9C%EC%8B%9C+%EC%97%85%EB%8D%B0%EC%9D%B4%ED%8A%B8&hl=ko&gl=KR&ceid=KR:ko"),
    ("AI", "Google AI 블로그", "https://blog.google/technology/ai/rss/"),
    ("AI", "OpenAI", "https://openai.com/news/rss.xml"),
    ("게임", "PC Gamer", "https://www.pcgamer.com/rss/"),
];

#[derive(Clone)]
struct App {
    db: PathBuf,
    root: PathBuf,
    status: Arc<Mutex<Status>>,
    busy: Arc<AtomicBool>,
    access_key: Arc<String>,
}
#[derive(Serialize, Default, Clone)]
struct Status {
    running: bool,
    last_run: Option<String>,
    errors: Vec<String>,
    ai: String,
    sources: Vec<String>,
    update: String,
    update_checked: Option<String>,
}
#[derive(Serialize)]
struct Article {
    url: String,
    title: String,
    category: String,
    source: String,
    description: String,
    published: String,
    collected: String,
    summary: Option<String>,
    ai_state: Option<String>,
    resolved_url: Option<String>,
}

fn connect(path: &PathBuf) -> anyhow::Result<Connection> {
    let db = Connection::open(path)?;
    db.busy_timeout(Duration::from_secs(15))?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS articles (url TEXT PRIMARY KEY, title TEXT, category TEXT, source TEXT, description TEXT, published TEXT, collected TEXT, summary TEXT, translation TEXT, ai_state TEXT)")?;
    let mut stmt = db.prepare("PRAGMA table_info(articles)")?;
    let cols = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    for name in [
        "summary",
        "translation",
        "ai_state",
        "ai_version",
        "resolved_url",
    ] {
        if !cols.iter().any(|c| c == name) {
            db.execute(&format!("ALTER TABLE articles ADD COLUMN {name} TEXT"), [])?;
        }
    }
    if !cols.iter().any(|c| c == "in_scope") {
        db.execute(
            "ALTER TABLE articles ADD COLUMN in_scope INTEGER NOT NULL DEFAULT 1",
            [],
        )?;
    }
    Ok(db)
}

fn reclassify_ai(db: &Connection) -> anyhow::Result<()> {
    let rows = {
        let mut stmt = db.prepare(
            "SELECT url,title,source,published,collected FROM articles WHERE category='AI'",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    let tx = db.unchecked_transaction()?;
    for (url, title, source, published, collected) in rows {
        tx.execute(
            "UPDATE articles SET in_scope=?1 WHERE url=?2",
            params![
                i32::from(ai_updates::selected(
                    &title, &source, &url, &published, &collected
                )),
                url
            ],
        )?;
    }
    tx.commit()?;
    Ok(())
}

fn clean(s: &str) -> String {
    let tags = Regex::new(r"<[^>]+>").unwrap();
    html_escape::decode_html_entities(&tags.replace_all(s, " "))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn access_key(root: &std::path::Path) -> anyhow::Result<String> {
    let file = root.join("access-key.txt");
    if file.exists() {
        let key = std::fs::read_to_string(file)?.trim().to_owned();
        if key.len() != 24 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
            anyhow::bail!("access-key.txt의 접속 암호가 올바르지 않습니다");
        }
        return Ok(key);
    }
    let bytes: [u8; 12] = rand::random();
    let key: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::write(file, &key)?;
    Ok(key)
}

async fn authorize(State(app): State<App>, req: Request<axum::body::Body>, next: Next) -> Response {
    let allowed = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Basic "))
        .and_then(|v| base64::engine::general_purpose::STANDARD.decode(v).ok())
        .map(|v| v == format!("news:{}", app.access_key).as_bytes())
        .unwrap_or(false);
    if allowed {
        return next.run(req).await;
    }
    (
        StatusCode::UNAUTHORIZED,
        [(
            header::WWW_AUTHENTICATE,
            "Basic realm=\"News Moa\", charset=\"UTF-8\"",
        )],
        "접속하려면 본체의 access-key.txt 암호가 필요합니다.",
    )
        .into_response()
}

fn save(db: &Connection, a: &Article) -> anyhow::Result<()> {
    let scope = a.category != "AI"
        || ai_updates::selected(&a.title, &a.source, &a.url, &a.published, &a.collected);
    db.execute("INSERT OR IGNORE INTO articles (url,title,category,source,description,published,collected,in_scope) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)", params![a.url,a.title,a.category,a.source,a.description,a.published,a.collected, i32::from(scope)])?;
    Ok(())
}

fn collect(app: &App) -> anyhow::Result<Vec<String>> {
    let client = Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent("NewsMoa/0.1")
        .build()?;
    let db = connect(&app.db)?;
    let mut errors = Vec::new();
    for (category, source, url) in FEEDS {
        let run = || -> anyhow::Result<()> {
            let mut bytes = Vec::new();
            let request = client.get(*url);
            let request = if source.starts_with("Google 뉴스 · AI ") {
                request.query(&[
                    (
                        "q",
                        if *source == "Google 뉴스 · AI 공식 발표" {
                            ai_updates::OFFICIAL_QUERY
                        } else {
                            ai_updates::GOOGLE_QUERY
                        },
                    ),
                    ("hl", "ko"),
                    ("gl", "KR"),
                    ("ceid", "KR:ko"),
                ])
            } else {
                request
            };
            request
                .send()?
                .error_for_status()?
                .take(3_000_001)
                .read_to_end(&mut bytes)?;
            if bytes.len() > 3_000_000 {
                anyhow::bail!("피드가 너무 큽니다")
            }
            let feed = rss::Channel::read_from(&bytes[..])?;
            for item in feed.items().iter().take(50) {
                let link = item.link().unwrap_or("");
                if !(link.starts_with("https://") || link.starts_with("http://")) {
                    continue;
                }
                let published = item
                    .pub_date()
                    .and_then(|d| DateTime::parse_from_rfc2822(d).ok())
                    .map(|d| d.with_timezone(&Utc).to_rfc3339())
                    .unwrap_or_default();
                let article = Article {
                    url: link.into(),
                    title: clean(item.title().unwrap_or("")),
                    category: (*category).into(),
                    source: (*source).into(),
                    description: clean(item.description().unwrap_or(""))
                        .chars()
                        .take(600)
                        .collect(),
                    published,
                    collected: Utc::now().to_rfc3339(),
                    summary: None,
                    ai_state: None,
                    resolved_url: None,
                };
                save(&db, &article)?;
            }
            Ok(())
        };
        match run() {
            Ok(()) => app
                .status
                .lock()
                .unwrap()
                .sources
                .push(format!("{source}: 성공")),
            Err(e) => {
                app.status
                    .lock()
                    .unwrap()
                    .sources
                    .push(format!("{source}: 실패"));
                errors.push(format!("{source}: {e}"));
            }
        }
    }
    let hacker_news = || -> anyhow::Result<()> {
        let ids: Vec<u64> = client
            .get("https://hacker-news.firebaseio.com/v0/topstories.json")
            .send()?
            .error_for_status()?
            .json()?;
        let ai = Regex::new(
            r"(?i)\b(ai|llm|gpt|chatgpt|claude|gemini|artificial intelligence|machine learning)\b",
        )?;
        let games = Regex::new(
            r"(?i)\b(game|games|gaming|videogame|video game|steam|playstation|nintendo|xbox)\b",
        )?;
        for id in ids.into_iter().take(100) {
            let item: serde_json::Value = match client
                .get(format!(
                    "https://hacker-news.firebaseio.com/v0/item/{id}.json"
                ))
                .send()
                .and_then(|r| r.error_for_status())
                .and_then(|r| r.json())
            {
                Ok(x) => x,
                Err(_) => continue,
            };
            if item["deleted"].as_bool().unwrap_or(false) || item["dead"].as_bool().unwrap_or(false)
            {
                continue;
            }
            let title = item["title"].as_str().unwrap_or("");
            let category = if ai.is_match(title)
                || ai_updates::is_update(
                    title,
                    "Hacker News · 커뮤니티",
                    "https://news.ycombinator.com/",
                ) {
                "AI"
            } else if games.is_match(title) {
                "게임"
            } else {
                continue;
            };
            let desc = format!(
                "인기 토론 · 추천 {} · 댓글 {}",
                item["score"].as_u64().unwrap_or(0),
                item["descendants"].as_u64().unwrap_or(0)
            );
            let article = Article {
                url: format!("https://news.ycombinator.com/item?id={id}"),
                title: clean(title),
                category: category.into(),
                source: "Hacker News · 커뮤니티".into(),
                description: desc,
                published: DateTime::from_timestamp(item["time"].as_i64().unwrap_or(0), 0)
                    .map(|d| d.to_rfc3339())
                    .unwrap_or_default(),
                collected: Utc::now().to_rfc3339(),
                summary: None,
                ai_state: None,
                resolved_url: None,
            };
            save(&db, &article)?;
            db.execute(
                "UPDATE articles SET description=?1 WHERE url=?2",
                params![article.description, article.url],
            )?;
        }
        Ok(())
    };
    match hacker_news() {
        Ok(()) => app
            .status
            .lock()
            .unwrap()
            .sources
            .push("Hacker News: 성공".into()),
        Err(e) => {
            app.status
                .lock()
                .unwrap()
                .sources
                .push("Hacker News: 실패".into());
            errors.push(format!("Hacker News: {e}"));
        }
    }
    reclassify_ai(&db)?;
    Ok(errors)
}

fn trigger(app: App) {
    if app.busy.swap(true, Ordering::SeqCst) {
        return;
    }
    app.status.lock().unwrap().running = true;
    app.status.lock().unwrap().sources.clear();
    std::thread::spawn(move || {
        let outcome = collect(&app);
        let mut status = app.status.lock().unwrap();
        status.errors = outcome.unwrap_or_else(|e| vec![e.to_string()]);
        status.last_run = Some(Utc::now().to_rfc3339());
        status.running = false;
        app.busy.store(false, Ordering::SeqCst);
    });
}

#[derive(Deserialize)]
struct NewsQuery {
    review: Option<u8>,
}
async fn news(State(app): State<App>, Query(query): Query<NewsQuery>) -> impl IntoResponse {
    let result = (|| -> anyhow::Result<(Vec<Article>, i64, i64, Vec<(String, i64)>)> {
        let db = connect(&app.db)?;
        let clause = if query.review == Some(1) {
            "category='AI' AND in_scope=0 AND collected >= datetime('now','-14 days')"
        } else {
            "in_scope=1"
        };
        let mut stmt = db.prepare(&format!("SELECT url,title,category,source,description,published,collected,CASE WHEN ai_version='rust-v2' THEN summary ELSE NULL END,ai_state,resolved_url FROM articles WHERE {clause} ORDER BY COALESCE(NULLIF(published,''),collected) DESC LIMIT 500"))?;
        let rows = stmt.query_map([], |r| {
            Ok(Article {
                url: r.get(0)?,
                title: r.get(1)?,
                category: r.get(2)?,
                source: r.get(3)?,
                description: r.get(4)?,
                published: r.get(5)?,
                collected: r.get(6)?,
                summary: r.get(7)?,
                ai_state: r.get(8)?,
                resolved_url: r.get(9)?,
            })
        })?;
        let articles = rows.collect::<Result<Vec<_>, _>>()?;
        let pending = db.query_row(
            "SELECT COUNT(*) FROM articles WHERE in_scope=1 AND ai_state IS NULL",
            [],
            |r| r.get(0),
        )?;
        let failed = db.query_row(
            "SELECT COUNT(*) FROM articles WHERE in_scope=1 AND ai_state LIKE '실패:%'",
            [],
            |r| r.get(0),
        )?;
        let mut failures = db.prepare("SELECT source, COALESCE(resolved_url, url) FROM articles WHERE in_scope=1 AND ai_state LIKE '실패:%'")?;
        let mut counts = std::collections::BTreeMap::<String, i64>::new();
        for row in
            failures.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        {
            let (source, url) = row?;
            let host = reqwest::Url::parse(&url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_string));
            let label = match host.as_deref() {
                Some("news.google.com") => format!("{source} (원문 미확인)"),
                Some(h) => h.to_string(),
                None => source,
            };
            *counts.entry(label).or_default() += 1;
        }
        let mut by_source: Vec<_> = counts.into_iter().collect();
        by_source.sort_by(|a, b| b.1.cmp(&a.1));
        by_source.truncate(8);
        Ok((articles, pending, failed, by_source))
    })();
    match result {
        Ok((rows, pending, failed, by_source)) => (
            StatusCode::OK,
            Json(
                serde_json::json!({"articles":rows,"status":app.status.lock().unwrap().clone(),"pending":pending,"failed":failed,"failures_by_source":by_source}),
            ),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":e.to_string()})),
        ),
    }
}

async fn refresh(State(app): State<App>, headers: HeaderMap) -> StatusCode {
    if let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) {
        let host = headers
            .get("host")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if origin != format!("http://{host}") {
            return StatusCode::FORBIDDEN;
        }
    }
    trigger(app);
    StatusCode::ACCEPTED
}

async fn retry(State(app): State<App>, headers: HeaderMap) -> StatusCode {
    if let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) {
        if origin
            != format!(
                "http://{}",
                headers
                    .get("host")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
            )
        {
            return StatusCode::FORBIDDEN;
        }
    }
    match connect(&app.db).and_then(|db| {
        Ok(db.execute(
            "UPDATE articles SET ai_state=NULL WHERE ai_state LIKE '실패:%' AND in_scope=1",
            [],
        )?)
    }) {
        Ok(_) => StatusCode::OK,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn ai_worker(app: App) {
    loop {
        match ai::prepare(&app.root, |s| {
            println!("{s}");
            app.status.lock().unwrap().ai = s;
        }) {
            Ok(()) => break,
            Err(e) => {
                app.status.lock().unwrap().ai = format!("모델 준비 실패 (60초 후 재시도): {e}");
                std::thread::sleep(Duration::from_secs(60));
            }
        }
    }
    let engine = ai::Engine::new(app.root.clone());
    let client = match Client::builder()
        .timeout(Duration::from_secs(25))
        .user_agent("Mozilla/5.0 NewsMoa/1.0")
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            app.status.lock().unwrap().ai = e.to_string();
            return;
        }
    };
    loop {
        let job = (|| -> anyhow::Result<Option<(String, String, Option<String>)>> {
            let db = connect(&app.db)?;
            let mut stmt=db.prepare("SELECT url,title,resolved_url FROM articles WHERE ai_state IS NULL AND in_scope=1 ORDER BY COALESCE(NULLIF(published,''),collected) DESC LIMIT 1")?;
            let mut rows = stmt.query([])?;
            Ok(if let Some(row) = rows.next()? {
                Some((row.get(0)?, row.get(1)?, row.get(2)?))
            } else {
                None
            })
        })();
        let (url, title, cached_url) = match job {
            Ok(Some(x)) => x,
            Ok(None) => {
                app.status.lock().unwrap().ai = "한국어 요약 대기 중".into();
                std::thread::sleep(Duration::from_secs(10));
                continue;
            }
            Err(e) => {
                app.status.lock().unwrap().ai = format!("저장소 오류: {e}");
                std::thread::sleep(Duration::from_secs(10));
                continue;
            }
        };
        app.status.lock().unwrap().ai = format!("본문 수집 중: {title}");
        let result = (|| -> anyhow::Result<String> {
            let target = if let Some(cached) = cached_url {
                cached
            } else if reqwest::Url::parse(&url)?.host_str() == Some("news.google.com") {
                let target = google_news::resolve(&client, &url)
                    .context("Google 원문 링크 연결 실패")?
                    .to_string();
                connect(&app.db)?.execute(
                    "UPDATE articles SET resolved_url=?1 WHERE url=?2",
                    params![target, url],
                )?;
                target
            } else {
                url.clone()
            };
            extract::article(&client, &target)
        })()
        .and_then(|body| {
            engine.run(&body, |s| {
                app.status.lock().unwrap().ai = format!("{s}: {title}");
            })
        });
        let (summary, state) = match result {
            Ok(s) => (Some(s), "완료 · 원문 요약 → 한국어".to_string()),
            Err(e) => (None, format!("실패: {e:#}")),
        };
        if let Ok(db) = connect(&app.db) {
            if let Err(e) = db.execute(
                "UPDATE articles SET summary=?1,ai_state=?2,ai_version='rust-v2' WHERE url=?3",
                params![summary, state, url],
            ) {
                app.status.lock().unwrap().ai = format!("요약 저장 실패: {e}");
                std::thread::sleep(Duration::from_secs(10));
            }
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let os_args: Vec<_> = std::env::args_os().collect();
    if os_args.get(1).is_some_and(|a| a == "--apply-update") {
        return update::apply(&os_args);
    }
    if std::env::var_os("RAYON_NUM_THREADS").is_none() {
        let threads = std::thread::available_parallelism()
            .map(|n| (n.get() / 2).clamp(1, 4))
            .unwrap_or(2);
        std::env::set_var("RAYON_NUM_THREADS", threads.to_string());
    }
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|s| s == "--version") {
        println!("뉴스 모아 {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let root = if let Some(pos) = args.iter().position(|s| s == "--data-dir") {
        PathBuf::from(
            args.get(pos + 1)
                .ok_or_else(|| anyhow::anyhow!("--data-dir에 폴더 경로가 필요합니다"))?,
        )
    } else {
        std::env::current_exe()?.parent().unwrap().to_path_buf()
    };
    std::fs::create_dir_all(&root)?;
    if let Some(pos) = args
        .iter()
        .position(|s| s == "--check-url" || s == "--summarize-url")
    {
        let summarize = args[pos] == "--summarize-url";
        let url = args.get(pos + 1).context("기사 주소가 필요합니다")?.clone();
        let result=tokio::task::spawn_blocking(move||->anyhow::Result<String>{
            let client=Client::builder().timeout(Duration::from_secs(30)).user_agent("Mozilla/5.0 NewsMoa/1.1").build()?;
            let target=if reqwest::Url::parse(&url)?.host_str()==Some("news.google.com"){google_news::resolve(&client,&url)?.to_string()}else{url.clone()};
            let body=extract::article(&client,&target)?;
            if summarize {
                eprintln!("원문: {target}\n본문 {}자 추출",body.chars().count());
                ai::prepare(&root,|s|eprintln!("{s}"))?;
                ai::Engine::new(root).run(&body,|s|eprintln!("{s}"))
            } else {Ok(serde_json::json!({"original_url":url,"resolved_url":target,"body_chars":body.chars().count(),"excerpt":body.chars().take(160).collect::<String>()}).to_string())}
        }).await??;
        println!("{result}");
        return Ok(());
    }
    if args.iter().any(|s| s == "--prepare") {
        tokio::task::spawn_blocking(move || ai::prepare(&root, |s| println!("{s}"))).await??;
        return Ok(());
    }
    if let Some(pos) = args.iter().position(|s| s == "--summarize-file") {
        let text = std::fs::read_to_string(
            args.get(pos + 1)
                .ok_or_else(|| anyhow::anyhow!("파일 경로 필요"))?,
        )?;
        let result = tokio::task::spawn_blocking(move || {
            ai::prepare(&root, |s| eprintln!("{s}"))?;
            ai::Engine::new(root).run(&text, |s| eprintln!("{s}"))
        })
        .await??;
        println!("{result}");
        return Ok(());
    }
    let app = App {
        db: root.join("news.db"),
        access_key: Arc::new(access_key(&root)?),
        root,
        status: Arc::new(Mutex::new(Status {
            ai: "모델 확인 대기 중".into(),
            ..Default::default()
        })),
        busy: Arc::new(AtomicBool::new(false)),
    };
    let db = connect(&app.db)?;
    reclassify_ai(&db)?;
    db.execute(
        "UPDATE articles SET ai_state=NULL WHERE COALESCE(ai_version,'')!='rust-v2'",
        [],
    )?;
    db.execute(
        "UPDATE articles SET ai_state=NULL WHERE ai_state LIKE '실패: Google 뉴스 중간 링크:%'",
        [],
    )?;
    drop(db);
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8765")
        .await
        .context("포트 8765를 열 수 없습니다. 이미 뉴스 모아가 실행 중인지 확인하세요.")?;
    if !args.iter().any(|x| x == "--skip-update-once") {
        let update_status = app.status.clone();
        std::thread::spawn(move || {
            update::watch(move |s| {
                let mut status = update_status.lock().unwrap();
                status.update = s;
                status.update_checked = Some(Utc::now().to_rfc3339());
            })
        });
    } else {
        app.status.lock().unwrap().update = "이전 버전 복구 후 자동 확인 일시 중지".into();
    }
    let worker = app.clone();
    std::thread::spawn(move || ai_worker(worker));
    trigger(app.clone());
    let scheduler = app.clone();
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_secs(3600));
        timer.tick().await;
        loop {
            timer.tick().await;
            trigger(scheduler.clone());
        }
    });
    let router = Router::new()
        .route("/", get(|| async { Html(include_str!("../ui.html")) }))
        .route("/api/news", get(news))
        .route("/api/refresh", post(refresh))
        .route("/api/retry", post(retry))
        .layer(middleware::from_fn_with_state(app.clone(), authorize))
        .with_state(app);
    if let Some(pos) = args.iter().position(|x| x == "--update-health") {
        if let Some(marker) = args.get(pos + 1) {
            std::fs::write(marker, b"ready")?;
        }
    }
    println!("뉴스 모아: http://localhost:8765 · 아이디 news · 암호는 access-key.txt 참조");
    axum::serve(listener, router).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hides_existing_ai_case_studies_without_deleting_history_or_games() {
        let path = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("scope-{}.db", std::process::id()));
        let db = connect(&path).unwrap();
        for (id, title, category) in [
            (
                "case",
                "인공지능이 포트홀·블랙아이스 위험 미리 알린다",
                "AI",
            ),
            ("update", "챗GPT, 새 음성 모드 공개", "AI"),
            ("game", "게임 신규 지역 업데이트", "게임"),
        ] {
            db.execute("INSERT INTO articles (url,title,category,source,description,published,collected) VALUES (?1,?2,?3,'Google 뉴스','','','')",params![format!("https://example.org/{id}"),title,category]).unwrap();
        }
        reclassify_ai(&db).unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM articles", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            3
        );
        assert_eq!(
            db.query_row(
                "SELECT in_scope FROM articles WHERE url='https://example.org/case'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM articles WHERE in_scope=1", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap(),
            2
        );
        let a = Article {
            url: "https://example.org/new-case".into(),
            title: "How a bank uses ChatGPT for customer support".into(),
            category: "AI".into(),
            source: "OpenAI".into(),
            description: "".into(),
            published: "".into(),
            collected: "".into(),
            summary: None,
            ai_state: None,
            resolved_url: None,
        };
        save(&db, &a).unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM articles", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            4
        );
        assert_eq!(
            db.query_row(
                "SELECT in_scope FROM articles WHERE url='https://example.org/new-case'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn upgrades_original_database_without_losing_articles() {
        let path = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("migration-{}.db", std::process::id()));
        let old = Connection::open(&path).unwrap();
        old.execute_batch("CREATE TABLE articles(url TEXT PRIMARY KEY,title TEXT,category TEXT,source TEXT,description TEXT,published TEXT,collected TEXT);INSERT INTO articles VALUES('https://example.org/a','title','AI','source','desc','','2026-10-28T00:00:00Z')").unwrap();
        drop(old);
        let db = connect(&path).unwrap();
        assert_eq!(
            db.query_row("SELECT title FROM articles", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "title"
        );
        assert!(db
            .prepare("SELECT summary,ai_version,ai_state FROM articles")
            .is_ok());
        let a = Article {
            url: "https://example.org/a".into(),
            title: "replacement".into(),
            category: "AI".into(),
            source: "x".into(),
            description: "x".into(),
            published: "".into(),
            collected: "".into(),
            summary: None,
            ai_state: None,
            resolved_url: None,
        };
        save(&db, &a).unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM articles", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
}
