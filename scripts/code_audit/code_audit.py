#!/usr/bin/env python3
"""CodeAudit — Unbiased structural analysis for bug detection.

Runs 5 analyzers on a Python file and returns a unified JSON report
of all anomalies found. No knowledge of the bug is needed — the tool
surfaces ALL structural issues for the model to cross-reference.

Usage:
    python3 code_audit.py <file_path> [--focus sym1,sym2] [--language python]

Output: JSON on stdout
"""

import argparse
import json
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

from analyzers import ast_analyzer, consistency, controlflow, dataflow, symbols


def run_analyzer(name, func, source, source_lines, **kwargs):
    """Run a single analyzer with error handling."""
    try:
        start = time.monotonic()
        result = func(source, source_lines, **kwargs)
        elapsed = time.monotonic() - start
        return name, result, elapsed, None
    except Exception as e:
        return name, None, 0, str(e)


def audit_file(file_path, focus_symbols=None, language="python"):
    """Run all analyzers on a file and return the unified report."""
    source = Path(file_path).read_text()
    source_lines = source.splitlines()

    analyzers = {
        "ast": ast_analyzer.analyze,
        "consistency": consistency.analyze,
        "controlflow": controlflow.analyze,
        "dataflow": dataflow.analyze,
    }

    all_anomalies = []
    errors = {}
    timings = {}

    # Run analyzers in parallel
    with ThreadPoolExecutor(max_workers=5) as pool:
        futures = {}
        for name, func in analyzers.items():
            f = pool.submit(run_analyzer, name, func, source, source_lines,
                           focus_symbols=focus_symbols)
            futures[f] = name

        # Also run symbols analyzer
        f = pool.submit(run_analyzer, "symbols", symbols.analyze, source, source_lines,
                       focus_symbols=focus_symbols)
        futures[f] = "symbols"

        for future in as_completed(futures):
            name, result, elapsed, error = future.result()
            timings[name] = round(elapsed * 1000, 1)
            if error:
                errors[name] = error
            elif name == "symbols":
                symbol_table = result
            elif isinstance(result, list):
                all_anomalies.extend(result)
            elif isinstance(result, dict) and "anomalies" in result:
                all_anomalies.extend(result["anomalies"])

    # Sort anomalies by severity (high > medium > low)
    severity_order = {"high": 0, "medium": 1, "low": 2}
    all_anomalies.sort(key=lambda a: severity_order.get(a.get("severity", "low"), 3))

    # Cap at 50 anomalies
    all_anomalies = all_anomalies[:50]

    # Build report
    report = {
        "file": str(file_path),
        "language": language,
        "anomalies": all_anomalies,
        "symbol_table": symbol_table.get("symbols", {}) if isinstance(symbol_table, dict) else {},
        "summary": f"{len(all_anomalies)} anomalies found",
        "timings_ms": timings,
    }

    if errors:
        report["errors"] = errors

    return report


def format_markdown(report):
    """Format report as concise markdown for the model."""
    lines = [f"## CodeAudit Report: {Path(report['file']).name}", ""]

    anomalies = report["anomalies"]
    if not anomalies:
        lines.append("No anomalies found.")
    else:
        lines.append(f"### Anomalies ({len(anomalies)} found)")
        lines.append("")
        for a in anomalies:
            sev = a.get("severity", "?").upper()
            analyzer = a.get("analyzer", "?")
            title = a.get("title", "?")
            line_nums = ", ".join(str(l) for l in a.get("lines", []))
            detail = a.get("detail", "")
            lines.append(f"**[{sev}] {title}** ({analyzer})")
            if line_nums:
                lines.append(f"Lines {line_nums}")
            lines.append(detail)
            lines.append("")

    # Symbol table (top 10)
    sym_table = report.get("symbol_table", {})
    if sym_table:
        lines.append("### Key Symbol Usages")
        for sym, usages in list(sym_table.items())[:10]:
            usage_strs = []
            seen_lines = set()
            for u in usages[:5]:
                l = u["line"]
                if l not in seen_lines:
                    ctx = u["context"][:60]
                    usage_strs.append(f"L{l} ({ctx})")
                    seen_lines.add(l)
            if len(usages) > 5:
                usage_strs.append(f"...+{len(usages)-5} more")
            lines.append(f"- `{sym}`: {', '.join(usage_strs)}")
        lines.append("")

    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description="CodeAudit — structural analysis")
    parser.add_argument("file_path", help="Path to the file to audit")
    parser.add_argument("--focus", help="Comma-separated symbols to focus on", default=None)
    parser.add_argument("--language", help="Programming language", default="python")
    parser.add_argument("--format", choices=["json", "markdown"], default="json",
                       help="Output format")
    args = parser.parse_args()

    focus_symbols = args.focus.split(",") if args.focus else None

    report = audit_file(args.file_path, focus_symbols=focus_symbols, language=args.language)

    if args.format == "markdown":
        print(format_markdown(report))
    else:
        print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
