//! Offering tools to a model (M3 item 3, TP-TOOL-01..03). A tool that can be offered describes
//! itself: a plain-words description and a JSON schema for its arguments. Models call tools by a
//! wire name made of letters, digits, `_` and `-`, so `fs.read` goes out as `fs_read`, and the
//! gateway maps the wire name back to the registered tool. Only tools with a spec are offered.

use corral_core::domain::ActionClass;
use corral_tools::fs::{FsDelete, FsRead, FsWrite, Sandbox};
use corral_tools::{
    BoxFuture, GatewayError, MAX_WIRE_NAME_CHARS, Tool, ToolError, ToolGateway, ToolSpec, wire_name,
};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// A fresh agent folder, cleaned up on drop.
struct Folder(PathBuf);

impl Folder {
    fn new() -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "corral-offer-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        if path.exists() {
            std::fs::remove_dir_all(&path).expect("clear");
        }
        std::fs::create_dir_all(&path).expect("folder");
        Self(path)
    }

    fn sandbox(&self) -> Arc<Sandbox> {
        Arc::new(Sandbox::new([self.0.clone()]).expect("sandbox"))
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        // Best effort: a leftover temp folder only costs disk space.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A tool with a name and, optionally, a spec.
struct Named {
    name: String,
    spec: Option<ToolSpec>,
}

impl Named {
    fn plain(name: &str) -> Arc<dyn Tool> {
        Arc::new(Self {
            name: name.to_owned(),
            spec: None,
        })
    }

    fn offered(name: &str) -> Arc<dyn Tool> {
        Arc::new(Self {
            name: name.to_owned(),
            spec: Some(ToolSpec {
                description: "A test tool.".into(),
                parameters: json!({"type": "object", "properties": {}, "additionalProperties": false}),
            }),
        })
    }
}

impl Tool for Named {
    fn name(&self) -> &str {
        &self.name
    }

    fn class(&self) -> ActionClass {
        ActionClass::ReadLocal
    }

    fn spec(&self) -> Option<ToolSpec> {
        self.spec.clone()
    }

    fn call<'a>(&'a self, _args: &'a Value) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async { Ok("ok".to_owned()) })
    }
}

fn file_tools(folder: &Folder) -> Vec<(Arc<dyn Tool>, Vec<&'static str>)> {
    let sandbox = folder.sandbox();
    vec![
        (Arc::new(FsRead::new(sandbox.clone())), vec!["path"]),
        (
            Arc::new(FsWrite::new(sandbox.clone())),
            vec!["path", "content"],
        ),
        (Arc::new(FsDelete::new(sandbox)), vec!["path"]),
    ]
}

#[test]
fn tp_tool_01_file_tools_describe_themselves() {
    let folder = Folder::new();
    for (tool, required) in file_tools(&folder) {
        let spec = tool
            .spec()
            .unwrap_or_else(|| panic!("{} has a spec", tool.name()));
        assert!(!spec.description.trim().is_empty(), "{}", tool.name());
        assert!(spec.description.chars().count() <= 1024, "{}", tool.name());
        let p = &spec.parameters;
        assert_eq!(p["type"], "object", "{}", tool.name());
        assert_eq!(p["additionalProperties"], false, "{}", tool.name());
        let listed: Vec<&str> = p["required"]
            .as_array()
            .expect("required")
            .iter()
            .map(|v| v.as_str().expect("a name"))
            .collect();
        assert_eq!(listed, required, "{}", tool.name());
        let properties = p["properties"].as_object().expect("properties");
        assert_eq!(properties.len(), required.len(), "{}", tool.name());
        for key in required {
            assert_eq!(properties[key]["type"], "string", "{} {key}", tool.name());
            let said = properties[key]["description"].as_str().unwrap_or_default();
            assert!(
                !said.trim().is_empty(),
                "{} {key} is described",
                tool.name()
            );
        }
    }
}

#[tokio::test]
async fn tp_tool_01_a_call_shaped_like_the_spec_works() {
    let folder = Folder::new();
    let sandbox = folder.sandbox();
    let write = FsWrite::new(sandbox.clone());
    let read = FsRead::new(sandbox.clone());
    let delete = FsDelete::new(sandbox);
    write
        .call(&json!({"path": "notes.md", "content": "hello"}))
        .await
        .expect("write");
    assert_eq!(
        read.call(&json!({"path": "notes.md"})).await.expect("read"),
        "hello"
    );
    delete
        .call(&json!({"path": "notes.md"}))
        .await
        .expect("delete");
}

#[test]
fn tp_tool_02_wire_names_use_only_safe_characters() {
    assert_eq!(wire_name("fs.read"), "fs_read");
    assert_eq!(wire_name("email.send"), "email_send");
    assert_eq!(wire_name("a b/c"), "a_b_c");
    assert_eq!(wire_name("keep-this_one9"), "keep-this_one9");
    assert_eq!(wire_name("café"), "caf_");
}

#[test]
fn tp_tool_02_the_gateway_offers_only_tools_with_a_spec() {
    let folder = Folder::new();
    let mut gateway = ToolGateway::new();
    gateway.register(Named::plain("echo")).expect("echo");
    for (tool, _) in file_tools(&folder) {
        gateway.register(tool).expect("file tool");
    }
    let offered = gateway.offered();
    let names: Vec<(&str, &str)> = offered
        .iter()
        .map(|o| (o.wire_name.as_str(), o.name.as_str()))
        .collect();
    assert_eq!(
        names,
        [
            ("fs_delete", "fs.delete"),
            ("fs_read", "fs.read"),
            ("fs_write", "fs.write"),
        ]
    );
    let read = &offered[1];
    let spec = FsRead::new(folder.sandbox()).spec().expect("spec");
    assert_eq!(read.description, spec.description);
    assert_eq!(read.parameters, spec.parameters);
}

#[test]
fn tp_tool_02_a_wire_name_maps_back_to_its_tool() {
    let folder = Folder::new();
    let mut gateway = ToolGateway::new();
    gateway.register(Named::plain("echo")).expect("echo");
    for (tool, _) in file_tools(&folder) {
        gateway.register(tool).expect("file tool");
    }
    assert_eq!(gateway.from_wire("fs_read"), Some("fs.read"));
    assert_eq!(gateway.from_wire("fs_delete"), Some("fs.delete"));
    // Only offered tools have wire names; anything else is not a tool the model was given.
    assert_eq!(gateway.from_wire("fs.read"), None);
    assert_eq!(gateway.from_wire("echo"), None);
    assert_eq!(gateway.from_wire("shell_exec"), None);
    assert_eq!(gateway.from_wire(""), None);
}

#[test]
fn tp_tool_03_two_tools_cant_share_a_wire_name() {
    let mut gateway = ToolGateway::new();
    gateway.register(Named::offered("fs.read")).expect("first");
    assert_eq!(
        gateway.register(Named::offered("fs_read")).err(),
        Some(GatewayError::WireClash {
            tool: "fs_read".into(),
            other: "fs.read".into(),
        })
    );
    // Even a tool that isn't offered can't take a name that would read as another's.
    assert_eq!(
        gateway.register(Named::plain("fs-read")).ok(),
        Some(()),
        "a different wire name is fine"
    );
    assert_eq!(
        gateway.register(Named::plain("fs/read")).err(),
        Some(GatewayError::WireClash {
            tool: "fs/read".into(),
            other: "fs.read".into(),
        })
    );
    assert_eq!(gateway.from_wire("fs_read"), Some("fs.read"));
}

#[test]
fn tp_tool_03_an_offered_tool_needs_a_usable_wire_name() {
    let mut gateway = ToolGateway::new();
    let long = "t".repeat(MAX_WIRE_NAME_CHARS + 1);
    assert_eq!(
        gateway.register(Named::offered(&long)).err(),
        Some(GatewayError::BadWireName(long.clone()))
    );
    assert_eq!(
        gateway.register(Named::offered("")).err(),
        Some(GatewayError::BadWireName(String::new()))
    );
    let fits = "t".repeat(MAX_WIRE_NAME_CHARS);
    gateway.register(Named::offered(&fits)).expect("64 fits");
    // A tool that isn't offered never goes on the wire, so its name can be longer.
    gateway.register(Named::plain(&long)).expect("not offered");
    assert_eq!(gateway.offered().len(), 1);
}
