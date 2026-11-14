//! The tool gateway (M2 item 3; T2, T30): tools run only from an `AllowToken`, exactly the call
//! the token was issued for, and only if the tool really is the class the token was decided for.
//! Tool output is capped before it reaches the model. TP-POL-10 (the gateway half), TP-GATE-04..07.

use corral_core::domain::ActionClass;
use corral_policy::{Action, Gate, Policy, Profile, Rule, ToolCall, Verdict};
use corral_tools::{
    BoxFuture, GatewayError, MODEL_OUTPUT_CAP, Tool, ToolError, ToolGateway, ToolOutput,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

/// A tool that records its calls and answers with a fixed text.
struct Echo {
    name: &'static str,
    class: ActionClass,
    answer: Result<String, ToolError>,
    untrusted: bool,
    calls: Arc<Mutex<Vec<Value>>>,
}

impl Echo {
    fn new(name: &'static str, class: ActionClass, answer: &str) -> Self {
        Self {
            name,
            class,
            answer: Ok(answer.to_owned()),
            untrusted: false,
            calls: Arc::default(),
        }
    }
}

impl Tool for Echo {
    fn name(&self) -> &str {
        self.name
    }

    fn class(&self) -> ActionClass {
        self.class
    }

    fn untrusted_source(&self, args: &Value) -> Option<String> {
        self.untrusted
            .then(|| args["url"].as_str().unwrap_or("unknown").to_owned())
    }

    fn call<'a>(&'a self, args: &'a Value) -> BoxFuture<'a, Result<String, ToolError>> {
        self.calls.lock().expect("calls").push(args.clone());
        let answer = self.answer.clone();
        Box::pin(async move { answer })
    }
}

/// A gate whose profile allows every class but unknown, so tests get tokens directly.
fn open_gate() -> Gate {
    let rules: Vec<String> = [
        "read.local",
        "write.local",
        "net.read",
        "net.write",
        "comms.send",
    ]
    .iter()
    .map(|c| format!(r#"{{ class = "{c}", decision = "allow" }}"#))
    .collect();
    let profile = Profile::from_toml(&format!("rules = [{}]", rules.join(", "))).expect("profile");
    Gate::new(Policy::new(profile))
}

fn token_for(
    gate: &Gate,
    class: ActionClass,
    tool: &str,
    args: Value,
) -> corral_policy::AllowToken {
    let Verdict::Allow(token) = gate.check(ToolCall {
        action: Action::new(class, tool),
        args,
    }) else {
        panic!("the open gate allows {class:?}");
    };
    token
}

#[tokio::test]
async fn tp_pol_10_a_token_runs_exactly_the_call_it_was_issued_for() {
    let echo = Echo::new("fs.write", ActionClass::WriteLocal, "written");
    let calls = Arc::clone(&echo.calls);
    let mut gateway = ToolGateway::new();
    gateway.register(Arc::new(echo)).expect("registered");
    let gate = open_gate();
    let token = token_for(
        &gate,
        ActionClass::WriteLocal,
        "fs.write",
        json!({"path": "a.txt"}),
    );
    let out = gateway.run(token).await.expect("runs");
    assert_eq!(
        out,
        ToolOutput {
            for_model: "written".into(),
            full: "written".into(),
            truncated: false,
        }
    );
    assert_eq!(*calls.lock().expect("calls"), [json!({"path": "a.txt"})]);
}

#[tokio::test]
async fn tp_pol_10_a_token_decided_for_another_class_is_refused() {
    // The adapter called a sending tool "read.local"; the registry knows better.
    let echo = Echo::new("email.send", ActionClass::CommsSend, "sent");
    let calls = Arc::clone(&echo.calls);
    let mut gateway = ToolGateway::new();
    gateway.register(Arc::new(echo)).expect("registered");
    let token = token_for(
        &open_gate(),
        ActionClass::ReadLocal,
        "email.send",
        json!({}),
    );
    assert_eq!(
        gateway.run(token).await,
        Err(GatewayError::ClassMismatch {
            tool: "email.send".into(),
            registered: ActionClass::CommsSend,
            decided: ActionClass::ReadLocal,
        })
    );
    assert!(calls.lock().expect("calls").is_empty(), "nothing ran");
}

#[tokio::test]
async fn tp_gate_04_unknown_and_duplicate_tools_are_refused() {
    let mut gateway = ToolGateway::new();
    gateway
        .register(Arc::new(Echo::new("fs.read", ActionClass::ReadLocal, "x")))
        .expect("registered");
    assert_eq!(
        gateway.register(Arc::new(Echo::new("fs.read", ActionClass::NetWrite, "y"))),
        Err(GatewayError::Duplicate("fs.read".into())),
        "a second tool can't take an existing name (and its class)"
    );
    assert_eq!(gateway.class_of("fs.read"), Some(ActionClass::ReadLocal));
    assert_eq!(gateway.class_of("nope"), None);
    let token = token_for(&open_gate(), ActionClass::ReadLocal, "nope", json!({}));
    assert_eq!(
        gateway.run(token).await,
        Err(GatewayError::UnknownTool("nope".into()))
    );
    assert_eq!(
        gateway.register(Arc::new(Echo::new("anything", ActionClass::Unknown, "z"))),
        Err(GatewayError::UnknownClass("anything".into())),
        "a tool must say what it does"
    );
}

#[tokio::test]
async fn tp_gate_05_output_is_capped_for_the_model_and_kept_whole() {
    let long = "é".repeat(MODEL_OUTPUT_CAP); // two bytes each: well over the cap
    let mut gateway = ToolGateway::new();
    gateway
        .register(Arc::new(Echo::new(
            "fs.read",
            ActionClass::ReadLocal,
            &long,
        )))
        .expect("registered");
    let token = token_for(&open_gate(), ActionClass::ReadLocal, "fs.read", json!({}));
    let out = gateway.run(token).await.expect("runs");
    assert!(out.truncated);
    assert_eq!(out.full, long, "the trace keeps everything");
    assert!(
        out.for_model.len() <= MODEL_OUTPUT_CAP + 200,
        "{}",
        out.for_model.len()
    );
    assert!(
        out.for_model.starts_with('é'),
        "cut on a character boundary"
    );
    assert!(
        out.for_model.ends_with(&format!(
            "[Output truncated: {} more bytes are in the trace.]",
            long.len() - out.for_model.find("\n[Output").expect("marker")
        )),
        "{}",
        &out.for_model[out.for_model.len() - 80..]
    );
}

#[tokio::test]
async fn tp_gate_06_a_tool_that_reads_untrusted_content_taints_the_run() {
    let mut fetch = Echo::new("http.get", ActionClass::NetRead, "<html>");
    fetch.untrusted = true;
    let mut gateway = ToolGateway::new();
    gateway.register(Arc::new(fetch)).expect("registered");
    let gate = open_gate();
    assert_eq!(gate.tainted_by(), None);
    let token = token_for(
        &gate,
        ActionClass::NetRead,
        "http.get",
        json!({"url": "https://news.example/page"}),
    );
    gateway.run(token).await.expect("runs");
    assert_eq!(
        gate.tainted_by().as_deref(),
        Some("https://news.example/page")
    );
}

#[tokio::test]
async fn tp_gate_07_a_tool_error_is_typed_and_doesnt_taint() {
    let mut broken = Echo::new("http.get", ActionClass::NetRead, "");
    broken.answer = Err(ToolError::Failed("connection refused".into()));
    broken.untrusted = true;
    let mut gateway = ToolGateway::new();
    gateway.register(Arc::new(broken)).expect("registered");
    let gate = open_gate();
    let token = token_for(
        &gate,
        ActionClass::NetRead,
        "http.get",
        json!({"url": "https://x.example"}),
    );
    assert_eq!(
        gateway.run(token).await,
        Err(GatewayError::Tool(ToolError::Failed(
            "connection refused".into()
        )))
    );
    assert_eq!(gate.tainted_by(), None, "nothing was read");
}

#[test]
fn rules_are_reexported_for_profiles() {
    // Callers build profiles without depending on corral-policy's internals.
    assert!(Rule::Allow > Rule::Ask);
}
