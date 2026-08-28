// SPDX-License-Identifier: Apache-2.0
import test from "node:test";
import assert from "node:assert/strict";
import { registerHooks, normalize } from "./hooks.mjs";
const ctx = { sessionId: "s", workspaceDir: "/tmp", runId: "r" };
function host() {
  const events = [];
  const client = { decision: { decision: "continue" }, fail: false, async request(event, data) {
    if (this.fail) throw Error("timeout");
    events.push({ event, data });
    if (event === "session_started" || event === "context_requested") return { hardknock_session_id: "hk-s-test", context_document: "Experience, not policy" };
    return event === "action_proposed" ? this.decision : { accepted: true };
  }};
  const api = { pluginConfig: { autostart: false }, logger: { warn() {} } };
  return { hooks: registerHooks(api, client), client, events };
}
test("typed hook lifecycle injects, observes and records without capturing prompts", async () => {
  const { hooks, client, events } = host();
  assert.match((await hooks.before_prompt_build({ prompt: "private" }, ctx)).prependContext, /Experience/);
  client.decision = { decision: "replan", reason: "Prefer tested action" };
  assert.equal(await hooks.before_tool_call({ toolName: "exec", toolCallId: "a", params: { command: "npm install" } }, ctx), undefined);
  await hooks.after_tool_call({ toolCallId: "a", error: "failure", durationMs: 2 }, ctx);
  assert.equal(events.at(-1).data.result.success, false);
  assert.match((await hooks.before_prompt_build({}, ctx)).prependContext, /Prefer tested/);
  await hooks.agent_end({ success: true, messages: ["private response"] }, ctx);
  assert.equal(events.at(-1).event, "run_completed");
  assert.ok(!JSON.stringify(events).includes("private"));
  await hooks.session_end({}, ctx);
});
test("timeouts fail open for advice and explicit policy blocks remain distinct", async () => {
  const { hooks, client } = host();
  await hooks.before_prompt_build({}, ctx);
  assert.equal(await hooks.before_tool_call({ toolName: 12, toolCallId: "malformed", params: {} }, ctx), undefined);
  client.fail = true;
  assert.equal(await hooks.before_tool_call({ toolName: "exec", toolCallId: "a", params: { command: "x" } }, ctx), undefined);
  client.fail = false; client.decision = { decision: "block", authority: "user_policy", reason: "Policy" };
  assert.equal((await hooks.before_tool_call({ toolName: "exec", toolCallId: "b", params: { command: "x" } }, ctx)).block, true);
});
test("code-mode exec is not misrepresented as a shell command", () => {
  assert.equal(normalize({ toolName: "exec", toolKind: "code_mode_exec", params: { command: "JavaScript" } }, "/tmp").type, "tool_call");
});
test("policy availability uses the input-gate result, while learning advice never denies", async () => {
  const hooks = registerHooks({ pluginConfig: { policyRequired: true } }, {});
  assert.deepEqual(await hooks.before_agent_run({}, ctx), {
    outcome: "block", reason: "Required Hardknock policy unavailable", message: "Required Hardknock policy unavailable"
  });
  const { hooks: advisory, client } = host();
  assert.equal(await advisory.before_agent_run({}, ctx), undefined);
  await advisory.before_prompt_build({}, ctx);
  for (const decision of ["continue", "advise", "warn", "replan"]) {
    client.decision = { decision, message: "Evidence", reason: "Evidence" };
    assert.equal(await advisory.before_tool_call({ toolName: "exec", toolCallId: decision, params: { command: "x" } }, ctx), undefined);
  }
  client.decision = { decision: "require_approval", reason: "Explicit user policy" };
  const response = await advisory.before_tool_call({ toolName: "exec", toolCallId: "approval", params: { command: "x" } }, ctx);
  assert.deepEqual(response.requireApproval.allowedDecisions, ["allow-once", "deny"]);
});
