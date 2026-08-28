// SPDX-License-Identifier: Apache-2.0
import type { OpenClawPluginApi } from "openclaw/plugin-sdk";
import { registerHooks } from "./hooks.mjs";
export default {
  id: "hardknock",
  name: "Hardknock Experience",
  register(api: OpenClawPluginApi) {
    const hooks = registerHooks(api);
    // Typed lifecycle API. No legacy registerHook calls.
    api.on("before_prompt_build", hooks.before_prompt_build);
    api.on("before_agent_run", hooks.before_agent_run);
    api.on("before_tool_call", hooks.before_tool_call);
    api.on("after_tool_call", hooks.after_tool_call);
    api.on("agent_end", hooks.agent_end);
    api.on("session_end", hooks.session_end);
  }
};
