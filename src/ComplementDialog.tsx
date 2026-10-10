// 插件补全对话框：把「源」中有、当前 profile 没有的插件补齐。
// 三源：A=已有 profile（文件级复制，不断网、保留精确版本）
//       B=在线整合包市场（只 peek manifest 差集，缺失项逐个联网安装）
//       C=本地 .dspack 文件（同 B，不新建 profile）
// 差集规则：包名精确匹配，同名无论版本异同都跳过；基础包永不参与。
import { useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, pickDspackFile } from "./api";
import type {
  ComplementCopyItem,
  DshEnv,
  InstallDone,
  InstallLogLine,
  PackMarketEntry,
  PackPeekResult,
  ProfileInfo,
} from "./types";

interface Props {
  env: DshEnv;
  profile: string;
  profilesDir: string;
  profiles: ProfileInfo[];
  currentPlugins: string[];
  onClose: () => void;
  onDone: () => void;
}

/** 基础组合包：永不参与补全 */
export const COMPLEMENT_INBOX = new Set([
  "@deepseek-ai/dsh-base",
  "@deepseek-ai/dsh-web-app",
  "@deepseek-ai/dsh-headless",
]);

type SourceKind = "profile" | "market" | "local";

interface DiffItem {
  name: string;
  spec: string;
  isBundle: boolean;
}

export interface ComplementDiff {
  missing: DiffItem[];
  skippedSame: number;
  skippedInbox: number;
}

export function diffAgainstCurrent(
  deps: Record<string, string>,
  bundles: string[],
  current: Set<string>,
): ComplementDiff {
  const missing: DiffItem[] = [];
  let skippedSame = 0;
  let skippedInbox = 0;
  for (const [name, spec] of Object.entries(deps ?? {})) {
    if (COMPLEMENT_INBOX.has(name)) {
      skippedInbox += 1;
      continue;
    }
    // 包名精确匹配：同名无论版本异同都跳过
    if (current.has(name)) {
      skippedSame += 1;
      continue;
    }
    missing.push({ name, spec, isBundle: bundles?.includes(name) ?? false });
  }
  missing.sort((a, b) => a.name.localeCompare(b.name));
  return { missing, skippedSame, skippedInbox };
}

export default function ComplementDialog({ env, profile, profilesDir, profiles, currentPlugins, onClose, onDone }: Props) {
  const [source, setSource] = useState<SourceKind>("profile");
  const current = useMemo(() => new Set(currentPlugins), [currentPlugins]);

  // A: 已有 profile
  const candidates = useMemo(
    () => profiles.filter((p) => !(p.name === profile && p.profilesDir === profilesDir)),
    [profiles, profile, profilesDir],
  );
  const [srcKey, setSrcKey] = useState<string>("");
  const srcProfile = useMemo(
    () => candidates.find((p) => `${p.name}::${p.profilesDir}` === srcKey) ?? null,
    [candidates, srcKey],
  );
  const [srcLoading, setSrcLoading] = useState(false);
  const [srcErr, setSrcErr] = useState("");
  const [srcDeps, setSrcDeps] = useState<Record<string, string>>({});
  const [srcBundles, setSrcBundles] = useState<string[]>([]);

  // B: 在线整合包市场
  const [packs, setPacks] = useState<PackMarketEntry[] | null>(null);
  const [packsLoading, setPacksLoading] = useState(false);
  const [packsErr, setPacksErr] = useState("");
  const [packId, setPackId] = useState("");

  // B+C 共用：peek 结果
  const [peek, setPeek] = useState<PackPeekResult | null>(null);
  const [peekUnit, setPeekUnit] = useState("");
  const [peekLoading, setPeekLoading] = useState(false);
  const [peekErr, setPeekErr] = useState("");
  const [localPath, setLocalPath] = useState("");

  // 执行
  const [checked, setChecked] = useState<Set<string>>(new Set());
  const [running, setRunning] = useState(false);
  const [logs, setLogs] = useState<InstallLogLine[]>([]);
  const [results, setResults] = useState<{ name: string; ok: boolean; detail: string }[]>([]);
  const logRef = useRef<HTMLDivElement>(null);
  const onDoneRef = useRef(onDone);
  onDoneRef.current = onDone;

  useEffect(() => {
    // 三源共用：文件复制不走日志流，但在线/本地包的逐个联网安装走 install-log/install-done
    let unLog: (() => void) | undefined;
    let unDone: (() => void) | undefined;
    (async () => {
      unLog = await listen<InstallLogLine>("install-log", (e) => {
        setLogs((prev) => [...prev.slice(-400), e.payload]);
      });
      unDone = await listen<InstallDone>("install-done", (e) => {
        if (e.payload.success) onDoneRef.current();
      });
    })();
    return () => {
      unLog?.();
      unDone?.();
    };
  }, []);

  useEffect(() => {
    if (logRef.current) logRef.current.scrollTop = logRef.current.scrollHeight;
  }, [logs]);

  // 切换来源：清掉旧差集与结果
  const switchSource = (s: SourceKind) => {
    if (s === source) return;
    setSource(s);
    setSrcDeps({});
    setSrcBundles([]);
    setSrcErr("");
    setPeek(null);
    setPeekUnit("");
    setPeekErr("");
    setChecked(new Set());
    setResults([]);
    setLogs([]);
  };

  // A: 拉取源 profile 清单
  const loadSrcManifest = async (key: string) => {
    setSrcKey(key);
    setChecked(new Set());
    setResults([]);
    setSrcDeps({});
    setSrcBundles([]);
    if (!key) return;
    const hit = candidates.find((p) => `${p.name}::${p.profilesDir}` === key);
    if (!hit) return;
    setSrcLoading(true);
    setSrcErr("");
    try {
      const m = await api.complementManifest(env.id, hit.name, hit.profilesDir);
      setSrcDeps(m.dependencies ?? {});
      setSrcBundles(m.bundles ?? []);
      setChecked(new Set(Object.keys(m.dependencies ?? {}).filter((n) => !current.has(n) && !COMPLEMENT_INBOX.has(n))));
    } catch (e) {
      setSrcErr(String(e));
    } finally {
      setSrcLoading(false);
    }
  };

  // B: 拉取在线市场
  useEffect(() => {
    if (source !== "market" || packs) return;
    setPacksLoading(true);
    setPacksErr("");
    api
      .marketPacks()
      .then((list) => setPacks(list ?? []))
      .catch((e) => setPacksErr(String(e)))
      .finally(() => setPacksLoading(false));
  }, [source, packs]);

  const peekPackPath = async (path: string) => {
    setPeekLoading(true);
    setPeekErr("");
    setPeek(null);
    setPeekUnit("");
    setChecked(new Set());
    setResults([]);
    try {
      const r = await api.peekPack(path);
      setPeek(r);
      const first = r.units[0]?.key ?? "";
      setPeekUnit(first);
      if (first) {
        const u = r.units[0];
        setChecked(new Set(Object.keys(u.dependencies ?? {}).filter((n) => !current.has(n) && !COMPLEMENT_INBOX.has(n))));
      }
    } catch (e) {
      setPeekErr(String(e));
    } finally {
      setPeekLoading(false);
    }
  };

  // B: 下载在线包后 peek
  const peekMarketPack = async (id: string) => {
    setPackId(id);
    const hit = packs?.find((p) => p.id === id);
    if (!hit) return;
    setPeekLoading(true);
    setPeekErr("");
    setPeek(null);
    setChecked(new Set());
    setResults([]);
    try {
      const local = await api.downloadPack(hit.downloadUrl, hit.sha256, hit.size);
      await peekPackPath(local);
    } catch (e) {
      setPeekErr(String(e));
    } finally {
      setPeekLoading(false);
    }
  };

  // C: 本地 .dspack
  const chooseLocalPack = async () => {
    const p = await pickDspackFile();
    if (!p) return;
    setLocalPath(p);
    await peekPackPath(p);
  };

  // 当前差集（A 用 srcDeps，B/C 用 peek 选中单元）
  const diff = useMemo(() => {
    if (source === "profile") return diffAgainstCurrent(srcDeps, srcBundles, current);
    const u = peek?.units.find((x) => x.key === peekUnit);
    if (!u) return { missing: [] as DiffItem[], skippedSame: 0, skippedInbox: 0 };
    return diffAgainstCurrent(u.dependencies, u.bundles, current);
  }, [source, srcDeps, srcBundles, peek, peekUnit, current]);

  const toggle = (name: string) => {
    setChecked((prev) => {
      const next = new Set(prev);
      if (next.has(name)) next.delete(name);
      else next.add(name);
      return next;
    });
  };
  const toggleAll = () => {
    setChecked((prev) =>
      prev.size === diff.missing.length ? new Set() : new Set(diff.missing.map((d) => d.name)),
    );
  };

  // A: 文件级复制
  const doCopy = async () => {
    if (!srcProfile || checked.size === 0 || running) return;
    setRunning(true);
    setResults([]);
    try {
      const r: ComplementCopyItem[] = await api.complementCopy(
        env.id,
        srcProfile.name,
        srcProfile.profilesDir,
        profile,
        profilesDir,
        [...checked],
      );
      setResults(
        r.map((x) => ({
          name: x.name,
          ok: x.ok,
          detail: x.ok ? "已复制（含实体+清单）" : x.reason || (x.skipped ? "已跳过" : "失败"),
        })),
      );
      if (r.some((x) => x.ok)) onDoneRef.current();
      // 复制后刷新源差集：已补的从缺失里消失
      const done = new Set(r.filter((x) => x.ok).map((x) => x.name));
      if (done.size > 0) {
        setChecked((prev) => new Set([...prev].filter((n) => !done.has(n))));
      }
    } catch (e) {
      setSrcErr(String(e));
    } finally {
      setRunning(false);
    }
  };

  // B/C: 逐个联网安装（spec 取 peek 单元的精确值）
  const doInstallFromPack = async () => {
    const u = peek?.units.find((x) => x.key === peekUnit);
    if (!u || checked.size === 0 || running) return;
    setRunning(true);
    setResults([]);
    setLogs([]);
    const items = diff.missing.filter((d) => checked.has(d.name));
    const acc: { name: string; ok: boolean; detail: string }[] = [];
    for (const it of items) {
      const spec = u.dependencies[it.name]?.trim();
      if (!spec) {
        acc.push({ name: it.name, ok: false, detail: "清单无版本，跳过" });
        setResults([...acc]);
        continue;
      }
      setLogs((prev) => [...prev, { line: `── 安装 ${it.name}@${spec}`, kind: "stdout" }]);
      try {
        const r = await api.installPlugin(env.id, profile, profilesDir, `${it.name}@${spec}`);
        acc.push({ name: it.name, ok: r.success, detail: r.success ? `已安装 ${it.name}@${spec}` : r.summary });
      } catch (e) {
        acc.push({ name: it.name, ok: false, detail: String(e) });
      }
      setResults([...acc]);
    }
    onDoneRef.current();
    setRunning(false);
  };

  const okCount = results.filter((r) => r.ok).length;

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal install-modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <h3>插件补全到 {profile}</h3>
          <button className="btn tiny" onClick={onClose}>✕</button>
        </div>
        <div className="install-target">
          <span className="tag bundle">目标 {env.name}（dsh {env.version}）</span>
          <span className="tag">× {profile}</span>
          <span className="install-target-src" title="该 profile 所在的 profiles 目录">{profilesDir}</span>
        </div>
        <div className="hit-meta dim-note">
          只补「源有、当前没有」的插件；同名插件无论版本异同都不处理；基础包（dsh-base/web-app/headless）永不参与。
        </div>

        <div className="tab-bar">
          <button className={`tab-btn ${source === "profile" ? "active" : ""}`} onClick={() => switchSource("profile")}>
            已有 profile
          </button>
          <button className={`tab-btn ${source === "market" ? "active" : ""}`} onClick={() => switchSource("market")}>
            在线整合包市场
          </button>
          <button className={`tab-btn ${source === "local" ? "active" : ""}`} onClick={() => switchSource("local")}>
            本地整合包
          </button>
        </div>

        {source === "profile" && (
          <>
            <div className="row">
              <select className="input grow" value={srcKey} onChange={(e) => loadSrcManifest(e.target.value)}>
                <option value="">选择源 profile（已排除当前）</option>
                {candidates.map((p) => (
                  <option key={`${p.name}::${p.profilesDir}`} value={`${p.name}::${p.profilesDir}`}>
                    {p.name}（{p.pluginCount} 插件 · {p.profilesDir}）
                  </option>
                ))}
              </select>
            </div>
            {srcLoading && <div className="hint">正在读取源清单…</div>}
            {srcErr && <div className="error">{srcErr}</div>}
            {srcProfile && (
              <div className="hit-meta dim-note">
                源 {srcProfile.name}：共 {Object.keys(srcDeps).length} 个插件声明，
                可补 {diff.missing.length} 个（同名跳过 {diff.skippedSame}，基础包排除 {diff.skippedInbox}）。
                复制为文件级复制：拷 node_modules 实体 + 合并清单，不断网、保留源精确版本。
              </div>
            )}
          </>
        )}

        {source === "market" && (
          <>
            {packsLoading && <div className="hint">正在拉取整合包市场…</div>}
            {packsErr && <div className="error">{packsErr}</div>}
            {packs && (
              <div className="row">
                <select className="input grow" value={packId} onChange={(e) => peekMarketPack(e.target.value)}>
                  <option value="">选择在线整合包（下载后只读清单，不新建 profile）</option>
                  {packs.map((p) => (
                    <option key={p.id} value={p.id}>
                      {p.displayName || p.name} v{p.version}{p.packType === "dshhome" ? "（多 profile）" : ""}{p.dshVersion ? ` · dsh ${p.dshVersion}` : ""}
                    </option>
                  ))}
                </select>
              </div>
            )}
            {peekLoading && <div className="hint">正在下载并解析清单…</div>}
            {peekErr && <div className="error">{peekErr}</div>}
          </>
        )}

        {source === "local" && (
          <>
            <div className="row">
              <button className="btn" onClick={chooseLocalPack} disabled={peekLoading}>
                📦 选择本地 .dspack
              </button>
              {localPath && <span className="hit-meta" style={{ wordBreak: "break-all" }}>{localPath}</span>}
            </div>
            {peekLoading && <div className="hint">正在解析清单…</div>}
            {peekErr && <div className="error">{peekErr}</div>}
          </>
        )}

        {/* B/C：多 profile 包选单元 */}
        {source !== "profile" && peek && peek.units.length > 1 && (
          <div className="row">
            <select className="input grow" value={peekUnit} onChange={(e) => {
              setPeekUnit(e.target.value);
              const u = peek.units.find((x) => x.key === e.target.value);
              if (u) setChecked(new Set(Object.keys(u.dependencies ?? {}).filter((n) => !current.has(n) && !COMPLEMENT_INBOX.has(n))));
              setResults([]);
            }}>
              {peek.units.map((u) => (
                <option key={u.key} value={u.key}>
                  {u.key}（{Object.keys(u.dependencies ?? {}).length} 插件）
                </option>
              ))}
            </select>
          </div>
        )}
        {source !== "profile" && peek && (
          <div className="hit-meta dim-note">
            {peek.packName} v{peek.packVersion} · {peek.units.length} 个单元
            {peek.dshHint ? ` · 适配 DSH ${peek.dshHint}` : ""}：
            当前单元可补 {diff.missing.length} 个（同名跳过 {diff.skippedSame}，基础包排除 {diff.skippedInbox}）。
            执行为逐个联网安装（取包内精确 spec），单项失败跳过继续。
          </div>
        )}

        {/* 差集列表 */}
        {((source === "profile" && srcProfile) || (source !== "profile" && peek)) && (
          <>
            {diff.missing.length === 0 ? (
              <div className="empty">无可补插件（源里缺失项为 0，同名与基础包已按规则排除）</div>
            ) : (
              <div className="search-results" style={{ maxHeight: 240 }}>
                <div className="search-hit" onClick={toggleAll} style={{ cursor: "pointer" }}>
                  <div className="hit-name">
                    <input type="checkbox" checked={checked.size === diff.missing.length && diff.missing.length > 0} readOnly />
                    <span style={{ marginLeft: 6 }}>全选 / 取消（已选 {checked.size}/{diff.missing.length}）</span>
                  </div>
                </div>
                {diff.missing.map((d) => (
                  <div key={d.name} className="search-hit" onClick={() => toggle(d.name)} style={{ cursor: "pointer" }}>
                    <div className="hit-name">
                      <input type="checkbox" checked={checked.has(d.name)} readOnly />
                      <span style={{ marginLeft: 6 }}>{d.name}</span>
                      {d.isBundle && <span className="tag bundle">bundle</span>}
                    </div>
                    <div className="hit-meta">{d.spec || "(无版本声明)"}</div>
                  </div>
                ))}
              </div>
            )}
            <div className="row actions">
              {source === "profile" ? (
                <button className="btn primary" onClick={doCopy} disabled={running || checked.size === 0 || !srcProfile}>
                  {running ? "复制中..." : `复制补全 ${checked.size} 个（文件级，不联网）`}
                </button>
              ) : (
                <button className="btn primary" onClick={doInstallFromPack} disabled={running || checked.size === 0 || !peek}>
                  {running ? "安装中..." : `联网安装 ${checked.size} 个（逐个，失败跳过）`}
                </button>
              )}
            </div>
          </>
        )}

        {results.length > 0 && (
          <div className="compat-box">
            <div className="compat-title">
              执行结果（成功 {okCount}/{results.length}）
              {okCount > 0 ? <span className="badge badge-green">部分/全部成功</span> : <span className="badge badge-red">全部失败</span>}
            </div>
            {results.map((r) => (
              <div key={r.name} className={`peer-row ${r.ok ? "ok" : "bad"}`}>
                <span className="peer-name">{r.name}</span>
                <span className="peer-state">{r.ok ? "✓ 成功" : "✗ 失败"}</span>
              </div>
            ))}
            {results.map((r) => (
              !r.ok && <div key={`${r.name}-reason`} className="hit-desc">{r.name}：{r.detail}</div>
            ))}
          </div>
        )}

        {logs.length > 0 && (
          <div className="log-box" ref={logRef}>
            {logs.map((l, i) => (
              <div key={i} className={`log-line ${l.kind === "stderr" ? "log-err" : ""}`}>{l.line}</div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
