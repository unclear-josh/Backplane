//! Codex transport. Keep instructions, input, images and session ids off argv.
//! One app-server per turn; the actor closes stdin after committing the terminal event.
use super::super::{Driver, Reaction, TurnIn};
use serde_json::{json, Value};

#[derive(Default)]
pub struct AppServer {
    turn: TurnIn,
    thread: String,
    pending: u64,
    usage: Value,
    done: bool,
    configured_tier: String,
}

fn request(id: u64, method: &str, params: Value) -> String { json!({"id":id,"method":method,"params":params}).to_string() }
fn text<'a>(v: &'a Value, key: &str) -> &'a str { v.get(key).and_then(Value::as_str).unwrap_or("") }

impl AppServer {
    fn fail(&mut self, message: &str) -> Reaction {
        self.done = true;
        Reaction { send: vec![], shapes: vec![json!({"type":"turn.failed","error":{"message":message}})] }
    }
    fn thread_request(&self) -> String {
        let a = &self.turn;
        // Same sandbox and noninteractive approval behavior as codex exec.
        let sandbox = match a.runtime.as_str() { "full-access" | "" => "danger-full-access", "approval-required" => "read-only", _ => "workspace-write" };
        let mut p = json!({"cwd":a.cwd,"sandbox":sandbox,"approvalPolicy":"never","developerInstructions":a.prompt});
        if !a.model.is_empty() { p["model"] = json!(a.model); }
        if a.session.is_empty() { request(2, "thread/start", p) }
        else { p["threadId"] = json!(a.session); p["excludeTurns"] = json!(true); request(2, "thread/resume", p) }
    }
    fn turn_request(&self) -> String {
        let a = &self.turn;
        let mut input = vec![json!({"type":"text","text":a.text})];
        input.extend(a.images.iter().map(|p| json!({"type":"localImage","path":p})));
        let mut p = json!({"threadId":self.thread,"input":input});
        // Explicitly reset a resumed session's previous tier. Codex default comes from config/read.
        let tier = super::service_tier(a.setting(super::SPEED_KEY)).unwrap_or(if self.configured_tier.is_empty() { "default" } else { &self.configured_tier });
        p["serviceTier"] = json!(tier);
        if ["low", "medium", "high", "xhigh"].contains(&a.effort.as_str()) { p["effort"] = json!(a.effort); }
        if a.think { p["summary"] = json!("detailed"); }
        request(3, "turn/start", p)
    }
}

impl Driver for AppServer {
    fn argv(&self, a: &TurnIn) -> Vec<String> {
        // Only the two bounded endpoint settings need to be present before thread/start.
        vec!["app-server".into(), "-c".into(), format!("mcp_servers.backplane.url={}", json!(a.mcp_url)),
             "-c".into(), format!("mcp_servers.pcbparts.url={}", json!(a.pcbparts_url))]
    }
    fn start(&mut self, a: &TurnIn) -> Vec<String> {
        *self = Self { turn: a.clone(), pending: 1, ..Self::default() };
        vec![request(1, "initialize", json!({"clientInfo":{"name":"backplane","version":env!("CARGO_PKG_VERSION")}}))]
    }
    fn closes_stdin_on_turn_end(&self) -> bool { true }
    fn on_line(&mut self, line: &Value) -> Reaction {
        if self.done || !line.is_object() { return Reaction::default() }
        let method = text(line, "method");
        if method.is_empty() {
            if self.pending == 0 || line["id"].as_u64() != Some(self.pending) { return Reaction::default() }
            if let Some(error) = line.get("error") { return self.fail(&format!("Codex app-server: {}", text(error, "message"))) }
            let Some(result) = line.get("result") else { return Reaction::default() };
            match self.pending {
                1 => {
                    let next = if super::service_tier(self.turn.setting(super::SPEED_KEY)).is_some() { self.pending = 2; self.thread_request() }
                        else { self.pending = 4; request(4, "config/read", json!({"includeLayers":false})) };
                    return Reaction { send: vec![json!({"method":"initialized"}).to_string(), next], shapes: vec![] }
                }
                4 => { self.configured_tier = text(&result["config"], "service_tier").into(); self.pending = 2; return Reaction { send: vec![self.thread_request()], shapes: vec![] } }
                2 => {
                    let id = text(&result["thread"], "id");
                    if id.is_empty() { return self.fail("Codex app-server returned no thread id") }
                    self.thread = id.into(); self.pending = 3;
                    return Reaction { send: vec![self.turn_request()], shapes: vec![json!({"type":"thread.started","thread_id":id})] }
                }
                3 => { self.pending = 0; }
                _ => {}
            }
            return Reaction::default()
        }
        // exec is noninteractive. Decline any unexpected approval rather than hanging or approving it.
        if let Some(id) = line.get("id") {
            let result = match method {
                "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => Some(json!({"decision":"decline"})),
                "item/tool/requestUserInput" => Some(json!({"answers":{}})),
                _ => None,
            };
            return match result {
                Some(result) => Reaction { send: vec![json!({"id":id,"result":result}).to_string()], shapes: vec![] },
                None => {
                    let mut r = self.fail(&format!("Codex requested unsupported interaction: {method}"));
                    r.send.push(json!({"id":id,"error":{"code":-32601,"message":"Unsupported by Backplane's noninteractive Codex driver"}}).to_string()); r
                }
            }
        }
        let p = &line["params"];
        if let Some(id) = p.get("threadId").and_then(Value::as_str) { if id != self.thread { return Reaction::default() } }
        let mut r = Reaction::default();
        match method {
            "item/agentMessage/delta" => r.shapes.push(json!({"type":"item.delta","text":p["delta"]})),
            "item/completed" => {
                let mut item = p["item"].clone();
                let kind = match text(&item, "type") {
                    "agentMessage" => "agent_message", "commandExecution" => "command_execution", "fileChange" => "file_change",
                    "reasoning" => {
                        let summary = item["summary"].as_array().map(|xs| xs.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n")).unwrap_or_default();
                        item["text"] = json!(summary); "reasoning"
                    }
                    _ => return r,
                };
                item["type"] = json!(kind); r.shapes.push(json!({"type":"item.completed","item":item}));
            }
            "turn/plan/updated" => {
                let items: Vec<Value> = p["plan"].as_array().map(|xs| xs.iter().map(|x| json!({"text":x["step"],"status":if text(x,"status") == "inProgress" { "in_progress" } else { text(x,"status") }})).collect()).unwrap_or_default();
                r.shapes.push(json!({"type":"item.updated","item":{"type":"todo_list","items":items}}));
            }
            "thread/tokenUsage/updated" => {
                let u = &p["tokenUsage"]; let total = &u["total"];
                self.usage = json!({"input_tokens":total["inputTokens"],"cached_input_tokens":total["cachedInputTokens"],
                    "output_tokens":total["outputTokens"],"reasoning_output_tokens":total["reasoningOutputTokens"]});
                r.shapes.push(json!({"type":"context.usage","used":u["last"]["totalTokens"],"window":u["modelContextWindow"]}));
            }
            "turn/completed" => {
                self.done = true;
                if text(&p["turn"], "status") == "completed" { r.shapes.push(json!({"type":"turn.completed","usage":self.usage})); }
                else {
                    let mut error = p["turn"]["error"].as_object().cloned().unwrap_or_default();
                    if error.get("message").and_then(Value::as_str).unwrap_or("").is_empty() { error.insert("message".into(), json!("Codex turn did not complete")); }
                    r.shapes.push(json!({"type":"turn.failed","error":error}));
                }
            }
            "error" => r.shapes.push(json!({"type":"error","message":p["error"]["message"]})),
            _ => {} // In particular, ignore legacy codex/event/* duplicates.
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn turn() -> TurnIn { TurnIn { cwd: "C:/project".into(), runtime: "auto".into(), prompt: "developer\n\"instructions\"".into(), text: "user 🦀\r\nmessage".into(), mcp_url: "http://127.0.0.1:1234/mcp".into(), pcbparts_url: "https://pcbparts.dev/mcp".into(), ..Default::default() } }
    fn ready(a: &TurnIn) -> AppServer {
        let mut d = AppServer::default(); d.start(a);
        d.on_line(&json!({"id":1,"result":{}}));
        if d.pending == 4 { d.on_line(&json!({"id":4,"result":{"config":{"service_tier":null}}})); }
        d.on_line(&json!({"id":2,"result":{"thread":{"id":"S"}}}));
        d.on_line(&json!({"id":3,"result":{}})); d
    }
    #[test] fn huge_fresh_and_resumed_turns_stay_off_the_command_line() {
        for session in ["", "existing-session"] {
            let a = TurnIn { prompt: "instruction 🦀\n\"\\".repeat(10000), text: "message 🦀\r\n\"\\".repeat(10000), session: session.into(), images: vec!["C:/image one.png".into()], model: "chosen-model".into(), effort: "xhigh".into(), think: true, ..turn() };
            let mut d = AppServer::default();
            assert!(d.argv(&a).join(" ").encode_utf16().count() < 1024);
            let st = d.start(&a); assert_eq!(serde_json::from_str::<Value>(&st[0]).unwrap()["method"], "initialize");
            let init = d.on_line(&json!({"id":1,"result":{}}));
            assert_eq!(serde_json::from_str::<Value>(&init.send[1]).unwrap()["method"], "config/read");
            let config = d.on_line(&json!({"id":4,"result":{"config":{}}}));
            let req: Value = serde_json::from_str(&config.send[0]).unwrap();
            assert_eq!(req["method"], if session.is_empty() { "thread/start" } else { "thread/resume" });
            assert_eq!(req["params"]["developerInstructions"], a.prompt);
            assert_eq!(req["params"]["model"], a.model);
            assert_eq!(req["params"]["sandbox"], "workspace-write");
            if !session.is_empty() { assert_eq!(req["params"]["threadId"], session); assert_eq!(req["params"]["excludeTurns"], true); }
            let run = d.on_line(&json!({"id":2,"result":{"thread":{"id":"S"}}}));
            let req: Value = serde_json::from_str(&run.send[0]).unwrap();
            assert_eq!(req["params"]["input"][0]["text"], a.text);
            assert_eq!(req["params"]["input"][1]["path"], a.images[0]);
            assert_eq!(req["params"]["effort"], "xhigh"); assert_eq!(req["params"]["summary"], "detailed");
            assert_eq!(run.shapes[0]["thread_id"], "S");
        }
    }
    #[test] fn runtime_modes_keep_the_exec_sandbox_policy() {
        for (runtime, sandbox) in [("", "danger-full-access"), ("full-access", "danger-full-access"), ("approval-required", "read-only"), ("auto", "workspace-write")] {
            let mut d = AppServer::default(); d.start(&TurnIn { runtime: runtime.into(), ..turn() });
            let req: Value = serde_json::from_str(&d.thread_request()).unwrap();
            assert_eq!(req["params"]["sandbox"], sandbox); assert_eq!(req["params"]["approvalPolicy"], "never");
        }
    }
    #[test] fn codex_speed_resets_resumed_tiers_and_keeps_the_selected_model() {
        for session in ["", "S"] { for speed in ["", "standard", "fast", "unknown"] {
            let a = TurnIn { session: session.into(), model: "chosen-model".into(), settings: vec![(super::super::SPEED_KEY.into(), speed.into())], ..turn() };
            let mut d = ready(&a); d.configured_tier = "flex".into();
            let request: Value = serde_json::from_str(&d.turn_request()).unwrap();
            assert_eq!(request["params"]["serviceTier"], super::super::service_tier(speed).unwrap_or("flex"));
            let thread: Value = serde_json::from_str(&d.thread_request()).unwrap();
            assert_eq!(thread["params"]["model"], "chosen-model");
        } }
    }
    #[test] fn transcript_preserves_messages_tools_plans_usage_and_completes_once() {
        let mut d = ready(&turn());
        let event = |method: &str, mut params: Value| { params["threadId"] = json!("S"); json!({"method":method,"params":params}) };
        assert_eq!(d.on_line(&event("item/agentMessage/delta", json!({"delta":"hi"}))).shapes[0]["text"], "hi");
        for (from, to) in [("agentMessage", "agent_message"), ("commandExecution", "command_execution"), ("fileChange", "file_change"), ("reasoning", "reasoning")] {
            let r = d.on_line(&event("item/completed", json!({"item":{"id":"i","type":from,"text":"hi","command":"dir","changes":[],"summary":["thinking"]}})));
            assert_eq!(r.shapes[0]["item"]["type"], to);
        }
        let r = d.on_line(&event("turn/plan/updated", json!({"plan":[{"step":"test","status":"inProgress"}]})));
        assert_eq!(r.shapes[0]["item"]["items"][0]["status"], "in_progress");
        d.on_line(&event("thread/tokenUsage/updated", json!({"tokenUsage":{"total":{"inputTokens":100,"cachedInputTokens":20,"outputTokens":8,"reasoningOutputTokens":3},"last":{"totalTokens":108},"modelContextWindow":1000}})));
        let end = event("turn/completed", json!({"turn":{"status":"completed"}}));
        let r = d.on_line(&end); assert_eq!(r.shapes[0]["usage"]["input_tokens"], 100);
        assert!(d.closes_stdin_on_turn_end()); assert!(!d.ends_on_turn_end()); assert!(d.on_line(&end).shapes.is_empty());
    }
    #[test] fn errors_and_foreign_events_do_not_hang_or_complete_another_thread() {
        for pending in [1, 2, 3, 4] {
            let mut d = AppServer { pending, ..Default::default() };
            assert_eq!(d.on_line(&json!({"id":pending,"error":{"message":"failed"}})).shapes[0]["type"], "turn.failed");
        }
        let mut d = ready(&turn());
        assert!(d.on_line(&json!({"method":"turn/completed","params":{"threadId":"other","turn":{"status":"completed"}}})).shapes.is_empty());
        assert!(d.on_line(&json!({"method":"codex/event/agent_message","params":{}})).shapes.is_empty());
        assert_eq!(d.on_line(&json!({"method":"turn/completed","params":{"threadId":"S","turn":{"status":"failed","error":{"message":"bad"}}}})).shapes[0]["error"]["message"], "bad");
        let mut d = ready(&turn());
        let r = d.on_line(&json!({"id":99,"method":"item/commandExecution/requestApproval","params":{}}));
        assert_eq!(serde_json::from_str::<Value>(&r.send[0]).unwrap()["result"]["decision"], "decline");
    }
    // A native stand-in checks the real process pipes, including Windows CreateProcess.
    // No installed agent, account or model is used by this test.
    #[test] fn stdio_child() {
        if std::env::var("BP_CODEX_TRANSPORT_CHILD").as_deref() != Ok("1") { return }
        use std::io::{BufRead, Write};
        println!();
        for line in std::io::stdin().lock().lines() {
            let v: Value = serde_json::from_str(&line.unwrap()).unwrap();
            let response = match text(&v, "method") {
                "initialize" => json!({"id":1,"result":{}}),
                "initialized" => continue,
                "config/read" => json!({"id":4,"result":{"config":{"service_tier":null}}}),
                "thread/start" | "thread/resume" => {
                    assert_eq!(v["params"]["developerInstructions"], "developer 🦀\n\"\\".repeat(10000));
                    json!({"id":2,"result":{"thread":{"id":"S"}}})
                }
                "turn/start" => {
                    assert_eq!(v["params"]["input"][0]["text"], "user 🦀\r\n\"\\".repeat(10000));
                    println!("{}", json!({"id":3,"result":{}}));
                    println!("{}", json!({"method":"turn/completed","params":{"threadId":"S","turn":{"status":"completed"}}}));
                    std::io::stdout().flush().unwrap(); return
                }
                _ => panic!("unexpected request"),
            };
            println!("{response}"); std::io::stdout().flush().unwrap();
        }
        panic!("stdin closed before the turn arrived")
    }
    #[tokio::test] async fn oversized_payloads_cross_native_process_pipes() {
        for session in ["", "existing-session"] {
            let a = TurnIn { prompt: "developer 🦀\n\"\\".repeat(10000), text: "user 🦀\r\n\"\\".repeat(10000), session: session.into(), ..turn() };
            let mut d = AppServer::default();
            let args = ["agents::providers::codex::appserver::tests::stdio_child", "--exact", "--nocapture", "--test-threads=1"].map(str::to_string);
            let mut p = crate::agents::proc::spawn(&std::env::current_exe().unwrap(), &args, &std::env::current_dir().unwrap(), None,
                &[("BP_CODEX_TRANSPORT_CHILD".into(), "1".into())]).unwrap();
            let pid = p.pid;
            let outcome = tokio::time::timeout(std::time::Duration::from_secs(15), async {
                for line in d.start(&a) { p.input.send(line).await.unwrap(); }
                while let Some(line) = p.lines.recv().await {
                    let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                    let r = d.on_line(&v);
                    for line in r.send { p.input.send(line).await.unwrap(); }
                    if r.shapes.iter().any(|s| s["type"] == "turn.completed") { return true }
                }
                false
            }).await;
            crate::agents::proc::terminate(pid, std::time::Duration::from_millis(100));
            assert!(outcome.unwrap_or(false), "large input did not reach the stand-in");
        }
    }
}
