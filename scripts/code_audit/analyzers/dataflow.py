"""DataFlowTracer — trace variable transformations through code paths.

Detects when a variable is normalized (e.g., .upper(), .strip()) in one
code path but used raw in another.
"""

import ast
from collections import defaultdict


def analyze(source, source_lines, **kwargs):
    """Run data flow analysis.

    Returns:
        list of anomaly dicts
    """
    tree = ast.parse(source)
    anomalies = []

    # Analyze each function separately
    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            anomalies.extend(_analyze_function(node, source_lines))

    return anomalies


def _analyze_function(func_node, source_lines):
    """Analyze data flow within a single function."""
    anomalies = []

    # Collect all variable assignments and their transformations
    assignments = _collect_assignments(func_node)

    # Collect all variable usages in comparisons
    comparisons = _collect_comparisons(func_node)

    # For each variable used in comparisons, check if it was transformed
    # in some paths but not others
    for var_name, comp_usages in comparisons.items():
        transforms = assignments.get(var_name, [])
        if not transforms:
            continue

        # Check: variable is transformed (e.g., .upper()) in some assignments
        # but used raw in some comparisons
        has_transform = any(t["transform"] for t in transforms)
        has_raw_comparison = any(not c["normalized"] for c in comp_usages)

        if has_transform and has_raw_comparison:
            raw_lines = [c["line"] for c in comp_usages if not c["normalized"]]
            transform_lines = [t["line"] for t in transforms if t["transform"]]
            anomalies.append({
                "analyzer": "dataflow",
                "severity": "medium",
                "title": f"Variable '{var_name}' transformed in some paths but used raw in others",
                "lines": raw_lines,
                "detail": (f"'{var_name}' is transformed ({transforms[0]['transform']}) at "
                          f"L{transform_lines[0] if transform_lines else '?'} "
                          f"but compared without transformation at L{raw_lines[0]}. "
                          f"Code: {_get_line(source_lines, raw_lines[0])}"),
            })

    return anomalies


def _collect_assignments(func_node):
    """Collect variable assignments and detect transformations."""
    assignments = defaultdict(list)

    for node in ast.walk(func_node):
        if isinstance(node, ast.Assign):
            for target in node.targets:
                if isinstance(target, ast.Name):
                    transform = _detect_transform(node.value)
                    assignments[target.id].append({
                        "line": node.lineno,
                        "transform": transform,
                    })
        elif isinstance(node, ast.AugAssign):
            if isinstance(node.target, ast.Name):
                transform = _detect_transform(node.value)
                assignments[node.target.id].append({
                    "line": node.lineno,
                    "transform": transform,
                })

    return assignments


def _collect_comparisons(func_node):
    """Collect all variable comparisons."""
    comparisons = defaultdict(list)

    for node in ast.walk(func_node):
        if not isinstance(node, ast.Compare):
            continue

        for op, comparator in zip(node.ops, node.comparators):
            if not isinstance(op, (ast.Eq, ast.NotEq)):
                continue

            # Check left side
            if isinstance(node.left, ast.Name):
                comparisons[node.left.id].append({
                    "line": node.lineno,
                    "normalized": _has_normalization(node.left),
                })
            elif isinstance(node.left, ast.Call) and isinstance(node.left.func, ast.Attribute):
                if isinstance(node.left.func.value, ast.Name):
                    comparisons[node.left.func.value.id].append({
                        "line": node.lineno,
                        "normalized": _has_normalization(node.left),
                    })

            # Check right side
            if isinstance(comparator, ast.Name):
                comparisons[comparator.id].append({
                    "line": node.lineno,
                    "normalized": _has_normalization(comparator),
                })

    return comparisons


def _detect_transform(node):
    """Detect if a value expression involves a transformation."""
    if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute):
        method = node.func.attr
        if method in ("upper", "lower", "strip", "lstrip", "rstrip",
                      "casefold", "replace", "encode", "decode"):
            return f".{method}()"
    # Check for chained calls
    for child in ast.walk(node):
        if isinstance(child, ast.Call) and isinstance(child.func, ast.Attribute):
            if child.func.attr in ("upper", "lower", "strip", "casefold"):
                return f".{child.func.attr}()"
    return None


def _has_normalization(node):
    """Check if a node uses .upper()/.lower() etc."""
    if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute):
        return node.func.attr in ("upper", "lower", "casefold", "strip")
    return False


def _get_line(source_lines, lineno):
    if 1 <= lineno <= len(source_lines):
        return source_lines[lineno - 1].strip()
    return ""
