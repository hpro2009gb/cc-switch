import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ProviderSetupWizard } from "@/components/providers/ProviderSetupWizard";

const api = vi.hoisted(() => ({
  probe: vi.fn(),
  preview: vi.fn(),
  apply: vi.fn(),
}));

vi.mock("@tanstack/react-query", () => ({
  useQueryClient: () => ({ invalidateQueries: vi.fn() }),
}));

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn() },
}));

vi.mock("@/lib/api/provider-wizard", async (importOriginal) => {
  const original =
    await importOriginal<typeof import("@/lib/api/provider-wizard")>();
  return {
    ...original,
    providerWizardApi: api,
  };
});

vi.mock("@/components/ui/dialog", () => ({
  Dialog: ({ children }: { children: React.ReactNode }) => (
    <div>{children}</div>
  ),
  DialogContent: ({ children }: { children: React.ReactNode }) => (
    <div>{children}</div>
  ),
  DialogDescription: ({ children }: { children: React.ReactNode }) => (
    <p>{children}</p>
  ),
  DialogFooter: ({ children }: { children: React.ReactNode }) => (
    <div>{children}</div>
  ),
  DialogHeader: ({ children }: { children: React.ReactNode }) => (
    <div>{children}</div>
  ),
  DialogTitle: ({ children }: { children: React.ReactNode }) => (
    <h1>{children}</h1>
  ),
}));

describe("ProviderSetupWizard", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    api.probe.mockResolvedValue({
      normalizedBaseUrl: "https://gateway.example/v1",
      urlMode: "base",
      models: [
        {
          id: "claude-opus-5-thinking",
          displayName: "Claude Opus 5",
          ownedBy: "Anthropic",
        },
        { id: "claude-sonnet-5", displayName: "Claude Sonnet 5" },
      ],
      capabilities: [
        {
          protocol: "open_ai_chat",
          endpoint: "/v1/chat/completions",
          authMode: "bearer",
          supported: true,
          confidence: "high",
          evidence: [],
        },
      ],
      recommendedModel: "claude-opus-5-thinking",
      warnings: [],
    });
    api.preview.mockResolvedValue({
      providerId: "wizard-gateway",
      normalizedBaseUrl: "https://gateway.example/v1",
      urlMode: "base",
      codex: {
        app: "codex",
        providerId: "wizard-gateway",
        protocol: "open_ai_chat",
        mode: "proxy",
        model: "claude-sonnet-5",
        filesToChange: [],
        restartRequired: true,
        redactedConfig: {},
        warnings: [],
      },
      proxyWillStart: true,
      warnings: [],
    });
    api.apply.mockResolvedValue({
      appliedApps: ["codex"],
      rolledBack: false,
      rollbackErrors: [],
      restartRequiredApps: ["codex"],
    });
  });

  it("gửi toàn bộ model vào preview/apply và giữ model được chọn làm mặc định", async () => {
    render(
      <ProviderSetupWizard open onOpenChange={vi.fn()} initialApp="codex" />,
    );

    fireEvent.change(screen.getByLabelText("Tên provider"), {
      target: { value: "Gateway" },
    });
    fireEvent.change(screen.getByLabelText("Base URL hoặc full endpoint"), {
      target: { value: "https://gateway.example/v1" },
    });
    fireEvent.change(screen.getByLabelText("API key"), {
      target: { value: "secret" },
    });
    fireEvent.click(
      screen.getByRole("button", { name: "Cho phép kiểm tra kết nối" }),
    );

    expect(
      await screen.findByRole("option", {
        name: "Claude Opus 5 (claude-opus-5-thinking)",
      }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("option", {
        name: "Claude Sonnet 5 (claude-sonnet-5)",
      }),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("Protocol Codex")).toHaveValue("open_ai_chat");

    fireEvent.change(screen.getByLabelText("Model mặc định"), {
      target: { value: "claude-sonnet-5" },
    });
    fireEvent.click(
      screen.getByRole("button", { name: "Xem preview cấu hình" }),
    );

    await waitFor(() => expect(api.preview).toHaveBeenCalledTimes(1));
    expect(api.preview.mock.calls[0][0]).toMatchObject({
      model: "claude-sonnet-5",
      models: [{ id: "claude-opus-5-thinking" }, { id: "claude-sonnet-5" }],
    });

    fireEvent.click(screen.getByRole("button", { name: "Apply cấu hình" }));
    await waitFor(() => expect(api.apply).toHaveBeenCalledTimes(1));
    expect(api.apply.mock.calls[0][0]).toMatchObject({
      model: "claude-sonnet-5",
      models: [{ id: "claude-opus-5-thinking" }, { id: "claude-sonnet-5" }],
    });
  });

  it("chọn OpenCode từ dialog sẽ cài catalog qua local gateway", async () => {
    render(
      <ProviderSetupWizard open onOpenChange={vi.fn()} initialApp="opencode" />,
    );

    fireEvent.change(screen.getByLabelText("Tên provider"), {
      target: { value: "Gateway" },
    });
    fireEvent.change(screen.getByLabelText("Base URL hoặc full endpoint"), {
      target: { value: "https://gateway.example/v1" },
    });
    fireEvent.change(screen.getByLabelText("API key"), {
      target: { value: "secret" },
    });
    fireEvent.click(
      screen.getByRole("button", { name: "Cho phép kiểm tra kết nối" }),
    );

    await screen.findByLabelText("Model mặc định");
    fireEvent.click(
      screen.getByRole("button", { name: "Xem preview cấu hình" }),
    );

    await waitFor(() => expect(api.preview).toHaveBeenCalledTimes(1));
    expect(api.preview.mock.calls[0][0]).toMatchObject({
      opencodeProtocol: "open_ai_chat",
      models: [{ id: "claude-opus-5-thinking" }, { id: "claude-sonnet-5" }],
    });
  });

  it("cho phép chọn protocol chưa xác minh thay vì khóa nút preview", async () => {
    api.probe.mockResolvedValueOnce({
      normalizedBaseUrl: "https://gateway.example/v1",
      urlMode: "base",
      models: [{ id: "claude-opus-5" }],
      capabilities: [
        {
          protocol: "open_ai_chat",
          endpoint: "/v1/chat/completions",
          authMode: "bearer",
          supported: false,
          confidence: "low",
          evidence: ["HTTP 404"],
        },
      ],
      recommendedModel: "claude-opus-5",
      warnings: ["No protocol probe succeeded"],
    });

    render(
      <ProviderSetupWizard open onOpenChange={vi.fn()} initialApp="claude" />,
    );
    fireEvent.change(screen.getByLabelText("Tên provider"), {
      target: { value: "Gateway" },
    });
    fireEvent.change(screen.getByLabelText("Base URL hoặc full endpoint"), {
      target: { value: "https://gateway.example/v1" },
    });
    fireEvent.change(screen.getByLabelText("API key"), {
      target: { value: "secret" },
    });
    fireEvent.click(
      screen.getByRole("button", { name: "Cho phép kiểm tra kết nối" }),
    );

    expect(
      await screen.findAllByRole("option", {
        name: "OpenAI Chat Completions (chưa xác minh)",
      }),
    ).toHaveLength(4);
    const previewButton = screen.getByRole("button", {
      name: "Xem preview cấu hình",
    });
    expect(previewButton).toBeEnabled();

    fireEvent.change(screen.getByLabelText("Protocol Claude Code"), {
      target: { value: "open_ai_chat" },
    });
    fireEvent.click(previewButton);

    await waitFor(() => expect(api.preview).toHaveBeenCalledTimes(1));
    expect(api.preview.mock.calls[0][0]).toMatchObject({
      claudeProtocol: "open_ai_chat",
      model: "claude-opus-5",
    });
  });

  it("không xóa protocol đã chọn khi probe lại cùng kết nối bị lỗi tạm thời", async () => {
    render(
      <ProviderSetupWizard open onOpenChange={vi.fn()} initialApp="codex" />,
    );
    fireEvent.change(screen.getByLabelText("Tên provider"), {
      target: { value: "Gateway" },
    });
    fireEvent.change(screen.getByLabelText("Base URL hoặc full endpoint"), {
      target: { value: "https://gateway.example/v1" },
    });
    fireEvent.change(screen.getByLabelText("API key"), {
      target: { value: "secret" },
    });
    fireEvent.click(
      screen.getByRole("button", { name: "Cho phép kiểm tra kết nối" }),
    );
    await waitFor(() =>
      expect(screen.getByLabelText("Protocol Codex")).toHaveValue(
        "open_ai_chat",
      ),
    );

    api.probe.mockResolvedValueOnce({
      normalizedBaseUrl: "https://gateway.example/v1",
      urlMode: "base",
      models: [{ id: "claude-opus-5-thinking" }],
      capabilities: [
        {
          protocol: "open_ai_chat",
          endpoint: "https://gateway.example/v1/chat/completions",
          authMode: "bearer",
          supported: false,
          confidence: "low",
          evidence: ["Bearer: network error: connection closed"],
        },
      ],
      recommendedModel: "claude-opus-5-thinking",
      warnings: ["No protocol probe succeeded"],
    });
    fireEvent.click(
      screen.getByRole("button", { name: "Cho phép kiểm tra kết nối" }),
    );

    await waitFor(() => expect(api.probe).toHaveBeenCalledTimes(2));
    expect(screen.getByLabelText("Protocol Codex")).toHaveValue("open_ai_chat");
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "connection closed",
    );
  });

  it("không gửi model của provider trước sau khi Base URL thay đổi", async () => {
    render(
      <ProviderSetupWizard open onOpenChange={vi.fn()} initialApp="codex" />,
    );
    fireEvent.change(screen.getByLabelText("Tên provider"), {
      target: { value: "Gateway" },
    });
    fireEvent.change(screen.getByLabelText("Base URL hoặc full endpoint"), {
      target: { value: "https://gateway.example/v1" },
    });
    fireEvent.change(screen.getByLabelText("API key"), {
      target: { value: "secret" },
    });
    fireEvent.click(
      screen.getByRole("button", { name: "Cho phép kiểm tra kết nối" }),
    );
    await screen.findByLabelText("Model mặc định");
    fireEvent.change(screen.getByLabelText("Model mặc định"), {
      target: { value: "claude-sonnet-5" },
    });

    api.probe.mockResolvedValueOnce({
      normalizedBaseUrl: "https://second.example/v1",
      urlMode: "base",
      models: [{ id: "fresh-model" }],
      capabilities: [
        {
          protocol: "open_ai_responses",
          endpoint: "https://second.example/v1/responses",
          authMode: "bearer",
          supported: true,
          confidence: "high",
          evidence: ["Bearer: HTTP 200 OK"],
        },
      ],
      recommendedModel: "fresh-model",
      warnings: [],
    });
    fireEvent.change(screen.getByLabelText("Base URL hoặc full endpoint"), {
      target: { value: "https://second.example/v1" },
    });
    fireEvent.click(
      screen.getByRole("button", { name: "Cho phép kiểm tra kết nối" }),
    );

    await waitFor(() => expect(api.probe).toHaveBeenCalledTimes(2));
    expect(api.probe.mock.calls[1][0]).toMatchObject({
      baseUrl: "https://second.example/v1",
      model: undefined,
    });
    await waitFor(() =>
      expect(screen.getByLabelText("Protocol Codex")).toHaveValue(
        "open_ai_responses",
      ),
    );
  });
});
