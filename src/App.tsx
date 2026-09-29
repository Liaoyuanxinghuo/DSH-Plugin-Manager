import { useCallback, useEffect, useRef, useState, type PointerEvent } from "react";
import { api, formatSize, pickDspackFile, pickFolder, pickSavePackPath, pickSaveZipPath, pickZipFile } from "./api";
import { listen } from "@tauri-apps/api/event";
import { applyStoredOrder, moveItem, readStoredOrder } from "./reorder";
import InstallDialog from "./InstallDialog";
import type {
  DshEnv,
  EnvPaths,
  DepIssue,
  JunkEntry,
  InstallDone,
  InstallLogLine,
  PackMarketEntry,
  PackImportResult,
  PluginInfo,
  PluginUpdate,
  ProfileInfo,
  ProfileNote,
  ProfileNotesMap,
  RunningProcess,
  DshVersionInfo,
  DshInstallResult,
  UpdateInfo,
} from "./types";
import "./App.css";

/** 列表拖拽排序：返回绑定到列表项的 props 与拖拽 class 后缀 */
const binDir = (p: string) => {
  const i = Math.max(p.lastIndexOf("\\"), p.lastIndexOf("/"));
  return i > 0 ? p.slice(0, i) : p;
};

/**
 * 列表拖拽排序（pointer 事件实现）。
 * 说明：Tauri WebView2 对 HTML5 drag/drop 事件支持不可靠（拖拽会被系统 OLE 拦截，
 * dragstart/drop 常不触发），故改用 pointerdown/move/up 手动跟踪：
 * 按住左键移动超过 5px 进入拖拽，pointermove 按指针 Y 位置实时计算悬停项，
 * pointerup 时完成换位并持久化。
 */
function useDragReorder<T>(list: T[], onPersist: (next: T[]) => void) {
  const [dragIdx, setDragIdx] = useState<number | null>(null);
  const [overIdx, setOverIdx] = useState<number | null>(null);
  const listRef = useRef<HTMLDivElement | null>(null);
  const start = useRef<{ x: number; y: number; i: number; active: boolean }>({
    x: 0,
    y: 0,
    i: -1,
    active: false,
  });

  const bind = (i: number) => ({
    onPointerDown: (e: PointerEvent) => {
      if (e.button !== 0) return;
      start.current = { x: e.clientX, y: e.clientY, i, active: true };
    },
    onPointerMove: (e: PointerEvent) => {
      if (!start.current.active) return;
      if (dragIdx === null) {
        const dist = Math.hypot(e.clientX - start.current.x, e.clientY - start.current.y);
        if (dist < 5) return; // 未超过阈值 → 视为普通点击，不做任何事
        setDragIdx(start.current.i);
        try {
          (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
        } catch {
          /* capture 失败不阻塞拖拽 */
        }
      }
      const container = listRef.current;
      if (!container) return;
      const items = Array.from(container.querySelectorAll<HTMLElement>(".drag-item"));
      let target = items.length - 1;
      for (let k = 0; k < items.length; k++) {
        const r = items[k].getBoundingClientRect();
        if (e.clientY < r.top + r.height / 2) {
          target = k;
          break;
        }
      }
      if (target !== overIdx) setOverIdx(target);
    },
    onPointerUp: (e: PointerEvent) => {
      start.current.active = false;
      if (dragIdx !== null) {
        const target = overIdx ?? dragIdx;
        if (target !== dragIdx) {
          onPersist(moveItem(list, dragIdx, target));
        }
        setDragIdx(null);
        setOverIdx(null);
      }
      try {
        (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
      } catch {
        /* 已释放则忽略 */
      }
    },
    onPointerCancel: () => {
      start.current.active = false;
      setDragIdx(null);
      setOverIdx(null);
    },
  });
  const dragCls = (i: number) =>
    dragIdx === i ? " drag-src" : overIdx === i && dragIdx !== null ? " drag-over" : "";
  return { bind, dragCls, listRef };
}

// 日志行样式分类：npm warn 等 → 黄色；error/ERR/ENOENT 等 → 红色；其余 stderr → 默认色
function logLineClass(l: { line: string; kind?: string }): string {
  const line = l.line.trimStart();
  if (/^(npm warn|npm WARN|warn|warning|WARN)/.test(line)) return "log-warn";
  if (
    /^(npm error|npm ERR|error|fatal|ENOENT|EACCES|EPERM|EADDRINUSE|ECONNREFUSED|ERR_|errored)/.test(line)
  ) {
    return "log-err";
  }
  return "";
}

export default function App() {
  const [envs, setEnvs] = useState<DshEnv[]>([]);
  const [selectedEnv, setSelectedEnv] = useState<string>(() => localStorage.getItem("dshpm-sel-env") ?? "");
  const [profiles, setProfiles] = useState<ProfileInfo[]>([]);
  const [selectedProfile, setSelectedProfile] = useState<string>(() => localStorage.getItem("dshpm-sel-profile") ?? "");
  const [selectedProfilesDir, setSelectedProfilesDir] = useState<string>("");
  const [plugins, setPlugins] = useState<PluginInfo[]>([]);
  // 多实例：全部运行中的 DSH 进程（跨环境跨 profile）
  const [runnings, setRunnings] = useState<RunningProcess[]>([]);
  const [paths, setPaths] = useState<EnvPaths | null>(null);
  const [port, setPort] = useState(() => Number(localStorage.getItem("dshpm-sel-port")) || 3080);
  // 自动分配端口（默认开启，保证多实例不冲突）
  const [autoPort, setAutoPort] = useState(() => localStorage.getItem("dshpm-auto-port") !== "0");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>("");
  const [info, setInfo] = useState<string>("");
  // 导出/导入状态
  const [exportTarget, setExportTarget] = useState<{ profile: string; profilesDir: string; nmSize: number } | null>(null);
  // 整合包导入对话框
  const [showImportDialog, setShowImportDialog] = useState(false);
  // profile 备注
  const [notes, setNotes] = useState<ProfileNotesMap>({});
  const [noteTarget, setNoteTarget] = useState<{ profile: string; profilesDir: string } | null>(null);
  const [showInitHint, setShowInitHint] = useState(false);
  const [initRunning, setInitRunning] = useState(false);
  // 在线安装对话框
  const [showInstall, setShowInstall] = useState(false);
  const [installTab, setInstallTab] = useState<"market" | "search" | "custom" | "local">("market");
  // M3 插件管理
  const [updates, setUpdates] = useState<PluginUpdate[] | null>(null);
  const [checkingUpdates, setCheckingUpdates] = useState(false);
  const [updatingName, setUpdatingName] = useState<string | null>(null);
  const [confirmUninstall, setConfirmUninstall] = useState<PluginInfo | null>(null);
  // 新建 / 删除 profile
  const [showCreateProfile, setShowCreateProfile] = useState(false);
  const [confirmDeleteProfile, setConfirmDeleteProfile] = useState<ProfileInfo | null>(null);
  const [creatingProfile, setCreatingProfile] = useState(false);
  const [deletingProfile, setDeletingProfile] = useState(false);
  // M4 依赖健康 / 残留清理
  const [deps, setDeps] = useState<DepIssue[] | null>(null);
  const [checkingDeps, setCheckingDeps] = useState(false);
  const [fixingDeps, setFixingDeps] = useState(false);
  const [junk, setJunk] = useState<JunkEntry[] | null>(null);
  const [scanningJunk, setScanningJunk] = useState(false);
  const [cleaningJunk, setCleaningJunk] = useState(false);
  const [confirmCleanJunk, setConfirmCleanJunk] = useState(false);
  const [diagExporting, setDiagExporting] = useState(false);
  // 关闭软件时仍有 DSH 在运行 → 三选一弹窗（关闭所有 / 放生 / 取消）
  const [closeCheck, setCloseCheck] = useState<number | null>(null);
  const [closingAll, setClosingAll] = useState(false);
  // 移除 DSH 环境二次确认（手动 = 磁盘删除版本目录；扫描目录 = 仅移出列表）
  const [confirmRemoveEnv, setConfirmRemoveEnv] = useState<{ env: DshEnv; isManual: boolean } | null>(null);
  // M5 设置 / DSH 下载
  const [showSettings, setShowSettings] = useState(false);
  const [showDshDownload, setShowDshDownload] = useState(false);
  // 便携运行时（%AppData%\dsh-plugin-manager\runtime）是否就绪；未就绪时「下载DSH」变红色「初始化环境」
  const [portableReady, setPortableReady] = useState<boolean | null>(null);
  const [initingPortable, setInitingPortable] = useState(false);
  const [portableBannerOpen, setPortableBannerOpen] = useState(true);
  const [portableLogs, setPortableLogs] = useState<string[]>([]);

  // 检测便携运行时
  useEffect(() => {
    api
      .checkPortableRuntime()
      .then((ok) => setPortableReady(!!ok))
      .catch(() => setPortableReady(false));
  }, []);

  // 「便携运行时就绪」横幅 2s 后自动关闭
  useEffect(() => {
    if (portableReady === true && portableBannerOpen) {
      const t = setTimeout(() => setPortableBannerOpen(false), 2000);
      return () => clearTimeout(t);
    }
  }, [portableReady, portableBannerOpen]);

  // 加载 profile 备注（仅提示，无任何约束）
  useEffect(() => {
    api
      .getProfileNotes()
      .then((m) => setNotes(m ?? {}))
      .catch(() => {});
  }, []);

  // 记住上次选择（环境 / profile / 端口）
  useEffect(() => {
    if (selectedEnv) localStorage.setItem("dshpm-sel-env", selectedEnv);
  }, [selectedEnv]);
  useEffect(() => {
    if (selectedProfile) localStorage.setItem("dshpm-sel-profile", selectedProfile);
  }, [selectedProfile]);
  useEffect(() => {
    localStorage.setItem("dshpm-sel-port", String(port));
  }, [port]);
  useEffect(() => {
    localStorage.setItem("dshpm-auto-port", autoPort ? "1" : "0");
  }, [autoPort]);

  // 加载环境列表
  const loadEnvs = useCallback(async () => {
    try {
      const list = await api.scanEnvs();
      setEnvs(applyStoredOrder(list, (e) => e.id, readStoredOrder("dshpm-env-order")));
      if (list.length > 0) {
        setSelectedEnv((cur) => (cur && list.some((e) => e.id === cur) ? cur : list[0].id));
      } else {
        setSelectedEnv("");
      }
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    loadEnvs();
  }, [loadEnvs]);

  // 加载 profile（全局合并：默认 DSH_HOME/profiles + 所有扫描目录；任意 dsh × 任意来源 profile 可组合）
  const loadProfiles = useCallback(async () => {
    setBusy(true);
    try {
      const ps = (await api.listAllProfiles()) ?? [];
      setProfiles(applyStoredOrder(ps, (x) => x.name, readStoredOrder("dshpm-prof-order")));
      setSelectedProfile((cur) => {
        if (cur && ps.some((p) => p.name === cur)) return cur;
        return ps[0]?.name ?? "";
      });
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, []);

  useEffect(() => {
    loadProfiles();
  }, [loadProfiles]);

  // 选中 profile 变化 → 同步其来源目录（同名多来源时保留已选目录，不回退到第一项）
  useEffect(() => {
    setSelectedProfilesDir((cur) => {
      const exact = profiles.find((x) => x.name === selectedProfile && x.profilesDir === cur);
      if (exact) return cur;
      return profiles.find((x) => x.name === selectedProfile)?.profilesDir ?? cur ?? "";
    });
  }, [selectedProfile, profiles]);

  /** 按 profile 名（可带目录提示）取来源目录，避免同名多来源时指到第一项 */
  const profileDirOf = useCallback(
    (name: string, dirHint?: string): string => {
      const hint = dirHint ?? selectedProfilesDir;
      const exact = profiles.find((x) => x.name === name && x.profilesDir === hint);
      if (exact) return exact.profilesDir;
      return profiles.find((x) => x.name === name)?.profilesDir ?? hint;
    },
    [profiles, selectedProfilesDir],
  );

  // 加载插件列表（来源目录直接按当前 profile 推导，避免选中瞬间 state 未同步的竞态）
  useEffect(() => {
    if (!selectedEnv || !selectedProfile) {
      setPlugins([]);
      setUpdates(null);
      return;
    }
    const p =
      profiles.find((x) => x.name === selectedProfile && x.profilesDir === selectedProfilesDir) ??
      profiles.find((x) => x.name === selectedProfile);
    if (!p) {
      // 记住的 profile 不在当前合并列表中（来源目录未扫到）：静默跳过，不报错
      setPlugins([]);
      setUpdates(null);
      return;
    }
    api
      .listPlugins(selectedEnv, selectedProfile, p.profilesDir)
      .then(setPlugins)
      .catch((e) => setError(String(e)));
    setUpdates(null);
  }, [selectedEnv, selectedProfile, selectedProfilesDir, profiles]);

  // 加载路径（随环境 × profile 变化：文件入口对准当前选中的 profile 目录）
  useEffect(() => {
    if (!selectedEnv) return;
    const pdir = profileDirOf(selectedProfile);
    api
      .getEnvPaths(selectedEnv, selectedProfile, pdir)
      .then(setPaths)
      .catch(() => {});
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedEnv, selectedProfile, selectedProfilesDir, profiles]);

  // 运行实例轮询（全局，跨环境跨 profile，3s 一次：进程退出后按钮/状态快速自动纠正）
  useEffect(() => {
    const tick = () => {
      api.listRunning().then((l) => setRunnings(l ?? [])).catch(() => {});
    };
    tick();
    const t = setInterval(tick, 3000);
    return () => clearInterval(t);
  }, []);

  // 关闭软件拦截：若有 DSH 在运行 → 阻止关闭并弹窗三选一
  useEffect(() => {
    let unsub: (() => void) | undefined;
    import("@tauri-apps/api/window")
      .then(async ({ getCurrentWindow }) => {
        const w = getCurrentWindow();
        unsub = await w.onCloseRequested(async (event) => {
          // 先同步阻止关闭（否则 await 期间窗口会直接关掉），再异步查询
          event.preventDefault();
          try {
            const runs = await api.listRunning();
            if (runs && runs.length > 0) {
              setCloseCheck(runs.length);
            } else {
              // 无运行实例 → 正常关闭（destroy 不触发本回调，不会死循环）
              await w.destroy();
            }
          } catch {
            await w.destroy();
          }
        });
      })
      .catch(() => {});
    return () => unsub?.();
  }, []);

  // 关闭选择：stop-all（停全部后退出）/ release（放生，DSH 继续后台运行）/ cancel
  const closeAppChoice = async (action: "stop-all" | "release" | "cancel") => {
    if (action === "cancel") {
      setCloseCheck(null);
      return;
    }
    try {
      if (action === "stop-all") {
        setClosingAll(true);
        const failed = await api.stopAllDsh();
        if (failed && failed.length > 0) {
          setInfo(`以下 DSH 停止失败（软件仍将退出）：${failed.join("；")}`);
        }
      }
      const w = (await import("@tauri-apps/api/window")).getCurrentWindow();
      await w.destroy();
    } catch (e) {
      setCloseCheck(null);
      setError(`关闭失败：${e}`);
    } finally {
      setClosingAll(false);
    }
  };

  // 启动 + 就绪轮询（供启动/重启共用）
  const startAndPoll = async (eid: string, pname: string, pdir: string) => {
    const r = await api.startDsh(eid, pname, pdir, autoPort ? undefined : port);
    setInfo(`${r.message} — 正在等待服务就绪...`);
    api.listRunning().then((l) => setRunnings(l ?? [])).catch(() => {});
    const deadline = Date.now() + 30000;
    const poll = setInterval(async () => {
      const st = await api.dshStatus(eid, pname, pdir).catch(() => null);
      if (st && !st.running) {
        // 进程已退出（后端自动清理了死进程）
        clearInterval(poll);
        api.listRunning().then((l) => setRunnings(l ?? [])).catch(() => {});
        setInfo("启动失败：进程已退出（可能是 DSH 版本与 profile 不匹配），请选择匹配的 DSH 版本");
        return;
      }
      if (st?.portOpen || Date.now() > deadline) {
        clearInterval(poll);
        api.listRunning().then((l) => setRunnings(l ?? [])).catch(() => {});
        setInfo(st?.portOpen ? `服务已就绪：${r.url}` : "启动超时，请查看日志");
      }
    }, 1000);
  };

  const handleStart = async (envIdArg?: string, profileArg?: string) => {
    const eid = envIdArg ?? selectedEnv;
    const pname = profileArg ?? selectedProfile;
    if (!eid || !pname) {
      setError("请先选择环境和 Profile");
      return;
    }
    setBusy(true);
    setError("");
    setInfo("");
    const pdir = profiles.find((x) => x.name === pname)?.profilesDir ?? "";
    if (!pdir) {
      setError(`未找到 profile「${pname}」的来源目录`);
      setBusy(false);
      return;
    }
    try {
      await startAndPoll(eid, pname, pdir);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  // 重启：先精确停止当前实例，再重新启动（自动模式下重新分配端口）
  const handleRestart = async () => {
    const eid = selectedEnv;
    const pname = selectedProfile;
    if (!eid || !pname || !curRun) return;
    setBusy(true);
    setError("");
    setInfo(`正在重启 ${pname}...`);
    try {
      // 停止用运行中实例自己的目录，启动用列表项解析出的目录
      await api.stopDsh(eid, pname, curRun.profilesDir || profileDirOf(pname));
      // 等旧进程完全退出后再启动（taskkill 异步生效）
      await new Promise((r) => setTimeout(r, 600));
      const list = await api.listRunning().catch(() => []);
      setRunnings(list);
      await startAndPoll(eid, pname, profileDirOf(pname));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const handleStop = async (envIdArg?: string, profileArg?: string, pdirArg?: string) => {
    const eid = envIdArg ?? selectedEnv;
    const pname = profileArg ?? selectedProfile;
    if (!eid || !pname) return;
    setBusy(true);
    try {
      const dir =
        pdirArg ??
        runnings.find((r) => r.envId === eid && r.profile === pname)?.profilesDir ??
        profileDirOf(pname);
      await api.stopDsh(eid, pname, dir);
      const list = await api.listRunning().catch(() => []);
      setRunnings(list);
      // 初始化中的 web 一并结束初始化状态
      if (pname === "web") {
        setInitRunning(false);
        setShowInitHint(false);
      }
      setInfo(`已停止 ${pname}`);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  // 拖拽排序：环境列表 / profile 列表（localStorage 持久化）
  const envReorder = useDragReorder(envs, (next) => {
    setEnvs(next);
    localStorage.setItem("dshpm-env-order", JSON.stringify(next.map((e) => e.id)));
  });
  const profileReorder = useDragReorder(profiles, (next) => {
    setProfiles(next);
    localStorage.setItem("dshpm-prof-order", JSON.stringify(next.map((x) => x.name)));
  });

  // 刷新：环境 + Profiles 都立刻重扫，列表与增删保持同步
  const refreshAll = async () => {
    setError("");
    setInfo("");
    await Promise.all([loadEnvs(), loadProfiles()]);
    setInfo("已刷新：环境与 Profile 列表已重新扫描");
  };

  // M3：检查插件更新（并行查询 npm）
  const handleCheckUpdates = async () => {
    if (!selectedEnv || !selectedProfile) return;
    setCheckingUpdates(true);
    setError("");
    setInfo("");
    try {
      const list = await api.checkUpdates(selectedEnv, selectedProfile, profileDirOf(selectedProfile));
      setUpdates(list);
      const n = list.filter((u) => u.updatable).length;
      setInfo(n > 0 ? `发现 ${n} 个插件有新版本` : "所有插件均为最新版本");
    } catch (e) {
      setError(String(e));
      setUpdates(null);
    } finally {
      setCheckingUpdates(false);
    }
  };

  // 启停开关（写 cordis.patch.yml）
  const handleTogglePlugin = async (p: PluginInfo, enabled: boolean) => {
    if (!selectedEnv || !selectedProfile) return;
    setBusy(true);
    setError("");
    try {
      await api.setPluginEnabled(selectedEnv, selectedProfile, profileDirOf(selectedProfile), p.name, enabled);
      // 刷新列表
      const list = await api.listPlugins(selectedEnv, selectedProfile, profileDirOf(selectedProfile)).catch(() => null);
      if (list) setPlugins(list);
      setInfo(enabled ? `已启用 ${p.name}` : `已停用 ${p.name}`);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  // 更新单个插件到最新版
  const handleUpdatePlugin = async (p: PluginInfo, latest: string) => {
    if (!selectedEnv || !selectedProfile) return;
    setUpdatingName(p.name);
    setError("");
    setInfo(`正在更新 ${p.name} → ${latest}...`);
    try {
      const r = await api.installPlugin(selectedEnv, selectedProfile, profileDirOf(selectedProfile), `${p.name}@${latest}`);
      if (r.success) {
        setInfo(`已更新 ${p.name} → ${latest}`);
      } else {
        setError(`更新失败（退出码 ${r.exitCode ?? "?"}）：${r.summary}`);
      }
      // 刷新插件列表与更新信息
      const list = await api.listPlugins(selectedEnv, selectedProfile, profileDirOf(selectedProfile)).catch(() => null);
      if (list) setPlugins(list);
      const up = await api.checkUpdates(selectedEnv, selectedProfile, profileDirOf(selectedProfile)).catch(() => null);
      if (up) setUpdates(up);
    } catch (e) {
      setError(String(e));
    } finally {
      setUpdatingName(null);
    }
  };

  // 卸载插件（需二次确认）
  const doUninstall = async () => {
    if (!selectedEnv || !selectedProfile || !confirmUninstall) return;
    const name = confirmUninstall.name;
    setConfirmUninstall(null);
    setBusy(true);
    setError("");
    setInfo(`正在卸载 ${name}...`);
    try {
      const r = await api.removePlugin(selectedEnv, selectedProfile, profileDirOf(selectedProfile), name);
      if (r.success) {
        setInfo(`已卸载 ${name}`);
      } else {
        setError(`卸载失败（退出码 ${r.exitCode ?? "?"}）：${r.summary}`);
      }
      const list = await api.listPlugins(selectedEnv, selectedProfile, profileDirOf(selectedProfile)).catch(() => null);
      if (list) setPlugins(list);
      setUpdates(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  // 导出 profile：先查 node_modules 大小，弹窗让用户决定是否排除
  const handleExport = async (profile: string) => {
    if (!selectedEnv) return;
    try {
      const pdir = profiles.find((x) => x.name === profile)?.profilesDir ?? "";
      const nmSize = await api.profileNodeModulesSize(selectedEnv, profile, pdir);
      setExportTarget({ profile, profilesDir: pdir, nmSize });
    } catch (e) {
      setError(String(e));
    }
  };

  const doExport = async (excludeNodeModules: boolean) => {
    if (!selectedEnv || !exportTarget) return;
    setBusy(true);
    setError("");
    setInfo("");
    try {
      const defaultName = `${exportTarget.profile}.zip`;
      const path = await pickSaveZipPath(defaultName);
      if (!path) return; // 用户取消
      const r = await api.exportProfile(selectedEnv, exportTarget.profile, exportTarget.profilesDir, path, excludeNodeModules);
      setInfo(
        `导出成功：${r.fileCount} 个文件，${formatSize(r.zipSize)}${r.skippedSymlinks.length ? `，跳过 symlink ${r.skippedSymlinks.length} 个` : ""}`,
      );
      api.openPath(path).catch(() => {});
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
      setExportTarget(null);
    }
  };

  // 导入 profile：打开导入对话框（完整 zip / 整合包）
  const handleImport = async () => {
    if (!selectedEnv) return;
    setShowImportDialog(true);
  };

  // 导入完成后刷新 profile 列表（与「刷新」同一套重扫逻辑）
  const refreshAfterImport = async (finalNames: string[]) => {
    await loadProfiles();
    if (finalNames.length > 0) {
      setSelectedProfile(finalNames[0]);
    }
  };

  // 导出整合包：.dspack v3（DSH-PackForge 规范）
  const doExportPack = async (
    packName: string,
    packVersion: string,
    displayName: string,
    dshHint?: string,
  ) => {
    if (!selectedEnv || !exportTarget) return;
    setBusy(true);
    setError("");
    setInfo("");
    try {
      const name = packName.trim() || exportTarget.profile;
      const version = packVersion.trim() || "1.0.0";
      const defaultName = `${name}-${version}.dspack`;
      const path = await pickSavePackPath(defaultName);
      if (!path) return; // 用户取消
      const r = await api.exportPack(
        selectedEnv,
        exportTarget.profile,
        exportTarget.profilesDir,
        path,
        name,
        version,
        displayName,
        dshHint?.trim() || undefined,
      );
      setInfo(
        `整合包导出成功：${r.fileCount} 个文件，${formatSize(r.zipSize)}，SHA-256 ${r.sha256.slice(0, 12)}…` +
          (dshHint ? ` · 适配 DSH ${dshHint}` : ""),
      );
      api.openPath(path).catch(() => {});
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
      setExportTarget(null);
    }
  };

  // 复制并重命名 profile 副本（同目录，自动 -1 递增；node_modules 一并复制，立即可用）
  const doClone = async () => {
    if (!selectedEnv || !exportTarget || busy) return;
    setBusy(true);
    try {
      const newName = await api.cloneProfile(selectedEnv, exportTarget.profile, exportTarget.profilesDir);
      setInfo(`已复制为副本「${newName}」（与「${exportTarget.profile}」同目录，node_modules 已一并复制）`);
      loadProfiles();
      setExportTarget(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  // 添加本地 profile 扫描目录
  const handleAddScanDir = async () => {
    const path = await pickFolder("选择要扫描的 DSH_HOME 或 profiles 目录");
    if (!path) return;
    setBusy(true);
    setError("");
    try {
      const entry = await api.addScanDir(path);
      setInfo(`已添加扫描目录：${entry.label}（${entry.profilesDir}）`);
      await loadEnvs();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  // 新建 profile（目标目录必须是 profiles 文件夹，由表单校验）
  const handleCreateProfile = async (name: string, profilesDir: string) => {
    if (!selectedEnv) return;
    setCreatingProfile(true);
    setError("");
    setInfo("");
    try {
      const r = await api.createProfile(selectedEnv, name, profilesDir);
      setInfo(`已创建 profile「${r.name}」于 ${profilesDir}（含默认 bundle，可直接启动）`);
      setShowCreateProfile(false);
      await loadProfiles();
      setSelectedProfile(r.name);
      setSelectedProfilesDir(profilesDir);
    } catch (e) {
      setError(String(e));
    } finally {
      setCreatingProfile(false);
    }
  };

  // 初始化按钮出现 >=5s 仍无 profile 时横幅提示（可点叉关闭；有 profile 后自动消失）
  useEffect(() => {
    if (envs.length > 0 && profiles.length === 0) {
      const t = setTimeout(() => setShowInitHint(true), 5000);
      return () => clearTimeout(t);
    }
    setShowInitHint(false);
  }, [envs.length, profiles.length]);

  // 初始化便携运行时（完整下载 Node LTS + pnpm 到 %AppData%\dsh-plugin-manager\runtime）
  // 通过 install-log 事件流显示进度，避免看起来像卡死
  const handleInitPortable = async () => {
    setInitingPortable(true);
    setError("");
    setInfo("正在初始化便携运行时（下载 Node.js LTS + pnpm，约 40MB）…");
    setPortableLogs([]);
    let unLog: (() => void) | undefined;
    try {
      const { listen } = await import("@tauri-apps/api/event");
      unLog = await listen<{ line: string; kind?: string }>("install-log", (e) => {
        const line = e.payload?.line ?? "";
        setPortableLogs((prev) => [...prev.slice(-30), line]);
        setInfo(`便携环境初始化中：${line}`);
      }).catch(() => undefined);
    } catch {
      /* 非 Tauri 环境忽略 */
    }
    try {
      const msg = await api.initPortableRuntime();
      setPortableReady(true);
      setPortableBannerOpen(true);
      setInfo(`✅ 便携运行时就绪 — ${msg}`);
    } catch (e) {
      setPortableReady(false);
      setError(`初始化便携运行时失败：${e}`);
    } finally {
      unLog?.();
      setInitingPortable(false);
    }
  };

  // 打开「下载 DSH」前强制检查便携包；缺失则走初始化
  const openDshDownload = async () => {
    try {
      const ok = await api.checkPortableRuntime();
      setPortableReady(!!ok);
      if (!ok) {
        await handleInitPortable();
        return;
      }
    } catch {
      /* 检测失败按缺失处理 */
    }
    setShowDshDownload(true);
  };

  // 一键初始化：用本地 dsh 启动内置 web profile（dsh 首次运行自动创建并初始化）
  const handleInitProfile = async () => {
    if (!selectedEnv) return;
    setCreatingProfile(true);
    setInitRunning(true);
    setError("");
    setInfo("正在初始化并启动默认 web profile（dsh 首次运行会自动创建）...");
    try {
      // 只启动一次（勿再 startDsh + startAndPoll 双开）
      await startAndPoll(selectedEnv, "web", "");
      api.listRunning().then((l) => setRunnings(l ?? [])).catch(() => {});
      const ps = await api.listAllProfiles().catch(() => []);
      setProfiles(ps);
      setSelectedProfile("web");
      setShowInitHint(false);
    } catch (e) {
      setInitRunning(false);
      setError(String(e));
    } finally {
      setCreatingProfile(false);
    }
  };

  // 停止初始化进程（与右栏「■ 停止」同一套逻辑）
  const handleStopInit = async () => {
    if (!selectedEnv) return;
    setError("");
    setInfo("正在停止初始化进程...");
    try {
      await api.stopDsh(selectedEnv, "web", profileDirOf("web") || "");
      setInfo("初始化进程已停止");
      api.listRunning().then((l) => setRunnings(l ?? [])).catch(() => {});
      const ps = await api.listAllProfiles().catch(() => []);
      setProfiles(ps);
    } catch (e) {
      setError(String(e));
    } finally {
      setCreatingProfile(false);
      setShowInitHint(false);
      setInitRunning(false);
    }
  };

  // 删除 profile（二次确认后执行）
  const doDeleteProfile = async () => {
    if (!selectedEnv || !confirmDeleteProfile) return;
    const name = confirmDeleteProfile.name;
    // 必须用该 profile 自己的来源目录；selectedProfilesDir 可能为空（新建/导入后曾被置空）
    const pdir = confirmDeleteProfile.profilesDir;
    setConfirmDeleteProfile(null);
    setDeletingProfile(true);
    setError("");
    setInfo(`正在删除 profile「${name}」...`);
    try {
      await api.deleteProfile(selectedEnv, name, pdir);
      setInfo(`已删除 profile「${name}」`);
      // 重扫列表；loadProfiles 会把失效选中项自动切到剩余第一个
      await loadProfiles();
    } catch (e) {
      setError(String(e));
    } finally {
      setDeletingProfile(false);
    }
  };

  // M4：依赖健康检查
  const handleCheckDeps = async () => {
    if (!selectedEnv || !selectedProfile) return;
    setCheckingDeps(true);
    setError("");
    setInfo("");
    try {
      const list = await api.checkDeps(selectedEnv, selectedProfile, profileDirOf(selectedProfile));
      setDeps(list);
      const missing = list.filter((d) => !d.installed).length;
      setInfo(missing > 0 ? `发现 ${missing} 个依赖缺失` : "依赖完整，全部已安装");
    } catch (e) {
      setError(String(e));
      setDeps(null);
    } finally {
      setCheckingDeps(false);
    }
  };

  // M4：修复依赖（dsh plugin install = pnpm install）
  const handleFixDeps = async () => {
    if (!selectedEnv || !selectedProfile) return;
    setFixingDeps(true);
    setError("");
    setInfo("正在修复依赖（pnpm install）...");
    try {
      const r = await api.fixDeps(selectedEnv, selectedProfile, profileDirOf(selectedProfile));
      if (r.success) {
        setInfo("依赖修复完成");
      } else {
        setError(`修复失败（退出码 ${r.exitCode ?? "?"}）：${r.summary}`);
      }
      const list = await api.checkDeps(selectedEnv, selectedProfile, profileDirOf(selectedProfile)).catch(() => null);
      if (list) setDeps(list);
    } catch (e) {
      setError(String(e));
    } finally {
      setFixingDeps(false);
    }
  };

  // M4：扫描残留
  const handleScanJunk = async () => {
    if (!selectedEnv || !selectedProfile) return;
    setScanningJunk(true);
    setError("");
    setInfo("");
    try {
      const list = await api.scanJunk(selectedEnv, selectedProfile, profileDirOf(selectedProfile));
      setJunk(list);
      if (list.length === 0) setInfo("未发现可清理的残留缓存");
    } catch (e) {
      setError(String(e));
      setJunk(null);
    } finally {
      setScanningJunk(false);
    }
  };

  // M4：清理残留（全部）
  const handleCleanJunk = async () => {
    if (!selectedEnv || !selectedProfile || !junk || junk.length === 0) return;
    setCleaningJunk(true);
    setError("");
    setInfo("正在清理残留缓存...");
    try {
      const names = junk.map((j) => j.name);
      const removed = await api.cleanJunk(selectedEnv, selectedProfile, profileDirOf(selectedProfile), names);
      setInfo(`已清理 ${removed.length} 项`);
      setJunk(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setCleaningJunk(false);
    }
  };

  // M4：导出诊断包
  const handleExportDiag = async () => {
    if (!selectedEnv) return;
    setDiagExporting(true);
    setError("");
    setInfo("");
    try {
      const path = await pickSaveZipPath("dsh-diag.zip");
      if (!path) return;
      const r = await api.exportDiag(selectedEnv, path);
      setInfo(`诊断包导出成功：${r.fileCount} 个文件，${formatSize(r.zipSize)}`);
      api.openPath(path).catch(() => {});
    } catch (e) {
      setError(String(e));
    } finally {
      setDiagExporting(false);
    }
  };

  // 扫描 dsh 本体（自动探测其中安装的 dsh 可执行文件）
  const handleAddDshScan = async () => {
    const path = await pickFolder("选择包含 dsh 本体的目录（如全局 npm 目录、项目 node_modules 或 DSH Desktop 安装目录）");
    if (!path) return;
    setBusy(true);
    setError("");
    setInfo("");
    try {
      const added = await api.addDshScanDir(path);
      if (added.length > 0) {
        setInfo(`扫描发现 ${added.length} 个 dsh 本体：${added.map((e) => e.name).join("、")}`);
      } else {
        setInfo("扫描完成，但未发现新的 dsh 本体");
      }
      await loadEnvs();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  // 板块帮助弹窗（"DSH 环境" / "Profiles" 的问号按钮）
  const [helpTopic, setHelpTopic] = useState<"envs" | "profiles" | null>(null);

  const selectedEnvObj = envs.find((e) => e.id === selectedEnv) ?? null;

  // 指定环境是否可启动（绑定 dsh 运行时）
  const envCanStart = (envId: string) => {
    const e = envs.find((x) => x.id === envId);
    return !!e && !!e.runCommand.trim();
  };
  // 当前选中 env×profile 的运行实例（目录以列表解析为准，避免 selectedProfilesDir 为空时匹配失败）
  const curPdir = profileDirOf(selectedProfile);
  const curRun =
    runnings.find(
      (r) =>
        r.envId === selectedEnv &&
        r.profile === selectedProfile &&
        (!curPdir || r.profilesDir === curPdir),
    ) ?? null;

  const fileButtons = paths
    ? [
        ...(paths.profileDir ? [{ label: "当前 Profile 目录", path: paths.profileDir }] : []),
        { label: "DSH_HOME", path: paths.dshHome ?? paths.homeDir },
        { label: "Profiles 目录", path: paths.profilesDir },
        { label: "日志目录", path: paths.logsDir },
        { label: "会话目录", path: paths.sessionsDir },
        { label: "dsh 可执行文件", path: paths.binPath },
      ].filter((b) => b.path)
    : [];

  return (
    <div className="app">
      <header className="topbar">
        <div className="brand">
          <span className="logo">◈</span>
          <h1>DSH Manager</h1>
        </div>
        <div className="topbar-actions">
          <button className="btn ghost" onClick={() => setShowSettings(true)} title="设置（镜像源 / DSH 下载目录）">
            ⚙ 设置
          </button>
          <button className="btn ghost" onClick={refreshAll} disabled={busy}>
            ⟳ 刷新
          </button>
        </div>
      </header>

      {error && (
        <div className="banner error">
          <span>⚠ {error}</span>
          <button className="btn tiny" onClick={() => setError("")}>×</button>
        </div>
      )}
      {info && (
        <div className="banner info">
          <span>ℹ {info}</span>
          <button className="btn tiny" onClick={() => setInfo("")}>×</button>
        </div>
      )}
      {portableReady === false && (
        <div className="banner warn">
          <span>
            ⚡ 尚未初始化便携运行时（Node.js LTS + pnpm）。请点
            <b>「初始化环境」</b>
            自动下载安装到 <code>%AppData%\dsh-plugin-manager\runtime\</code>
            ，不改动系统 PATH。
            {initingPortable && portableLogs.length > 0 && (
              <span style={{ display: "block", opacity: 0.85, marginTop: 4, fontFamily: "Consolas, monospace" }}>
                {portableLogs[portableLogs.length - 1]}
              </span>
            )}
          </span>
          <button
            className="btn tiny danger"
            disabled={initingPortable}
            onClick={handleInitPortable}
          >
            {initingPortable ? "初始化中…" : "⚡ 初始化环境"}
          </button>
        </div>
      )}
      {portableReady === true && portableBannerOpen && (
        <div className="banner ready">
          <span>✅ 便携运行时就绪：%AppData%\dsh-plugin-manager\runtime\</span>
          <button className="btn tiny" onClick={() => setPortableBannerOpen(false)}>×</button>
        </div>
      )}
      {showInitHint && envs.length > 0 && profiles.length === 0 && (
        <div className="banner info">
          <span>
            ℹ 当前没有 profile，请点击中栏「初始化」按钮完成初始化（dsh 会自动创建默认 web profile）
          </span>
          <button className="btn tiny" onClick={() => setShowInitHint(false)} title="关闭提示">×</button>
        </div>
      )}

      <main className="layout">
        {/* 左：环境列表 */}
        <section className="panel">
          <div className="panel-title">
            <h2>DSH 环境</h2>
            <div className="title-actions">
              {portableReady === false || initingPortable ? (
                <button
                  className="btn tiny danger"
                  disabled={initingPortable}
                  onClick={handleInitPortable}
                  title="下载并安装便携运行时（Node.js LTS + pnpm）到 %AppData%\dsh-plugin-manager\runtime"
                >
                  {initingPortable ? "初始化中…" : "⚡ 初始化环境"}
                </button>
              ) : (
                <button
                  className="btn tiny"
                  onClick={openDshDownload}
                  title="下载并安装 DSH 多版本到指定目录（需已初始化便携运行时）"
                >
                  ⬇ 下载DSH
                </button>
              )}
              <button className="btn tiny" onClick={handleAddDshScan} disabled={busy} title="把电脑上已安装的 dsh（CLI / DSH Desktop 内置 / 本地项目）添加为可启动、可装插件的环境">
                ＋扫本体
              </button>
              <button className="help-btn" onClick={() => setHelpTopic("envs")} title="DSH 环境使用说明">?</button>
            </div>
          </div>
          <div className="env-list" ref={envReorder.listRef}>
            {envs.length === 0 && <div className="empty">未检测到 DSH，请下载或扫描目录</div>}
            {envs.map((env, i) => (
              <div
                key={env.id}
                className={`env-item drag-item ${env.id === selectedEnv ? "active" : ""}${envReorder.dragCls(i)}`}
                {...envReorder.bind(i)}
                onClick={() => setSelectedEnv(env.id)}
              >
                <div className="env-name">
                  <span className="drag-handle" title="拖动排序">☰</span>
                  <span className={`dot ${env.source === "globalCli" ? "global" : env.source === "manual" ? "manual" : "scan"}`} />
                  {env.name}
                </div>
                <div className="env-meta">
                  dsh {env.version} ·{" "}
                  {env.source === "globalCli" ? "全局" : env.source === "manual" ? "手动" : "扫描目录"}
                </div>
                {env.binPath && (
                  <div className="env-meta dim" title="dsh 本体（程序）所在目录">
                    程序: {binDir(env.binPath)}
                  </div>
                )}
                <div className="env-meta dim" title="该环境管理的 profile（DSH_HOME）目录">
                  profiles: {env.homeDir}
                </div>
                {(env.source === "manual" || env.source === "scanDir") && (
                  <>
                    <button
                      className="btn tiny danger-inline"
                      onClick={(e) => {
                        e.stopPropagation();
                        setConfirmRemoveEnv({ env, isManual: env.source === "manual" });
                      }}
                    >
                      移除
                    </button>
                  </>
                )}
              </div>
            ))}
          </div>
        </section>

        {/* 中：Profile 列表 */}
        <section className="panel">
          <div className="panel-title">
            <h2>Profiles</h2>
            <div className="title-actions">
              {initRunning ? (
                <button className="btn tiny init-btn" onClick={handleStopInit} disabled={creatingProfile || !selectedEnv} title="停止初始化启动的 DSH 进程">
                  结束初始化
                </button>
              ) : envs.length > 0 && profiles.length === 0 ? (
                <button className="btn tiny init-btn" onClick={handleInitProfile} disabled={creatingProfile || !selectedEnv} title="用本地 dsh 启动内置 web profile（首次自动创建并初始化）">
                  初始化
                </button>
              ) : (
                <button className="btn tiny" onClick={() => setShowCreateProfile(true)} disabled={creatingProfile || !selectedEnv} title="新建空白 profile（含默认 bundle）">
                  ＋新建
                </button>
              )}
              <button className="btn tiny" onClick={handleAddScanDir} disabled={busy} title="扫描本地 DSH_HOME 或 profiles 目录">
                ＋扫Profile
              </button>
              <button className="btn tiny" onClick={handleImport} disabled={!selectedEnv}>
                ⬇导入
              </button>
              <button className="help-btn" onClick={() => setHelpTopic("profiles")} title="Profiles 使用说明">?</button>
            </div>
          </div>
          <div className="profile-list" ref={profileReorder.listRef}>
            {profiles.length === 0 && <div className="empty">该环境暂无 profile</div>}
            {profiles.map((p, i) => (
              <div
                key={p.name}
                className={`profile-item drag-item ${p.name === selectedProfile ? "active" : ""}${profileReorder.dragCls(i)}`}
                {...profileReorder.bind(i)}
                onClick={() => {
                  setSelectedProfile(p.name);
                  setSelectedProfilesDir(p.profilesDir);
                }}
                onContextMenu={(e) => {
                  e.preventDefault();
                  setNoteTarget({ profile: p.name, profilesDir: p.profilesDir });
                }}
                title="右键可添加备注"
              >
                <div className="profile-name"><span className="drag-handle" title="拖动排序">☰</span>{p.name}</div>
                {(() => {
                  const n = notes?.[`${p.name}::${p.profilesDir}`];
                  if (!n || (!n.note && !n.hintVersion)) return null;
                  return (
                    <div className="profile-notes">
                      {n.note && <span className="tag note">📝 {n.note}</span>}
                      {n.hintVersion && <span className="tag hint">适配 {n.hintVersion}</span>}
                    </div>
                  );
                })()}
                <div className="profile-meta">
                  {p.pluginCount} 插件 · {formatSize(p.dataSize)}
                </div>
                <div className="profile-meta dim">更新于 {p.modified}</div>
                <div className="profile-flags">
                  {p.hasPatch && <span className="tag">patch</span>}
                  {p.hasLock && <span className="tag">lock</span>}
                </div>
                <div className="profile-buttons">
                  <button
                    className="btn tiny"
                    onClick={(e) => {
                      e.stopPropagation();
                      api.openPath(p.path).catch((x) => setError(String(x)));
                    }}
                  >
                    📂 打开目录
                  </button>
                  {(() => {
                    const rp = runnings.find(
                      (r) => r.envId === selectedEnv && r.profile === p.name,
                    );
                    return rp ? (
                      <>
                        <span className="run-pill" title={`运行中：PID ${rp.pid}，端口 ${rp.port}`}>
                          ● :{rp.port}
                        </span>
                        <button
                          className="btn tiny danger"
                          onClick={(e) => {
                            e.stopPropagation();
                            handleStop(selectedEnv, p.name);
                          }}
                          title={`停止 ${p.name}（PID ${rp.pid}）`}
                        >
                          ■ 停止
                        </button>
                      </>
                    ) : (
                      <button
                        className="btn tiny primary"
                        onClick={(e) => {
                          e.stopPropagation();
                          handleStart(selectedEnv, p.name);
                        }}
                        disabled={!envCanStart(selectedEnv)}
                        title={envCanStart(selectedEnv) ? `启动 ${p.name}` : "该环境未配置 dsh 程序，无法启动"}
                      >
                        ▶ 启动
                      </button>
                    );
                  })()}
                  <button
                    className="btn tiny"
                    onClick={(e) => {
                      e.stopPropagation();
                      handleExport(p.name);
                    }}
                  >
                    ⬆ 导出
                  </button>
                  <button
                    className="btn tiny uninstall"
                    onClick={(e) => {
                      e.stopPropagation();
                      setConfirmDeleteProfile(p);
                    }}
                    title={`删除 profile「${p.name}」（含 node_modules，不可恢复）`}
                  >
                    🗑 删除
                  </button>
                </div>
              </div>
            ))}
          </div>
        </section>

        {/* 右：详情与操作 */}
        <section className="panel wide">
          {!selectedEnv || !selectedProfile ? (
            <div className="empty">请选择环境与 Profile 查看详情</div>
          ) : (
            <>
              <div className="panel-title">
                <h2>
                  Profile: <code>{selectedProfile}</code>
                </h2>
                {selectedProfilesDir && (
                  <div className="profile-src" title="该 profile 所在的 profiles 目录（任意 dsh × 任意来源组合）">
                    来源: <code>{selectedProfilesDir}</code>
                  </div>
                )}
              </div>

              {/* 启动控制（多实例：任意 env×profile 独立进程，端口自动分配） */}
              <div className="control-card">
                <div className="card-row">
                  <label className="check-row" title="开启后从 3080 起自动分配空闲端口，多实例互不冲突">
                    <input
                      type="checkbox"
                      checked={autoPort}
                      onChange={(e) => setAutoPort(e.target.checked)}
                    />
                    自动分配端口
                  </label>
                  {!autoPort && (
                    <>
                      <label>端口</label>
                      <input
                        type="number"
                        value={port}
                        min={1}
                        max={65535}
                        onChange={(e) => setPort(Number(e.target.value))}
                      />
                    </>
                  )}
                  <div className="status-pill">
                    {curRun ? (
                      <span className="pill running">
                        ● 运行中 PID {curRun.pid} · 端口 {curRun.port}
                      </span>
                    ) : (
                      <span className="pill stopped">○ 已停止</span>
                    )}
                  </div>
                </div>
                <div className="card-row actions">
                  {curRun ? (
                    <button
                      className="btn warn"
                      onClick={handleRestart}
                      disabled={busy}
                      title={`停止并重启 ${selectedProfile}`}
                    >
                      ↻ 重启 DSH
                    </button>
                  ) : (
                    <button
                      className="btn primary"
                      onClick={() => handleStart()}
                      disabled={busy || !selectedProfile || !selectedEnvObj?.runCommand}
                      title={selectedEnvObj && !selectedEnvObj.runCommand ? "该环境未配置 dsh 程序，无法启动" : ""}
                    >
                      ▶ 启动 DSH
                    </button>
                  )}
                  <button
                    className="btn danger"
                    onClick={() => {
                      // 初始化中的 web：结束初始化与普通停止都走停进程
                      if (initRunning && selectedProfile === "web") {
                        void handleStopInit();
                      } else {
                        void handleStop();
                      }
                    }}
                    disabled={busy || (!curRun && !initRunning)}
                    title={initRunning ? "停止初始化进程" : "停止当前 DSH"}
                  >
                    ■ 停止
                  </button>
                  {curRun && (
                    <button
                      className="btn"
                      onClick={() =>
                        api
                          .openDshWeb(curRun.envId, curRun.profile, curRun.profilesDir)
                          .catch(() => {})
                      }
                    >
                      🌐 打开界面
                    </button>
                  )}
                </div>
                <div className="run-hint">
                  {runnings.length === 0
                    ? "当前无运行实例。多个 DSH 可同时运行：不同版本 × 不同 profile 各自独立进程与端口。"
                    : `正在运行 ${runnings.length} 个实例：${runnings
                        .map((r) => `${r.profile}@${r.port}`)
                        .join("、")}`}
                </div>
              </div>

              {/* 文件入口 */}
              <div className="control-card">
                <div className="card-title">文件系统快捷入口</div>
                <div className="file-buttons">
                  {fileButtons.map((b) => (
                    <button
                      key={b.label}
                      className="btn"
                      onClick={() => api.openPath(b.path!).catch((x) => setError(String(x)))}
                    >
                      📂 {b.label}
                    </button>
                  ))}
                </div>
              </div>

              {/* M4 健康与维护 */}
              <div className="control-card">
                <div className="card-title">健康与维护</div>
                <div className="card-row actions">
                  <button className="btn tiny" onClick={handleCheckDeps} disabled={checkingDeps || !selectedEnvObj?.runCommand || !selectedProfile}>
                    {checkingDeps ? "检查中..." : "⟳ 依赖检查"}
                  </button>
                  <button
                    className="btn tiny"
                    onClick={handleFixDeps}
                    disabled={fixingDeps || !selectedEnvObj?.runCommand || !deps?.some((d) => !d.installed)}
                    title={deps?.some((d) => !d.installed) ? "运行 pnpm install 补齐缺失依赖" : "先做依赖检查，有缺失时可用"}
                  >
                    {fixingDeps ? "修复中..." : "⚙ 修复依赖"}
                  </button>
                  <button className="btn tiny" onClick={handleScanJunk} disabled={scanningJunk || !selectedProfile}>
                    {scanningJunk ? "扫描中..." : "🧹 扫描残留"}
                  </button>
                  <button
                    className="btn tiny"
                    onClick={() => setConfirmCleanJunk(true)}
                    disabled={cleaningJunk || !junk || junk.length === 0}
                    title="清理扫描到的全部残留缓存（二次确认）"
                  >
                    {cleaningJunk ? "清理中..." : "🗑 清理残留"}
                  </button>
                  <button className="btn tiny" onClick={handleExportDiag} disabled={diagExporting}>
                    {diagExporting ? "导出中..." : "📦 导出诊断包"}
                  </button>
                </div>
                {deps && (
                  <div className="dep-list">
                    <div className="dep-summary">
                      依赖：{deps.filter((d) => d.installed).length}/{deps.length} 已安装
                      {deps.some((d) => !d.installed) && <span className="ver-err"> 有缺失，可用「⚙ 修复依赖」</span>}
                    </div>
                    {deps.some((d) => !d.installed) && (
                      <div className="dep-missing">
                        {deps.filter((d) => !d.installed).map((d) => (
                          <div key={d.name} className="dep-row bad">
                            <span className="dep-name">{d.name}</span>
                            <span className="dep-req">{d.declared}</span>
                            <span className="dep-state">缺失</span>
                          </div>
                        ))}
                      </div>
                    )}
                  </div>
                )}
                {junk && (
                  <div className="junk-box">
                    {junk.length === 0 ? (
                      <div className="junk-none">无残留缓存</div>
                    ) : (
                      <>
                        <div className="junk-list">
                          {junk.map((j) => (
                            <div key={j.name} className="junk-row">
                              <span className="junk-name">{j.name}</span>
                              <span className="junk-size">{formatSize(j.size)}</span>
                            </div>
                          ))}
                        </div>
                        {!cleaningJunk && (
                          <div className="junk-hint">点击「🗑 清理残留」将删除以上 {junk.length} 项缓存（不影响插件与配置）</div>
                        )}
                      </>
                    )}
                  </div>
                )}
              </div>

              {/* 插件列表 */}
              <div className="control-card grow">
                <div className="card-title">
                  已安装插件（{plugins.length}）
                  <div className="title-actions">
                    <button
                      className="btn tiny"
                      onClick={handleCheckUpdates}
                      disabled={checkingUpdates || !selectedEnvObj?.runCommand || plugins.length === 0}
                      title="并行查询 npm，检查各插件是否有新版本"
                    >
                      {checkingUpdates ? "检查中..." : "⟳ 检查更新"}
                    </button>
                    <button
                      className="btn tiny"
                      onClick={() => {
                        setInstallTab("local");
                        setShowInstall(true);
                      }}
                      disabled={!selectedEnvObj?.runCommand}
                      title={selectedEnvObj && !selectedEnvObj.runCommand ? "该环境未配置 dsh 程序" : "从本地文件夹或 tgz 导入插件"}
                    >
                      ＋本地导入
                    </button>
                    <button
                      className="btn tiny"
                      onClick={() => {
                        setInstallTab("market");
                        setShowInstall(true);
                      }}
                      disabled={!selectedEnvObj?.runCommand}
                      title={selectedEnvObj && !selectedEnvObj.runCommand ? "该环境未配置 dsh 程序" : "在线安装插件（市场/npm/自定义源）"}
                    >
                      ＋安装插件
                    </button>
                  </div>
                </div>
                {plugins.length === 0 ? (
                  <div className="empty">暂无插件</div>
                ) : (
                  <div className="plugin-table">
                    {plugins.map((p) => {
                      const up = updates?.find((u) => u.name === p.name);
                      const updatable = up?.updatable && up.latest;
                      return (
                        <div className={`plugin-row ${p.isDisabled ? "disabled" : ""}`} key={p.name}>
                          <div className="plugin-main">
                            <div className="plugin-name">
                              {p.name}
                              {p.isBundle && <span className="tag bundle">bundle</span>}
                              {p.isDisabled && <span className="tag off">已停用</span>}
                            </div>
                            <div className="plugin-spec">
                              {p.spec}
                              {up && (
                                <span className="ver-info">
                                  {up.error ? (
                                    <span className="ver-err">更新查询失败</span>
                                  ) : updatable ? (
                                    <span className="ver-new">
                                      新版 {up.latest}
                                    </span>
                                  ) : up.latest ? (
                                    <span className="ver-ok">已是最新</span>
                                  ) : null}
                                </span>
                              )}
                            </div>
                          </div>
                          <div className="plugin-actions">
                            {updatable && (
                              <button
                                className="btn tiny primary"
                                onClick={() => handleUpdatePlugin(p, up.latest)}
                                disabled={busy || updatingName !== null}
                                title={`更新到 ${up.latest}`}
                              >
                                {updatingName === p.name ? "更新中..." : "更新"}
                              </button>
                            )}
                            <button
                              className="btn tiny uninstall"
                              onClick={() => setConfirmUninstall(p)}
                              disabled={busy}
                              title="卸载该插件"
                            >
                              卸载
                            </button>
                            {/* 拉杆开关 */}
                            <button
                              className={`switch ${p.isDisabled ? "" : "on"}`}
                              role="switch"
                              aria-checked={!p.isDisabled}
                              onClick={() => handleTogglePlugin(p, p.isDisabled)}
                              disabled={busy}
                              title={p.isDisabled ? "启用插件" : "停用插件"}
                            >
                              <span className="switch-knob" />
                            </button>
                          </div>
                        </div>
                      );
                    })}
                  </div>
                )}
              </div>
            </>
          )}
        </section>
      </main>

      {/* 板块帮助弹窗 */}
      {helpTopic && (
        <div className="modal-mask" onClick={() => setHelpTopic(null)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h3>{helpTopic === "envs" ? "DSH 环境（左栏）使用说明" : "Profiles（中栏）使用说明"}</h3>
            {helpTopic === "envs" ? (
              <div className="scan-help-body">
                <div className="card-title">这是什么？</div>
                <ul>
                  <li>左栏每一项 = 你电脑上的一个 <strong>DSH 程序本体</strong>（可执行文件），决定"用哪个 DSH 来运行"。</li>
                  <li>一个环境对应一个 dsh 版本/安装；可同时存在多个版本（如 0.1.0-rc.7 全局版、0.1.7-rc.2-mc 本地版）。</li>
                </ul>
                <div className="card-title">怎么添加环境？</div>
                <ul>
                  <li><strong>⬇ 下载DSH</strong>：从镜像下载任意版本到指定目录，自动添加为环境。</li>
                  <li><strong>＋扫本体</strong>：选择可能包含 dsh 的目录，应用向下递归 3 层自动探测（识别 <code>dsh.cmd</code> / <code>bin.js</code> 并读取版本）。
                    例如：全局 npm 目录 <code>…\AppData\Roaming\npm</code>、DSH Desktop 的 <code>…\.dsh-win\versions\…</code>、或你下载 dsh 的文件夹。</li>
                  <li>全局安装的 dsh CLI 通常已自动识别，无需手动添加。</li>
                </ul>
                <div className="card-title">怎么用？</div>
                <ul>
                  <li>选中一个环境 + 在中栏选中任意 profile → 点击「▶ 启动」＝ 用该 DSH 加载该 profile。</li>
                  <li>任意环境 × 任意 profile 可自由组合；切换环境不影响 profile 列表。</li>
                  <li>可同时运行多个实例：不同环境 × 不同 profile 各自独立进程、独立端口（从 3080 起自动分配），每行可单独停止/重启。</li>
                  <li>每行「移除」按钮：会删除磁盘上对应的 DSH 版本目录（仅限受管下载目录内、带版本标识与安装特征的目录，四重校验防误删）；全局 CLI 等非受管环境只移出列表。</li>
                </ul>
              </div>
            ) : (
              <div className="scan-help-body">
                <div className="card-title">这是什么？</div>
                <ul>
                  <li>每个 profile = 一套独立的插件集合与配置（<code>package.json</code> + <code>cordis.patch.yml</code>），相当于 DSH 的"配置档"。</li>
                  <li>所有 profile 统一堆放在列表中，<strong>不绑定任何 DSH 版本</strong>——切换环境不会影响 profile。</li>
                  <li>列表自动合并：默认 <code>~/.dsh/profiles</code> + 你通过「＋扫Profile」添加的目录（如 DSH Desktop 的 <code>.dsh-packs</code> 目录），每个 profile 标注自己的来源目录。</li>
                </ul>
                <div className="card-title">常见操作</div>
                <ul>
                  <li><strong>＋新建</strong>：新建空白 profile（含默认 base + web-app bundle，可直接启动）。</li>
                  <li><strong>＋扫Profile</strong>：选择本地 DSH_HOME 或 profiles 目录，把里面的 profile 加入列表。</li>
                  <li><strong>⬇导入 / 导出</strong>：把 profile 打包成 zip 迁移到其他电脑（可排除 node_modules 减小体积；重名自动改名）。</li>
                  <li>每行：📂 打开目录 / ▶ 启动 / ⬆ 导出 / 🗑 删除。</li>
                  <li>选中 profile 后，右侧可安装/管理插件（市场、npm 搜索、自定义源、本地文件夹/tgz）。</li>
                </ul>
                <div className="card-title">与 DSH 的组合</div>
                <ul>
                  <li>启动时自动把该 profile 的来源目录注入 <code>DSH_HOME</code>，任意 DSH 版本都能加载任意来源的 profile。</li>
                  <li>同名的不同来源 profile 会被区分对待，可同时运行。</li>
                </ul>
              </div>
            )}
            <div className="modal-actions">
              <button className="btn primary" onClick={() => setHelpTopic(null)}>知道了</button>
            </div>
          </div>
        </div>
      )}

      {/* 在线安装对话框 */}
      {showInstall && selectedEnvObj && selectedProfile && (
        <InstallDialog
          env={selectedEnvObj}
          profile={selectedProfile}
          profilesDir={profileDirOf(selectedProfile)}
          initialTab={installTab}
          onClose={() => setShowInstall(false)}
          onInstalled={() => {
            api.listPlugins(selectedEnv, selectedProfile, profileDirOf(selectedProfile)).then(setPlugins).catch(() => {});
          }}
        />
      )}

      {/* 设置弹窗 */}
      {showSettings && (
        <SettingsDialog
          onClose={() => setShowSettings(false)}
          onSaved={() => {
            setShowSettings(false);
            setInfo("设置已保存");
          }}
        />
      )}

      {/* DSH 下载弹窗 */}
      {showDshDownload && (
        <DshDownloadDialog
          onClose={() => setShowDshDownload(false)}
          onInstalled={() => loadEnvs()}
        />
      )}

      {/* 清理残留确认弹窗 */}
      {confirmCleanJunk && junk && (
        <div className="modal-mask" onClick={() => setConfirmCleanJunk(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h3>清理残留缓存？</h3>
            <p className="modal-hint">
              将删除 {junk.length} 项缓存目录，共{" "}
              {formatSize(junk.reduce((s, j) => s + j.size, 0))}（
              {junk.map((j) => j.name).join("、")}）。不影响已安装插件与配置。
            </p>
            <div className="modal-actions">
              <button className="btn" onClick={() => setConfirmCleanJunk(false)} disabled={cleaningJunk}>
                取消
              </button>
              <button className="btn danger" onClick={() => { setConfirmCleanJunk(false); handleCleanJunk(); }} disabled={cleaningJunk}>
                {cleaningJunk ? "清理中..." : "确认清理"}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* 新建 profile 弹窗 */}
      {showCreateProfile && (
        <div className="modal-mask" onClick={() => setShowCreateProfile(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h3>新建 Profile</h3>
            <p className="modal-hint">
              将创建空白 profile（默认含 <code>@deepseek-ai/dsh-base</code> 与{" "}
              <code>@deepseek-ai/dsh-web-app</code> bundle，可直接启动或安装插件）。
            </p>
            <CreateProfileForm
              busy={creatingProfile}
              onCancel={() => setShowCreateProfile(false)}
              onCreate={handleCreateProfile}
            />
          </div>
        </div>
      )}

      {/* 删除 profile 确认弹窗 */}
      {confirmDeleteProfile && (
        <div className="modal-mask" onClick={() => setConfirmDeleteProfile(null)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h3>删除 profile「{confirmDeleteProfile.name}」？</h3>
            <p className="modal-hint">
              将删除整个目录（含 node_modules，{formatSize(confirmDeleteProfile.dataSize)}），
              该操作<strong>不可恢复</strong>。如需保留可先用「⬇ 导出」备份。
            </p>
            <div className="modal-actions">
              <button className="btn" onClick={() => setConfirmDeleteProfile(null)} disabled={deletingProfile}>
                取消
              </button>
              <button className="btn danger" onClick={doDeleteProfile} disabled={deletingProfile}>
                {deletingProfile ? "删除中..." : "确认删除"}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* 卸载确认弹窗 */}
      {confirmUninstall && (
        <div className="modal-mask" onClick={() => setConfirmUninstall(null)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h3>卸载插件「{confirmUninstall.name}」？</h3>
            <p className="modal-hint">
              将从当前 Profile 移除该依赖（含 node_modules 中的包）。
              {confirmUninstall.isDisabled && " 该插件当前处于停用状态，卸载后如需恢复需重新安装。"}
            </p>
            <div className="modal-actions">
              <button className="btn" onClick={() => setConfirmUninstall(null)} disabled={busy}>
                取消
              </button>
              <button className="btn danger" onClick={doUninstall} disabled={busy}>
                {busy ? "卸载中..." : "确认卸载"}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* 导出确认弹窗 */}
      {exportTarget && (
        <div className="modal-mask" onClick={() => setExportTarget(null)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h3>导出 Profile「{exportTarget.profile}」</h3>
            <p className="modal-hint">
              node_modules 体积：{formatSize(exportTarget.nmSize)}
              {exportTarget.nmSize > 20 * 1024 * 1024
                ? "（较大，建议排除后导入再重建依赖）"
                : ""}
            </p>
            <ExportOptions
              profile={exportTarget.profile}
              nmSize={exportTarget.nmSize}
              versions={Array.from(new Set(envs.map((e) => e.version).filter((v) => v && v !== "—")))}
              onConfirmZip={(exclude) => doExport(exclude)}
              onConfirmPack={(name, version, displayName, dshHint) =>
                doExportPack(name, version, displayName, dshHint)
              }
              onClone={doClone}
              onCancel={() => setExportTarget(null)}
              busy={busy}
            />
          </div>
        </div>
      )}

      {/* 关闭软件时仍有 DSH 在运行 → 三选一 */}
      {closeCheck !== null && (
        <div className="modal-mask">
          <div className="modal">
            <h3>仍有 DSH 在运行</h3>
            <p className="modal-hint">
              当前有 <b>{closeCheck}</b> 个 DSH 实例正在后台运行。关闭软件前请选择：
            </p>
            <div className="modal-actions" style={{ flexDirection: "column", alignItems: "stretch" }}>
              <button className="btn primary" disabled={closingAll} onClick={() => closeAppChoice("stop-all")}>
                {closingAll ? "正在关闭全部 DSH..." : "关闭所有 DSH 并退出"}
              </button>
              <button className="btn" disabled={closingAll} onClick={() => closeAppChoice("release")}>
                放生 DSH（软件退出，DSH 继续运行）
              </button>
              <button className="btn" disabled={closingAll} onClick={() => closeAppChoice("cancel")}>
                取消
              </button>
            </div>
          </div>
        </div>
      )}

      {/* 移除 DSH 环境二次确认 */}
      {confirmRemoveEnv && (
        <div className="modal-mask" onClick={() => setConfirmRemoveEnv(null)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h3>{confirmRemoveEnv.isManual ? "移除 DSH 环境" : "移除扫描目录"}</h3>
            <p className="modal-hint">
              {confirmRemoveEnv.isManual ? (
                <>
                  将从磁盘删除该 DSH 版本目录：
                  <br />
                  <b>{confirmRemoveEnv.env.name}</b>（dsh {confirmRemoveEnv.env.version}）
                  <br />
                  程序: {binDir(confirmRemoveEnv.env.binPath ?? "")}
                  <br />
                  <br />
                  仅删除受管下载目录内带版本标识的目录（如 C:\dsh-versions\dsh-
                  {confirmRemoveEnv.env.version}），其他文件不受影响。此操作不可恢复。
                </>
              ) : (
                <>将移出该 profile 扫描目录（磁盘文件不受影响）。</>
              )}
            </p>
            <div className="modal-actions">
              <button className="btn" onClick={() => setConfirmRemoveEnv(null)}>取消</button>
              <button
                className="btn danger"
                onClick={() => {
                  const { env, isManual } = confirmRemoveEnv;
                  setConfirmRemoveEnv(null);
                  const act = isManual ? api.removeManualEnv(env.id) : api.removeScanDir(env.id);
                  act.then((msg) => {
                    loadEnvs();
                    if (typeof msg === "string" && msg) setError(msg);
                  }).catch((x) => setError(String(x)));
                }}
              >
                确认移除
              </button>
            </div>
          </div>
        </div>
      )}

      {/* 导入对话框（完整 zip / 整合包） */}
      {showImportDialog && selectedEnv && (
        <ImportDialog
          envId={selectedEnv}
          envName={envs.find((e) => e.id === selectedEnv)?.name ?? ""}
          profilesDir={profileDirOf(selectedProfile)}
          onClose={() => setShowImportDialog(false)}
          onImported={refreshAfterImport}
          setGlobalError={(m) => setError(m)}
          setGlobalInfo={(m) => setInfo(m)}
        />
      )}

      {/* 备注对话框（右键 profile） */}
      {noteTarget && (
        <NoteDialog
          profile={noteTarget.profile}
          profilesDir={noteTarget.profilesDir}
          versions={Array.from(new Set(envs.map((e) => e.version).filter((v) => v && v !== "—")))}
          initial={notes?.[`${noteTarget.profile}::${noteTarget.profilesDir}`]}
          onClose={() => setNoteTarget(null)}
          onSaved={(note) => {
            setNotes((prev) => {
              const key = `${noteTarget.profile}::${noteTarget.profilesDir}`;
              const next = { ...prev };
              if (!note.note.trim() && !note.hintVersion.trim()) {
                delete next[key];
              } else {
                next[key] = note;
              }
              return next;
            });
            setNoteTarget(null);
          }}
        />
      )}
    </div>
  );
}

/** 导出选项：完整 zip / 整合包 .dspack */
function ExportOptions({
  profile,
  nmSize,
  versions,
  onConfirmZip,
  onConfirmPack,
  onClone,
  onCancel,
  busy,
}: {
  profile: string;
  nmSize: number;
  versions: string[];
  onConfirmZip: (exclude: boolean) => void;
  onConfirmPack: (name: string, version: string, displayName: string, dshHint: string) => void;
  onClone: () => void;
  onCancel: () => void;
  busy: boolean;
}) {
  const [mode, setMode] = useState<"zip" | "pack">("zip");
  const [exclude, setExclude] = useState(nmSize > 20 * 1024 * 1024);
  const [packName, setPackName] = useState(profile);
  const [packVersion, setPackVersion] = useState("1.0.0");
  const [displayName, setDisplayName] = useState("");
  const [dshHint, setDshHint] = useState("");
  const [customHint, setCustomHint] = useState("");
  return (
    <div>
      <div className="seg-row">
        <button className={`seg ${mode === "zip" ? "on" : ""}`} onClick={() => setMode("zip")} disabled={busy}>
          完整 profile zip
        </button>
        <button className={`seg ${mode === "pack" ? "on" : ""}`} onClick={() => setMode("pack")} disabled={busy}>
          整合包（.dspack）
        </button>
      </div>
      {mode === "zip" ? (
        <label className="check-row">
          <input type="checkbox" checked={exclude} onChange={(e) => setExclude(e.target.checked)} />
          排除 node_modules（压缩包更小；导入后可重建依赖）
        </label>
      ) : (
        <div className="pack-form">
          <div className="field-row">
            <label>
              包名（小写 slug）
              <input value={packName} onChange={(e) => setPackName(e.target.value)} placeholder="如 my-pack" />
            </label>
            <label>
              版本
              <input value={packVersion} onChange={(e) => setPackVersion(e.target.value)} placeholder="1.0.0" />
            </label>
          </div>
          <label>
            显示名（可选）
            <input value={displayName} onChange={(e) => setDisplayName(e.target.value)} placeholder="我的整合包" />
          </label>
          <label>
            适配版本（写入 manifest.dshVersion）
            <select value={dshHint} onChange={(e) => setDshHint(e.target.value)}>
              <option value="">（使用当前环境版本）</option>
              {versions.map((v) => (
                <option key={v} value={v}>{v}</option>
              ))}
              <option value="__custom__">自定义…</option>
            </select>
          </label>
          {dshHint === "__custom__" && (
            <label>
              自定义 DSH 版本
              <input
                value={customHint}
                onChange={(e) => setCustomHint(e.target.value)}
                placeholder="如 0.1.7-rc.2（不要求本机已安装）"
              />
            </label>
          )}
          <p className="modal-hint">
            按 DSH-PackForge 规范导出 .dspack v3（含 manifest v5；自动排除 node_modules / 运行数据 / 凭据）。
            适配版本仅作提示，导入方会写入 profile 备注。
          </p>
        </div>
      )}
      <div className="modal-actions" style={{ flexWrap: "wrap", gap: 8 }}>
        <button className="btn" onClick={onCancel} disabled={busy}>
          取消
        </button>
        <button className="btn" onClick={onClone} disabled={busy} title="复制到同目录，自动补 -1 递增名（含 node_modules，立即可用）">
          📋 复制并重命名副本
        </button>
        {mode === "zip" ? (
          <button className="btn primary" onClick={() => onConfirmZip(exclude)} disabled={busy}>
            {busy ? "导出中..." : "选择位置并导出"}
          </button>
        ) : (
          <button
            className="btn primary"
            onClick={() =>
              onConfirmPack(
                packName,
                packVersion,
                displayName,
                dshHint === "__custom__" ? customHint : dshHint,
              )
            }
            disabled={busy || (dshHint === "__custom__" && !customHint.trim())}
          >
            {busy ? "导出中..." : "选择位置并导出整合包"}
          </button>
        )}
      </div>
    </div>
  );
}

/** 新建 profile 表单：必选 profiles 目录（不是则要求重新选） */
function CreateProfileForm({
  busy,
  onCancel,
  onCreate,
}: {
  busy: boolean;
  onCancel: () => void;
  onCreate: (name: string, profilesDir: string) => void;
}) {
  const [name, setName] = useState("");
  const [dir, setDir] = useState("");
  const [err, setErr] = useState("");
  const pick = async () => {
    setErr("");
    const p = await pickFolder("选择 profiles 目录（文件夹名须为 profiles）");
    if (!p) return;
    // 严格：必须是 profiles 文件夹本体，否则要求重新选
    const leaf = p.split(/[\\/]/).filter(Boolean).pop() ?? "";
    if (leaf.toLowerCase() !== "profiles") {
      setErr("所选目录不是 profiles 文件夹（文件夹名须为 profiles），请重新选择");
      return;
    }
    setDir(p);
  };
  const submit = () => {
    const n = name.trim();
    if (!n) {
      setErr("请输入 profile 名称");
      return;
    }
    if (/[<>:"/\\|?*]/.test(n)) {
      setErr("名称不能包含 < > : \" / \\ | ? * 字符");
      return;
    }
    if (!dir.trim()) {
      setErr("请选择 profiles 目录");
      return;
    }
    onCreate(n, dir.trim());
  };
  return (
    <div>
      <label>
        Profile 名称
        <input
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="如：dev / test / 0.1.7-rc.2"
          autoFocus
          onKeyDown={(e) => e.key === "Enter" && submit()}
        />
      </label>
      <label>
        目标 profiles 目录（必选）
        <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
          <input
            value={dir}
            readOnly
            placeholder="点击「选择目录」指定 profiles 文件夹"
            style={{ flex: 1 }}
          />
          <button type="button" className="btn tiny" onClick={pick} disabled={busy}>
            选择目录
          </button>
        </div>
      </label>
      {err && <div className="modal-err">{err}</div>}
      <div className="modal-actions">
        <button className="btn" onClick={onCancel} disabled={busy}>
          取消
        </button>
        <button className="btn primary" onClick={submit} disabled={busy || !name.trim() || !dir.trim()}>
          {busy ? "创建中..." : "创建"}
        </button>
      </div>
    </div>
  );
}

/** npm 镜像源下拉预设（空 = npm 官方直连） */
const NPM_REGISTRY_PRESETS: { value: string; label: string }[] = [
  { value: "", label: "npm 官方源（直连）" },
  { value: "https://registry.npmmirror.com", label: "npmmirror（淘宝，默认推荐）" },
  { value: "https://mirrors.cloud.tencent.com/npm/", label: "腾讯云镜像" },
  { value: "https://repo.huaweicloud.com/repository/npm/", label: "华为云镜像" },
  { value: "__custom__", label: "自定义…" },
];

/** GitHub 下载镜像下拉预设（空 = 直连 GitHub） */
const GITHUB_MIRROR_PRESETS: { value: string; label: string }[] = [
  { value: "", label: "直连 GitHub（不做代理）" },
  { value: "https://ghfast.top", label: "ghfast.top（默认推荐）" },
  { value: "https://ghproxy.net", label: "ghproxy.net" },
  { value: "https://gh-proxy.com", label: "gh-proxy.com" },
  { value: "__custom__", label: "自定义…" },
];

/** 设置弹窗：npm 镜像源 + GitHub 镜像 + DSH 下载目录 */
function SettingsDialog({ onClose, onSaved }: { onClose: () => void; onSaved: () => void }) {
  const [registry, setRegistry] = useState("");
  const [downloadDir, setDownloadDir] = useState("");
  const [githubMirror, setGithubMirror] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");
  const [aboutOpen, setAboutOpen] = useState(false);
  const [checking, setChecking] = useState(false);
  const [updateInfo, setUpdateInfo] = useState<UpdateInfo | null>(null);
  const [downloading, setDownloading] = useState(false);
  const [downPercent, setDownPercent] = useState(0);
  const [downPath, setDownPath] = useState("");
  const [downErr, setDownErr] = useState("");
  const [closingForUpdate, setClosingForUpdate] = useState(false);

  // 监听更新下载进度
  useEffect(() => {
    const un = listen("update-download", (e) => {
      const p = e.payload as { done?: number | boolean; total?: number; percent?: number; path?: string; launched?: boolean };
      if (typeof p?.done === "boolean" || p?.path) {
        setDownloading(false);
        if (p.path) setDownPath(p.path);
      } else if (typeof p?.percent === "number") {
        setDownPercent(p.percent);
      }
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  useEffect(() => {
    api.getSettings().then((s) => {
      setRegistry(typeof s?.npmRegistry === "string" ? s.npmRegistry : "");
      setDownloadDir(typeof s?.dshDownloadDir === "string" ? s.dshDownloadDir : "");
      setGithubMirror(typeof s?.githubMirror === "string" ? s.githubMirror : "");
    }).catch((e) => setErr(String(e)));
  }, []);

  const save = async () => {
    setBusy(true);
    setErr("");
    try {
      await api.setSettings(registry, downloadDir, githubMirror);
      onSaved();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  // 当前值是否在预设内（决定下拉显示与是否出现自定义输入框）
  const inPresets = (value: string, presets: { value: string; label: string }[]) =>
    presets.some((p) => p.value === value);
  const selectValue = (value: string, presets: { value: string; label: string }[]) =>
    inPresets(value, presets) ? value : "__custom__";

  return (
    <div className="modal-mask" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <h3>设置</h3>
        <p className="modal-hint">
          npm 镜像源用于：插件市场/搜索、包信息查询、DSH 下载、写各 profile 的 .npmrc（插件安装）。
          <b>留空 = 不使用镜像源</b>（走 npm 官方源）。大陆网络可填
          <code>registry.npmmirror.com</code>。
        </p>
        <label>
          npm 镜像源（registry）
          <select
            value={selectValue(registry, NPM_REGISTRY_PRESETS)}
            onChange={(e) => {
              const v = e.target.value;
              setRegistry(v === "__custom__" ? "" : v);
            }}
          >
            {NPM_REGISTRY_PRESETS.map((p) => (
              <option key={p.value} value={p.value}>{p.label}</option>
            ))}
          </select>
        </label>
        {selectValue(registry, NPM_REGISTRY_PRESETS) === "__custom__" && (
          <label>
            自定义 npm 镜像源
            <input
              value={registry}
              onChange={(e) => setRegistry(e.target.value)}
              placeholder="https://registry.npmmirror.com"
            />
          </label>
        )}
        <label>
          GitHub 下载镜像
          <select
            value={selectValue(githubMirror, GITHUB_MIRROR_PRESETS)}
            onChange={(e) => {
              const v = e.target.value;
              setGithubMirror(v === "__custom__" ? "" : v);
            }}
          >
            {GITHUB_MIRROR_PRESETS.map((p) => (
              <option key={p.value} value={p.value}>{p.label}</option>
            ))}
          </select>
        </label>
        {selectValue(githubMirror, GITHUB_MIRROR_PRESETS) === "__custom__" && (
          <label>
            自定义 GitHub 镜像前缀
            <input
              value={githubMirror}
              onChange={(e) => setGithubMirror(e.target.value)}
              placeholder="https://ghfast.top"
            />
          </label>
        )}
        <div className="modal-hint" style={{ marginTop: 6 }}>
          <b>关于 / 更新</b>
        </div>
        <div style={{ display: "flex", gap: 8, marginBottom: 10 }}>
          <button className="btn tiny" onClick={() => setAboutOpen(!aboutOpen)}>
            ℹ 关于本项目
          </button>
          <button
            className="btn tiny"
            disabled={checking}
            onClick={async () => {
              setChecking(true);
              setUpdateInfo(null);
              try {
                setUpdateInfo(await api.checkUpdate());
              } catch (e) {
                setUpdateInfo({ current: "", latest: "", hasUpdate: false, url: "", error: String(e) });
              } finally {
                setChecking(false);
              }
            }}
          >
            {checking ? "检查中…" : "🔄 检查更新"}
          </button>
        </div>
        {aboutOpen && (
          <div style={{ background: "var(--panel)", border: "1px solid var(--border)", borderRadius: 8, padding: "10px 12px", marginBottom: 10, fontSize: 12, lineHeight: 1.8 }}>
            <div><b>DSH Manager</b> <span style={{ color: "var(--text-dim)" }}>v0.3.9</span></div>
            <div style={{ color: "var(--text-dim)" }}>
              图形化 DSH 环境与插件管理工具（Tauri 2 + React）。仅管理本地 CLI 版 DSH；
              支持多版本下载、Profile 管理、插件安装、整合包、多实例独立运行。
            </div>
            <div style={{ marginTop: 6 }}>
              <button className="btn tiny" onClick={() => api.openUrl("https://github.com/Liaoyuanxinghuo/DSH-Plugin-Manager/")}>
                🌐 项目地址（GitHub）
              </button>
            </div>
          </div>
        )}
        {updateInfo && (
          <div style={{ background: "var(--panel)", border: "1px solid var(--border)", borderRadius: 8, padding: "10px 12px", marginBottom: 10, fontSize: 12, lineHeight: 1.8 }}>
            {updateInfo.error ? (
              <div style={{ color: "#ffb3ad" }}>{updateInfo.error}</div>
            ) : updateInfo.hasUpdate ? (
              <div>
                <div style={{ color: "#ffd479" }}>
                  发现新版本：当前 v{updateInfo.current} → v{updateInfo.latest}
                </div>
                {!downloading && !downPath && (
                  <button
                    className="btn tiny"
                    style={{ marginTop: 6 }}
                    disabled={downloading}
                    onClick={async () => {
                      setDownloading(true);
                      setDownPercent(0);
                      setDownPath("");
                      setDownErr("");
                      try {
                        const path = await api.downloadUpdate(updateInfo.latest);
                        setDownPath(path);
                      } catch (e) {
                        setDownErr(String(e));
                        setDownloading(false);
                      }
                    }}
                  >
                    ⬇ 下载 v{updateInfo.latest}
                  </button>
                )}
                {downloading && (
                  <div style={{ marginTop: 6, display: "flex", alignItems: "center", gap: 8 }}>
                    <div style={{ flex: 1, height: 6, background: "var(--border)", borderRadius: 3, overflow: "hidden" }}>
                      <div style={{ width: `${downPercent}%`, height: "100%", background: "#5b8cff", transition: "width .15s" }} />
                    </div>
                    <span style={{ color: "var(--text-dim)", width: 44, textAlign: "right" }}>{downPercent}%</span>
                  </div>
                )}
                {downErr && <div style={{ color: "#ffb3ad", marginTop: 6 }}>{downErr}</div>}
                {downPath && (
                  <div style={{ marginTop: 6 }}>
                    <div style={{ color: "#7fd6a2", wordBreak: "break-all" }}>✅ 已保存：{downPath}</div>
                    <div style={{ display: "flex", gap: 8, marginTop: 6 }}>
                      <button className="btn tiny" onClick={() => api.openPath(downPath)}>
                        📂 打开所在文件夹
                      </button>
                      <button
                        className="btn tiny"
                        disabled={closingForUpdate}
                        onClick={async () => {
                          setClosingForUpdate(true);
                          try {
                            await api.launchInstallerAndExit(downPath);
                          } catch (e) {
                            setDownErr(String(e));
                            setClosingForUpdate(false);
                          }
                        }}
                      >
                        {closingForUpdate ? "正在启动安装程序…" : "🔄 关闭程序并更新"}
                      </button>
                    </div>
                  </div>
                )}
              </div>
            ) : (
              <div style={{ color: "var(--text)" }}>
                ✅ 已是最新版本（v{updateInfo.current || "?"}）{updateInfo.latest ? `（最新 v${updateInfo.latest}）` : ""}
              </div>
            )}
          </div>
        )}
        <label>
          DSH 下载目录（多版本共存根目录）
          <input
            value={downloadDir}
            onChange={(e) => setDownloadDir(e.target.value)}
            placeholder="C:\dsh-versions"
          />
          <button
            className="btn tiny"
            style={{ marginTop: 4 }}
            onClick={async () => {
              const p = await pickFolder("选择 DSH 多版本下载根目录");
              if (p) setDownloadDir(p);
            }}
          >
            📂 选择目录
          </button>
        </label>
        {err && <div className="modal-err">{err}</div>}
        <div className="modal-actions">
          <button className="btn" onClick={onClose} disabled={busy}>取消</button>
          <button className="btn primary" onClick={save} disabled={busy || !downloadDir.trim()}>
            {busy ? "保存中..." : "保存"}
          </button>
        </div>
      </div>
    </div>
  );
}

/** DSH 多版本下载弹窗 */
function DshDownloadDialog({ onClose, onInstalled }: { onClose: () => void; onInstalled: () => void }) {
  const [versions, setVersions] = useState<DshVersionInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [dir, setDir] = useState("");
  const [ver, setVer] = useState("");
  const [installing, setInstalling] = useState(false);
  const [elapsed, setElapsed] = useState(0);
  const [prog, setProg] = useState<{ bytes: number; total: number; percent: number } | null>(null);
  const [logs, setLogs] = useState<InstallLogLine[]>([]);
  const [err, setErr] = useState("");
  const [done, setDone] = useState<DshInstallResult | null>(null);

  // 安装计时：让用户知道安装仍在进行（npm 安装无进度输出，容易误以为卡死）
  useEffect(() => {
    if (!installing) {
      setElapsed(0);
      return;
    }
    setElapsed(0);
    const t = setInterval(() => setElapsed((e) => e + 1), 1000);
    return () => clearInterval(t);
  }, [installing]);

  useEffect(() => {
    let alive = true;
    const unLogListeners: (() => void)[] = [];
    api.getSettings().then((s) => { if (alive && typeof s?.dshDownloadDir === "string") setDir(s.dshDownloadDir); }).catch(() => {});
    api.listDshVersions()
      .then((vs) => {
        if (!alive) return;
        setVersions(vs);
        // 默认选中 latest（带 latest tag 或第一个）
        const latest = vs.find((v) => v.tags.includes("latest")) ?? vs[0];
        if (latest) setVer(latest.version);
      })
      .catch((e) => setErr(String(e)))
      .finally(() => { if (alive) setLoading(false); });
    // 监听安装日志事件（install-log / install-done）；非 Tauri 环境（测试）忽略
    import("@tauri-apps/api/event").then(({ listen }) => {
      if (!alive) return;
      try {
        listen<InstallLogLine>("install-log", (e) => {
          if (alive) setLogs((prev) => [...prev.slice(-200), e.payload]);
        }).then((un) => unLogListeners.push(un)).catch(() => {});
        listen("install-done", () => {}).then((un) => unLogListeners.push(un)).catch(() => {});
        listen<{ bytes: number; total: number; percent: number }>("install-progress", (e) => {
          if (alive) setProg(e.payload);
        }).then((un) => unLogListeners.push(un)).catch(() => {});
      } catch {
        // 无 Tauri IPC 桥时忽略
      }
    }).catch(() => {});
    return () => {
      alive = false;
      unLogListeners.forEach((u) => u());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const start = async () => {
    if (!ver || !(dir ?? "").trim()) return;
    setInstalling(true);
    setErr("");
    setDone(null);
    setLogs([]);
    try {
      const r = await api.installDshVersion(ver, dir);
      setDone(r);
      if (r.success && r.binPath) {
        // 添加为手动环境（命令 = dsh.cmd 路径）
        try {
          await api.addManualEnv(`dsh ${r.version}`, r.binPath);
          setErr("");
          onInstalled();
        } catch (ae) {
          setErr(`安装成功但添加环境失败：${String(ae)}（可点击「＋扫本体」扫描该目录添加）`);
        }
      }
    } catch (e) {
      setErr(String(e));
    } finally {
      setInstalling(false);
    }
  };

  return (
    <div className="modal-mask">
      <div className="modal install-modal">
        <div className="modal-head">
          <h3>下载 DSH 版本</h3>
          <button className="btn tiny" onClick={onClose} disabled={installing}>×</button>
        </div>
        {loading ? (
          <div className="empty">加载版本列表...</div>
        ) : (
          <>
            <div className="row">
              <input
                className="input grow"
                value={dir}
                onChange={(e) => setDir(e.target.value)}
                placeholder="下载根目录（每版本独立子目录）"
              />
              <button className="btn tiny" onClick={async () => { const p = await pickFolder("选择下载根目录"); if (p) setDir(p); }}>
                📂
              </button>
            </div>
            <div className="ver-list" style={{ maxHeight: 260 }}>
              {versions.map((v) => (
                <div
                  key={v.version}
                  className={`ver-row ${ver === v.version ? "active" : ""}`}
                  onClick={() => setVer(v.version)}
                >
                  <span className="ver-no">{v.version}</span>
                  <span className="tag-row" style={{ marginTop: 0 }}>
                    {v.tags.map((t) => (
                      <span key={t} className="tag-latest tag">{t}</span>
                    ))}
                  </span>
                </div>
              ))}
            </div>
            {err && <div className="error">{err}</div>}
            {done && (
              <div className={done.success ? "info" : "error"}>
                {done.success ? `✅ ${done.summary}` : `❌ ${done.summary}`}
              </div>
            )}
            {installing && prog && prog.percent !== 4294967295 && prog.percent > 0 && (
              <div className="install-progress">
                <div className="bar"><div className="bar-fill" style={{ width: `${Math.min(100, Math.max(3, prog.percent))}%` }} /></div>
                <div className="bar-text">
                  {Math.round(prog.percent)}% · 已安装 {(prog.bytes / 1024 / 1024).toFixed(1)} MB / {(prog.total / 1024 / 1024).toFixed(1)} MB（参考）
                </div>
              </div>
            )}
            {installing && prog && prog.percent === 4294967295 && (
              <div className="install-progress">
                <div className="bar"><div className="bar-fill indeterminate" /></div>
                <div className="bar-text">已安装 {(prog.bytes / 1024 / 1024).toFixed(1)} MB（估算中…）</div>
              </div>
            )}
            {logs.length > 0 && (
              <div className="log-box" style={{ maxHeight: 120 }}>
                {logs.map((l, i) => (
                  <div key={i} className={`log-line ${logLineClass(l)}`}>{l.line}</div>
                ))}
              </div>
            )}
            <div className="modal-actions">
              <button className="btn" onClick={onClose} disabled={installing}>关闭</button>
              <button className="btn primary" onClick={start} disabled={installing || !ver || !(dir ?? "").trim()}>
                {installing ? `下载安装中...（已运行 ${elapsed}s）` : `⬇ 安装 dsh ${ver}`}
              </button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}


// ==================== 导入对话框（完整 zip / 整合包） ====================
function ImportDialog({
  envId,
  envName,
  profilesDir,
  onClose,
  onImported,
  setGlobalError,
  setGlobalInfo,
}: {
  envId: string;
  envName: string;
  profilesDir: string;
  onClose: () => void;
  onImported: (finalNames: string[]) => void;
  setGlobalError: (m: string) => void;
  setGlobalInfo: (m: string) => void;
}) {
  const [step, setStep] = useState<"choose" | "pack-source" | "pack-online" | "busy">("choose");
  const [marketPacks, setMarketPacks] = useState<PackMarketEntry[] | null>(null);
  const [marketErr, setMarketErr] = useState("");
  const [loadingMarket, setLoadingMarket] = useState(false);
  const [logs, setLogs] = useState<InstallLogLine[]>([]);
  const [installing, setInstalling] = useState(false);
  const [done, setDone] = useState<{ success: boolean; summary: string } | null>(null);
  const unsubsRef = useRef<Array<() => void>>([]);
  // 导入目标目录：默认记住上次选择的目录，否则用当前选中 profile 的来源目录
  const [targetDir, setTargetDir] = useState<string>(() => {
    const saved = localStorage.getItem("dshpm-import-dir");
    return saved || profilesDir;
  });
  const chooseTargetDir = async () => {
    const chosen = await pickFolder("选择 DSH_HOME 或 profiles 目录（目标目录名必须为 profiles）");
    if (!chosen) return;
    const target = chosen.replace(/[\\/]+$/, "");
    if (!target.toLowerCase().endsWith("profiles")) {
      setGlobalError("目标目录名必须以 profiles 结尾，请重新选择（例如选择 DSH_HOME\profiles 目录）");
      return;
    }
    setTargetDir(target);
    localStorage.setItem("dshpm-import-dir", target);
  };

  // 监听依赖重建日志（dsh plugin install 流式输出）
  useEffect(() => {
    let alive = true;
    (async () => {
      const unLog = await listen<InstallLogLine>("install-log", (e) => {
        if (alive) setLogs((prev) => [...prev.slice(-400), e.payload]);
      });
      const unDone = await listen<InstallDone>("install-done", (e) => {
        if (!alive) return;
        if (e.payload.success) {
          setLogs((prev) => [...prev, { line: "✅ 依赖重建完成", kind: "stdout" }]);
        } else {
          setLogs((prev) => [...prev, { line: "❌ 依赖重建失败（可稍后在插件管理中「修复依赖」重试）", kind: "stderr" }]);
        }
      });
      if (alive) unsubsRef.current = [unLog, unDone];
    })();
    return () => {
      alive = false;
      unsubsRef.current.forEach((u) => u());
      unsubsRef.current = [];
    };
  }, []);

  const close = () => {
    if (installing) return;
    setDone(null);
    setLogs([]);
    onClose();
  };

  const finish = async (result: PackImportResult) => {
    setGlobalInfo(
      `整合包导入成功：${result.finalNames.join("、")}${result.renamed ? "（重名已自动改名）" : ""}${result.homeWritten ? `，写入全局文件 ${result.homeWritten} 个` : ""}${result.dshHint ? ` · 已写入备注适配版本 DSH ${result.dshHint}` : ""}`,
    );
    onImported(result.finalNames);
    close();
  };

  // 导入整合包 + 逐个重建依赖（日志展示）
  const importAndInstall = async (packPath: string) => {
    if (installing) return;
    setInstalling(true);
    setLogs([]);
    setDone(null);
    setGlobalError("");
    setStep("busy");
    try {
      const r = await api.importPack(envId, packPath, targetDir);
      setLogs((prev) => [...prev, { line: `已落盘：${r.finalNames.join("、")}，开始重建依赖…`, kind: "stdout" }]);
      for (const name of r.finalNames) {
        setLogs((prev) => [...prev, { line: `── 重建 ${name} 依赖（dsh plugin install）`, kind: "stdout" }]);
        const out = await api.fixDeps(envId, name, targetDir);
        if (!out.success) {
          setDone({ success: false, summary: `依赖重建失败（${name}）：${out.summary}` });
          setInstalling(false);
          return;
        }
      }
      await finish(r);
    } catch (e) {
      setGlobalError(String(e));
      setInstalling(false);
    }
  };

  // 在线市场列表
  const loadMarket = async () => {
    if (loadingMarket) return;
    setLoadingMarket(true);
    setMarketErr("");
    try {
      const list = await api.marketPacks();
      setMarketPacks(list);
      setStep("pack-online");
    } catch (e) {
      setMarketErr(String(e));
    } finally {
      setLoadingMarket(false);
    }
  };

  return (
    <div className="modal-mask" onClick={close}>
      <div className="modal" onClick={(e) => e.stopPropagation()} style={{ width: 620 }}>
        <div className="modal-head">
          <h3>导入 Profile（目标：{envName}）</h3>
          <button className="btn tiny" onClick={close} disabled={installing}>✕</button>
        </div>
        <p className="modal-hint">
          导入到 profiles 目录：{targetDir || "（当前环境默认）"}；重名自动改名。
          <button className="btn tiny" onClick={chooseTargetDir} style={{ marginLeft: 10 }} disabled={installing}>
            选择目录
          </button>
        </p>

        {step === "choose" && (
          <div className="import-choices">
            <button className="choice-card" onClick={async () => {
              const path = await pickZipFile();
              if (!path) return;
              setInstalling(true);
              try {
                const r = await api.importProfile(envId, path, targetDir);
                setGlobalInfo(
                  r.renamed
                    ? `导入成功：原 "${r.profileName}" 与现有重名，已改名为 "${r.finalName}"（${r.pluginCount} 个插件）`
                    : `导入成功：${r.finalName}（${r.pluginCount} 个插件）`,
                );
                onImported([r.finalName]);
                setInstalling(false);
                close();
              } catch (e) {
                setGlobalError(String(e));
                setInstalling(false);
              }
            }} disabled={installing}>
              <div className="choice-title">🗜 完整 profile zip</div>
              <div className="choice-desc">整目录打包（含数据），原样恢复；兼容旧版备份。</div>
            </button>
            <button className="choice-card" onClick={() => setStep("pack-source")} disabled={installing}>
              <div className="choice-title">📦 整合包（.dspack）</div>
              <div className="choice-desc">DSH-PackForge 规范：配置 + 插件 + patch，导入后自动重建依赖。</div>
            </button>
          </div>
        )}

        {step === "pack-source" && (
          <div className="import-choices">
            <button className="choice-card" onClick={loadMarket} disabled={loadingMarket || installing}>
              <div className="choice-title">🌐 在线下载整合包</div>
              <div className="choice-desc">浏览 dsh-pack-market 整合包市场，下载并校验 SHA-256 后导入。</div>
              {loadingMarket && <div className="dim">加载市场…</div>}
            </button>
            <button className="choice-card" onClick={async () => {
              const path = await pickDspackFile();
              if (!path) return;
              importAndInstall(path);
            }} disabled={installing}>
              <div className="choice-title">💾 本地导入整合包</div>
              <div className="choice-desc">选择本地 .dspack 文件（他人分享 / 已下载的整合包）。</div>
            </button>
            {marketErr && <div className="error">{marketErr}</div>}
          </div>
        )}

        {step === "pack-online" && (
          <div>
            {marketErr && <div className="error">{marketErr}</div>}
            {marketPacks && marketPacks.length === 0 && <div className="empty">市场暂无整合包</div>}
            <div className="pack-list">
              {marketPacks?.map((mp) => (
                <div key={mp.id} className="pack-item">
                  <div className="pack-item-head">
                    <span className="pack-name">{mp.displayName || mp.name}</span>
                    <span className="tag">v{mp.version}</span>
                    {mp.packType === "dshhome" && <span className="tag">多 profile</span>}
                    {mp.dshVersion && <span className="tag">dsh {mp.dshVersion}</span>}
                  </div>
                  <div className="pack-desc">
                    {mp.description || "（无描述）"}
                    {mp.author && <span className="dim"> — {mp.author}</span>}
                  </div>
                  <div className="pack-meta">
                    {mp.bundleCount > 0 && `${mp.bundleCount} bundle`}
                    {mp.depCount > 0 && ` · ${mp.depCount} 依赖`}
                    {mp.updatedAt && ` · 更新于 ${mp.updatedAt}`}
                    {mp.sha256 ? ` · ${formatSize(mp.size)}` : " · 无哈希（跳过校验）"}
                  </div>
                  <div className="pack-actions">
                    <button
                      className="btn tiny primary"
                      disabled={installing}
                      onClick={async () => {
                        try {
                          const local = await api.downloadPack(mp.downloadUrl, mp.sha256, mp.size);
                          await importAndInstall(local);
                        } catch (e) {
                          setGlobalError(String(e));
                        }
                      }}
                    >
                      下载并导入
                    </button>
                  </div>
                </div>
              ))}
            </div>
          </div>
        )}

        {step === "busy" && (
          <div>
            <p className="modal-hint">正在导入并重建依赖（pnpm install），请稍候…</p>
            {logs.length > 0 && (
              <div className="log-box" style={{ maxHeight: 220 }}>
                {logs.map((l, i) => (
                  <div key={i} className={`log-line ${logLineClass(l)}`}>{l.line}</div>
                ))}
              </div>
            )}
            {done && (
              <div className={done.success ? "info" : "error"}>{done.success ? `✅ ${done.summary}` : `❌ ${done.summary}`}</div>
            )}
          </div>
        )}

        {step !== "busy" && !installing && (
          <div className="modal-actions">
            <button className="btn" onClick={() => (step === "pack-source" ? setStep("choose") : setStep("pack-source"))}>
              返回
            </button>
            <button className="btn" onClick={close}>关闭</button>
          </div>
        )}
      </div>
    </div>
  );
}

// ==================== 备注对话框（右键 profile，仅提示无约束） ====================
function NoteDialog({
  profile,
  profilesDir,
  versions,
  initial,
  onClose,
  onSaved,
}: {
  profile: string;
  profilesDir: string;
  versions: string[];
  initial: ProfileNote | undefined;
  onClose: () => void;
  onSaved: (note: ProfileNote) => void;
}) {
  const [note, setNote] = useState(initial?.note ?? "");
  const [hintVersion, setHintVersion] = useState(initial?.hintVersion ?? "");
  const [saving, setSaving] = useState(false);
  const [err, setErr] = useState("");

  const save = async (clear: boolean) => {
    setSaving(true);
    setErr("");
    try {
      await api.saveProfileNote(profile, profilesDir, clear ? "" : note, clear ? "" : hintVersion);
      onSaved({ note: clear ? "" : note, hintVersion: clear ? "" : hintVersion });
    } catch (e) {
      setErr(String(e));
      setSaving(false);
    }
  };

  return (
    <div className="modal-mask" onClick={() => !saving && onClose()}>
      <div className="modal" onClick={(e) => e.stopPropagation()} style={{ width: 480 }}>
        <div className="modal-head">
          <h3>备注「{profile}」</h3>
          <button className="btn tiny" onClick={onClose} disabled={saving}>✕</button>
        </div>
        <p className="modal-hint">备注仅作提示，无任何约束；适配版本只用于提醒，不影响实际运行。</p>
        <label>
          备注内容
          <textarea
            value={note}
            onChange={(e) => setNote(e.target.value)}
            rows={3}
            placeholder="如：此 profile 用于日常网页浏览 / 专门跑 CodeX 工作流…"
          />
        </label>
        <label>
          适配版本（提示）
          <select value={hintVersion} onChange={(e) => setHintVersion(e.target.value)}>
            <option value="">（不指定）</option>
            {versions.map((v) => (
              <option key={v} value={v}>{v}</option>
            ))}
          </select>
        </label>
        {err && <div className="error">{err}</div>}
        <div className="modal-actions">
          <button className="btn" onClick={onClose} disabled={saving}>取消</button>
          <button className="btn" onClick={() => save(true)} disabled={saving}>
            清除备注
          </button>
          <button className="btn primary" onClick={() => save(false)} disabled={saving}>
            {saving ? "保存中..." : "保存备注"}
          </button>
        </div>
      </div>
    </div>
  );
}
