"""ControlFlowGraph — enumerate paths to raise/return statements.

Shows which conditions lead to which error messages, making it visible
when a single raise serves multiple distinct failure modes.
"""

import ast


def analyze(source, source_lines, **kwargs):
    """Run control flow analysis.

    Returns:
        list of anomaly dicts
    """
    tree = ast.parse(source)
    anomalies = []

    # Analyze each function
    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            anomalies.extend(_analyze_function(node, source_lines))

    return anomalies


def _analyze_function(func_node, source_lines):
    """Analyze control flow within a function."""
    anomalies = []

    # Find all raise statements and trace conditions leading to them
    raise_paths = _collect_raise_paths(func_node, source_lines)

    # Check for raises reachable by multiple distinct conditions
    for raise_info in raise_paths:
        if len(raise_info["conditions"]) >= 2:
            # Multiple conditions lead to same raise — potential lossy message
            conditions_str = "; ".join(
                f"L{c['line']}: {c['text']}" for c in raise_info["conditions"]
            )
            anomalies.append({
                "analyzer": "controlflow",
                "severity": "medium",
                "title": "Single raise serves multiple failure modes",
                "lines": [raise_info["line"]] + [c["line"] for c in raise_info["conditions"]],
                "detail": (f"raise at L{raise_info['line']} is reachable from "
                          f"{len(raise_info['conditions'])} different conditions: "
                          f"{conditions_str}"),
            })

    # Check for if/elif chains where one branch has no raise but others do
    _check_missing_error_handling(func_node, source_lines, anomalies)

    return anomalies


def _collect_raise_paths(func_node, source_lines):
    """Collect all raise statements and the conditions that guard them."""
    raises = []

    def _walk_branch(node, conditions):
        """Recursively walk if/elif/else branches tracking conditions."""
        if isinstance(node, ast.If):
            cond_text = _get_line(source_lines, node.lineno)
            new_conditions = conditions + [{"line": node.lineno, "text": cond_text}]

            # Check body for raises
            for child in node.body:
                if isinstance(child, ast.Raise):
                    raises.append({
                        "line": child.lineno,
                        "conditions": new_conditions,
                        "code": _get_line(source_lines, child.lineno),
                    })
                elif isinstance(child, ast.If):
                    _walk_branch(child, new_conditions)

            # Check elif/else
            for child in node.orelse:
                if isinstance(child, ast.If):
                    _walk_branch(child, conditions)
                elif isinstance(child, ast.Raise):
                    raises.append({
                        "line": child.lineno,
                        "conditions": conditions + [{"line": child.lineno, "text": "else"}],
                        "code": _get_line(source_lines, child.lineno),
                    })

    for node in ast.iter_child_nodes(func_node):
        if isinstance(node, ast.If):
            _walk_branch(node, [])

    return raises


def _check_missing_error_handling(func_node, source_lines, anomalies):
    """Check for if/elif chains where some branches raise but others don't."""
    for node in ast.walk(func_node):
        if not isinstance(node, ast.If):
            continue

        # Count branches with and without raises
        branches = _count_branches(node)
        if branches["total"] >= 2:
            if 0 < branches["with_raise"] < branches["total"]:
                # Some branches raise, others don't — might be intentional
                # Only flag if it looks like validation code
                if branches["with_raise"] >= 2 and branches["without_raise"] == 1:
                    anomalies.append({
                        "analyzer": "controlflow",
                        "severity": "low",
                        "title": "Validation chain has branch without error handling",
                        "lines": [node.lineno],
                        "detail": (f"if/elif chain at L{node.lineno}: "
                                  f"{branches['with_raise']}/{branches['total']} branches raise, "
                                  f"but {branches['without_raise']} branch(es) don't"),
                    })


def _count_branches(if_node):
    """Count branches in an if/elif/else chain."""
    result = {"total": 0, "with_raise": 0, "without_raise": 0}

    def _check_body(body):
        result["total"] += 1
        has_raise = any(isinstance(n, ast.Raise) for n in body)
        if has_raise:
            result["with_raise"] += 1
        else:
            result["without_raise"] += 1

    _check_body(if_node.body)
    for node in if_node.orelse:
        if isinstance(node, ast.If):
            _check_body(node.body)
            # Check its orelse too
            if node.orelse and not isinstance(node.orelse[0], ast.If):
                _check_body(node.orelse)
        else:
            # else branch
            result["total"] += 1
            result["without_raise"] += 1

    return result


def _get_line(source_lines, lineno):
    if 1 <= lineno <= len(source_lines):
        return source_lines[lineno - 1].strip()
    return ""
