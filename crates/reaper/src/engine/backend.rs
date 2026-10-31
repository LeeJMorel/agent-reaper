//! Watcher model backends. Any OpenAI-compatible endpoint (OpenAI, OpenRouter, Ollama,
//! llama.cpp, vLLM, SGLang…) reuses prompt prefixes on its own; Anthropic needs explicit
//! cache breakpoints, one per prefix tier.
//!
//! The watcher is a classifier on the agent's path, so it answers with the verdict and
//! nothing else: a JSON schema whose only allowed ids are the rules', and no reasoning.
//! Measured with qwen3:4b on Ollama (GTX 1070): left to think, it spent its whole token
//! budget reasoning and never answered (an empty answer, so every action passed) after
//! 5 to 14 s; answering to the schema without reasoning, it took 0.4 s and was right.
//! Endpoints that turn either down (OpenAI's non-reasoning models refuse `reasoning_effort`)
//! are asked once more without it, and remembered.
//!
//! A watcher with fallbacks asks them in turn until one answers. One that just failed (not
//! running, unloaded, rate-limited) waits at the back of the line for a minute, so the
//! checks after it don't each spend their time on it first.

use std::time::{Duration, Instant};

use reaper_core::events::now;
use reaper_core::rules::Watcher;
use serde_json::{Value, json};

use super::prefix::Prompt;
use super::store::Store;

/// What OpenAI-compatible requests ask for beyond the prompt, in the order they're dropped
/// when an endpoint refuses them.
const EXTRAS: [&str; 2] = ["reasoning_effort", "response_format"];

pub trait Backend {
    fn complete(&self, prompt: &Prompt, timeout: Duration, max_tokens: u32) -> Result<String, String>;
}

/// All the time a check spends on watchers, fallbacks included, unless the watcher's own
/// timeout is longer: Claude Code gives a hook 30 s.
const BUDGET: Duration = Duration::from_secs(20);
/// How long an endpoint that failed waits at the back of the line.
const DOWN_MS: i64 = 60_000;

pub fn from_config<'a>(watcher: &Watcher, store: &'a Store) -> Option<Box<dyn Backend + 'a>> {
    let mut endpoints: Vec<(String, Box<dyn Backend + 'a>)> = std::iter::once(watcher)
        .chain(&watcher.fallback)
        .filter_map(|w| {
            Some((format!("{} {}", w.base_url.as_deref()?.trim_end_matches('/'), w.model.as_deref()?), one(w, store)?))
        })
        .collect();
    match endpoints.len() {
        0 => None,
        1 => endpoints.pop().map(|(_, backend)| backend),
        _ => Some(Box::new(Chain { endpoints, store })),
    }
}

fn one<'a>(watcher: &Watcher, store: &'a Store) -> Option<Box<dyn Backend + 'a>> {
    let base_url = watcher.base_url.clone()?.trim_end_matches('/').to_string();
    let model = watcher.model.clone()?;
    Some(match watcher.kind.as_deref() {
        Some("anthropic") => Box::new(Anthropic {
            base_url,
            model,
            key: std::env::var(watcher.api_key_env.as_deref().unwrap_or("ANTHROPIC_API_KEY")).unwrap_or_default(),
        }),
        _ => Box::new(OpenAi {
            base_url,
            model,
            key: watcher.api_key_env.as_ref().and_then(|name| std::env::var(name).ok()),
            cache_key: watcher.cache_key.unwrap_or(false),
            store,
        }),
    })
}

/// A watcher and its fallbacks, by endpoint (base URL and model).
struct Chain<'a> {
    endpoints: Vec<(String, Box<dyn Backend + 'a>)>,
    store: &'a Store,
}

impl Backend for Chain<'_> {
    fn complete(&self, prompt: &Prompt, timeout: Duration, max_tokens: u32) -> Result<String, String> {
        let (start, budget) = (Instant::now(), timeout.max(BUDGET));
        let down = self.store.down();
        let mut order: Vec<_> = self.endpoints.iter().collect();
        order.sort_by_key(|(name, _)| down.get(name).is_some_and(|&until| until > now()));
        let mut errors = Vec::new();
        for (name, backend) in order {
            let left = budget.saturating_sub(start.elapsed());
            if left.is_zero() {
                break;
            }
            match backend.complete(prompt, timeout.min(left), max_tokens) {
                Ok(reply) => {
                    self.store.set_down(name, None);
                    return Ok(reply);
                }
                Err(error) => {
                    self.store.set_down(name, Some(now() + DOWN_MS));
                    errors.push(error);
                }
            }
        }
        Err(errors.join("; then "))
    }
}

/// An answer that isn't one: the endpoint said no to the request, with its reason.
const REFUSED: &str = "watcher backend refused the request";

fn post(url: &str, headers: &[(&str, &str)], body: &Value, timeout: Duration) -> Result<Value, String> {
    let agent: ureq::Agent =
        ureq::Agent::config_builder().timeout_global(Some(timeout)).http_status_as_error(false).build().into();
    let mut request = agent.post(url).header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let mut response = request.send(body.to_string()).map_err(|e| format!("watcher backend: {e}"))?;
    let status = response.status().as_u16();
    let text = response.body_mut().read_to_string().map_err(|e| format!("watcher backend: {e}"))?;
    match status {
        400..=499 => Err(format!("{REFUSED} (HTTP {status}): {}", text.chars().take(300).collect::<String>())),
        500.. => Err(format!("watcher backend: HTTP {status}")),
        _ => serde_json::from_str(&text).map_err(|e| format!("watcher backend sent invalid JSON: {e}")),
    }
}

struct OpenAi<'a> {
    base_url: String,
    model: String,
    key: Option<String>,
    cache_key: bool,
    /// Where refusals are remembered.
    store: &'a Store,
}

fn verdict_schema(ids: &[String]) -> Value {
    json!({ "type": "json_schema", "json_schema": { "name": "verdict", "strict": true, "schema": {
        "type": "object",
        "properties": { "violated": { "type": "array", "items": { "type": "string", "enum": ids } } },
        "required": ["violated"],
        "additionalProperties": false,
    } } })
}

impl Backend for OpenAi<'_> {
    fn complete(&self, prompt: &Prompt, timeout: Duration, max_tokens: u32) -> Result<String, String> {
        let system: Vec<&str> = prompt.system.iter().map(String::as_str).filter(|s| !s.is_empty()).collect();
        let mut body = json!({
            "model": self.model,
            "temperature": 0,
            "max_tokens": max_tokens,
            "messages": [
                { "role": "system", "content": system.join("\n\n") },
                { "role": "user", "content": format!("{}\n\n{}", prompt.goal, prompt.action) },
            ],
        });
        if self.cache_key {
            body["prompt_cache_key"] = prompt.prefix_id.clone().into();
        }
        let endpoint = format!("{} {}", self.base_url, self.model);
        let refused = self.store.refused(&endpoint);
        if !refused.iter().any(|r| r == "reasoning_effort") {
            body["reasoning_effort"] = "none".into();
        }
        if !refused.iter().any(|r| r == "response_format") && !prompt.ids.is_empty() {
            body["response_format"] = verdict_schema(&prompt.ids);
        }
        let auth = self.key.as_ref().map(|k| format!("Bearer {k}"));
        let headers: Vec<(&str, &str)> = auth.iter().map(|a| ("authorization", a.as_str())).collect();
        let url = format!("{}/chat/completions", self.base_url);
        let reply = loop {
            match post(&url, &headers, &body, timeout) {
                Err(e) if e.starts_with(REFUSED) => {
                    // Turned down for something extra it asked for: drop it, for good.
                    let map = body.as_object_mut().expect("an object");
                    let Some(extra) = EXTRAS.into_iter().find(|x| map.contains_key(*x) && e.contains(x)) else {
                        return Err(e);
                    };
                    map.remove(extra);
                    self.store.refuse(&endpoint, extra);
                }
                other => break other?,
            }
        };
        Ok(reply["choices"][0]["message"]["content"].as_str().unwrap_or_default().to_string())
    }
}

struct Anthropic {
    base_url: String,
    model: String,
    key: String,
}

impl Backend for Anthropic {
    fn complete(&self, prompt: &Prompt, timeout: Duration, max_tokens: u32) -> Result<String, String> {
        let cached = |text: &str| json!({ "type": "text", "text": text, "cache_control": { "type": "ephemeral" } });
        let [preamble, global, project] = &prompt.system;
        let first = [preamble.as_str(), global.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>();
        let mut system = vec![cached(&first.join("\n\n"))];
        if !project.is_empty() {
            system.push(cached(project));
        }
        let body = json!({
            "model": self.model,
            "max_tokens": max_tokens,
            "temperature": 0,
            "system": system,
            "messages": [{ "role": "user", "content": [cached(&prompt.goal), { "type": "text", "text": prompt.action }] }],
        });
        let headers = [("x-api-key", self.key.as_str()), ("anthropic-version", "2023-06-01")];
        let reply = post(&format!("{}/v1/messages", self.base_url), &headers, &body, timeout)?;
        let text = reply["content"].as_array().into_iter().flatten().filter_map(|b| b["text"].as_str()).collect();
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// An endpoint that refuses `reasoning_effort` the way OpenAI's non-reasoning models do.
    fn picky(bodies: Arc<Mutex<Vec<Value>>>) -> String {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = n.trim().parse().unwrap();
                    }
                    if line.trim().is_empty() {
                        break;
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let body: Value = serde_json::from_slice(&body).unwrap();
                let refuse = body.get("reasoning_effort").is_some();
                bodies.lock().unwrap().push(body);
                let (status, reply) = if refuse {
                    (
                        "400 Bad Request",
                        r#"{"error":{"message":"Unsupported parameter: 'reasoning_effort' is not supported with this model."}}"#,
                    )
                } else {
                    ("200 OK", r#"{"choices":[{"message":{"content":"{\"violated\":[\"prod\"]}"}}]}"#)
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    reply.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        url
    }

    struct Fake {
        name: &'static str,
        answers: bool,
        asked: Arc<Mutex<Vec<&'static str>>>,
    }

    impl Backend for Fake {
        fn complete(&self, _: &Prompt, _: Duration, _: u32) -> Result<String, String> {
            self.asked.lock().unwrap().push(self.name);
            if self.answers { Ok(self.name.into()) } else { Err(format!("{} is down", self.name)) }
        }
    }

    #[test]
    fn a_watcher_that_doesnt_answer_waits_at_the_back_of_the_line() {
        let dir = std::env::temp_dir().join(format!("reaper-chain-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::at(&dir);
        let asked = Arc::new(Mutex::new(Vec::new()));
        let chain = |endpoints: &[(&'static str, bool)]| Chain {
            endpoints: endpoints
                .iter()
                .map(|&(name, answers)| {
                    let fake: Box<dyn Backend> = Box::new(Fake { name, answers, asked: asked.clone() });
                    (name.to_string(), fake)
                })
                .collect(),
            store: &store,
        };
        let prompt = Prompt {
            system: Default::default(),
            goal: String::new(),
            action: String::new(),
            prefix_id: String::new(),
            ids: vec![],
        };
        let ask = |chain: &Chain| {
            asked.lock().unwrap().clear();
            let reply = chain.complete(&prompt, Duration::from_secs(1), 10);
            (reply, asked.lock().unwrap().clone())
        };

        let both = chain(&[("local", false), ("hosted", true)]);
        assert_eq!(ask(&both), (Ok("hosted".into()), vec!["local", "hosted"]));
        assert_eq!(ask(&both), (Ok("hosted".into()), vec!["hosted"]), "the one that failed waits its turn");
        store.set_down("local", Some(0));
        assert_eq!(ask(&chain(&[("local", true), ("hosted", true)])), (Ok("local".into()), vec!["local"]));
        assert!(store.down().is_empty(), "back up, and first again");

        let none = chain(&[("local", false), ("hosted", false)]);
        assert_eq!(ask(&none), (Err("local is down; then hosted is down".into()), vec!["local", "hosted"]));
        assert_eq!(ask(&none).1, vec!["local", "hosted"], "when all are down, all are still asked");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_watcher_that_isnt_running_hands_over_to_its_fallback() {
        let url = picky(Arc::new(Mutex::new(Vec::new())));
        let dir = std::env::temp_dir().join(format!("reaper-fallback-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::at(&dir);
        let config = reaper_core::rules::parse(&format!(
            "watcher: {{ baseURL: \"http://127.0.0.1:1/v1\", model: m, fallback: [{{ baseURL: \"{url}/v1\", model: m }}] }}"
        ))
        .unwrap();
        let backend = from_config(config.watcher.as_ref().unwrap(), &store).unwrap();
        let prompt = Prompt {
            system: Default::default(),
            goal: String::new(),
            action: String::new(),
            prefix_id: String::new(),
            ids: vec![],
        };
        assert_eq!(backend.complete(&prompt, Duration::from_secs(5), 10).unwrap(), r#"{"violated":["prod"]}"#);
        assert!(store.down().contains_key("http://127.0.0.1:1/v1 m"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn answers_to_the_rules_ids_and_drops_what_an_endpoint_refuses_for_good() {
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let url = picky(bodies.clone());
        let dir = std::env::temp_dir().join(format!("reaper-backend-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::at(&dir);
        let config = reaper_core::rules::parse(&format!(
            "watcher: {{ baseURL: \"{url}/v1\", model: m }}\nrules: [{{ id: prod, judge: touching production }}]"
        ))
        .unwrap();
        let (watcher, rule) = (config.watcher.unwrap(), config.rules);
        let prompt = super::super::prefix::layout(&[&rule[0]], &[], "fix", "bash", &json!({ "command": "psql prod" }));
        let backend = from_config(&watcher, &store).unwrap();

        assert_eq!(backend.complete(&prompt, Duration::from_secs(5), 400).unwrap(), r#"{"violated":["prod"]}"#);
        let sent = bodies.lock().unwrap().clone();
        assert_eq!(sent.len(), 2, "refused once, then asked without it");
        assert_eq!(sent[0]["reasoning_effort"], "none");
        assert_eq!(
            sent[1]["response_format"]["json_schema"]["schema"]["properties"]["violated"]["items"]["enum"],
            json!(["prod"])
        );
        // The next check remembers, and asks once.
        backend.complete(&prompt, Duration::from_secs(5), 400).unwrap();
        assert_eq!(bodies.lock().unwrap().len(), 3);
    }
}
