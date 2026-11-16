#!/usr/bin/env python3
"""Generate a synthetic HOME with fake Claude Code, Codex and Gemini CLI logs for the README demos.

Usage: scripts/demo/gen-demo-home.py <out-dir>

Everything is made up: fake UUIDs, example.com emails, /Users/user/... paths. Timestamps are
relative to now, so `atkusage blocks --active` always has a running block. The output dir is
created fresh; it must not exist or must be empty.
"""

import json
import random
import sys
import uuid
from datetime import datetime, timedelta, timezone
from pathlib import Path

DAYS = 24
RNG = random.Random(20261115)
NOW = datetime.now(timezone.utc).replace(microsecond=0)

PERSONAL = "00000000-0000-4000-8000-0000000000a1"
WORK = "00000000-0000-4000-8000-0000000000b2"
CODEX_ACCOUNT = "00000000-0000-4000-8000-0000000000c3"

PROJECTS = ["web-app", "api-server", "cli-tool", "docs-site"]


def fake_uuid() -> str:
    return str(uuid.UUID(int=RNG.getrandbits(128), version=4))


def iso(t: datetime) -> str:
    return t.strftime("%Y-%m-%dT%H:%M:%S.000Z")


def write_jsonl(path: Path, rows: list) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("".join(json.dumps(r, separators=(",", ":")) + "\n" for r in rows))


def session_times(day_offset: int, count: int) -> list:
    """Start times for `count` sessions on the day `day_offset` days ago (UTC, working hours)."""
    day = (NOW - timedelta(days=day_offset)).replace(hour=0, minute=0, second=0)
    starts = []
    for _ in range(count):
        t = day + timedelta(hours=RNG.randint(1, 13), minutes=RNG.randint(0, 59))
        if t < NOW - timedelta(minutes=30):
            starts.append(t)
    if day_offset == 0:
        # Keep a session inside the current 5-hour block.
        starts.append(NOW - timedelta(minutes=RNG.randint(60, 100)))
    return sorted(starts)


def claude_profile(home: Path, cfg_dir: str, json_path: Path, account: str, email: str,
                   org: str, models: list, weight: float) -> None:
    json_path.parent.mkdir(parents=True, exist_ok=True)
    json_path.write_text(json.dumps({
        "oauthAccount": {
            "accountUuid": account,
            "emailAddress": email,
            "displayName": "Example User",
            "organizationUuid": fake_uuid(),
            "organizationName": org,
        }
    }))
    for d in range(DAYS):
        if RNG.random() < 0.15 and d != 0:
            continue
        for start in session_times(d, RNG.randint(1, 3)):
            sid = fake_uuid()
            proj = RNG.choice(PROJECTS)
            cwd = f"/Users/user/{proj}"
            rows = [{"type": "session-owner", "accountUuid": account, "sessionId": sid},
                    {"type": "user", "sessionId": sid, "cwd": cwd, "timestamp": iso(start),
                     "message": {"role": "user", "content": "demo"}}]
            t = start
            for _ in range(RNG.randint(15, 45)):
                t += timedelta(seconds=RNG.randint(20, 120))
                if t > NOW:
                    break
                model = RNG.choices(models, weights=[3, 1][: len(models)])[0]
                cache_write = int(RNG.randint(500, 6000) * weight)
                rows.append({
                    "type": "assistant", "sessionId": sid, "cwd": cwd, "timestamp": iso(t),
                    "requestId": "req_" + fake_uuid().replace("-", ""), "isSidechain": False,
                    "uuid": fake_uuid(),
                    "message": {
                        "id": "msg_" + fake_uuid().replace("-", ""), "model": model,
                        "content": [],
                        "usage": {
                            "input_tokens": int(RNG.randint(5, 400) * weight),
                            "output_tokens": int(RNG.randint(200, 3000) * weight),
                            "cache_creation_input_tokens": cache_write,
                            "cache_read_input_tokens": int(RNG.randint(20000, 90000) * weight),
                            "cache_creation": {"ephemeral_1h_input_tokens": cache_write,
                                               "ephemeral_5m_input_tokens": 0},
                        },
                    },
                })
            write_jsonl(home / cfg_dir / "projects" / f"-Users-user-{proj}" / f"{sid}.jsonl", rows)


def codex(home: Path) -> None:
    for d in range(DAYS):
        if RNG.random() < 0.25 and d != 0:
            continue
        for start in session_times(d, RNG.randint(1, 2)):
            sid = fake_uuid()
            cwd = f"/Users/user/{RNG.choice(PROJECTS)}"
            rows = [{"timestamp": iso(start), "type": "session_meta",
                     "payload": {"id": sid, "timestamp": iso(start), "cwd": cwd,
                                 "cli_version": "0.99.0", "model_provider": "openai",
                                 "creator_account_id": CODEX_ACCOUNT, "source": "cli"}}]
            t = start
            used = RNG.uniform(5, 20)
            for turn in range(RNG.randint(8, 25)):
                t += timedelta(seconds=RNG.randint(30, 150))
                if t > NOW:
                    break
                model = RNG.choices(["gpt-6.1-sol", "gpt-6-luna"], weights=[4, 1])[0]
                tid = f"turn-{turn}"
                rows.append({"timestamp": iso(t), "type": "turn_context",
                             "payload": {"model": model, "cwd": cwd, "turn_id": tid}})
                inp = RNG.randint(20000, 80000)
                cached = int(inp * RNG.uniform(0.6, 0.9))
                out = RNG.randint(300, 4000)
                rows.append({"timestamp": iso(t), "type": "token_usage_record",
                             "payload": {"session_id": sid, "turn_id": tid,
                                         "response_id": "resp_" + fake_uuid(),
                                         "usage": {"input_tokens": inp, "cached_input_tokens": cached,
                                                   "output_tokens": out,
                                                   "reasoning_output_tokens": out // 3,
                                                   "total_tokens": inp + out}}})
                used = min(used + RNG.uniform(0.5, 2.5), 95)
                rows.append({"timestamp": iso(t), "type": "event_msg", "payload": {
                    "type": "token_count",
                    "rate_limits": {
                        "limit_id": "codex",
                        "primary": {"used_percent": round(used, 1), "window_minutes": 300,
                                    "resets_at": int((t + timedelta(hours=3)).timestamp())},
                        "secondary": {"used_percent": round(used * 0.45, 1), "window_minutes": 10080,
                                      "resets_at": int((NOW + timedelta(days=4)).timestamp())},
                        "plan_type": "pro"}}})
            path = (home / ".codex" / "sessions" / start.strftime("%Y/%m/%d")
                    / f"rollout-{start.strftime('%Y-%m-%dT%H-%M-%S')}-{sid}.jsonl")
            write_jsonl(path, rows)


def gemini(home: Path) -> None:
    project = "%064x" % RNG.getrandbits(256)
    for d in range(0, DAYS, 2):
        for start in session_times(d, 1):
            sid = fake_uuid()
            msgs, t = [], start
            for i in range(RNG.randint(5, 15)):
                t += timedelta(seconds=RNG.randint(20, 90))
                if t > NOW:
                    break
                msgs.append({"id": f"u{i}", "timestamp": iso(t), "type": "user", "content": "demo"})
                inp = RNG.randint(8000, 40000)
                out = RNG.randint(200, 2000)
                cached = int(inp * 0.7)
                msgs.append({"id": f"g{i}", "timestamp": iso(t), "type": "gemini",
                             "model": "gemini-3.8-flash",
                             "tokens": {"input": inp, "output": out, "cached": cached,
                                        "thoughts": out // 4, "tool": 0, "total": inp + out}})
            path = (home / ".gemini" / "tmp" / project / "chats"
                    / f"session-{start.strftime('%Y-%m-%dT%H-%M')}-{sid[:8]}.json")
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(json.dumps({"sessionId": sid, "projectHash": project,
                                        "startTime": iso(start), "lastUpdated": iso(t),
                                        "messages": msgs}))


def config(home: Path) -> None:
    path = home / ".config" / "atkusage" / "config.toml"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        "[aliases]\n"
        f'"{PERSONAL}" = "Personal"\n'
        f'"{WORK}" = "Work"\n'
        f'"{CODEX_ACCOUNT}" = "Codex Pro"\n'
    )


def main() -> None:
    if len(sys.argv) != 2:
        sys.exit("usage: gen-demo-home.py <out-dir>")
    home = Path(sys.argv[1])
    if home.exists() and any(home.iterdir()):
        sys.exit(f"{home} is not empty")
    home.mkdir(parents=True, exist_ok=True)
    claude_profile(home, ".claude", home / ".claude.json", PERSONAL, "user@example.com",
                   "Personal", ["claude-opus-5-5", "claude-haiku-5-5"], 1.0)
    claude_profile(home, ".claude-work", home / ".claude-work" / ".claude.json", WORK,
                   "work@example.com", "Example Org", ["claude-sonnet-5-5", "claude-haiku-5-5"], 0.8)
    codex(home)
    gemini(home)
    config(home)


if __name__ == "__main__":
    main()
