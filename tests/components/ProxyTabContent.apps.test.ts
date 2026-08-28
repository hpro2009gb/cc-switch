import { describe, expect, it } from "vitest";
import { FAILOVER_APPS } from "@/components/settings/ProxyTabContent";

describe("ProxyTabContent failover apps", () => {
  it("only exposes applications with local routing support", () => {
    expect(FAILOVER_APPS.map(({ id }) => id)).toEqual([
      "claude",
      "claude-desktop",
      "codex",
      "gemini",
      "grokbuild",
      "opencode",
    ]);
  });
});
