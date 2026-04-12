#!/usr/bin/env python3
"""Run a single SWE-bench issue with full logs + harness validation.
Usage: python3 benchmark/run_single.py 29
       python3 benchmark/run_single.py 29 --no-harness
"""
import json, subprocess, sys, time, shutil
from pathlib import Path

UPPLI = "/Users/sayahfarid/uppli-code/claurst/src-rust/target/release/uppli-code"
REPOS = Path("/Users/sayahfarid/uppli-code/claurst/benchmark/repos")
PREDS = Path("/Users/sayahfarid/uppli-code/claurst/benchmark/predictions")
REPORTS = Path("/Users/sayahfarid/uppli-code/claurst/benchmark/reports")

skip_harness = "--no-harness" in sys.argv
idx = int(sys.argv[1])
LOCAL_DATASET = Path(__file__).parent / "swebench_verified.json"
if LOCAL_DATASET.exists():
    issues = json.loads(LOCAL_DATASET.read_text())
else:
    from datasets import load_dataset
    issues = list(load_dataset("princeton-nlp/SWE-bench_Verified", split="test"))
issue = issues[idx]
iid = issue["instance_id"]
repo = issue["repo"]
base = issue["base_commit"]

print(f"Issue #{idx}: {iid}")
print(f"Repo: {repo} @ {base[:12]}")
print(f"Problem: {issue['problem_statement'][:200]}\n")

# Setup worktree
main = REPOS / repo.replace("/", "_")
if not main.exists():
    print(f"Cloning {repo}...")
    subprocess.run(["git", "clone", f"https://github.com/{repo}.git", str(main)], capture_output=True, timeout=600)

work = REPOS / f"work_{iid}"
if work.exists():
    subprocess.run(["git", "worktree", "remove", "--force", str(work)], cwd=main, capture_output=True)
    shutil.rmtree(work, ignore_errors=True)

subprocess.run(["git", "worktree", "prune"], cwd=main, capture_output=True)
r = subprocess.run(["git", "worktree", "add", "--detach", str(work), base], cwd=main, capture_output=True, text=True)
if r.returncode != 0:
    print(f"WORKTREE FAILED: {r.stderr[:200]}")
    sys.exit(1)

print(f"Worktree: {work}\n")

# Run agent
prompt = f"You are an expert software engineer. Fix the following bug in the repository at {work}.\n\n{issue['problem_statement']}\n\nDo not modify any existing tests. You can use any tools available."

reqs = [
    {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}},
    {"jsonrpc": "2.0", "id": 2, "method": "initialized", "params": {}},
    {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
        "name": "uppli_query",
        "arguments": {"prompt": prompt, "max_turns": 250, "working_dir": str(work)}
    }}
]

print("Running agent...")
start = time.time()
proc = subprocess.Popen(
    [UPPLI, "--mcp-server", "--provider", "alibaba", "--permission-mode", "bypass-permissions"],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True
)
stdout, stderr = proc.communicate(input="\n".join(json.dumps(r) for r in reqs) + "\n", timeout=3600)
elapsed = time.time() - start

tools = 0
errors = 0
for line in stdout.strip().split("\n"):
    if not line.strip(): continue
    try: d = json.loads(line)
    except: continue
    if d.get("method") == "notifications/progress":
        p = d["params"]
        e = p.get("event", "")
        if e == "tool_start":
            tools += 1
            print(f"  [{p.get('tool')}] {p.get('input_preview','')[:150]}")
        elif e == "tool_end":
            errors += int(bool(p.get("is_error")))
            err = " ERR" if p.get("is_error") else ""
            print(f"    ->{err} {p.get('result_preview','')[:100]}")
        elif e == "text_delta":
            sys.stdout.write(p.get("text", ""))
        elif e == "status":
            print(f"  [{p.get('message')}]")

# Get diff
diff = subprocess.run(["git", "diff"], cwd=work, capture_output=True, text=True).stdout
if not diff.strip():
    diff = subprocess.run(["git", "diff", f"{base}..HEAD"], cwd=work, capture_output=True, text=True).stdout

print(f"\n{'='*60}")
print(f"Agent: {elapsed:.0f}s | {tools} tools | {errors} errors | {'DIFF' if diff.strip() else 'NO DIFF'}")

if not diff.strip():
    print("NO DIFF — agent did not produce a patch")
    with open(REPOS / "../results.md", "a") as f:
        f.write(f"| {idx} | {iid} | ❌ | {elapsed:.0f}s | {tools} | {errors} | — | no diff |\n")
    sys.exit(1)

# Save prediction
PREDS.mkdir(parents=True, exist_ok=True)
pred = PREDS / f"{iid}.jsonl"
pred.write_text(json.dumps({"instance_id": iid, "model_patch": diff, "model_name_or_path": "uppli-code"}) + "\n")

# Quick sanity check: did the agent modify test files? (not allowed)
import re
test_modified = any(re.search(r'^\+\+\+.*test', l) for l in diff.split('\n') if l.startswith('+++'))
if test_modified:
    print("⚠️  WARNING: Agent modified test files!")

# Launch harness in background (non-blocking, with corrected report path)
print(f"\nLaunching harness in background...")
subprocess.Popen(
    [sys.executable, str(Path(__file__).parent / "run_harness_one.py"), str(idx), iid, str(pred), str(elapsed), str(tools), str(errors)],
    stdout=open(REPOS / f"../logs/harness_{idx}.log", "w"),
    stderr=subprocess.STDOUT,
)

with open(REPOS / "../results.md", "a") as f:
    f.write(f"| {idx} | {iid} | ✅ | {elapsed:.0f}s | {tools} | {errors} | ⏳ | |\n")

print(f"Result written to benchmark/results.md (harness in background)")
