#!/usr/bin/env python3
"""Run SWE-bench harness on all pending predictions.
Usage: python3 benchmark/run_harness.py [--workers 4]
"""
import json, subprocess, sys, os, re
from pathlib import Path
from concurrent.futures import ProcessPoolExecutor, as_completed

PREDS = Path("/Users/sayahfarid/uppli-code/claurst/benchmark/predictions")
REPORTS = Path("/Users/sayahfarid/uppli-code/claurst/benchmark/reports")
RESULTS = Path("/Users/sayahfarid/uppli-code/claurst/benchmark/results.md")

def get_validated_ids():
    """Return set of instance_ids already validated (✅ or ❌, not ⏳)."""
    text = RESULTS.read_text()
    validated = set()
    for line in text.split("\n"):
        m = re.match(r'^\|\s*(\d+)\s*\|\s*(\S+)', line)
        if m and "⏳" not in line:
            # Already has harness result (✅, ❌, or ⏰)
            if "✅ | |" in line or "❌ |" in line or "⏰" in line:
                validated.add(m.group(2))
    return validated

def run_one(pred_file):
    """Run harness for one prediction, return (iid, passed, error)."""
    iid = pred_file.stem
    REPORTS.mkdir(parents=True, exist_ok=True)
    try:
        h = subprocess.run(
            [sys.executable, "-m", "swebench.harness.run_evaluation",
             "-d", "princeton-nlp/SWE-bench_Verified", "-p", str(pred_file),
             "-id", "uppli-code", "-i", iid, "--report_dir", str(REPORTS), "--timeout", "600"],
            capture_output=True, text=True, timeout=1200
        )
        from pathlib import Path as _P
        report = _P("logs/run_evaluation/uppli-code/uppli-code") / iid / "report.json"
        if report.exists():
            data = json.loads(report.read_text())
            passed = data.get(iid, {}).get("resolved", False) if iid in data else data.get("resolved", False)
        else:
            import re as _re
            m = _re.search(r'Instances resolved:\s*(\d+)', h.stdout)
            passed = m is not None and int(m.group(1)) > 0
        return (iid, passed, None)
    except subprocess.TimeoutExpired:
        return (iid, None, "timeout")
    except Exception as e:
        return (iid, None, str(e))

def update_results(iid, icon):
    """Replace ⏳ with actual result for this iid in results.md."""
    text = RESULTS.read_text()
    updated = text.replace(f"| {iid} | ✅ |", f"| {iid} | ✅ |", 1)
    # Find the line with this iid and ⏳, replace ⏳ with icon
    lines = text.split("\n")
    new_lines = []
    for line in lines:
        if iid in line and "⏳" in line:
            line = line.replace("⏳", icon)
        new_lines.append(line)
    RESULTS.write_text("\n".join(new_lines))

def main():
    workers = 4
    if "--workers" in sys.argv:
        workers = int(sys.argv[sys.argv.index("--workers") + 1])

    validated = get_validated_ids()
    pending = []
    for f in sorted(PREDS.glob("*.jsonl")):
        iid = f.stem
        if iid not in validated:
            pending.append(f)

    print(f"Pending harness validations: {len(pending)}")
    print(f"Already validated: {len(validated)}")
    print(f"Workers: {workers}")
    print()

    passed_count = 0
    failed_count = 0
    timeout_count = 0

    with ProcessPoolExecutor(max_workers=workers) as pool:
        futures = {pool.submit(run_one, f): f for f in pending}
        for future in as_completed(futures):
            iid, passed, error = future.result()
            if error == "timeout":
                icon = "⏰"
                timeout_count += 1
                print(f"  ⏰ {iid} — harness timeout")
            elif passed:
                icon = "✅"
                passed_count += 1
                print(f"  ✅ {iid}")
            else:
                icon = "❌"
                failed_count += 1
                print(f"  ❌ {iid}")
            update_results(iid, icon)

    print(f"\nDone: {passed_count} ✅ | {failed_count} ❌ | {timeout_count} ⏰")

if __name__ == "__main__":
    main()
