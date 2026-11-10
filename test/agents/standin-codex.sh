#!/bin/sh
# A stand-in for `codex app-server` and one-shot writing calls (`exec --ephemeral`). Never a real model.
# A turn reads its text from the last argument; "slow" waits 2 s, "die" exits 3, "hang" waits a minute; every turn says thread.started, a delta, a command
# and a reply that echoes the text. The argv goes to argv.log beside the script and to $FAKE_LOG; a one-shot answers one agent_message naming its model.
case "$1" in --version) echo "codex-cli 0.0.0"; exit 0;; esac
for a in "$@"; do last="$a"; done
D=$(dirname "$0"); echo "$@" >> "$D/argv.log"
[ -n "$FAKE_LOG" ] && echo "CODEX $*" >> "$FAKE_LOG"
if [ "$1" = app-server ]; then
  exec node -e '
const fs = require("node:fs"), readline = require("node:readline");
const dir = process.argv[1]; let thread = "cs-1";
fs.writeFileSync(`${dir}/pid`, String(process.pid));
const send = x => process.stdout.write(JSON.stringify(x) + "\n");
const event = (method, params) => send({method, params: {threadId: thread, ...params}});
const input = readline.createInterface({input: process.stdin});
input.on("line", async line => {
  fs.appendFileSync(`${dir}/requests.log`, line + "\n");
  const q = JSON.parse(line), p = q.params || {};
  if (q.method === "initialize") send({id: q.id, result: {}});
  if (q.method === "config/read") send({id: q.id, result: {config: {}}});
  if (q.method === "thread/start" || q.method === "thread/resume") {
    thread = p.threadId || "cs-1"; send({id: q.id, result: {thread: {id: thread}}});
  }
  if (q.method !== "turn/start") return;
  const text = p.input.filter(x => x.type === "text").map(x => x.text).join("\n");
  send({id: q.id, result: {turn: {id: "turn-1"}}});
  event("item/agentMessage/delta", {delta: "par"});
  if (text.includes("slow")) await new Promise(r => setTimeout(r, 2000));
  else if (text.includes("die")) process.exit(3);
  else if (text.includes("hang")) await new Promise(r => setTimeout(r, 60000));
  event("item/completed", {item: {id: "it1", type: "commandExecution", command: "echo ok"}});
  let reply = "re: " + text.replace(/\n/g, " ").replace(/"/g, "");
  if (process.env.FAKE_READ_SKILL === "1") {
    const path = text.match(/read "([^"]*SKILL.md)"/);
    const secret = path && fs.readFileSync(path[1], "utf8").match(/^SECRET=(.*)$/m);
    reply = "The skill says " + (secret ? secret[1] : "missing");
  }
  event("item/completed", {item: {id: "it2", type: "agentMessage", text: reply}});
  event("thread/tokenUsage/updated", {tokenUsage: {total: {inputTokens: 100, cachedInputTokens: 40, outputTokens: 10}, last: {totalTokens: 110}, modelContextWindow: 1000}});
  event("turn/completed", {turn: {status: "completed"}});
});
' "$D"
fi
echo $$ > "$D/pid"
case " $* " in *" --ephemeral "*)
  m=default; p=; for a in "$@"; do [ "$p" = -m ] && m=$a; p=$a; done
  printf '{"type":"item.completed","item":{"type":"agent_message","text":"Codex title from %s"}}\n' "$m"; exit 0;; esac
echo '{"type":"thread.started","thread_id":"cs-1"}'
echo '{"type":"item.delta","text":"par"}'
case "$last" in *slow*) sleep 2;; *die*) exit 3;; *hang*) sleep 60;; esac
echo '{"type":"item.completed","item":{"id":"it1","type":"command_execution","command":"echo ok"}}'
printf '{"type":"item.completed","item":{"id":"it2","type":"agent_message","text":"re: %s"}}\n' "$(echo "$last" | tr '\n' ' ' | tr -d '"')"
echo '{"type":"turn.completed","usage":{"input_tokens":100,"cached_input_tokens":40,"output_tokens":10}}'
