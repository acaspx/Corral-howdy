import { invoke } from "@tauri-apps/api/core";
import type {
  AgentPolicyEdit,
  AgentPolicyView,
  PolicyClassView,
  PolicyRuleChange,
  RuleChoice,
  RuleOrigin,
  TimeoutChoice,
} from "./bindings";

/**
 * Each agent's policy (M4 item 6): what it may do on its own, what stops for your approval, and
 * what's blocked. The core decides and checks every edit against the ceilings; this only shows
 * the policy and sends the form.
 */
export type {
  AgentPolicyEdit,
  AgentPolicyView,
  PolicyClassView,
  PolicyRuleChange,
  RuleChoice,
  RuleOrigin,
  TimeoutChoice,
};

export interface PolicyApi {
  list(): Promise<AgentPolicyView[]>;
  save(agent: string, edit: AgentPolicyEdit): Promise<AgentPolicyView>;
}

/** The core's commands (apps/desktop/src-tauri/src/agent_policies.rs). */
export const policyApi: PolicyApi = {
  list: () => invoke<AgentPolicyView[]>("agent_policies"),
  save: (agent, edit) => invoke<AgentPolicyView>("save_agent_policy", { agent, edit }),
};
