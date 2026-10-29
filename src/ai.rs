use anyhow::{bail, Context, Result};
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::{
    generation::LogitsProcessor,
    models::{marian, t5},
};
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokenizers::Tokenizer;

const SUM_REPO: &str = "csebuetnlp/mT5_multilingual_XLSum";
const SUM_REV: &str = "403d8ec3bf635f61bdcfcdd084449e12591272c7";
const TR_REPO: &str = "Helsinki-NLP/opus-mt-tc-big-en-ko";
const TR_REV: &str = "ae8606b7b29a495f31ce679cee2007f536a3a5ce";
const TOK_REV: &str = "61debba12d30888d9c38e8d952c4a536eeaf457b";

struct Asset {
    folder: &'static str,
    repo: &'static str,
    rev: &'static str,
    file: &'static str,
    size: u64,
    sha: Option<&'static str>,
}
const ASSETS: &[Asset] = &[
    Asset {
        folder: "summary",
        repo: SUM_REPO,
        rev: SUM_REV,
        file: "config.json",
        size: 730,
        sha: None,
    },
    Asset {
        folder: "summary",
        repo: SUM_REPO,
        rev: SUM_REV,
        file: "model.safetensors",
        size: 2329638764,
        sha: Some("c691c8e17e5c83c5893f10a90b64e2a2937eb1e27e9ad0ec9153f544531e3084"),
    },
    Asset {
        folder: "summary",
        repo: "lmz/mt5-tokenizers",
        rev: TOK_REV,
        file: "mt5-base.tokenizer.json",
        size: 16330562,
        sha: Some("87a036c0dfd2d80e1202a7e2961aeee653ff63d67cd369b155c78a6e2003a390"),
    },
    Asset {
        folder: "translation",
        repo: TR_REPO,
        rev: TR_REV,
        file: "config.json",
        size: 1068,
        sha: None,
    },
    Asset {
        folder: "translation",
        repo: TR_REPO,
        rev: TR_REV,
        file: "model.safetensors",
        size: 418346322,
        sha: Some("f7d6ccf642f1672e6b06d46bc406a3f12220b70603f6745dfbce5c097f8511c2"),
    },
    Asset {
        folder: "translation",
        repo: TR_REPO,
        rev: TR_REV,
        file: "source.spm",
        size: 789870,
        sha: Some("3d0591e65c49541d82f48df33d7b322c3d4ee7aa0ee8747f9a7f9355dbf22c95"),
    },
    Asset {
        folder: "translation",
        repo: TR_REPO,
        rev: TR_REV,
        file: "target.spm",
        size: 815483,
        sha: Some("3d2aa641a0890d8966ab8703b109895a4e522713ce99b4a0192bfacb495bc97c"),
    },
    Asset {
        folder: "translation",
        repo: TR_REPO,
        rev: TR_REV,
        file: "vocab.json",
        size: 958876,
        sha: None,
    },
];

pub fn prepare(root: &Path, mut progress: impl FnMut(String)) -> Result<()> {
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(7200))
        .user_agent("NewsMoa/1.0")
        .build()?;
    for a in ASSETS {
        let folder = root.join("models").join(a.folder);
        fs::create_dir_all(&folder)?;
        let target = folder.join(a.file);
        if fs::metadata(&target).map(|m| m.len()).unwrap_or(0) == a.size {
            if let Some(expected) = a.sha {
                progress(format!("모델 확인 중: {}/{}", a.folder, a.file));
                if file_sha(&target)? == expected {
                    continue;
                }
            } else {
                continue;
            }
        }
        let url = format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            a.repo, a.rev, a.file
        );
        progress(format!("모델 다운로드: {}/{}", a.folder, a.file));
        let mut response = client.get(url).send()?.error_for_status()?;
        let part = target.with_extension("download");
        let mut output = File::create(&part)?;
        let mut hash = Sha256::new();
        let mut buffer = vec![0u8; 1024 * 1024];
        let mut total = 0u64;
        let mut last = Instant::now();
        loop {
            let n = response.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > a.size {
                bail!("모델 파일 크기가 예상보다 큽니다: {}", a.file);
            }
            output.write_all(&buffer[..n])?;
            hash.update(&buffer[..n]);
            if last.elapsed() > Duration::from_secs(2) {
                progress(format!(
                    "{} 다운로드 {}% ({}/{} MB)",
                    a.folder,
                    total * 100 / a.size,
                    total / 1_000_000,
                    a.size / 1_000_000
                ));
                last = Instant::now();
            }
        }
        output.sync_all()?;
        drop(output);
        if total != a.size {
            bail!("모델 파일 다운로드가 끝나지 않았습니다. 다시 실행하세요.");
        }
        if let Some(expected) = a.sha {
            if format!("{:x}", hash.finalize()) != expected {
                bail!("모델 파일 검증 실패: {}", a.file);
            }
        }
        if target.exists() {
            fs::remove_file(&target)?;
        }
        fs::rename(part, target)?;
    }
    Ok(())
}

fn file_sha(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub struct Engine {
    root: PathBuf,
}
impl Engine {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
    pub fn run(&self, text: &str, mut progress: impl FnMut(&str)) -> Result<String> {
        let language = whatlang::detect(text)
            .context("본문 언어를 확인할 수 없습니다")?
            .lang();
        if language != whatlang::Lang::Eng && language != whatlang::Lang::Kor {
            bail!("현재 요약은 한국어·영어 원문을 지원합니다");
        }
        progress("원문 요약 중");
        let generated = self.summarize(text)?;
        let fallback = !summary_supported(text, &generated);
        let summary = if fallback {
            extractive_summary(text)
        } else {
            generated
        };
        if std::env::var_os("NEWS_AI_DEBUG").is_some() {
            eprintln!("원문 요약 결과: {summary}");
        }
        if language == whatlang::Lang::Kor {
            return Ok(summary
                + if fallback {
                    "\n[원문에서 핵심 문장을 발췌했습니다.]"
                } else {
                    ""
                });
        }
        progress("요약을 한국어로 번역 중");
        let mut result = self.translate(&summary)?;
        if fallback {
            result.push_str("\n[원문에서 핵심 문장을 발췌한 뒤 번역했습니다.]");
        }
        Ok(result)
    }

    fn summarize(&self, text: &str) -> Result<String> {
        let folder = self.root.join("models/summary");
        let device = Device::Cpu;
        let mut config: t5::Config =
            serde_json::from_slice(&fs::read(folder.join("config.json"))?)?;
        config.use_cache = true;
        let tokenizer = Tokenizer::from_file(folder.join("mt5-base.tokenizer.json"))
            .map_err(anyhow::Error::msg)?;
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(
                &[folder.join("model.safetensors")],
                DType::F32,
                &device,
            )?
        };
        let mut model = t5::T5ForConditionalGeneration::load(vb, &config)?;
        let tokens = tokenizer
            .encode(text, true)
            .map_err(anyhow::Error::msg)?
            .get_ids()
            .to_vec();
        // Summarize every chunk of up to 480 tokens, then return the chunk summaries.
        let mut chunks = Vec::new();
        for chunk in tokens.chunks(480).take(12) {
            model.clear_kv_cache();
            let mut input = chunk.to_vec();
            if input.last() != Some(&(config.eos_token_id as u32)) {
                input.push(config.eos_token_id as u32);
            }
            let encoded = model.encode(&Tensor::new(input.as_slice(), &device)?.unsqueeze(0)?)?;
            let mut ids = vec![config.decoder_start_token_id.unwrap_or(config.pad_token_id) as u32];
            let mut sample = LogitsProcessor::new(0, None, None);
            for index in 0..90 {
                let context = if index == 0 {
                    ids.as_slice()
                } else {
                    &ids[ids.len() - 1..]
                };
                let logits = model
                    .decode(&Tensor::new(context, &device)?.unsqueeze(0)?, &encoded)?
                    .squeeze(0)?;
                let logits = candle_transformers::utils::apply_repeat_penalty(&logits, 1.1, &ids)?;
                let next = sample.sample(&logits)?;
                if next == config.eos_token_id as u32 {
                    break;
                }
                ids.push(next);
            }
            let result = tokenizer.decode(&ids, true).map_err(anyhow::Error::msg)?;
            if !result.trim().is_empty() {
                chunks.push(result);
            }
        }
        if chunks.is_empty() {
            bail!("요약 결과가 비어 있습니다");
        }
        let mut result = chunks.join("\n");
        if tokens.len() > 480 * 12 {
            result.push_str("\n[긴 본문으로 앞부분 5,760토큰만 요약했습니다.]");
        }
        Ok(result)
    }

    fn translate(&self, summary: &str) -> Result<String> {
        let folder = self.root.join("models/translation");
        let mut json: serde_json::Value =
            serde_json::from_slice(&fs::read(folder.join("config.json"))?)?;
        // Transformers omits this field in older Marian configurations.
        if json.get("share_encoder_decoder_embeddings").is_none() {
            json["share_encoder_decoder_embeddings"] = true.into();
        }
        if json.get("decoder_vocab_size").is_none() {
            json["decoder_vocab_size"] = json["vocab_size"].clone();
        }
        let config: marian::Config = serde_json::from_value(json)?;
        let device = Device::Cpu;
        let tokenizer =
            crate::spm::tokenizer(folder.join("source.spm"), folder.join("vocab.json"), true)?;
        let decoder =
            crate::spm::tokenizer(folder.join("target.spm"), folder.join("vocab.json"), false)?;
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(
                &[folder.join("model.safetensors")],
                DType::F32,
                &device,
            )?
        };
        let mut model = marian::MTModel::new(&config, vb)?;
        let mut results = Vec::new();
        for paragraph in summary.lines().filter(|s| !s.trim().is_empty()) {
            if paragraph.starts_with('[') {
                results.push("[긴 본문으로 일부 내용만 요약했습니다.]".into());
                continue;
            }
            model.reset_kv_cache();
            let mut input = tokenizer
                .encode(paragraph, true)
                .map_err(anyhow::Error::msg)?
                .get_ids()
                .to_vec();
            if std::env::var_os("NEWS_AI_DEBUG").is_some() {
                eprintln!("번역 입력 토큰: {:?}", input);
            }
            input.truncate(480);
            input.push(config.eos_token_id);
            let encoded = model
                .encoder()
                .forward(&Tensor::new(input.as_slice(), &device)?.unsqueeze(0)?, 0)?;
            let mut ids = vec![config.decoder_start_token_id];
            let mut sample = LogitsProcessor::new(0, None, None);
            for index in 0..180 {
                let start = if index == 0 { 0 } else { ids.len() - 1 };
                let logits = model
                    .decode(
                        &Tensor::new(&ids[start..], &device)?.unsqueeze(0)?,
                        &encoded,
                        start,
                    )?
                    .squeeze(0)?;
                let next = sample.sample(&logits.get(logits.dim(0)? - 1)?)?;
                if next == config.eos_token_id || next == config.forced_eos_token_id {
                    break;
                }
                ids.push(next);
            }
            let result = decoder.decode(&ids, true).map_err(anyhow::Error::msg)?;
            if result.trim().is_empty() {
                bail!("번역 결과가 비어 있습니다");
            }
            results.push(result);
        }
        Ok(results.join("\n"))
    }
}

fn summary_supported(source: &str, summary: &str) -> bool {
    let lower = source.to_lowercase();
    let entities = regex::Regex::new(r"\b(?:[A-Z][A-Z0-9]{1,}|[0-9]+)\b").unwrap();
    if entities
        .find_iter(summary)
        .any(|m| !lower.contains(&m.as_str().to_lowercase()))
    {
        return false;
    }
    let korean = regex::Regex::new(r"[가-힣]{2,}").unwrap();
    let endings = [
        "에서는",
        "에서도",
        "으로는",
        "이라고",
        "이라는",
        "에서",
        "으로",
        "에게",
        "까지",
        "부터",
        "와",
        "과",
        "이",
        "가",
        "을",
        "를",
        "은",
        "는",
        "의",
        "도",
        "로",
    ];
    let stop = [
        "있다",
        "한다",
        "하는",
        "대한",
        "통해",
        "이번",
        "해당",
        "위한",
        "그리고",
        "그러나",
        "것으로",
        "위해",
    ];
    for line in summary
        .split(['\n', '.', '。'])
        .filter(|s| !s.starts_with('['))
    {
        let tokens: Vec<_> = korean
            .find_iter(line)
            .map(|m| m.as_str())
            .filter(|s| !stop.contains(s))
            .collect();
        if tokens.len() < 2 {
            continue;
        }
        let matched = tokens
            .iter()
            .filter(|token| {
                if lower.contains(**token) {
                    return true;
                }
                endings
                    .iter()
                    .find_map(|ending| {
                        token
                            .strip_suffix(*ending)
                            .filter(|stem| stem.chars().count() >= 2)
                    })
                    .map(|stem| lower.contains(stem))
                    .unwrap_or(false)
            })
            .count();
        if matched * 100 / tokens.len() < 70 {
            return false;
        }
    }
    let words = regex::Regex::new(r"[a-zA-Z]{4,}").unwrap();
    let selected: Vec<_> = words
        .find_iter(summary)
        .map(|m| m.as_str().to_lowercase())
        .collect();
    if selected.len() < 4 {
        return true;
    }
    selected
        .iter()
        .filter(|w| lower.contains(w.as_str()))
        .count()
        * 100
        / selected.len()
        >= 60
}

fn extractive_summary(text: &str) -> String {
    use std::collections::HashMap;
    let split = regex::Regex::new(r"[.!?。！？](?:\s+|$)|\n+").unwrap();
    let sentences: Vec<_> = split
        .split(text)
        .map(str::trim)
        .filter(|s| s.chars().count() >= 35)
        .take(80)
        .collect();
    let words = regex::Regex::new(r"[a-zA-Z]{4,}").unwrap();
    let stop = [
        "this", "that", "with", "from", "have", "will", "they", "their", "there", "been", "were",
        "would", "which", "about", "also", "more", "than",
    ];
    let mut counts: HashMap<String, usize> = HashMap::new();
    for sentence in &sentences {
        for m in words.find_iter(sentence) {
            let word = m.as_str().to_lowercase();
            if !stop.contains(&word.as_str()) {
                *counts.entry(word).or_default() += 1;
            }
        }
    }
    let mut ranked: Vec<_> = sentences
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let tokens: Vec<_> = words
                .find_iter(s)
                .map(|m| m.as_str().to_lowercase())
                .collect();
            let weight = tokens
                .iter()
                .map(|w| counts.get(w).copied().unwrap_or(0).min(5) as f64)
                .sum::<f64>()
                / (tokens.len().max(1) as f64).sqrt()
                + if i < 3 { 4.0 } else { 0.0 };
            (i, weight)
        })
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    ranked.truncate(3);
    ranked.sort_by_key(|(i, _)| *i);
    if ranked.is_empty() {
        return text.chars().take(1000).collect();
    }
    ranked
        .iter()
        .map(|(i, _)| format!("{}.", sentences[*i]))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    #[test]
    fn rejects_invented_names_and_numbers() {
        assert!(!super::summary_supported(
            "Claude Code changes the terminal status line.",
            "The BBC reported 300 errors in Claude Code."
        ));
        assert!(super::summary_supported(
            "A new version of a video game has been released for Windows.",
            "A new video game version has been released."
        ));
        assert!(!super::summary_supported(
            "LG유플러스가 인공지능 토큰 최적화 기술을 발표했다.",
            "LG유플러스가 인공지능 기술을 검토하고 있다.\n텍사스에서 일하는 배우가 있다."
        ));
    }
    #[test]
    #[ignore = "requires model assets in test-data; run --prepare first"]
    fn translates_reference_sentence() {
        let engine = super::Engine::new(std::path::PathBuf::from("test-data"));
        let result = engine.translate("2, 4, 6 etc. are even numbers.").unwrap();
        assert!(result.contains("짝수"), "{result}");
        for n in ["2", "4", "6"] {
            assert!(result.contains(n), "{result}");
        }
    }
}
