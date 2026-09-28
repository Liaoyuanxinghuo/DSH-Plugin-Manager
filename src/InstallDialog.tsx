// 在线插件安装对话框：
// 标签1「市场」= curated 插件目录（awesome-dsh-plugin，含 GitHub-only 插件）
// 标签2「npm 搜索」= 搜索 → 版本浏览 → 兼容预检 → 安装
// 标签3「自定义源」= 任意 spec（npm 包 / @scope/pkg@ver / github:user/repo / file:...）
// 标签4「本地导入」= 选择插件文件夹 或 .tgz 压缩包
import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, pickFolder, pickTgzFile } from "./api";
import type {
  DshEnv,
  InstallDone,
  InstallLogLine,
  MarketPlugin,
  NpmPackageInfo,
  NpmSearchHit,
  PeerIssue,
} from "./types";

interface Props {
  env: DshEnv;
  profile: string;
  profilesDir: string;
  onClose: () => void;
  onInstalled: () => void;
  initialTab?: Tab;
}

type Tab = "market" | "search" | "custom" | "local";

export default function InstallDialog({ env, profile, profilesDir, onClose, onInstalled, initialTab = "market" }: Props) {
  const [tab, setTab] = useState<Tab>(initialTab);

  // 市场标签
  const [catalog, setCatalog] = useState<MarketPlugin[] | null>(null);
  const [catLoading, setCatLoading] = useState(false);
  const [catErr, setCatErr] = useState("");
  const [categories, setCategories] = useState<string[]>([]);
  const [catFilter, setCatFilter] = useState("");
  const [q, setQ] = useState("");

  // npm 搜索标签
  const [query, setQuery] = useState("");
  const [searching, setSearching] = useState(false);
  const [hits, setHits] = useState<NpmSearchHit[] | null>(null);
  const [searchErr, setSearchErr] = useState("");

  // 版本浏览 + 预检
  const [pkg, setPkg] = useState<NpmPackageInfo | null>(null);
  const [selVer, setSelVer] = useState<string>("");
  const [issues, setIssues] = useState<PeerIssue[] | null>(null);
  // 市场选中的 GitHub-only 插件
  const [ghPlugin, setGhPlugin] = useState<MarketPlugin | null>(null);

  // 自定义源
  const [customSpec, setCustomSpec] = useState("");
  // 本地导入
  const [localPath, setLocalPath] = useState("");
  const [localKind, setLocalKind] = useState<"folder" | "tgz" | "">("");

  // 安装/日志
  const [installing, setInstalling] = useState(false);
  const [logs, setLogs] = useState<InstallLogLine[]>([]);
  const [installed, setInstalled] = useState(false);
  const logRef = useRef<HTMLDivElement>(null);
  const onInstalledRef = useRef(onInstalled);
  onInstalledRef.current = onInstalled;

  // 实时日志事件（仅注册一次）
  useEffect(() => {
    let unLog: (() => void) | undefined;
    let unDone: (() => void) | undefined;
    (async () => {
      unLog = await listen<InstallLogLine>("install-log", (e) => {
        setLogs((prev) => [...prev.slice(-800), e.payload]);
      });
      unDone = await listen<InstallDone>("install-done", (e) => {
        setInstalling(false);
        setInstalled(e.payload.success);
        if (e.payload.success) onInstalledRef.current();
      });
    })();
    return () => {
      unLog?.();
      unDone?.();
    };
  }, []);

  useEffect(() => {
    if (logRef.current) {
      logRef.current.scrollTop = logRef.current.scrollHeight;
    }
  }, [logs]);

  // 加载市场目录
  useEffect(() => {
    if (tab !== "market" || catalog) return;
    setCatLoading(true);
    setCatErr("");
    api
      .marketCatalog()
      .then((cat) => {
        setCatalog(cat.plugins);
        setCategories(cat.categories);
      })
      .catch((e) => setCatErr(String(e)))
      .finally(() => setCatLoading(false));
  }, [tab, catalog]);

  const doSearch = async () => {
    const q = query.trim();
    if (!q) return;
    setSearching(true);
    setSearchErr("");
    setHits(null);
    setPkg(null);
    setGhPlugin(null);
    setIssues(null);
    setSelVer("");
    setLogs([]);
    setInstalled(false);
    try {
      setHits(await api.npmSearch(q));
    } catch (e) {
      setSearchErr(String(e));
    } finally {
      setSearching(false);
    }
  };

  const openPackage = async (name: string) => {
    setPkg(null);
    setGhPlugin(null);
    setIssues(null);
    setSelVer("");
    setLogs([]);
    setInstalled(false);
    try {
      const info = await api.npmPackageInfo(name);
      setPkg(info);
      const latest = info.distTags["latest"];
      const first = latest && info.versions.some((v) => v.version === latest) ? latest : info.versions[0]?.version ?? "";
      setSelVer(first);
      if (first) {
        const issues = await api.checkCompat(name, first, env.version);
        setIssues(issues);
      }
    } catch (e) {
      setSearchErr(String(e));
    }
  };

  // 点击市场条目
  const openMarketPlugin = async (p: MarketPlugin) => {
    setPkg(null);
    setGhPlugin(null);
    setIssues(null);
    setSelVer("");
    setLogs([]);
    setInstalled(false);
    if (p.npm) {
      await openPackage(p.npm);
    } else {
      // GitHub-only：直接提供安装
      setGhPlugin(p);
    }
  };

  const pickVersion = async (ver: string) => {
    setSelVer(ver);
    setIssues(null);
    setLogs([]);
    setInstalled(false);
    if (pkg) {
      try {
        const issues = await api.checkCompat(pkg.name, ver, env.version);
        setIssues(issues);
      } catch (e) {
        setSearchErr(String(e));
      }
    }
  };

  // 当前要安装的 spec
  const spec = ghPlugin
    ? ghPlugin.install
    : pkg && selVer
      ? `${pkg.name}@${selVer}`
      : customSpec.trim();

  const doInstall = async (target?: string) => {
    const s = target ?? spec;
    if (!s) return;
    setInstalling(true);
    setInstalled(false);
    setLogs([]);
    try {
      await api.installPlugin(env.id, profile, profilesDir, s);
    } catch (e) {
      setLogs((prev) => [...prev, { line: String(e), kind: "stderr" }]);
      setInstalling(false);
    }
  };

  const doAllow = async () => {
    if (!spec) return;
    setInstalling(true);
    setLogs([]);
    try {
      await api.allowVersion(env.id, profile, profilesDir, spec);
      setLogs((prev) => [...prev, { line: `豁免命令已提交：${spec}`, kind: "stdout" }]);
    } catch (e) {
      setLogs((prev) => [...prev, { line: String(e), kind: "stderr" }]);
    } finally {
      setInstalling(false);
    }
  };

  const hasConflict = (issues ?? []).some((i) => !i.satisfied);

  // 检测 pnpm 构建白名单错误（git/本地插件 build scripts 被阻止）
  const buildErr = logs.some(
    (l) => l.line.includes("onlyBuiltDependencies") || l.line.includes("ERR_PNPM_GIT_DEP_PREPARE_NOT_ALLOWED"),
  );
  const [fixing, setFixing] = useState(false);
  const doFixAndRetry = async () => {
    setFixing(true);
    try {
      const names = extractBuildPkgNames(logs.map((l) => l.line).join("\n"));
      if (names.length === 0) {
        setLogs((prev) => [
          ...prev,
          { line: "未能从日志识别需要允许的包名，请在 profile 的 pnpm-workspace.yaml 手动添加 onlyBuiltDependencies", kind: "stderr" },
        ]);
        return;
      }
      await api.fixBuildPermit(env.id, profile, names);
      setLogs((prev) => [
        ...prev,
        { line: `已写入 onlyBuiltDependencies: ${names.join(", ")}，正在重试安装...`, kind: "stdout" },
      ]);
      await api.installPlugin(env.id, profile, profilesDir, spec);
    } catch (e) {
      setLogs((prev) => [...prev, { line: String(e), kind: "stderr" }]);
      setInstalling(false);
    } finally {
      setFixing(false);
    }
  };

  // 选择本地插件源
  const chooseFolder = async () => {
    const p = await pickFolder("选择插件源码文件夹（含 package.json）");
    if (p) {
      setLocalPath(p);
      setLocalKind("folder");
      setLogs([]);
      setInstalled(false);
    }
  };
  const chooseTgz = async () => {
    const p = await pickTgzFile();
    if (p) {
      setLocalPath(p);
      setLocalKind("tgz");
      setLogs([]);
      setInstalled(false);
    }
  };
  const doInstallLocal = async () => {
    if (!localPath) return;
    setInstalling(true);
    setInstalled(false);
    setLogs([]);
    try {
      await api.installLocalPlugin(env.id, profile, profilesDir, localPath);
    } catch (e) {
      setLogs((prev) => [...prev, { line: String(e), kind: "stderr" }]);
      setInstalling(false);
    }
  };

  // 市场过滤（本地）
  const filtered = (catalog ?? [])
    .filter((p) => (catFilter ? p.category === catFilter : true))
    .filter((p) => {
      const k = q.trim().toLowerCase();
      if (!k) return true;
      return (
        p.name.toLowerCase().includes(k) ||
        p.descriptionZh.toLowerCase().includes(k) ||
        p.descriptionEn.toLowerCase().includes(k)
      );
    })
    .sort((a, b) => (b.stars ?? 0) - (a.stars ?? 0))
    .slice(0, 200);

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal install-modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <h3>安装插件到 {profile}</h3>
          <button className="btn tiny" onClick={onClose}>
            ✕
          </button>
        </div>
        {/* 安装目标：锁定为当前选中的 环境 × profile，明确展示版本与来源 */}
        <div className="install-target">
          <span className="tag bundle">目标 {env.name}（dsh {env.version}）</span>
          <span className="tag">× {profile}</span>
          <span className="install-target-src" title="该 profile 所在的 profiles 目录">{profilesDir}</span>
        </div>

        {/* 标签页 */}
        <div className="tab-bar">
          <button className={`tab-btn ${tab === "market" ? "active" : ""}`} onClick={() => setTab("market")}>
            市场
          </button>
          <button className={`tab-btn ${tab === "search" ? "active" : ""}`} onClick={() => setTab("search")}>
            npm 搜索
          </button>
          <button className={`tab-btn ${tab === "custom" ? "active" : ""}`} onClick={() => setTab("custom")}>
            自定义源
          </button>
          <button className={`tab-btn ${tab === "local" ? "active" : ""}`} onClick={() => setTab("local")}>
            本地导入
          </button>
        </div>

        {tab === "market" && (
          <>
            <div className="row">
              <input
                className="input grow"
                placeholder="在市场中搜索（名称/描述）"
                value={q}
                onChange={(e) => setQ(e.target.value)}
              />
              <select className="input" value={catFilter} onChange={(e) => setCatFilter(e.target.value)}>
                <option value="">全部分类</option>
                {categories.map((c) => (
                  <option key={c} value={c}>
                    {c}
                  </option>
                ))}
              </select>
            </div>
            {catLoading && <div className="hint">正在拉取市场目录（约 4MB）...</div>}
            {catErr && <div className="error">{catErr}</div>}
            {!catLoading && catalog && (
              <>
                <div className="search-results market-list">
                  {filtered.length === 0 && <div className="empty">无匹配插件</div>}
                  {filtered.map((p) => (
                    <div key={p.name} className="search-hit" onClick={() => openMarketPlugin(p)}>
                      <div className="hit-name">
                        {p.name}
                        {p.githubOnly && <span className="tag tag-gh">GitHub</span>}
                        <span className="tag">{p.category}</span>
                        {p.stars != null && <span className="tag">★ {p.stars}</span>}
                        {p.downloads != null && <span className="tag">⬇ {p.downloads}</span>}
                      </div>
                      <div className="hit-desc">
                        {p.descriptionZh || p.descriptionEn || "(无描述)"}
                      </div>
                      <div className="hit-meta">{p.install}</div>
                    </div>
                  ))}
                </div>
                <div className="hit-meta dim-note">
                  共 {catalog.length} 个插件，展示前 {filtered.length} 个（按 star 排序）
                </div>
              </>
            )}
          </>
        )}

        {tab === "search" && (
          <>
            <div className="row">
              <input
                className="input grow"
                placeholder="搜索 npm 插件包，如 dshmarket / @scope/pkg"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && doSearch()}
              />
              <button className="btn primary" onClick={doSearch} disabled={searching || !query.trim()}>
                {searching ? "搜索中..." : "搜索"}
              </button>
            </div>
            {searchErr && <div className="error">{searchErr}</div>}
            {hits && !pkg && (
              <div className="search-results">
                {hits.map((h) => (
                  <div key={h.name} className="search-hit" onClick={() => openPackage(h.name)}>
                    <div className="hit-name">{h.name}</div>
                    <div className="hit-desc">{h.description || "(无描述)"}</div>
                    <div className="hit-meta">v{h.version}</div>
                  </div>
                ))}
              </div>
            )}
          </>
        )}

        {tab === "custom" && (
          <>
            <div className="row">
              <input
                className="input grow"
                placeholder="如：dshmarket / @scope/pkg@1.2.3 / github:user/repo / file:C:\插件目录"
                value={customSpec}
                onChange={(e) => setCustomSpec(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && doInstall()}
              />
              <button className="btn primary" onClick={() => doInstall()} disabled={installing || !customSpec.trim()}>
                {installing ? "执行中..." : `安装到 ${env.name} × ${profile}`}
              </button>
            </div>
            <div className="hit-meta dim-note">
              支持任意 pnpm/dsh 安装源：npm 包名、@scope/pkg@版本、github:user/repo、git+https:、file:本地路径
            </div>
          </>
        )}

        {tab === "local" && (
          <>
            <div className="row">
              <button className="btn" onClick={chooseFolder} disabled={installing}>
                📁 选择插件文件夹
              </button>
              <button className="btn" onClick={chooseTgz} disabled={installing}>
                📦 选择 .tgz 压缩包
              </button>
            </div>
            {localPath && (
              <div className="compat-box">
                <div className="compat-title">
                  <strong>{localKind === "folder" ? "插件文件夹" : "tgz 压缩包"}</strong>
                  <span className="badge badge-green">已选择</span>
                </div>
                <div className="hit-desc" style={{ wordBreak: "break-all" }}>{localPath}</div>
                <div className="hit-meta dim-note">
                  {localKind === "folder"
                    ? "将按 file: 协议复制安装到 profile；文件夹需包含 package.json"
                    : "将直接按压缩包安装；支持 npm pack / pnpm pack 产物"}
                </div>
              </div>
            )}
            <div className="row actions">
              <button className="btn primary" onClick={doInstallLocal} disabled={installing || !localPath}>
                {installing ? "执行中..." : `安装到 ${env.name} × ${profile}`}
              </button>
            </div>
          </>
        )}

        {/* 版本浏览 + 预检（npm 包） */}
        {pkg && (
          <div className="pkg-view">
            <div className="pkg-head">
              <strong>{pkg.name}</strong>
              <span className="hit-desc">{pkg.description}</span>
              <div className="tag-row">
                {Object.entries(pkg.distTags).map(([tag, ver]) => (
                  <span key={tag} className={`tag ${tag === "latest" ? "tag-latest" : ""}`}>
                    {tag}: {ver}
                  </span>
                ))}
              </div>
            </div>
            <div className="ver-list">
              {pkg.versions.map((v) => {
                const isTag = Object.values(pkg.distTags).includes(v.version);
                const peerCount = Object.keys(v.peerDependencies).length;
                return (
                  <div
                    key={v.version}
                    className={`ver-row ${v.version === selVer ? "active" : ""}`}
                    onClick={() => pickVersion(v.version)}
                  >
                    <span className="ver-no">{v.version}</span>
                    {isTag && <span className="tag tag-latest">dist-tag</span>}
                    {peerCount > 0 && <span className="tag">{peerCount} peer</span>}
                  </div>
                );
              })}
            </div>
          </div>
        )}

        {/* GitHub-only 插件 */}
        {ghPlugin && (
          <div className="compat-box">
            <div className="compat-title">
              <strong>{ghPlugin.name}</strong>
              <span className="badge badge-gh">GitHub 源</span>
            </div>
            <div className="hit-desc">{ghPlugin.descriptionZh || ghPlugin.descriptionEn}</div>
            <div className="hit-meta dim-note">
              无 npm 包，按 GitHub 仓库安装：<code>{ghPlugin.install}</code>；无法预检兼容性，由 dsh 安装时校验
            </div>
          </div>
        )}

        {/* 兼容预检 */}
        {issues && (
          <div className="compat-box">
            <div className="compat-title">
              兼容性预检（运行时 dsh {env.version}）
              {hasConflict ? (
                <span className="badge badge-red">存在不兼容，可豁免</span>
              ) : (
                <span className="badge badge-green">全部兼容</span>
              )}
            </div>
            {issues.length === 0 && <div className="hit-desc">该版本无 peer 依赖声明</div>}
            {issues.map((i) => (
              <div key={i.peer} className={`peer-row ${i.satisfied ? "ok" : "bad"}`}>
                <span className="peer-name">{i.peer}</span>
                <span className="peer-req">要求: {i.requirement}</span>
                <span className="peer-state">{i.satisfied ? "✓ 满足" : "✗ 不满足"}</span>
              </div>
            ))}
            <div className="hit-meta dim-note">
              注：预检基于包声明的 peerDependencies 与 dsh 版本近似比对，最终以 dsh 实际校验为准
            </div>
          </div>
        )}

        {/* 操作按钮 */}
        {(ghPlugin || pkg) && (
          <div className="row actions">
            <button className="btn primary" onClick={() => doInstall()} disabled={installing || !spec}>
              {installing ? "执行中..." : `安装 ${spec}`}
            </button>
            {hasConflict && (
              <button className="btn warn" onClick={doAllow} disabled={installing || !spec}>
                豁免此版本（allow-version）
              </button>
            )}
            {buildErr && (
              <button className="btn warn" onClick={doFixAndRetry} disabled={fixing || installing}>
                {fixing ? "修复中..." : "⚙ 允许构建脚本并重试"}
              </button>
            )}
          </div>
        )}
        {installed && <div className="info">安装完成 ✓ 插件列表已刷新</div>}

        {/* 日志 */}
        {logs.length > 0 && (
          <div className="log-box" ref={logRef}>
            {logs.map((l, i) => (
              <div key={i} className={`log-line ${l.kind === "stderr" ? "log-err" : ""}`}>
                {l.line}
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

/** 从 pnpm 错误日志提取需要允许构建的包名（去掉版本号） */
function extractBuildPkgNames(log: string): string[] {
  const out: string[] = [];
  const re = /The git-hosted package\s+"([\w@/.-]+)@/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(log)) !== null) {
    if (!out.includes(m[1])) out.push(m[1]);
  }
  if (out.length === 0) {
    // 兜底：onlyBuiltDependencies 示例行
    const re2 = /- "([\w@/.-]+)"/g;
    while ((m = re2.exec(log)) !== null) {
      if (!out.includes(m[1])) out.push(m[1]);
    }
  }
  return out;
}
