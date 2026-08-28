// SPDX-License-Identifier: Apache-2.0
import { BridgeClient } from "./bridge.mjs";
import { homedir } from "node:os";
import { resolve, join } from "node:path";
import { randomUUID } from "node:crypto";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
export function normalize(event, cwd) {
  if (!event || typeof event.toolName !== "string" || !event.toolName || (event.params != null && (typeof event.params !== "object" || Array.isArray(event.params)))) throw Error("Malformed native tool proposal");
  const args = event.params || {};
  if (event.toolName === "exec" && event.toolKind !== "code_mode_exec" && typeof args.command === "string")
    return { type: "shell", command: args.command, cwd: args.workdir || cwd };
  if (["read", "write", "edit"].includes(event.toolName) && typeof args.path === "string")
    return { type: event.toolName === "read" ? "file_read" : "file_write", path: args.path };
  return { type: "tool_call", tool: event.toolName, arguments: { arguments_omitted: true } };
}
export function registerHooks(api, client = new BridgeClient(api.pluginConfig?.home || process.env.HARDKNOCK_HOME || join(homedir(), ".hardknock"))) {
  const sessions = new Map(), options = api.pluginConfig || {};
  const key = ctx => ctx.sessionId || ctx.sessionKey;
  const warn = () => api.logger?.warn("Hardknock advisory unavailable (payload omitted)");
  async function session(ctx) {
    const id = key(ctx); if (!id) return null;
    if (sessions.has(id)) return sessions.get(id);
    const cwd = resolve(ctx.workspaceDir || process.cwd());
    const data = { session_id: id, agent: { name: "openclaw", adapter_version: "0.3.0" }, cwd, environment: {} };
    let response;
    try { response = await client.request("session_started", data, 5000); }
    catch (error) {
      if (options.autostart === false) throw error;
      await promisify(execFile)(process.env.HARDKNOCK_BIN || "hardknock", ["--home", client.home, "bridge", "start"], { timeout: 6000, maxBuffer: 8192 });
      response = await client.request("session_started", data, 5000);
    }
    const state = { id: response.hardknock_session_id, cwd, context: response.context_document || "", advice: [], actions: new Map(), run: randomUUID() };
    sessions.set(id, state); return state;
  }
  return {
    async before_prompt_build(_event, ctx) {
      try {
        const s = await session(ctx); if (!s) return;
        s.run = ctx.runId || randomUUID();
        try { const fresh = await client.request("context_requested", { hardknock_session_id: s.id }, 2000); s.context = fresh.context_document || ""; } catch { warn(); }
        const text = [s.context, ...s.advice].join("\n").slice(0, 32768); s.advice = [];
        return { prependContext: text };
      } catch { warn(); }
    },
    async before_agent_run(_event, ctx) {
      // Metadata only: do not capture final prompts or session messages.
      if (!sessions.has(key(ctx)) && options.policyRequired) return { outcome: "block", reason: "Required Hardknock policy unavailable", message: "Required Hardknock policy unavailable" };
    },
    async before_tool_call(event, ctx) {
      const s = sessions.get(key(ctx)), actionId = event.toolCallId || ctx.toolCallId;
      if (!s || !actionId) return options.policyRequired ? { block: true, blockReason: "Required Hardknock policy unavailable" } : undefined;
      try {
        const action = normalize(event, s.cwd);
        const d = await client.request("action_proposed", { hardknock_session_id: s.id, action_id: actionId, action, context: { can_intercept: true } }, 20);
        s.actions.set(actionId, action);
        if (d.decision === "block" && ["user_policy", "external_policy"].includes(d.authority)) return { block: true, blockReason: d.reason };
        if (d.decision === "require_approval") return { requireApproval: { title: "Hardknock policy", description: d.reason, severity: "warning", allowedDecisions: ["allow-once", "deny"] } };
        const message = d.message || d.reason;
        if (message) { s.advice = [...s.advice, message].slice(-5); api.logger?.warn(message); }
        // Native hook cannot inject an advisory into the current tool call. Do not rewrite arguments or hard-block learning advice.
      } catch { warn(); if (options.policyRequired) return { block: true, blockReason: "Required Hardknock policy unavailable" }; }
    },
    async after_tool_call(event, ctx) {
      const s = sessions.get(key(ctx)), actionId = event.toolCallId || ctx.toolCallId;
      if (!s?.actions.has(actionId)) return;
      const action = s.actions.get(actionId); s.actions.delete(actionId);
      try {
        await client.request("action_completed", { hardknock_session_id: s.id, action_id: actionId, action,
          duration_ms: Math.max(0, event.durationMs || 0), result: { success: !event.error && event.result?.isError !== true, error_class: event.error ? "tool_failure" : null } });
      } catch { warn(); }
    },
    async agent_end(event, ctx) {
      const s = sessions.get(key(ctx)); if (!s) return;
      try { await client.request("run_completed", { hardknock_session_id: s.id, run_id: ctx.runId || s.run, success: event.success ?? null, duration_ms: Math.max(0, event.durationMs || 0) }); }
      catch { warn(); }
    },
    async session_end(_event, ctx) {
      const s = sessions.get(key(ctx)); if (!s) return;
      sessions.delete(key(ctx));
      try { await client.request("session_ended", { hardknock_session_id: s.id }); } catch { warn(); }
    }
  };
}
