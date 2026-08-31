#!/usr/bin/env python3
"""PR #3 一键更新: PATCH body 追加段落 / 检查状态.

用法:
  GITHUB_TOKEN=ghp_xxx python3 scripts/nas/pr_update.py check    # 查看当前 body 尾部
  GITHUB_TOKEN=ghp_xxx python3 scripts/nas/pr_update.py append pr_body_a6prep.md
凭据: 沙盒重建会丢 token — 每次会话开始若 push 401, 向用户要一次 PAT.
"""
import json, os, sys, urllib.request

REPO = "tomjiu/smart-downloader"
PR = 3
API = f"https://api.github.com/repos/{REPO}/pulls/{PR}"


def api(method, data=None, token=None):
    req = urllib.request.Request(API, method=method, data=data)
    req.add_header("Accept", "application/vnd.github+json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    with urllib.request.urlopen(req) as r:
        return json.loads(r.read())


def main():
    cmd = sys.argv[1] if len(sys.argv) > 1 else "check"
    token = os.environ.get("GITHUB_TOKEN", "")
    if cmd == "check":
        d = api("GET", token=token) if token else json.loads(
            urllib.request.urlopen(urllib.request.Request(API)).read())
        body = d.get("body", "")
        print(f"state={d.get('state')} head={d.get('head',{}).get('label')} "
              f"updated={d.get('updated_at')} body_len={len(body)}")
        print("--- tail 1200 ---")
        print(body[-1200:])
        return
    if cmd == "append":
        if not token:
            sys.exit("[!] 需要 GITHUB_TOKEN 环境变量")
        section_file = sys.argv[2]
        section = open(section_file, encoding="utf-8").read()
        d = api("GET", token=token)
        body = d["body"]
        if section.strip().splitlines()[0].strip() in body:
            print("[=] 段落已在 body 中, 跳过")
            return
        new_body = body.rstrip() + "\n\n\n" + section.strip() + "\n"
        payload = json.dumps({"body": new_body}).encode()
        api("PATCH", data=payload, token=token)
        print(f"[+] PR #{PR} body 追加完成, 新长度 {len(new_body)}")
        return
    print(__doc__)


if __name__ == "__main__":
    main()
