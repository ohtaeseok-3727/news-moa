//! Precision-first selection of AI product/model capability announcements.
use regex::Regex;
use std::sync::OnceLock;

pub const GOOGLE_QUERY: &str = "(ChatGPT OR Claude OR Gemini OR Codex OR DeepSeek OR Qwen OR Llama OR Copilot OR AI OR 인공지능 OR 업스테이지 OR 챗GPT OR 클로드 OR 제미나이) (출시 OR 공개 OR 업데이트 OR \"새 기능\" OR \"기능 추가\" OR \"API 지원\") -\"활용 사례\" -\"도입 사례\" -투자 -협약 -규제 -수상 when:14d";
pub const OFFICIAL_QUERY: &str = "(site:anthropic.com OR site:ai.meta.com OR site:huggingface.co OR site:developers.googleblog.com) (AI OR model OR API) (launch OR release OR introduce OR update) when:14d";

struct Rules {
    product: Regex,
    ai_context: Regex,
    release: Regex,
    new_capability: Regex,
    capability: Regex,
    excluded: Regex,
    tutorial: Regex,
    adoption: Regex,
    version: Regex,
}

fn rules() -> &'static Rules {
    static RULES: OnceLock<Rules> = OnceLock::new();
    RULES.get_or_init(|| Rules {
        ai_context: Regex::new(r"(?i)(\bai\b|인공지능|생성형)").unwrap(),
        product:Regex::new(r"(?i)(chatgpt|챗\s*gpt|지피티|gpt[- ]?\d|openai|오픈ai|claude|클로드|anthropic|앤트로픽|gemini|제미나이|deepmind|copilot|코파일럿|codex|코덱스|deepseek|딥시크|qwen|llama|라마|hyperclova|하이퍼클로바|clova|클로바|upstage|업스테이지|exaone|엑사원|kimi|키미|minimax|미니맥스|z\.ai|glm[- ]?\d|mistral|미스트랄|gemma|grok|그록|perplexity|퍼플렉시티|midjourney|미드저니|stable diffusion|stability ai|firefly|runway|sora|소라|elevenlabs|cursor|커서|windsurf|ollama|lm studio|hugging face|\bllm\b|ai\s*모델|언어\s*모델|ai\s*에이전트|ai\s*비서|ai\s*어시스턴트|language models?|foundation models?|text.to.image|text.to.video)").unwrap(),
        release:Regex::new(r"(?i)(\b(introduc(?:e|es|ed|ing)|launch(?:es|ed|ing)?|releas(?:e|es|ed|ing)|roll(?:s|ed|ing)? out|unveil(?:s|ed|ing)?|add(?:s|ed|ing)?|updat(?:e|es|ed|ing)|upgrad(?:e|es|ed|ing)|now available|now supports?|support for|preview|beta|changelog|open.sourced)\b|공개|출시|추가|업데이트|업그레이드|지원\s*확대|탑재|프리뷰|베타|신기능)").unwrap(),
        new_capability:Regex::new(r"(?i)((새로운|신규|새)\s*(?:AI\s*)?(기능|모델|버전|모드|API|도구)|\bnew\s+(?:[\w-]+\s+){0,3}(features?|models?|versions?|modes?|api|tools?|capabilit(?:y|ies))\b)").unwrap(),
        capability:Regex::new(r"(?i)(기능|모드|API|음성|이미지|영상|동영상|검색|메모리|추론|파일|코딩|에이전트|브라우저|\b(features?|modes?|api|voice|image|video|search|memory|reasoning|coding|files?|tools?|agents?|browser|talk|analytics|usage limits|spend controls)\b)").unwrap(),
        excluded:Regex::new(r"(?i)(\b(funding|fundraise|investment|investors?|acquisition|acquires?|lawsuit|regulation|policy|policies|awards?|academy|courses?|training program|training initiative|case stud(?:y|ies)|customer stor(?:y|ies)|customer success|partnership|earnings|board of directors|data ?centers?|workforce|reviews?|benchmarks?|comparison)\b|활용\s*사례|적용\s*사례|도입\s*사례|고객\s*사례|투자\s*(유치|라운드|확대)|자금\s*조달|인수\s*(합병|계약)|파트너십|업무\s*협약|양해각서|교육\s*(과정|프로그램)|특강|수상|규제|법안|소송|정책|윤리\s*(교육|가이드)|이사회|데이터\s*센터|리뷰|사용기|품질\s*저하|성능\s*저하|사용\s*불가|전략\s*공개|계획\s*공개|[0-9]+(?:억|만|천)?\s*명\s*돌파|(?-i:\b[A-Z][A-Za-z]*Bench\b))").unwrap(),
        tutorial:Regex::new(r"(?i)^\s*(how\b|why\b|using\b|what we learned\b|case study\b|\[?(활용|도입|적용|고객)\s*사례)").unwrap(),
        adoption:Regex::new(r"(?i)(\b(adopts?|adoption|deployed at|powered by)\b|도입|적용|활용|구축|실증|현장)").unwrap(),
        version:Regex::new(r"(?i)^(?:claude(?: opus| sonnet| haiku)?|gemini(?: pro| flash)?|gpt|deepseek|qwen|llama|gemma|mistral|grok|클로드|제미나이|딥시크|라마)[- ]?\d+(?:\.\d+)*(?:[- ](?:preview|beta|프리뷰|베타))?\s*$").unwrap(),
    })
}

pub fn is_update(title: &str, source: &str, url: &str) -> bool {
    let r = rules();
    let lower_url = url.to_lowercase();
    if [
        "/customers/",
        "/customer-stories/",
        "/case-studies/",
        "/success-stories/",
    ]
    .iter()
    .any(|part| lower_url.contains(part))
    {
        return false;
    }
    if r.excluded.is_match(title) || r.tutorial.is_match(title) {
        return false;
    }
    let official = source == "OpenAI"
        || source == "Google AI 블로그"
        || reqwest::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .map(|host| {
                matches!(
                    host.as_str(),
                    "openai.com"
                        | "www.openai.com"
                        | "anthropic.com"
                        | "www.anthropic.com"
                        | "blog.google"
                        | "ai.google.dev"
                )
            })
            .unwrap_or(false);
    let explicit_new = r.new_capability.is_match(title);
    if !r.product.is_match(title) {
        if (!official && !(r.ai_context.is_match(title) && explicit_new))
            || (!r.capability.is_match(title) && !explicit_new)
        {
            return false;
        }
        if (source == "Google AI 블로그" || lower_url.contains("blog.google/"))
            && !r.ai_context.is_match(title)
        {
            return false;
        }
    }
    // "도입" can mean either a customer's adoption or a product adding a feature.
    let feature_adoption = r.product.is_match(title)
        && r.capability.is_match(title)
        && (explicit_new || title.contains("기능 도입") || title.contains("모드 도입"));
    if r.adoption.is_match(title) && !feature_adoption {
        return false;
    }
    r.release.is_match(title)
        || explicit_new
        || r.version.is_match(title)
        || feature_adoption
        || (official && title.to_lowercase().contains("new") && r.capability.is_match(title))
}

pub fn selected(title: &str, source: &str, url: &str, published: &str, collected: &str) -> bool {
    is_update(title, source, url) && recent_enough(published, collected, chrono::Utc::now())
}

fn recent_enough(published: &str, collected: &str, now: chrono::DateTime<chrono::Utc>) -> bool {
    chrono::DateTime::parse_from_rfc3339(published)
        .or_else(|_| chrono::DateTime::parse_from_rfc3339(collected))
        .map(|date| date.with_timezone(&chrono::Utc) >= now - chrono::Duration::days(14))
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    #[test]
    fn unknown_provider_can_announce_new_ai_model() {
        assert!(super::is_update(
            "새 AI 모델 출시",
            "뉴스",
            "https://example.org/article"
        ));
        assert!(!super::is_update(
            "병원에 AI 모델 도입",
            "뉴스",
            "https://example.org/article"
        ));
    }
    #[test]
    fn accepts_product_model_api_and_feature_announcements() {
        for title in [
            "챗GPT, 새 음성 모드 공개",
            "Claude Code adds background agents",
            "Introducing GPT-5.6",
            "Gemini API now supports image generation",
            "딥시크, 새 추론 모델 출시",
            "ChatGPT에 메모리 기능 도입",
            "GPT-5.6",
            "LLM SDK 2.0 released",
            "하이퍼클로바X, 새 모델 공개",
        ] {
            assert!(
                super::is_update(
                    title,
                    "Google 뉴스 · AI 업데이트",
                    "https://news.google.com/a"
                ),
                "{title}"
            );
        }
        assert!(super::is_update(
            "New usage analytics and updated spend controls for enterprises",
            "OpenAI",
            "https://openai.com/index/analytics"
        ));
        assert!(super::is_update(
            "OpenAI frontier models and Codex are now available on AWS",
            "OpenAI",
            "https://openai.com/index/aws"
        ));
    }
    #[test]
    fn excludes_real_world_adoption_business_policy_and_general_discussion() {
        for title in [
            "인공지능이 포트홀·블랙아이스 위험 미리 알린다",
            "AI로 무박 2일 게임 만든다…콘진원, 예비 개발자 경진대회 개최",
            "LG유플, AI 처리 4.5배 높이고 배터리 소모 77% 절감",
            "은행, ChatGPT 도입해 고객 상담 자동화",
            "How avatarin built a 24/7 retail agent with GPT-Realtime",
            "How Preply combines AI and human tutors to personalize learning",
            "OpenAI announces a funding round",
            "OpenAI to acquire Ona",
            "OpenAI launches new Academy courses",
            "Claude Code burns 20% of tokens on a status line",
            "GPT-5.6 review",
            "New AI hub will support hospitals",
            "인공지능기본법 개정 토론회 개최",
            "Claude Code, 업데이트 이후 복잡한 작업에서 사용 불가 수준으로 품질 저하",
            "알트먼 챗GPT 전 세계 10% 사용...8억명 돌파 공개",
            "Introducing LifeSciBench",
        ] {
            assert!(
                !super::is_update(title, "OpenAI", "https://openai.com/index/example"),
                "{title}"
            );
        }
        assert!(!super::is_update(
            "Introducing new ChatGPT workflows",
            "OpenAI",
            "https://openai.com/customer-stories/retailer"
        ));
        assert!(!super::is_update(
            "Get ready for the game with new football features in Search",
            "Google AI 블로그",
            "https://blog.google/innovation-and-ai/football"
        ));
    }
    #[test]
    fn excludes_old_announcements_without_using_collection_time_as_publication_time() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-29T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert!(super::recent_enough("2026-10-20T00:00:00Z", "", now));
        assert!(!super::recent_enough(
            "2023-05-01T00:00:00Z",
            "2026-10-29T00:00:00Z",
            now
        ));
    }
}
