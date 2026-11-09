#!/usr/bin/env python3
"""对运行中的 cloud-shuttle-rs 做接口冒烟测试（仅标准库，Windows / Linux 通用）。

用法：
    python scripts/smoke.py --url http://127.0.0.1:5566
    python scripts/smoke.py --url http://127.0.0.1:5566 --root D:\\Downloads

会在下载根目录里临时创建一个文件并改名，测试结束自动清理。
"""
import argparse
import json
import os
import re
import sys
import urllib.error
import urllib.request

PASS, FAIL = [], []


def check(name, cond, detail=""):
    (PASS if cond else FAIL).append(name)
    print(("  PASS  " if cond else "  FAIL  ") + name + (("  " + str(detail)) if detail else ""))


def http(url, data=None, headers=None):
    req = urllib.request.Request(
        url,
        data=None if data is None else json.dumps(data).encode("utf-8"),
        headers=headers or {},
        method="GET" if data is None else "POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=20) as r:
            return r.status, dict(r.headers), r.read()
    except urllib.error.HTTPError as e:
        return e.code, dict(e.headers), e.read()


def api(base, action, **body):
    body["action"] = action
    st, hd, raw = http(base + "/api", body, {"Content-Type": "application/json"})
    return st, json.loads(raw.decode("utf-8"))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", default="http://127.0.0.1:5566")
    ap.add_argument("--root", default="", help="下载根目录；不填则从 health 接口读取")
    args = ap.parse_args()
    base = args.url.rstrip("/")

    print("== 静态资源 ==")
    st, hd, body = http(base + "/")
    check("GET / 返回 200", st == 200, st)
    check("首页含控制台标题", b"PikPak" in body or "云梭".encode() in body)
    st, _, _ = http(base + "/app.js")
    check("GET /app.js 返回 200", st == 200, st)

    print("== 基础接口 ==")
    st, v = api(base, "jobs")
    check("jobs 含 jobs/settings/effectiveProxyUrl",
          all(k in v for k in ("jobs", "settings", "effectiveProxyUrl")), list(v.keys()))
    check("响应带 runtime=rust", v.get("runtime") == "rust")
    st, ver = api(base, "version")
    check("version 有原生版本号",
          bool(re.match(r"^(rs-)?v\d", str(ver.get("consoleVersion", "")))),
          ver.get("consoleVersion"))
    check("version 标识平台", ver.get("os") in ("windows", "linux", "macos"), ver.get("os"))
    st, h = api(base, "health")
    check("health 正常", "error" not in h and h.get("engine") == "rust")
    st, s = api(base, "settings")
    check("settings 返回完整设置", isinstance(s.get("settings"), dict) and "maxConcurrent" in s["settings"])
    st, stt = api(base, "stats")
    check("stats 有 speed/bytes", "speed" in stt and "bytes" in stt)
    st, p = api(base, "ping")
    check("ping ok", p.get("ok") is True)
    st, u = api(base, "some_future_action_xyz")
    check("未知 action 明确返回 unsupported", u.get("unsupported") is True)

    print("== 设置读写与代理回显 ==")
    old_proxy = s["settings"].get("proxyUrl", "")
    _, saved = api(base, "savesettings", proxyUrl="socks5://127.0.0.1:1080")
    check("保存自定义代理地址", saved.get("ok") is True and saved["settings"]["proxyUrl"] == "socks5://127.0.0.1:1080")
    check("生效地址回显", saved.get("effectiveProxyUrl") == "socks5://127.0.0.1:1080", saved.get("effectiveProxyUrl"))
    _, off = api(base, "savesettings", proxyUrl="off")
    check("off 关闭代理", "直连" in str(off.get("effectiveProxyUrl", "")), off.get("effectiveProxyUrl"))
    api(base, "savesettings", proxyUrl=old_proxy)

    print("== 本地文件与重命名 ==")
    root = args.root or h.get("downloadRoot", "")
    if not root or not os.path.isdir(root):
        check("下载根目录可用（跳过后续文件测试）", False, repr(root))
    else:
        src = "smoke_%d.txt" % os.getpid()
        dst = "smoke_renamed_%d.txt" % os.getpid()
        p1, p2 = os.path.join(root, src), os.path.join(root, dst)
        try:
            with open(p1, "w", encoding="utf-8") as f:
                f.write("cloud-shuttle-rs smoke test\n")
            _, lf = api(base, "localfiles", path="")
            check("localfiles 返回 dirs/files", "dirs" in lf and "files" in lf)
            _, rr = api(base, "localrenamebatch", path="", items=[{"from": src, "to": dst}])
            check("单项改名成功", rr.get("ok") == 1, rr)
            check("改名后文件存在", os.path.isfile(p2))
            _, bad = api(base, "localrenamebatch", path="", items=[{"from": "../evil", "to": dst}])
            check("拒绝非法名称", bad.get("ok") == 0 and len(bad.get("failed", [])) == 1, bad)
            _, bad2 = api(base, "localfiles", path="../../etc")
            check("拒绝路径越界", "error" in bad2, bad2.get("error"))
            st, hd, body = http(base + "/media/" + dst, headers={"Range": "bytes=0-4"})
            check("Range 播放返回 206", st == 206, st)
            check("Range 内容正确", body == b"cloud", body[:10])
        finally:
            for p in (p1, p2):
                if os.path.isfile(p):
                    os.remove(p)

    print("== 账号/云盘（rclone 未配置时允许返回空或错误） ==")
    _, rem = api(base, "remotes")
    check("remotes 返回列表字段", isinstance(rem.get("remotes"), list))
    _, acc = api(base, "accounts")
    check("accounts 返回列表字段", isinstance(acc.get("accounts"), list))
    _, upd = api(base, "updatestate")
    check("updatestate 标识 rust", upd.get("runtime") == "rust")

    print("\n%d passed, %d failed" % (len(PASS), len(FAIL)))
    if FAIL:
        print("失败项：" + ", ".join(FAIL))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
