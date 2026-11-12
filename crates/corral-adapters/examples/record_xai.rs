//! Records real xAI exchanges as cassettes and checks the decoder against them (SK-4, Option B).
//!
//! Run on a Mac with an xAI key saved in Corral (the "xAI API key" form):
//!
//!     cargo run -p corral-adapters --example record_xai -- [model]
//!
//! It sends one short prompt with your key (a fraction of a cent) and one with a made-up key
//! (rejected, free), writes `tests/cassettes/xai/recorded/*.json`, and reports whether the decoder
//! understood every event. It never writes the key or any request header, replaces reasoning
//! `encrypted_content` with `<redacted>`, and refuses to save a file that contains the key.
//! macOS may ask once to let this program read the `corral.xai` Keychain item.

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("record_xai reads the key from the macOS Keychain; run it on a Mac.");
    std::process::exit(1);
}

#[cfg(target_os = "macos")]
fn main() {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("couldn't start: {e}");
            std::process::exit(1);
        }
    };
    if let Err(message) = runtime.block_on(mac::run()) {
        eprintln!("record_xai: {message}");
        std::process::exit(1);
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use corral_adapters::cassette::{Cassette, RecordedResponse};
    use corral_adapters::responses::{ResponseDecoder, ResponsesConfig};
    use corral_adapters::sse::SseParser;
    use corral_adapters::transport::{ReqwestTransport, Transport};
    use corral_core::event::EventBody;
    use corral_secrets::{KeychainStore, Provider, Secret, resolve};
    use futures_util::StreamExt;
    use serde_json::Value;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    const PROMPT: &str = "Reply with exactly these four words: Corral is ready now.";
    const MADE_UP_KEY: &str = "xai-not-a-real-key-made-up-for-recording-0000"; // gitleaks:allow (made-up)

    pub async fn run() -> Result<(), String> {
        let model = std::env::args().nth(1).unwrap_or_else(|| "grok-4.7".into());
        let key = resolve(&KeychainStore::corral(), Provider::Xai).map_err(|e| {
            format!("{e}. Save it in Corral first (just dev, then the xAI API key form).")
        })?;
        let mut config = ResponsesConfig::xai(&model);
        config.max_output_tokens = 2048;
        let transport = ReqwestTransport::new().map_err(|e| e.to_string())?;
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/cassettes/xai/recorded");
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("couldn't create {}: {e}", dir.display()))?;

        println!("Recording a short answer from {model} (your key; a fraction of a cent)…");
        let text = record(&transport, &config, &key).await?;
        let made_up = Secret::new(MADE_UP_KEY).map_err(|e| e.to_string())?;
        println!("Recording a rejected request (made-up key; free)…");
        let rejected = record(&transport, &config, &made_up).await?;

        for (name, description, response) in [
            (
                format!("text-{model}.json"),
                format!("A real short answer from {model}, recorded by record_xai."),
                text.clone(),
            ),
            (
                "unauthorized.json".to_owned(),
                "A real rejection of a made-up key, recorded by record_xai.".to_owned(),
                rejected,
            ),
        ] {
            let cassette = Cassette {
                description,
                responses: vec![response],
            };
            let json = serde_json::to_string_pretty(&cassette).map_err(|e| e.to_string())?;
            if json.contains(key.expose()) {
                return Err(format!("{name} would contain your key; nothing was saved"));
            }
            let path = dir.join(&name);
            std::fs::write(&path, json + "\n")
                .map_err(|e| format!("couldn't write {}: {e}", path.display()))?;
            println!("Saved {}", path.display());
        }
        check(&text)
    }

    /// One request; the response as a cassette entry, with reasoning content redacted.
    async fn record(
        transport: &ReqwestTransport,
        config: &ResponsesConfig,
        key: &Secret,
    ) -> Result<RecordedResponse, String> {
        let body = config.request_body(PROMPT);
        let response = transport
            .post_json(&config.url(), key, &body)
            .await
            .map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        let mut stream = response.body;
        while let Some(chunk) = stream.next().await {
            bytes.extend_from_slice(&chunk.map_err(|e| e.to_string())?);
        }
        let text = String::from_utf8(bytes).map_err(|_| "the response wasn't UTF-8".to_owned())?;
        let lines = text
            .strip_suffix('\n')
            .unwrap_or(&text)
            .split('\n')
            .map(|line| line.trim_end_matches('\r'))
            .map(redact_line)
            .collect();
        Ok(RecordedResponse {
            status: response.status,
            retry_after_secs: response.retry_after.map(|d| d.as_secs()),
            stall_after_body: false,
            body: lines,
        })
    }

    /// Replaces every `encrypted_content` value in a `data:` line's JSON with `<redacted>`.
    fn redact_line(line: &str) -> String {
        let Some(json) = line.strip_prefix("data: ") else {
            return line.to_owned();
        };
        match serde_json::from_str::<Value>(json) {
            Ok(mut value) => {
                redact(&mut value);
                format!("data: {value}")
            }
            Err(_) => line.to_owned(),
        }
    }

    fn redact(value: &mut Value) {
        match value {
            Value::Object(map) => {
                for (k, v) in map.iter_mut() {
                    if k == "encrypted_content" && v.is_string() {
                        *v = Value::String("<redacted>".into());
                    } else {
                        redact(v);
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(redact),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }

    /// Runs the recorded answer through the decoder and reports what it saw.
    fn check(text: &RecordedResponse) -> Result<(), String> {
        println!("\nHTTP {}", text.status);
        let mut parser = SseParser::default();
        let mut decoder = ResponseDecoder::default();
        let mut seen: BTreeMap<String, usize> = BTreeMap::new();
        let mut out = Vec::new();
        for event in parser.push(&text.body_bytes()).map_err(|e| e.to_string())? {
            let kind = serde_json::from_str::<Value>(&event.data)
                .ok()
                .and_then(|v| v.get("type").and_then(Value::as_str).map(str::to_owned))
                .unwrap_or_else(|| event.event.clone().unwrap_or_else(|| "(no type)".into()));
            *seen.entry(kind).or_default() += 1;
            out.extend(decoder.on_event(&event));
        }
        println!("Stream events from xAI:");
        for (kind, n) in &seen {
            println!("  {n:>3} × {kind}");
        }
        let kinds: Vec<&str> = out.iter().map(|b| b.kind().as_str()).collect();
        println!("Corral events: {}", kinds.join(", "));
        for body in &out {
            if let EventBody::ModelResponse(r) = body {
                println!("Answer: {:?} (finish: {:?})", r.text, r.finish_reason);
            }
            if let EventBody::UsageReported(u) = body {
                println!(
                    "Usage: {} in, {} out, ${:.6}",
                    u.input_tokens,
                    u.output_tokens,
                    u.cost_micro_usd as f64 / 1_000_000.0
                );
            }
            if let EventBody::RunFailed(f) = body {
                println!("Failed: {:?}: {}", f.reason, f.message);
            }
        }
        let ok = kinds.contains(&"model.delta")
            && kinds.contains(&"usage.reported")
            && kinds.last() == Some(&"run.completed");
        if ok {
            println!("\nRECORDING OK: the decoder understood xAI's real stream.");
            Ok(())
        } else {
            Err("the decoder didn't get a complete answer from the recording; send Claude this output".into())
        }
    }
}
