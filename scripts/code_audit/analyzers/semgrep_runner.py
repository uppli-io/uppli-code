"""Semgrep integration — run semgrep rules and return anomalies.

Wraps the semgrep CLI to run community rules on the target file.
Falls back gracefully if semgrep is not installed.
"""

import subprocess
import json
import os


def analyze(source, source_lines, file_path=None, **kwargs):
    """Run semgrep on the file and return anomalies.

    Args:
        source: source code (unused, semgrep reads the file directly)
        source_lines: source lines (unused)
        file_path: path to the file to analyze

    Returns:
        list of anomaly dicts
    """
    if file_path is None:
        return []

    # Check if semgrep is available
    try:
        subprocess.run(["semgrep", "--version"], capture_output=True, timeout=5)
    except (FileNotFoundError, subprocess.TimeoutExpired):
        return []  # semgrep not installed, skip silently

    # Detect language for rule selection
    ext = os.path.splitext(file_path)[1]
    lang_rules = {
        ".py": ["p/python", "p/python-lang-best-practice"],
        ".js": ["p/javascript", "p/nodejs"],
        ".ts": ["p/typescript"],
        ".go": ["p/golang"],
        ".java": ["p/java"],
        ".rs": ["p/rust"],
    }
    rules = lang_rules.get(ext, ["p/default"])

    anomalies = []
    for ruleset in rules:
        try:
            result = subprocess.run(
                ["semgrep", "--config", ruleset, "--json", "--quiet",
                 "--timeout", "10", "--max-target-bytes", "500000",
                 file_path],
                capture_output=True, text=True, timeout=30
            )
            if result.returncode == 0 and result.stdout:
                data = json.loads(result.stdout)
                for finding in data.get("results", []):
                    severity = finding.get("extra", {}).get("severity", "WARNING")
                    sev_map = {"ERROR": "high", "WARNING": "medium", "INFO": "low"}
                    anomalies.append({
                        "analyzer": "semgrep",
                        "severity": sev_map.get(severity, "medium"),
                        "title": finding.get("check_id", "unknown").split(".")[-1],
                        "lines": [finding.get("start", {}).get("line", 0)],
                        "detail": finding.get("extra", {}).get("message", ""),
                    })
        except (subprocess.TimeoutExpired, json.JSONDecodeError, Exception):
            continue

    return anomalies
