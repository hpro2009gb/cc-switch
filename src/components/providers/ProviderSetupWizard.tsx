import { useEffect, useRef, useState } from "react";
import { Loader2, ShieldCheck, WandSparkles } from "lucide-react";
import { useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import type { AppId } from "@/lib/api";
import {
  providerWizardApi,
  type ApplyProviderInstallResult,
  type DetectedModel,
  type ProviderInstallPreview,
  type ProviderProbeResult,
  type UpstreamProtocol,
} from "@/lib/api/provider-wizard";

interface ProviderSetupWizardProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  initialApp: AppId;
}

const PROTOCOL_LABELS: Record<UpstreamProtocol, string> = {
  anthropic_messages: "Anthropic Messages",
  open_ai_chat: "OpenAI Chat Completions",
  open_ai_responses: "OpenAI Responses",
};

function modelLabel(model: DetectedModel): string {
  const displayName = model.displayName?.trim();
  if (
    !displayName ||
    displayName === model.id ||
    displayName.includes(model.id)
  ) {
    return displayName || model.id;
  }
  return `${displayName} (${model.id})`;
}

function preferredProtocol(
  probe: ProviderProbeResult,
  app: "claude" | "codex" | "claude-desktop" | "opencode",
): UpstreamProtocol | undefined {
  const preference: UpstreamProtocol[] =
    app === "claude" || app === "claude-desktop"
      ? ["anthropic_messages", "open_ai_responses", "open_ai_chat"]
      : ["open_ai_responses", "open_ai_chat", "anthropic_messages"];
  return preference.find((protocol) =>
    probe.capabilities.some(
      (capability) => capability.protocol === protocol && capability.supported,
    ),
  );
}

export function ProviderSetupWizard({
  open,
  onOpenChange,
  initialApp,
}: ProviderSetupWizardProps) {
  const queryClient = useQueryClient();
  const [name, setName] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [model, setModel] = useState("");
  const [probe, setProbe] = useState<ProviderProbeResult | null>(null);
  const [preview, setPreview] = useState<ProviderInstallPreview | null>(null);
  const [claudeSelected, setClaudeSelected] = useState(initialApp === "claude");
  const [codexSelected, setCodexSelected] = useState(initialApp === "codex");
  const [claudeDesktopSelected, setClaudeDesktopSelected] = useState(
    initialApp === "claude-desktop",
  );
  const [opencodeSelected, setOpencodeSelected] = useState(
    initialApp === "opencode",
  );
  const [claudeProtocol, setClaudeProtocol] = useState<
    UpstreamProtocol | undefined
  >();
  const [codexProtocol, setCodexProtocol] = useState<
    UpstreamProtocol | undefined
  >();
  const [claudeDesktopProtocol, setClaudeDesktopProtocol] = useState<
    UpstreamProtocol | undefined
  >();
  const [opencodeProtocol, setOpencodeProtocol] = useState<
    UpstreamProtocol | undefined
  >();
  const [busy, setBusy] = useState(false);
  const probeRevision = useRef(0);

  const invalidateConnectionProbe = () => {
    probeRevision.current += 1;
    setModel("");
    setProbe(null);
    setPreview(null);
    setClaudeProtocol(undefined);
    setCodexProtocol(undefined);
    setClaudeDesktopProtocol(undefined);
    setOpencodeProtocol(undefined);
    setBusy(false);
  };

  useEffect(() => {
    if (!open) {
      probeRevision.current += 1;
      setName("");
      setBaseUrl("");
      setApiKey("");
      setModel("");
      setProbe(null);
      setPreview(null);
      setClaudeSelected(initialApp === "claude");
      setCodexSelected(initialApp === "codex");
      setClaudeDesktopSelected(initialApp === "claude-desktop");
      setOpencodeSelected(initialApp === "opencode");
      setClaudeProtocol(undefined);
      setCodexProtocol(undefined);
      setClaudeDesktopProtocol(undefined);
      setOpencodeProtocol(undefined);
      setBusy(false);
    }
  }, [initialApp, open]);

  const runProbe = async () => {
    if (!name.trim() || !baseUrl.trim() || !apiKey.trim()) {
      toast.error("Nhập tên, Base URL và API key trước khi kiểm tra.");
      return;
    }
    const revision = ++probeRevision.current;
    setBusy(true);
    try {
      const result = await providerWizardApi.probe({
        baseUrl,
        apiKey,
        model: model || undefined,
        allowInferenceProbe: true,
      });
      if (revision !== probeRevision.current) return;
      setProbe(result);
      setModel(result.recommendedModel ?? model);
      const nextClaude = preferredProtocol(result, "claude");
      const nextCodex = preferredProtocol(result, "codex");
      const nextClaudeDesktop = preferredProtocol(result, "claude-desktop");
      const nextOpenCode = preferredProtocol(result, "opencode");
      setClaudeProtocol((current) => nextClaude ?? current);
      setCodexProtocol((current) => nextCodex ?? current);
      setClaudeDesktopProtocol((current) => nextClaudeDesktop ?? current);
      setOpencodeProtocol((current) => nextOpenCode ?? current);
      setPreview(null);
      if (result.capabilities.some((capability) => capability.supported)) {
        toast.success("Đã kiểm tra protocol và model.");
      } else {
        toast.error(
          "Không xác minh được protocol; xem trạng thái HTTP trong phần chẩn đoán.",
        );
      }
    } catch (error) {
      if (revision === probeRevision.current) {
        toast.error(String(error));
      }
    } finally {
      if (revision === probeRevision.current) {
        setBusy(false);
      }
    }
  };

  const buildPreview = async () => {
    if (
      !probe ||
      !model.trim() ||
      (!claudeSelected &&
        !codexSelected &&
        !claudeDesktopSelected &&
        !opencodeSelected)
    ) {
      toast.error("Chọn ứng dụng và model trước khi xem preview.");
      return;
    }
    const missingProtocols = [
      claudeSelected && !claudeProtocol ? "Claude Code" : null,
      codexSelected && !codexProtocol ? "Codex" : null,
      claudeDesktopSelected && !claudeDesktopProtocol ? "Claude Cowork" : null,
      opencodeSelected && !opencodeProtocol ? "OpenCode" : null,
    ].filter(Boolean);
    if (missingProtocols.length > 0) {
      toast.error(
        `Chọn protocol cho ${missingProtocols.join(", ")} trước khi xem preview.`,
      );
      return;
    }
    setBusy(true);
    try {
      const result = await providerWizardApi.preview({
        name,
        baseUrl,
        apiKey,
        model,
        models: probe.models,
        claudeProtocol: claudeSelected ? claudeProtocol : undefined,
        codexProtocol: codexSelected ? codexProtocol : undefined,
        claudeDesktopProtocol: claudeDesktopSelected
          ? claudeDesktopProtocol
          : undefined,
        opencodeProtocol: opencodeSelected ? opencodeProtocol : undefined,
      });
      setPreview(result);
    } catch (error) {
      toast.error(String(error));
    } finally {
      setBusy(false);
    }
  };

  const apply = async () => {
    if (!preview || !probe) return;
    setBusy(true);
    try {
      const result: ApplyProviderInstallResult = await providerWizardApi.apply({
        name,
        baseUrl,
        apiKey,
        model,
        models: probe.models,
        claudeProtocol: claudeSelected ? claudeProtocol : undefined,
        codexProtocol: codexSelected ? codexProtocol : undefined,
        claudeDesktopProtocol: claudeDesktopSelected
          ? claudeDesktopProtocol
          : undefined,
        opencodeProtocol: opencodeSelected ? opencodeProtocol : undefined,
      });
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["providers", "claude"] }),
        queryClient.invalidateQueries({ queryKey: ["providers", "codex"] }),
        queryClient.invalidateQueries({
          queryKey: ["providers", "claude-desktop"],
        }),
        queryClient.invalidateQueries({ queryKey: ["providers", "opencode"] }),
      ]);
      toast.success(
        `Đã cài ${result.appliedApps.length} ứng dụng. Hãy mở lại IDE nếu được yêu cầu.`,
      );
      onOpenChange(false);
    } catch (error) {
      toast.error(String(error), { duration: 8000 });
    } finally {
      setBusy(false);
    }
  };

  const selectedModelIsDetected =
    probe?.models.some((item) => item.id === model) ?? false;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-2xl" zIndex="top">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <WandSparkles className="h-5 w-5" /> Thiết lập provider nhanh
          </DialogTitle>
          <DialogDescription>
            Nhập thông tin kết nối. CC Switch sẽ kiểm tra sau khi bạn xác nhận,
            nhưng chưa thay đổi cấu hình trước bước Apply.
          </DialogDescription>
        </DialogHeader>

        <div className="grid gap-4 overflow-y-auto py-2">
          <div className="grid gap-2">
            <Label htmlFor="wizard-provider-name">Tên provider</Label>
            <Input
              id="wizard-provider-name"
              value={name}
              onChange={(event) => {
                setName(event.target.value);
                setPreview(null);
              }}
              placeholder="Ví dụ: My Gateway"
            />
          </div>
          <div className="grid gap-2">
            <Label htmlFor="wizard-base-url">Base URL hoặc full endpoint</Label>
            <Input
              id="wizard-base-url"
              value={baseUrl}
              onChange={(event) => {
                setBaseUrl(event.target.value);
                invalidateConnectionProbe();
              }}
              placeholder="https://provider.example/v1"
              type="url"
            />
          </div>
          <div className="grid gap-2">
            <Label htmlFor="wizard-api-key">API key</Label>
            <Input
              id="wizard-api-key"
              value={apiKey}
              onChange={(event) => {
                setApiKey(event.target.value);
                invalidateConnectionProbe();
              }}
              placeholder="Key chỉ được giữ trong phiên thiết lập"
              type="password"
              autoComplete="off"
            />
          </div>

          <div className="rounded-lg border border-amber-500/30 bg-amber-500/5 p-3 text-sm">
            Probe sẽ gửi tối đa vài request nhỏ để kiểm tra `/models` và
            protocol. Có thể phát sinh một lượng token rất nhỏ.
          </div>

          <Button onClick={runProbe} disabled={busy} variant="secondary">
            {busy && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
            Cho phép kiểm tra kết nối
          </Button>

          {probe && (
            <div className="grid gap-3 rounded-lg border p-3">
              <div className="flex items-center gap-2 text-sm font-medium">
                <ShieldCheck className="h-4 w-4 text-emerald-500" />
                {probe.models.length} model được phát hiện tại{" "}
                {probe.normalizedBaseUrl}
              </div>
              {(probe.warnings.length > 0 ||
                probe.capabilities.every(
                  (capability) => !capability.supported,
                )) && (
                <div
                  role="alert"
                  className="rounded-md border border-amber-500/30 bg-amber-500/5 p-2 text-xs text-amber-700"
                >
                  {probe.warnings.map((warning) => (
                    <p key={warning}>{warning}</p>
                  ))}
                  {probe.capabilities
                    .filter((capability) => !capability.supported)
                    .map((capability) => (
                      <p key={capability.protocol}>
                        {PROTOCOL_LABELS[capability.protocol]}:{" "}
                        {capability.evidence.join("; ") || "không có phản hồi"}
                      </p>
                    ))}
                </div>
              )}
              <div className="grid gap-2 sm:grid-cols-2">
                {(
                  ["claude", "codex", "claude-desktop", "opencode"] as const
                ).map((app) => {
                  const config = {
                    claude: {
                      label: "Claude Code",
                      selected: claudeSelected,
                      protocol: claudeProtocol,
                      setSelected: setClaudeSelected,
                      setProtocol: setClaudeProtocol,
                    },
                    codex: {
                      label: "Codex",
                      selected: codexSelected,
                      protocol: codexProtocol,
                      setSelected: setCodexSelected,
                      setProtocol: setCodexProtocol,
                    },
                    "claude-desktop": {
                      label: "Claude Cowork",
                      selected: claudeDesktopSelected,
                      protocol: claudeDesktopProtocol,
                      setSelected: setClaudeDesktopSelected,
                      setProtocol: setClaudeDesktopProtocol,
                    },
                    opencode: {
                      label: "OpenCode",
                      selected: opencodeSelected,
                      protocol: opencodeProtocol,
                      setSelected: setOpencodeSelected,
                      setProtocol: setOpencodeProtocol,
                    },
                  }[app];
                  return (
                    <div key={app} className="grid gap-2 rounded-md border p-3">
                      <label className="flex items-center gap-2 text-sm font-medium">
                        <input
                          type="checkbox"
                          checked={config.selected}
                          onChange={(event) => {
                            config.setSelected(event.target.checked);
                            setPreview(null);
                          }}
                        />
                        {config.label}
                      </label>
                      <select
                        aria-label={`Protocol ${config.label}`}
                        className="h-9 rounded-md border bg-background px-2 text-sm"
                        value={config.protocol ?? ""}
                        disabled={!config.selected}
                        onChange={(event) => {
                          config.setProtocol(
                            (event.target.value || undefined) as
                              | UpstreamProtocol
                              | undefined,
                          );
                          setPreview(null);
                        }}
                      >
                        <option value="">Chọn protocol</option>
                        {probe.capabilities.map((capability) => (
                          <option
                            key={capability.protocol}
                            value={capability.protocol}
                          >
                            {PROTOCOL_LABELS[capability.protocol]}
                            {capability.supported ? "" : " (chưa xác minh)"}
                          </option>
                        ))}
                      </select>
                      {config.selected && !config.protocol && (
                        <p className="text-xs text-amber-600">
                          Probe chưa xác nhận được protocol bằng model thử
                          nghiệm. Bạn có thể chọn protocol phù hợp để tiếp tục
                          hoặc chọn model khác rồi kiểm tra kết nối lại.
                        </p>
                      )}
                    </div>
                  );
                })}
              </div>
              <div className="grid gap-2">
                <Label htmlFor="wizard-model">Model mặc định</Label>
                <select
                  id="wizard-model"
                  className="h-9 rounded-md border bg-background px-2 text-sm"
                  value={selectedModelIsDetected ? model : "__custom__"}
                  onChange={(event) => {
                    setModel(
                      event.target.value === "__custom__"
                        ? ""
                        : event.target.value,
                    );
                    setPreview(null);
                  }}
                >
                  {probe.models.map((item) => (
                    <option key={item.id} value={item.id}>
                      {modelLabel(item)}
                    </option>
                  ))}
                  <option value="__custom__">Tự nhập model khác...</option>
                </select>
                {!selectedModelIsDetected && (
                  <Input
                    aria-label="Model tùy chỉnh"
                    value={model}
                    onChange={(event) => {
                      setModel(event.target.value);
                      setPreview(null);
                    }}
                    placeholder="Nhập chính xác model ID upstream"
                  />
                )}
                <p className="text-xs text-muted-foreground">
                  Toàn bộ {probe.models.length} model được giữ trong catalog của
                  Codex, Claude Cowork và OpenCode. Tên trong ngoặc là model ID
                  thực sự gửi lên provider.
                </p>
              </div>
              <Button onClick={buildPreview} disabled={busy}>
                Xem preview cấu hình
              </Button>
            </div>
          )}

          {preview && (
            <div className="grid gap-2 rounded-lg border border-primary/30 bg-primary/5 p-3 text-sm">
              <strong>Preview</strong>
              {[
                preview.claude,
                preview.codex,
                preview.claudeDesktop,
                preview.opencode,
              ]
                .filter(Boolean)
                .map((item) => (
                  <div key={item!.app}>
                    <b>
                      {item!.app === "claude"
                        ? "Claude Code"
                        : item!.app === "codex"
                          ? "Codex"
                          : item!.app === "claude-desktop"
                            ? "Claude Cowork"
                            : "OpenCode"}
                    </b>
                    : {item!.mode}, {PROTOCOL_LABELS[item!.protocol]}, model `
                    {item!.model}`
                    <div className="text-muted-foreground">
                      {item!.filesToChange.join(", ")}
                    </div>
                  </div>
                ))}
              {preview.proxyWillStart && (
                <div className="text-amber-600">
                  Một ứng dụng cần local routing. Apply sẽ bật takeover sau khi
                  bạn xác nhận.
                </div>
              )}
            </div>
          )}
        </div>

        <DialogFooter>
          <Button
            variant="outline"
            onClick={() => onOpenChange(false)}
            disabled={busy}
          >
            Hủy
          </Button>
          <Button onClick={apply} disabled={busy || !preview}>
            {busy && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
            Apply cấu hình
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
