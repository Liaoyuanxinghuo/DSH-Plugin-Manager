import { describe, expect, it, vi, beforeEach } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import App from "./App";

// mock Tauri invoke
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

// mock 保存对话框（导出诊断/导出 profile 用）
vi.mock("@tauri-apps/plugin-dialog", () => ({
  save: vi.fn().mockResolvedValue("C:\\diag.zip"),
  open: vi.fn().mockResolvedValue(null),
}));

import { invoke } from "@tauri-apps/api/core";
import { save as mockSave, open as mockOpen } from "@tauri-apps/plugin-dialog";

const mockInvoke = vi.mocked(invoke);

function mockAll() {
  mockInvoke.mockImplementation((cmd: string) => {
    switch (cmd) {
      case "scan_envs":
        return Promise.resolve([
          {
            id: "global",
            name: "全局 CLI (dsh 0.1.0-rc.7)",
            source: "globalCli",
            version: "0.1.0-rc.7",
            homeDir: "C:\\Users\\test\\.dsh",
            runCommand: "dsh",
            binPath: "C:\\Users\\test\\AppData\\Roaming\\npm\\dsh.cmd",
            scanProfilesDir: null,
          },
        ]);
      case "list_all_profiles":
        return Promise.resolve([
          {
            name: "web",
            profilesDir: "C:\\Users\\test\\.dsh\\profiles",
            path: "C:\\Users\\test\\.dsh\\profiles\\web",
            pluginCount: 3,
            dataSize: 1024 * 1024,
            modified: "2026-09-27 10:00",
            hasPatch: true,
            hasLock: true,
            hasPackage: true,
          },
          {
            name: "headless",
            profilesDir: "C:\\Users\\test\\.dsh\\profiles",
            path: "C:\\Users\\test\\.dsh\\profiles\\headless",
            pluginCount: 0,
            dataSize: 0,
            modified: "2026-09-01 08:00",
            hasPatch: false,
            hasLock: false,
            hasPackage: false,
          },
        ]);
      case "list_plugins":
        return Promise.resolve([
          {
            name: "@deepseek-ai/dsh-mcp-client",
            spec: "0.0.1-rc.1",
            isBundle: false,
            compatible: null,
            incompatibleReason: null,
            isDisabled: false,
          },
          {
            name: "@michengai/dsh-codex-ui",
            spec: "^1.1.18",
            isBundle: true,
            compatible: null,
            incompatibleReason: null,
            isDisabled: true,
          },
        ]);
      case "dsh_status":
        return Promise.resolve({ running: false, process: null, portOpen: false });
      case "list_running_cmd":
        return Promise.resolve([]);
      case "get_env_paths":
        return Promise.resolve({
          homeDir: "C:\\Users\\test\\.dsh",
          profileDir: "C:\\Users\\test\\.dsh\\profiles\\web",
          profilesDir: "C:\\Users\\test\\.dsh\\profiles",
          sessionsDir: "C:\\Users\\test\\.dsh\\sessions",
          logsDir: "C:\\Users\\test\\.dsh\\logs",
          binPath: "C:\\Users\\test\\AppData\\Roaming\\npm\\dsh.cmd",
        });
      case "get_env":
        return Promise.resolve(null);
      case "check_portable_runtime_cmd":
        return Promise.resolve(true);
      case "init_portable_runtime_cmd":
        return Promise.resolve("便携运行时已就绪");
      default:
        return Promise.resolve(null);
    }
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  // mockReset: true 会清掉 vi.mock 工厂设置的实现，需重新设置
  vi.mocked(mockSave).mockResolvedValue("C:\\diag.zip");
  vi.mocked(mockOpen).mockResolvedValue(null);
  mockAll();
});

describe("App 主界面", () => {
  it("扫本体按钮点击直接打开目录选择，不弹教学框", async () => {
    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("＋扫本体")).toBeInTheDocument();
    });
    await user.click(screen.getByText("＋扫本体"));
    // 不应出现教学弹窗文案
    expect(screen.queryByText(/扫描 DSH 本体/)).not.toBeInTheDocument();
    expect(screen.queryByText(/什么时候需要用它/)).not.toBeInTheDocument();
  });

  it("DSH 环境问号按钮显示使用说明", async () => {
    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("DSH 环境")).toBeInTheDocument();
    });
    await user.click(screen.getAllByText("?")[0]);
    await waitFor(() => {
      expect(screen.getByText("DSH 环境（左栏）使用说明")).toBeInTheDocument();
    });
    expect(screen.getByText(/任意环境 × 任意 profile 可自由组合/)).toBeInTheDocument();
    await user.click(screen.getByText("知道了"));
    expect(screen.queryByText("DSH 环境（左栏）使用说明")).not.toBeInTheDocument();
  });

  it("Profiles 问号按钮显示使用说明", async () => {
    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("Profiles")).toBeInTheDocument();
    });
    await user.click(screen.getAllByText("?")[1]);
    await waitFor(() => {
      expect(screen.getByText("Profiles（中栏）使用说明")).toBeInTheDocument();
    });
    expect(screen.getByText(/不绑定任何 DSH 版本/)).toBeInTheDocument();
  });

  it("渲染标题与环境列表", async () => {
    render(<App />);
    expect(screen.getByText("DSH Manager")).toBeInTheDocument();
    await waitFor(() => {
      expect(screen.getByText(/全局 CLI/)).toBeInTheDocument();
    });
  });

  it("渲染 profile 列表并默认选中第一个", async () => {
    render(<App />);
    await waitFor(() => {
      // web 出现在 profile 列表和标题两处，用 getAllByText
      expect(screen.getAllByText("web").length).toBeGreaterThan(0);
      expect(screen.getByText("headless")).toBeInTheDocument();
    });
    // 默认选中 web profile，显示其插件
    await waitFor(() => {
      expect(screen.getByText(/已安装插件/)).toBeInTheDocument();
      expect(screen.getByText("@michengai/dsh-codex-ui")).toBeInTheDocument();
    });
  });

  it("点击刷新会立刻重扫 Profiles 并与新增/删除同步", async () => {
    let listCalls = 0;
    const web = {
      name: "web",
      profilesDir: "C:\\Users\\test\\.dsh\\profiles",
      path: "C:\\Users\\test\\.dsh\\profiles\\web",
      pluginCount: 1,
      dataSize: 0,
      modified: "",
      hasPatch: false,
      hasLock: false,
      hasPackage: true,
    };
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "list_all_profiles") {
        listCalls += 1;
        // 第一次：只有 web；刷新后：多出 newone（模拟外部新建）
        if (listCalls === 1) return Promise.resolve([web]);
        return Promise.resolve([
          web,
          { ...web, name: "newone", path: "C:\\Users\\test\\.dsh\\profiles\\newone" },
        ]);
      }
      return mockAllDefault(cmd);
    });
    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => {
      expect(screen.getAllByText("web").length).toBeGreaterThan(0);
    });
    expect(screen.queryByText("newone")).toBeNull();
    await user.click(screen.getByRole("button", { name: /刷新/ }));
    // 刷新后 list_all_profiles 再次拉取，新增 profile 出现在列表
    await waitFor(() => {
      expect(screen.getByText("newone")).toBeInTheDocument();
    });
    expect(listCalls).toBeGreaterThanOrEqual(2);
    expect(screen.getByText(/已刷新/)).toBeInTheDocument();
  });

  it("文件系统快捷入口显示「当前 Profile 目录」并点击调用 open_path", async () => {
    const user = userEvent.setup();
    render(<App />);
    // 按钮文本带 📂 前缀 → 用正则匹配
    await waitFor(() => {
      expect(screen.getByText(/当前 Profile 目录/)).toBeInTheDocument();
    });
    await user.click(screen.getByText(/当前 Profile 目录/));
    expect(mockInvoke).toHaveBeenCalledWith("open_path", {
      path: "C:\\Users\\test\\.dsh\\profiles\\web",
    });
  });

  it("显示启动按钮与文件入口", async () => {
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("▶ 启动 DSH")).toBeInTheDocument();
    });
    // 文件入口依赖异步的 get_env_paths，单独等待（按钮文本含 emoji，用正则）
    await waitFor(() => {
      expect(screen.getByText(/DSH_HOME/)).toBeInTheDocument();
    });
    expect(screen.getByText(/Profiles 目录/)).toBeInTheDocument();
  });

  it("进程退出后，重启按钮自动变回启动按钮", async () => {
    // 初始：web 实例运行中 → 显示「重启」
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "list_running_cmd") {
        return Promise.resolve([
          { envId: "global", profile: "web", profilesDir: "C:\\Users\\test\\.dsh\\profiles", pid: 123, port: 3080, startedAt: "2026-09-27 12:00" },
        ]);
      }
      return mockAllDefault(cmd);
    });
    render(<App />);
    await screen.findByText("↻ 重启 DSH");
    // 进程退出（后端 list_running 清理死进程返回空）→ 等一个轮询周期后按钮变回「启动」
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "list_running_cmd") return Promise.resolve([]);
      return mockAllDefault(cmd);
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 3100));
    });
    expect(screen.getByText("▶ 启动 DSH")).toBeInTheDocument();
  });
  it("启动就绪后不再自动打开浏览器，点「打开界面」才打开带 token 的地址", async () => {
    mockInvoke.mockImplementation((cmd: string, args?: unknown) => {
      const a = (args ?? {}) as Record<string, unknown>;
      if (cmd === "start_dsh_cmd") {
        return Promise.resolve({
          success: true,
          pid: 12345,
          message: "已启动",
          port: a.port ?? 3080,
          url: "http://127.0.0.1:3080/?token=abc",
        });
      }
      if (cmd === "dsh_status") {
        return Promise.resolve({ running: true, process: null, portOpen: true });
      }
      return mockAllDefault(cmd);
    });
    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("web")).toBeInTheDocument();
    });
    await user.click(screen.getByText("▶ 启动 DSH"));
    // 启动就绪后（portOpen=true）不应自动调用 open_url_cmd
    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith("start_dsh_cmd", expect.anything());
    });
    // 轮询周期内 open_url_cmd 一次都不能被自动调用
    await new Promise((res) => setTimeout(res, 3500));
    const autoCalls = mockInvoke.mock.calls.filter((c) => c[0] === "open_url_cmd");
    expect(autoCalls).toHaveLength(0);
  });

  it("点击「打开界面」调用 open_dsh_web_cmd（后端实时提取带 token 地址）", async () => {
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "list_running_cmd") {
        return Promise.resolve([
          {
            envId: "global",
            profilesDir: "C:\\Users\\test\\.dsh\\profiles",
            profile: "web",
            pid: 999,
            port: 3080,
            startedAt: "",
            url: "http://127.0.0.1:3080/?token=abc",
            logPath: "C:\\Users\\test\\AppData\\Local\\Temp\\dshpm-run-web-1.log",
          },
        ]);
      }
      return mockAllDefault(cmd);
    });
    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("🌐 打开界面")).toBeInTheDocument();
    });
    await user.click(screen.getByText("🌐 打开界面"));
    expect(mockInvoke).toHaveBeenCalledWith("open_dsh_web_cmd", {
      envId: "global",
      profile: "web",
      profilesDirStr: "C:\\Users\\test\\.dsh\\profiles",
    });
  });

  it("安装插件对话框明确展示目标：当前环境版本 × profile 及来源目录", async () => {
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "list_all_profiles") {
        return Promise.resolve([
          { name: "web", profilesDir: "C:\\Users\\test\\.dsh\\profiles", path: "C:\\Users\\test\\.dsh\\profiles\\web", pluginCount: 1, dataSize: 0, modified: "", hasPatch: false, hasLock: false, hasPackage: true },
        ]);
      }
      return mockAllDefault(cmd);
    });
    const user = userEvent.setup();
    render(<App />);

    await waitFor(() => {
      expect(screen.getByText(/当前 Profile 目录/)).toBeInTheDocument();
    });
    // 插件列表卡片的安装按钮（注意排除"已安装插件（N）"标题，用精确按钮文本）
    const installBtn = screen.getByText("＋安装插件");
    await user.click(installBtn);
    // env.name 含半角括号（如 "全局 CLI (dsh 0.1.0-rc.7)"），目标条追加全角括号版本信息
    await waitFor(() => {
      expect(screen.getByText(/目标 全局 CLI/)).toBeInTheDocument();
    });
    expect(screen.getByText(/× web/)).toBeInTheDocument();
    expect(screen.getByText(/dsh 0.1.0-rc.7）/)).toBeInTheDocument();
    // 切到"本地导入" tab，安装按钮明确标注目标组合
    await user.click(screen.getByText("本地导入"));
    expect(screen.getByText(/安装到 全局 CLI.* × web/)).toBeInTheDocument();
  });

  it("记住的 profile 不在当前合并列表时静默跳过插件加载，不报不存在", async () => {
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "list_all_profiles") {
        // 空的 profile 列表：上次记住的 profile 来源目录未扫到
        return Promise.resolve([]);
      }
      return mockAllDefault(cmd);
    });
    render(<App />);
    await waitFor(() => {
      expect(
        mockInvoke.mock.calls.some((c) => c[0] === "list_all_profiles"),
      ).toBe(true);
    });
    // 不应调用 list_plugins，也不应显示错误
    const pluginCalls = mockInvoke.mock.calls.filter((c) => c[0] === "list_plugins");
    expect(pluginCalls).toHaveLength(0);
    expect(screen.queryByText(/不存在/)).not.toBeInTheDocument();
  });

  it("点击启动按钮会调用 start_dsh_cmd", async () => {
    // 启动命令 mock：返回成功
    mockInvoke.mockImplementation((cmd: string, args?: unknown) => {
      const a = (args ?? {}) as Record<string, unknown>;
      if (cmd === "start_dsh_cmd") {
        return Promise.resolve({
          success: true,
          pid: 12345,
          message: "已启动",
          port: a.port ?? 3080,
          url: "http://localhost:3080",
        });
      }
      if (cmd === "dsh_status") {
        return Promise.resolve({ running: true, process: null, portOpen: true });
      }
      return mockAllDefault(cmd);
    });

    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("▶ 启动 DSH")).toBeInTheDocument();
    });
    await user.click(screen.getByText("▶ 启动 DSH"));
    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith("start_dsh_cmd", {
        envId: "global",
        profile: "web",
        profilesDirStr: "C:\\Users\\test\\.dsh\\profiles",
      });
    });
  });
});

describe("M3 插件管理", () => {
  it("点击跨来源目录的 profile 后，插件加载携带该 profile 的来源目录（竞态回归）", async () => {
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "list_all_profiles") {
        return Promise.resolve([
          { name: "web", profilesDir: "C:\\Users\\test\\.dsh\\profiles", path: "C:\\Users\\test\\.dsh\\profiles\\web", pluginCount: 1, dataSize: 0, modified: "", hasPatch: false, hasLock: false, hasPackage: true },
          { name: "harness-p", profilesDir: "C:\\Users\\test\\.dsh-packs\\hp\\profiles", path: "C:\\Users\\test\\.dsh-packs\\hp\\profiles\\harness-p", pluginCount: 2, dataSize: 0, modified: "", hasPatch: false, hasLock: false, hasPackage: true },
        ]);
      }
      if (cmd === "list_plugins") {
        return Promise.resolve([]);
      }
      return mockAllDefault(cmd);
    });
    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("harness-p")).toBeInTheDocument();
    });
    await user.click(screen.getByText("harness-p"));
    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith("list_plugins", {
        envId: "global",
        profile: "harness-p",
        profilesDirStr: "C:\\Users\\test\\.dsh-packs\\hp\\profiles",
      });
    });
  });

  it("插件行渲染拉杆开关，停用插件显示已停用标签", async () => {
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("@michengai/dsh-codex-ui")).toBeInTheDocument();
    });
    // 两个插件各有一个开关；codex-ui 停用 → 开关无 on 类
    const switches = screen.getAllByRole("switch");
    expect(switches.length).toBe(2);
    expect(switches[0].getAttribute("aria-checked")).toBe("true");
    expect(switches[1].getAttribute("aria-checked")).toBe("false");
    expect(screen.getByText("已停用")).toBeInTheDocument();
  });

  it("点击开关调用 set_plugin_enabled_cmd 并刷新列表", async () => {
    mockInvoke.mockImplementation((cmd: string, args?: unknown) => {
      const a = (args ?? {}) as Record<string, unknown>;
      if (cmd === "set_plugin_enabled_cmd") {
        return Promise.resolve(a.enabled ? [] : ["dsh-mcp-client"]);
      }
      return mockAllDefault(cmd);
    });
    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => {
      expect(screen.getAllByRole("switch").length).toBe(2);
    });
    // 点击第一个（启用中的 mcp-client）开关 → 停用
    await user.click(screen.getAllByRole("switch")[0]);
    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith("set_plugin_enabled_cmd", {
        envId: "global",
        profile: "web",
        profilesDirStr: "C:\\Users\\test\\.dsh\\profiles",
        packageName: "@deepseek-ai/dsh-mcp-client",
        enabled: false,
      });
    });
  });

  it("点击卸载弹出确认框，确认后调用 remove_plugin_cmd", async () => {
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "remove_plugin_cmd") {
        return Promise.resolve({ success: true, exitCode: 0, summary: "ok" });
      }
      return mockAllDefault(cmd);
    });
    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => {
      expect(screen.getAllByText("卸载").length).toBeGreaterThan(0);
    });
    await user.click(screen.getAllByText("卸载")[0]);
    await waitFor(() => {
      expect(screen.getByText(/卸载插件/)).toBeInTheDocument();
    });
    await user.click(screen.getByText("确认卸载"));
    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith("remove_plugin_cmd", {
        envId: "global",
        profile: "web",
        profilesDirStr: "C:\\Users\\test\\.dsh\\profiles",
        name: "@deepseek-ai/dsh-mcp-client",
      });
    });
  });

  it("检查更新按钮调用 check_updates_cmd 并显示新版提示", async () => {
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "check_updates_cmd") {
        return Promise.resolve([
          {
            name: "@deepseek-ai/dsh-mcp-client",
            current: "0.0.1-rc.1",
            latest: "0.1.0-rc.2",
            updatable: true,
            error: null,
          },
          {
            name: "@michengai/dsh-codex-ui",
            current: "^1.1.18",
            latest: "1.1.18",
            updatable: false,
            error: null,
          },
        ]);
      }
      return mockAllDefault(cmd);
    });
    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("⟳ 检查更新")).toBeInTheDocument();
    });
    await user.click(screen.getByText("⟳ 检查更新"));
    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith("check_updates_cmd", {
        envId: "global",
        profile: "web",
        profilesDirStr: "C:\\Users\\test\\.dsh\\profiles",
      });
    });
    await waitFor(() => {
      expect(screen.getByText(/新版 0\.1\.0-rc\.2/)).toBeInTheDocument();
    });
  });
});

describe("新建/删除 Profile", () => {
  it("点击＋新建弹出表单，选择 profiles 目录后调用 create_profile_cmd 并选中新 profile", async () => {
    const profilesDir = "C:\\Users\\test\\.dsh\\profiles";
    vi.mocked(mockOpen).mockResolvedValue(profilesDir);
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "create_profile_cmd") {
        return Promise.resolve({ name: "dev", path: "C:\\Users\\test\\.dsh\\profiles\\dev" });
      }
      if (cmd === "list_all_profiles") {
        return Promise.resolve([
          { name: "dev", profilesDir, path: "", pluginCount: 0, dataSize: 0, modified: "", hasPatch: false, hasLock: false, hasPackage: true },
        ]);
      }
      return mockAllDefault(cmd);
    });
    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("＋新建")).toBeInTheDocument();
    });
    await user.click(screen.getByText("＋新建"));
    await waitFor(() => {
      expect(screen.getByText("新建 Profile")).toBeInTheDocument();
    });
    await user.click(screen.getByText("选择目录"));
    await waitFor(() => {
      expect(screen.getByDisplayValue(profilesDir)).toBeInTheDocument();
    });
    await user.type(screen.getByPlaceholderText("如：dev / test / 0.1.7-rc.2"), "dev");
    await user.click(screen.getByText("创建"));
    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith("create_profile_cmd", {
        envId: "global",
        name: "dev",
        profilesDirStr: profilesDir,
      });
    });
    await waitFor(() => {
      expect(screen.getByText(/已创建 profile/)).toBeInTheDocument();
    });
  });

  it("新建表单校验非法字符", async () => {
    const profilesDir = "C:\\Users\\test\\.dsh\\profiles";
    vi.mocked(mockOpen).mockResolvedValue(profilesDir);
    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("＋新建")).toBeInTheDocument();
    });
    await user.click(screen.getByText("＋新建"));
    await user.click(screen.getByText("选择目录"));
    await waitFor(() => {
      expect(screen.getByDisplayValue(profilesDir)).toBeInTheDocument();
    });
    await user.type(screen.getByPlaceholderText("如：dev / test / 0.1.7-rc.2"), "a/b");
    await user.click(screen.getByText("创建"));
    await waitFor(() => {
      expect(screen.getByText(/不能包含/)).toBeInTheDocument();
    });
    expect(mockInvoke).not.toHaveBeenCalledWith("create_profile_cmd", expect.anything());
  });

  it("新建时选到非 profiles 目录须重新选择", async () => {
    vi.mocked(mockOpen).mockResolvedValue("C:\\Users\\test\\Downloads");
    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("＋新建")).toBeInTheDocument();
    });
    await user.click(screen.getByText("＋新建"));
    await user.click(screen.getByText("选择目录"));
    await waitFor(() => {
      expect(screen.getByText(/不是 profiles 文件夹/)).toBeInTheDocument();
    });
    expect(screen.getByText("创建")).toBeDisabled();
    expect(mockInvoke).not.toHaveBeenCalledWith("create_profile_cmd", expect.anything());
  });

  it("点击删除弹出确认框，确认后调用 delete_profile_cmd", async () => {
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "delete_profile_cmd") {
        return Promise.resolve(null);
      }
      return mockAllDefault(cmd);
    });
    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => {
      expect(screen.getAllByText("🗑 删除").length).toBeGreaterThan(0);
    });
    await user.click(screen.getAllByText("🗑 删除")[0]);
    await waitFor(() => {
      expect(screen.getByText(/删除 profile/)).toBeInTheDocument();
    });
    await user.click(screen.getByText("确认删除"));
    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith("delete_profile_cmd", {
        envId: "global",
        name: "web",
        profilesDirStr: "C:\\Users\\test\\.dsh\\profiles",
      });
    });
  });

  it("删除非选中项时，profilesDir 用该项自己的来源（回归：勿传空/选中项目录）", async () => {
    const dirA = "C:\\Users\\test\\.dsh\\profiles";
    const dirB = "C:\\scan\\other\\profiles";
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "list_all_profiles") {
        return Promise.resolve([
          {
            name: "web",
            profilesDir: dirA,
            path: `${dirA}\\web`,
            pluginCount: 0,
            dataSize: 0,
            modified: "",
            hasPatch: false,
            hasLock: false,
            hasPackage: true,
          },
          {
            name: "from-scan",
            profilesDir: dirB,
            path: `${dirB}\\from-scan`,
            pluginCount: 0,
            dataSize: 0,
            modified: "",
            hasPatch: false,
            hasLock: false,
            hasPackage: true,
          },
        ]);
      }
      if (cmd === "delete_profile_cmd") {
        return Promise.resolve(null);
      }
      return mockAllDefault(cmd);
    });
    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("from-scan")).toBeInTheDocument();
    });
    // 删除第二项（from-scan，来源 dirB），而不是选中的 web
    const delBtns = screen.getAllByText("🗑 删除");
    await user.click(delBtns[1]);
    await waitFor(() => {
      expect(screen.getByText(/删除 profile「from-scan」/)).toBeInTheDocument();
    });
    await user.click(screen.getByText("确认删除"));
    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith("delete_profile_cmd", {
        envId: "global",
        name: "from-scan",
        profilesDirStr: dirB,
      });
    });
    expect(mockInvoke).not.toHaveBeenCalledWith("delete_profile_cmd", {
      envId: "global",
      name: "from-scan",
      profilesDirStr: "",
    });
  });
});

describe("M4 健康与维护", () => {
  it("依赖检查显示缺失项并可修复", async () => {
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "check_deps_cmd") {
        return Promise.resolve([
          { name: "@deepseek-ai/dsh-mcp-client", declared: "0.0.1", installed: true, issue: "ok" },
          { name: "dshmarket", declared: "^1.0", installed: false, issue: "missing" },
        ]);
      }
      if (cmd === "fix_deps_cmd") {
        return Promise.resolve({ success: true, exitCode: 0, summary: "done" });
      }
      return mockAllDefault(cmd);
    });
    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("⟳ 依赖检查")).toBeInTheDocument();
    });
    await user.click(screen.getByText("⟳ 依赖检查"));
    await waitFor(() => {
      expect(screen.getByText(/1\/2 已安装/)).toBeInTheDocument();
      expect(screen.getByText("dshmarket")).toBeInTheDocument();
      expect(screen.getByText("缺失")).toBeInTheDocument();
    });
    // 修复按钮此时可用
    await user.click(screen.getByText("⚙ 修复依赖"));
    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith("fix_deps_cmd", {
        envId: "global",
        profile: "web",
        profilesDirStr: "C:\\Users\\test\\.dsh\\profiles",
      });
    });
  });

  it("扫描残留后显示列表，清理需二次确认", async () => {
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "scan_junk_cmd") {
        return Promise.resolve([
          { name: ".generations", path: "C:\\p\\.generations", size: 2048 },
          { name: "node_modules/.cache", path: "C:\\p\\nm\\.cache", size: 1024 },
        ]);
      }
      if (cmd === "clean_junk_cmd") {
        return Promise.resolve([".generations", "node_modules/.cache"]);
      }
      return mockAllDefault(cmd);
    });
    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("🧹 扫描残留")).toBeInTheDocument();
    });
    await user.click(screen.getByText("🧹 扫描残留"));
    await waitFor(() => {
      expect(screen.getByText(".generations")).toBeInTheDocument();
      expect(screen.getByText("node_modules/.cache")).toBeInTheDocument();
    });
    // 清理前需确认
    await user.click(screen.getByText("🗑 清理残留"));
    await waitFor(() => {
      expect(screen.getByText(/清理残留缓存/)).toBeInTheDocument();
    });
    await user.click(screen.getByText("确认清理"));
    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith("clean_junk_cmd", {
        envId: "global",
        profile: "web",
        profilesDirStr: "C:\\Users\\test\\.dsh\\profiles",
        names: [".generations", "node_modules/.cache"],
      });
    });
  });

  it("导出诊断包调用 export_diag_cmd", async () => {
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "export_diag_cmd") {
        return Promise.resolve({ fileCount: 12, zipSize: 1024, skippedSymlinks: [], targetPath: "C:\\diag.zip" });
      }
      return mockAllDefault(cmd);
    });
    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("📦 导出诊断包")).toBeInTheDocument();
    });
    await user.click(screen.getByText("📦 导出诊断包"));
    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalled();
    });
    const calls = mockInvoke.mock.calls.filter((c) => c[0] === "export_diag_cmd");

    expect(calls.length).toBeGreaterThan(0);
    const last = calls[calls.length - 1];
    expect(last[0]).toBe("export_diag_cmd");
    expect(last[1]).toEqual({ envId: "global", targetPath: "C:\\diag.zip" });
  });
});

describe("M5 设置与 DSH 下载", () => {
  it("设置弹窗加载并保存镜像源与下载目录", async () => {
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "get_settings_cmd") {
        return Promise.resolve({ npmRegistry: "https://registry.npmmirror.com", dshDownloadDir: "C:\\dsh-versions", githubMirror: "https://ghfast.top" });
      }
      if (cmd === "set_settings_cmd") {
        return Promise.resolve(null);
      }
      return mockAllDefault(cmd);
    });
    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("⚙ 设置")).toBeInTheDocument();
    });
    await user.click(screen.getByText("⚙ 设置"));
    // 镜像源为下拉列表：getByDisplayValue 匹配 select 时取选项文本，改用 combobox + toHaveValue
    const regSelect = screen.getByRole("combobox", { name: /npm 镜像源/ });
    await waitFor(() => {
      expect(regSelect).toHaveValue("https://registry.npmmirror.com");
    });
    // 选择「npm 官方源（直连）」选项（value 为空字符串）
    await user.selectOptions(regSelect, "");
    await user.click(screen.getByText("保存"));
    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith("set_settings_cmd", {
        npmRegistry: "",
        dshDownloadDir: "C:\\dsh-versions",
        githubMirror: "https://ghfast.top",
        githubMirrors: [],
      });
    });
  });

  it("DSH 下载：列版本 → 选版本 → 安装 → 添加环境", async () => {
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "get_settings_cmd") {
        return Promise.resolve({ npmRegistry: "https://registry.npmmirror.com", dshDownloadDir: "C:\\dsh-versions" });
      }
      if (cmd === "list_dsh_versions_cmd") {
        return Promise.resolve([
          { version: "0.1.7-rc.2", tags: ["next"] },
          { version: "0.1.5-rc.3", tags: ["latest"] },
        ]);
      }
      if (cmd === "install_dsh_version_cmd") {
        return Promise.resolve({ success: true, exitCode: 0, summary: "ok", binPath: "C:\\dsh-versions\\dsh-0.1.5-rc.3\\node_modules\\.bin\\dsh.cmd", version: "0.1.5-rc.3" });
      }
      if (cmd === "add_manual_env") {
        return Promise.resolve({ id: "manual-x", name: "dsh 0.1.5-rc.3 (dsh 0.1.5-rc.3)", source: "manual", version: "0.1.5-rc.3", homeDir: "C:\\Users\\test\\.dsh", runCommand: "C:\\dsh-versions\\dsh-0.1.5-rc.3\\node_modules\\.bin\\dsh.cmd", binPath: null, scanProfilesDir: null });
      }
      return mockAllDefault(cmd);
    });
    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("⬇ 下载DSH")).toBeInTheDocument();
    });
    await user.click(screen.getByText("⬇ 下载DSH"));
    await waitFor(() => {
      expect(screen.getByText("0.1.5-rc.3")).toBeInTheDocument();
    });
    await user.click(screen.getByText("⬇ 安装 dsh 0.1.5-rc.3"));
    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith("install_dsh_version_cmd", {
        version: "0.1.5-rc.3",
        targetDir: "C:\\dsh-versions",
      });
    });
    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith("add_manual_env", expect.anything());
    });
  });

  it("后端返回 snake_case 字段（旧序列化泄漏）时下载弹窗不崩溃", async () => {
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "get_settings_cmd") {
        // 模拟旧版本 Settings 未加 camelCase 时的真实返回
        return Promise.resolve({ npm_registry: "https://registry.npmmirror.com", dsh_download_dir: "C:\\dsh-versions" });
      }
      if (cmd === "list_dsh_versions_cmd") {
        return Promise.resolve([{ version: "0.1.5-rc.3", tags: ["latest"] }]);
      }
      return mockAllDefault(cmd);
    });
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("⬇ 下载DSH")).toBeInTheDocument();
    });
    // 点击后不崩溃、能正常显示版本列表与安装按钮
    fireEvent.click(screen.getByText("⬇ 下载DSH"));
    await waitFor(() => {
      expect(screen.getByText("⬇ 安装 dsh 0.1.5-rc.3")).toBeInTheDocument();
    });
    // 目录未从 snake_case 读到 → 显示占位符而非崩溃
    expect(screen.getByPlaceholderText("下载根目录（每版本独立子目录）")).toBeInTheDocument();
  });

  it("从 localStorage 恢复上次选择的环境与 profile", async () => {
    localStorage.setItem("dshpm-sel-env", "global");
    localStorage.setItem("dshpm-sel-profile", "web");
    localStorage.setItem("dshpm-sel-port", "3456");
    // 关闭自动端口，端口输入框才可见
    localStorage.setItem("dshpm-auto-port", "0");
    render(<App />);
    await waitFor(() => {
      expect(screen.getByDisplayValue("3456")).toBeInTheDocument();
    });
    await waitFor(() => {
      expect(screen.getByText(/已安装插件/)).toBeInTheDocument();
    });
    localStorage.removeItem("dshpm-sel-env");
    localStorage.removeItem("dshpm-sel-profile");
    localStorage.removeItem("dshpm-sel-port");
    localStorage.removeItem("dshpm-auto-port");
  });

  it("设置弹窗：关于本项目与检查更新", async () => {
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "get_settings_cmd") {
        return Promise.resolve({ npmRegistry: "https://registry.npmmirror.com", dshDownloadDir: "C:\\dsh-versions", githubMirror: "https://ghfast.top" });
      }
      if (cmd === "check_update_cmd") {
        return Promise.resolve({ current: "0.2.0", latest: "0.3.11", hasUpdate: true, url: "https://github.com/Liaoyuanxinghuo/DSH-Plugin-Manager/", error: "" });
      }
      return mockAllDefault(cmd);
    });
    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("⚙ 设置")).toBeInTheDocument();
    });
    await user.click(screen.getByText("⚙ 设置"));
    // 关于本项目：点开后显示项目地址按钮
    await user.click(screen.getByText("ℹ 关于本项目"));
    await waitFor(() => {
      expect(screen.getByText("🌐 项目地址（GitHub）")).toBeInTheDocument();
    });
    // 检查更新：mock 返回有新版本 → 显示提示
    await user.click(screen.getByText("🔄 检查更新"));
    await waitFor(() => {
      expect(screen.getByText(/发现新版本/)).toBeInTheDocument();
    });
    expect(mockInvoke).toHaveBeenCalledWith("check_update_cmd");
    // 下载更新：mock 返回保存路径 → 显示完成
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "get_settings_cmd") {
        return Promise.resolve({ npmRegistry: "https://registry.npmmirror.com", dshDownloadDir: "C:\\dsh-versions", githubMirror: "https://ghfast.top" });
      }
      if (cmd === "check_update_cmd") {
        return Promise.resolve({ current: "0.2.0", latest: "0.3.11", hasUpdate: true, url: "https://github.com/Liaoyuanxinghuo/DSH-Plugin-Manager/", error: "" });
      }
      if (cmd === "download_update_cmd") {
        return Promise.resolve("C:\\Users\\test\\Downloads\\DSH Manager_0.3.11_x64-setup.exe");
      }
      return mockAllDefault(cmd);
    });
    await user.click(screen.getByText("⬇ 下载 v0.3.11"));
    await waitFor(() => {
      expect(screen.getByText(/已保存/)).toBeInTheDocument();
    });
    expect(mockInvoke).toHaveBeenCalledWith("download_update_cmd", { version: "0.3.11" });
    // 打开所在文件夹
    await user.click(screen.getByText("📂 打开所在文件夹"));
    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith("open_path", { path: "C:\\Users\\test\\Downloads\\DSH Manager_0.3.11_x64-setup.exe" });
    });
    // 关闭程序并更新：启动安装程序
    await user.click(screen.getByText("🔄 关闭程序并更新"));
    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith("launch_installer_and_exit_cmd", { path: "C:\\Users\\test\\Downloads\\DSH Manager_0.3.11_x64-setup.exe" });
    });
  });
});

describe("拖拽排序与箭头", () => {
  // jsdom 的 getBoundingClientRect 恒为 0，拖拽悬停计算依赖它 → 按 DOM 顺序模拟每项高度 50
  const mockRects = () => {
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (
      this: HTMLElement,
    ) {
      const items = document.body.querySelectorAll(".drag-item");
      const idx = Array.prototype.indexOf.call(items, this);
      const top = idx >= 0 ? idx * 50 : 0;
      return {
        top,
        height: 50,
        bottom: top + 50,
        left: 0,
        right: 100,
        width: 100,
        x: 0,
        y: top,
        toJSON: () => ({}),
      } as DOMRect;
    });
  };

  it("拖动 profile 排序并持久化到 localStorage", async () => {
    mockRects();
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "list_all_profiles") {
        return Promise.resolve([
          { name: "web", profilesDir: "C:\\Users\\test\\.dsh\\profiles", path: "C:\\p\\web", pluginCount: 1, dataSize: 0, modified: "", hasPatch: false, hasLock: false, hasPackage: true },
          { name: "pmgh", profilesDir: "C:\\Users\\test\\.dsh\\profiles", path: "C:\\p\\pmgh", pluginCount: 2, dataSize: 0, modified: "", hasPatch: false, hasLock: false, hasPackage: true },
        ]);
      }
      return mockAllDefault(cmd);
    });
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("pmgh")).toBeInTheDocument();
    });
    const items = document.body.querySelectorAll(".profile-item");
    expect(items.length).toBe(2);
    // pointer 拖拽：按下 pmgh（第 2 项）→ 移动到第 1 项上方 → 松手
    fireEvent.pointerDown(items[1], { button: 0, clientX: 10, clientY: 120 });
    fireEvent.pointerMove(items[1], { clientX: 10, clientY: 60, pointerId: 1 });
    fireEvent.pointerUp(items[1], { clientX: 10, clientY: 60, pointerId: 1 });
    const names = [...document.body.querySelectorAll(".profile-name")].map((x) => x.textContent ?? "");
    expect(names[0]).toContain("pmgh");
    expect(names[1]).toContain("web");
    const stored = localStorage.getItem("dshpm-prof-order");
    expect(stored).toBeTruthy();
    expect(stored!.indexOf("pmgh")).toBeLessThan(stored!.indexOf("web"));
  });

  it("拖动 DSH 环境排序并持久化到 localStorage", async () => {
    mockRects();
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "scan_envs") {
        return Promise.resolve([
          { id: "global", name: "Global CLI", source: "globalCli", version: "0.1.0-rc.7", homeDir: "C:\\Users\\test\\.dsh", runCommand: "dsh", binPath: null, scanProfilesDir: null },
          { id: "manual-x", name: "Manual dsh", source: "manual", version: "0.1.5-rc.3", homeDir: "C:\\Users\\test\\.dsh", runCommand: "C:\\d\\dsh.cmd", binPath: null, scanProfilesDir: null },
        ]);
      }
      return mockAllDefault(cmd);
    });
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("Manual dsh")).toBeInTheDocument();
    });
    const items = document.body.querySelectorAll(".env-item");
    expect(items.length).toBe(2);
    // 拖动第 1 项（global）下移到第 2 项之后
    fireEvent.pointerDown(items[0], { button: 0, clientX: 10, clientY: 0 });
    fireEvent.pointerMove(items[0], { clientX: 10, clientY: 140, pointerId: 1 });
    fireEvent.pointerUp(items[0], { clientX: 10, clientY: 140, pointerId: 1 });
    const stored = localStorage.getItem("dshpm-env-order");
    expect(stored).toBeTruthy();
    const order = JSON.parse(stored!);
    expect(order[0]).toBe("manual-x");
    expect(order[1]).toBe("global");
  });

  it("导入导出按钮箭头方向正确", async () => {
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("⬇导入")).toBeInTheDocument();
    });
    await waitFor(() => {
      // 默认 mock 有两个 profile，各有一个导出按钮
      expect(screen.getAllByText("⬆ 导出").length).toBeGreaterThan(0);
    });
  });
});


describe("整合包导入导出与备注", () => {
  it("导入按钮打开导入对话框：提供完整 zip 与整合包两个选项", async () => {
    render(<App />);
    // 等环境加载完成（selectedEnv 就绪，否则导入按钮 disabled 点击无效）
    await waitFor(() => {
      expect(screen.getByText(/全局 CLI/)).toBeInTheDocument();
    });
    const user = userEvent.setup();
    await user.click(screen.getByText("⬇导入"));
    // 选项卡片文本带 emoji 前缀，用正则匹配
    expect(screen.getByText(/完整 profile zip/)).toBeInTheDocument();
    expect(screen.getByText(/整合包（.dspack）/)).toBeInTheDocument();
  });

  it("导出弹窗提供整合包类型：填包名后出现导出整合包按钮", async () => {
    render(<App />);
    await waitFor(() => {
      expect(screen.getAllByText("⬆ 导出").length).toBeGreaterThan(0);
    });
    const user = userEvent.setup();
    await user.click(screen.getAllByText("⬆ 导出")[0]);
    expect(screen.getByText("完整 profile zip")).toBeInTheDocument();
    await user.click(screen.getByText("整合包（.dspack）"));
    expect(screen.getByText("选择位置并导出整合包")).toBeInTheDocument();
    // 包名默认取 profile 名
    const nameInput = screen.getByPlaceholderText("如 my-pack") as HTMLInputElement;
    expect(nameInput.value).toBeTruthy();
  });

  it("右键 profile 打开备注弹窗，保存调用 save_profile_note_cmd", async () => {
    mockInvoke.mockClear();
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("web")).toBeInTheDocument();
    });
    const item = document.querySelector(".profile-item") as HTMLElement;
    fireEvent.contextMenu(item);
    expect(screen.getByText(/备注「web」/)).toBeInTheDocument();
    const user = userEvent.setup();
    await user.type(screen.getByPlaceholderText(/此 profile 用于/), "测试备注");
    await user.selectOptions(screen.getByRole("combobox"), "0.1.0-rc.7");
    await user.click(screen.getByText("保存备注"));
    await waitFor(() => {
      expect(
        mockInvoke.mock.calls.some((c) => {
          const args = c[1] as Record<string, unknown>;
          return c[0] === "save_profile_note_cmd" && String(args.note).includes("测试备注") && args.hintVersion === "0.1.0-rc.7";
        }),
      ).toBe(true);
    });
  });
});

function mockAllDefault(cmd: string) {
  switch (cmd) {
    case "check_portable_runtime_cmd":
      return Promise.resolve(true);
    case "init_portable_runtime_cmd":
      return Promise.resolve("便携运行时已就绪");
    case "scan_envs":
      return Promise.resolve([
        {
          id: "global",
          name: "全局 CLI (dsh 0.1.0-rc.7)",
          source: "globalCli",
          version: "0.1.0-rc.7",
          homeDir: "C:\\Users\\test\\.dsh",
          runCommand: "dsh",
          binPath: null,
          scanProfilesDir: null,
        },
      ]);
    case "list_all_profiles":
      return Promise.resolve([
        {
          name: "web",
          profilesDir: "C:\\Users\\test\\.dsh\\profiles",
          path: "C:\\Users\\test\\.dsh\\profiles\\web",
          pluginCount: 1,
          dataSize: 0,
          modified: "",
          hasPatch: false,
          hasLock: false,
          hasPackage: true,
        },
      ]);
    case "list_plugins":
      return Promise.resolve([
        {
          name: "@deepseek-ai/dsh-mcp-client",
          spec: "0.0.1-rc.1",
          isBundle: false,
          compatible: null,
          incompatibleReason: null,
          isDisabled: false,
        },
        {
          name: "@michengai/dsh-codex-ui",
          spec: "^1.1.18",
          isBundle: true,
          compatible: null,
          incompatibleReason: null,
          isDisabled: true,
        },
      ]);
    case "list_running_cmd":
      return Promise.resolve([]);
    case "get_env_paths":
      return Promise.resolve({
        homeDir: "C:\\Users\\test\\.dsh",
        profileDir: "C:\\Users\\test\\.dsh\\profiles\\web",
        profilesDir: "C:\\Users\\test\\.dsh\\profiles",
        sessionsDir: "C:\\Users\\test\\.dsh\\sessions",
        logsDir: "C:\\Users\\test\\.dsh\\logs",
        binPath: null,
      });
    default:
      return Promise.resolve(null);
  }
}
