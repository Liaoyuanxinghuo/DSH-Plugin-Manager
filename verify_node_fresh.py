# -*- coding: utf-8 -*-
# 模拟"从零装 Node"全链路：官方源 + npmmirror 镜像
import json, os, re, shutil, subprocess, sys, tempfile, urllib.request, zipfile

def get(url, timeout=60):
    req = urllib.request.Request(url, headers={"User-Agent": "DSH-Manager/0.3.0"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return r.read()

def latest_lts(base):
    body = json.loads(get(f"{base}/index.json").decode("utf-8"))
    lts = [e for e in body if e.get("lts") and not e["version"].startswith("v0.")]
    def key(v):
        return [int(x) for x in re.findall(r"\d+", v)]
    lts.sort(key=lambda e: key(e["version"]), reverse=True)
    return lts[0]["version"]

results = {}
for tag, base in [("官方", "https://nodejs.org/dist"), ("npmmirror", "https://registry.npmmirror.com/-/binary/node")]:
    try:
        ver = latest_lts(base)
        results[tag] = ("OK", ver)
        print(f"[{tag}] index.json 可达，最新 LTS = {ver}")
    except Exception as e:
        results[tag] = ("FAIL", str(e)[:120])
        print(f"[{tag}] 失败: {e}")

# 用 npmmirror 的 LTS 完整下载验证（中国网络主场景）
if results.get("npmmirror", ("",))[0] == "OK":
    ver = results["npmmirror"][1]
    base = "https://registry.npmmirror.com/-/binary/node"
    zip_url = f"{base}/{ver}/node-{ver}-win-x64.zip"
    print(f"\n下载 {zip_url} …")
    tmp = os.path.join(tempfile.gettempdir(), f"node-{ver}-win-x64.zip")
    data = get(zip_url, timeout=300)
    open(tmp, "wb").write(data)
    print(f"下载完成 {len(data)/1024/1024:.1f} MB，解压检查…")
    dest = os.path.join(tempfile.gettempdir(), f"node-{ver}-win-x64")
    if os.path.exists(dest):
        shutil.rmtree(dest)
    os.makedirs(dest)
    with zipfile.ZipFile(tmp) as z:
        names = z.namelist()
        for n in names:
            rel = n.split("/", 1)[1] if "/" in n else ""
            if not rel:
                continue
            out = os.path.join(dest, rel.replace("/", os.sep))
            if n.endswith("/"):
                os.makedirs(out, exist_ok=True)
            else:
                os.makedirs(os.path.dirname(out), exist_ok=True)
                with z.open(n) as src, open(out, "wb") as dst:
                    shutil.copyfileobj(src, dst)
    node_exe = os.path.join(dest, "node.exe")
    npm_cmd = os.path.join(dest, "npm.cmd")
    print(f"node.exe 存在: {os.path.exists(node_exe)} | npm.cmd 存在: {os.path.exists(npm_cmd)}")
    if os.path.exists(node_exe):
        v = subprocess.run([node_exe, "-v"], capture_output=True, text=True, timeout=30)
        print(f"node -v => {v.stdout.strip()}（退出码 {v.returncode}）")
    # 装 pnpm（真实 npm install -g，到临时 node 目录）
    if os.path.exists(npm_cmd):
        print("\n安装 pnpm（npm install -g pnpm --registry npmmirror）…")
        env = dict(os.environ, PATH=dest + ";" + os.environ.get("PATH", ""))
        p = subprocess.run(
            [npm_cmd, "install", "-g", "pnpm", "--registry", "https://registry.npmmirror.com", "--no-audit", "--no-fund"],
            capture_output=True, text=True, timeout=300, env=env, cwd=dest,
        )
        pnpm = os.path.join(dest, "pnpm.cmd")
        print(f"pnpm.cmd 存在: {os.path.exists(pnpm)} | npm 退出码 {p.returncode}")
        if p.returncode != 0:
            print("  npm stderr:", p.stderr[-300:])
    # 清理临时 zip
    os.remove(tmp)
    print("\n临时文件已清理")
else:
    print("\nnpmmirror 不可达，未做下载验证")
