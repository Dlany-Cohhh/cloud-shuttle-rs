#!/usr/bin/env python3
"""端到端测试：编译「假 rclone」→ 启动原生进程 → 走完整个下载任务生命周期。

只需要本机有 rustc + Python 标准库，不需要真实网盘 / 网络 / Docker。

用法：
    python scripts/e2e.py
    python scripts/e2e.py --exe E:\\A-MyProject\\cloud-shuttle-rs\\target\\release\\cloud-shuttle-rs.exe
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PASS, FAIL = [], []


def check(name, cond, detail=""):
    (PASS if cond else FAIL).append(name)
    print(("  PASS  " if cond else "  FAIL  ") + name + (("  " + str(detail)) if detail else ""))


def api(base, action, **body):
    body["action"] = action
    req = urllib.request.Request(
        base + "/api",
        data=json.dumps(body).encode("utf-8"),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=25) as r:
            return json.loads(r.read().decode("utf-8"))
    except urllib.error.HTTPError as e:
        return json.loads(e.read().decode("utf-8"))


def wait_health(base, timeout=25):
    end = time.time() + timeout
    while time.time() < end:
        try:
            req = urllib.request.Request(
                base + "/api",
                data=b'{"action":"ping"}',
                headers={"Content-Type": "application/json"},
                method="POST",
            )
            with urllib.request.urlopen(req, timeout=2) as r:
                if r.status == 200:
                    return True
        except Exception:
            time.sleep(0.3)
    return False


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--exe", default="")
    ap.add_argument("--port", type=int, default=5601)
    args = ap.parse_args()

    exe = args.exe
    if not exe:
        candidate = "cloud-shuttle-rs.exe" if os.name == "nt" else "cloud-shuttle-rs"
        for p in [os.path.join(ROOT, "target", "release", candidate),
                  os.path.join(ROOT, "dist", "windows" if os.name == "nt" else "linux", candidate)]:
            if os.path.isfile(p):
                exe = p
                break
    if not exe or not os.path.isfile(exe):
        print("找不到可执行文件，请先 cargo build --release 或用 --exe 指定", file=sys.stderr)
        return 2
    exe = os.path.abspath(exe)
    static_dir = os.path.join(os.path.dirname(exe), "static")
    if not os.path.isdir(static_dir):
        static_dir = os.path.join(ROOT, "static")

    work = tempfile.mkdtemp(prefix="cs-rs-e2e-")
    fake_src = os.path.join(ROOT, "scripts", "fixtures", "fake_rclone.rs")
    fake = os.path.join(work, "rclone.exe" if os.name == "nt" else "rclone")
    data = os.path.join(work, "data")
    downloads = os.path.join(work, "downloads")
    os.makedirs(data, exist_ok=True)
    os.makedirs(downloads, exist_ok=True)
    print("工作目录：" + work)

    print("== 编译假 rclone ==")
    cp = subprocess.run(["rustc", "--edition", "2021", "-O", fake_src, "-o", fake],
                        capture_output=True, text=True)
    check("rustc 编译测试夹具", cp.returncode == 0, cp.stderr[-300:])
    if cp.returncode != 0:
        return 1

    base = "http://127.0.0.1:%d" % args.port
    proc = subprocess.Popen(
        [exe, "--port", str(args.port), "--data-dir", data,
         "--download-root", downloads, "--static-dir", static_dir, "--rclone-bin", fake],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    try:
        check("原生进程启动并响应", wait_health(base))
        if FAIL:
            return 1

        print("== 账号与云盘 ==")
        rem = api(base, "remotes")
        check("listremotes 解析出账号", "local" in rem.get("remotes", []), rem.get("remotes"))
        acc = api(base, "accounts")
        names = [a.get("name") for a in acc.get("accounts", [])]
        check("config dump 解析账号类型", "local" in names, names)
        check("无 type 的空壳被识别为 ghost", "ghost-old" in acc.get("ghosts", []), acc.get("ghosts"))
        d = api(base, "dirs", remote="local", path="")
        check("lsjson 映射为云盘列表", len(d.get("dirs", [])) == 1 and d["dirs"][0]["name"] == "demo.mp4", d.get("listError"))
        check("about 返回容量", api(base, "about", remotes=["local"])["accounts"][0]["total"] > 0)
        check("accttest 通过", api(base, "accttest", name="local").get("ok") is True)
        purge = api(base, "acctpurge")
        check("清理 ghost 段", purge.get("removed", 0) >= 1, purge)

        print("== 下载任务生命周期 ==")
        st = api(base, "start", remote="local", path="")
        job = st.get("job", {})
        check("start 创建任务", bool(job.get("id")), st.get("error"))
        jid = str(job.get("id", ""))
        state, last = "", None
        for _ in range(40):
            jobs = api(base, "jobs").get("jobs", [])
            last = next((j for j in jobs if str(j.get("id")) == jid), None)
            if last:
                state = last.get("state", "")
                if state in ("done", "error"):
                    break
            time.sleep(0.5)
        check("任务跑到完成状态", state == "done", "state=%s error=%s" % (state, (last or {}).get("error")))
        check("进度采样到落盘字节", (last or {}).get("bytes", 0) > 0, (last or {}).get("bytes"))
        host_dest = (last or {}).get("hostDest") or job.get("hostDest") or ""
        check("文件真的落盘", os.path.isfile(os.path.join(host_dest, "demo.mp4")), host_dest)
        detail = api(base, "jobdetail", id=jid)
        check("jobdetail 列出落盘文件", detail.get("diskCount", 0) >= 1, detail.get("diskCount"))
        check("stats 不报错", "speed" in api(base, "stats"))
        check("remove 删除任务记录", api(base, "remove", id=jid).get("ok") is True)
        check("任务记录已清空", not any(str(j.get("id")) == jid for j in api(base, "jobs").get("jobs", [])))

        print("== 更新与降级提示 ==")
        upd = api(base, "updatecheck")
        check("updatecheck 不把 Python 版当升级", upd.get("hasUpdate") is False, upd.get("error"))
        ua = api(base, "updateapply")
        check("在线替换明确拒绝", ua.get("unsupported") is True)
        check("未知高级操作有中文提示", "unsupported" in api(base, "cloudlink"))
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=8)
        except subprocess.TimeoutExpired:
            proc.kill()
        time.sleep(0.3)
        shutil.rmtree(work, ignore_errors=True)

    print("\n%d passed, %d failed" % (len(PASS), len(FAIL)))
    if FAIL:
        print("失败项：" + ", ".join(FAIL))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
