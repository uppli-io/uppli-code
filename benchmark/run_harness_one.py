#!/usr/bin/env python3
"""Run harness for a single issue and update results.md.
Called as background process by run_single.py — do not run manually.
"""
import json, subprocess, sys, re
from pathlib import Path

idx = int(sys.argv[1])
iid = sys.argv[2]
pred = Path(sys.argv[3])
elapsed = sys.argv[4]
tools = sys.argv[5]
errors = sys.argv[6]

REPORTS = Path("/Users/sayahfarid/uppli-code/claurst/benchmark/reports")
RESULTS = Path("/Users/sayahfarid/uppli-code/claurst/benchmark/results.md")
REPORTS.mkdir(parents=True, exist_ok=True)

harness_timeout = False
try:
    h = subprocess.run(
        [sys.executable, "-m", "swebench.harness.run_evaluation",
         "-d", "princeton-nlp/SWE-bench_Verified", "-p", str(pred),
         "-id", "uppli-code", "-i", iid, "--report_dir", str(REPORTS), "--timeout", "600"],
        capture_output=True, text=True, timeout=1200
    )
except subprocess.TimeoutExpired:
    harness_timeout = True
    h = None

# Check report in swebench's actual log dir (not --report_dir)
report = Path("logs/run_evaluation/uppli-code/uppli-code") / iid / "report.json"
if harness_timeout:
    passed = False
    icon = "⏰"
elif report.exists():
    data = json.loads(report.read_text())
    passed = data.get(iid, {}).get("resolved", False) if iid in data else data.get("resolved", False)
    icon = "✅" if passed else "❌"
else:
    # Fallback: parse "Instances resolved: N" properly
    import re as _re
    m = _re.search(r'Instances resolved:\s*(\d+)', h.stdout if h else "")
    passed = m is not None and int(m.group(1)) > 0
    icon = "✅" if passed else "❌"

# Update results.md: replace ⏳ with actual result for this issue
text = RESULTS.read_text()
lines = text.split("\n")
new_lines = []
for line in lines:
    if f"| {idx} |" in line and iid in line and "⏳" in line:
        line = line.replace("⏳", icon)
    new_lines.append(line)
RESULTS.write_text("\n".join(new_lines))

print(f"HARNESS {iid}: {icon} ({'timeout' if harness_timeout else 'pass' if passed else 'fail'})")
