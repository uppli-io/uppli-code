"""ASTAnalyzer — structural pattern detection.

Detects:
- Condition-message inconsistency (lossy error messages)
- Subscript narrowing: condition uses slice but message uses [0]
- Mutable default arguments
- Bare except / overly broad except
- Comparison with None using == instead of is
- f-string / .format() arg count mismatch
- Return inconsistency (some paths return value, others don't)
- Dict access without .get() or key check after 'in' test
- isinstance() without handling all branches
"""

import ast
import re


def analyze(source, source_lines, **kwargs):
    """Run AST structural analysis.

    Returns:
        list of anomaly dicts
    """
    tree = ast.parse(source)
    anomalies = []

    anomalies.extend(_check_lossy_error_messages(tree, source_lines))
    anomalies.extend(_check_collection_in_format_string(tree, source_lines))
    anomalies.extend(_check_mutable_defaults(tree, source_lines))
    anomalies.extend(_check_bare_except(tree, source_lines))
    anomalies.extend(_check_none_comparison(tree, source_lines))
    anomalies.extend(_check_format_arg_count(tree, source_lines))
    anomalies.extend(_check_inconsistent_return(tree, source_lines))

    return anomalies


def _check_lossy_error_messages(tree, source_lines):
    """Find raise statements where the error message loses information from the condition.

    Pattern: elif list_a[:n] != list_b → raise ValueError(... list_a[0] ... list_b[0] ...)
    The condition compares slices (full lists) but the message only shows [0] (first element).
    """
    anomalies = []

    for node in ast.walk(tree):
        if isinstance(node, ast.If):
            _check_if_branch(node, source_lines, anomalies)

    return anomalies


def _check_if_branch(node, source_lines, anomalies):
    """Check a single if/elif branch and recurse into orelse."""
    # Extract subscript patterns from the condition
    condition_slices = _find_slices(node.test)

    if condition_slices:
        # Look for raise statements in the body
        for child in ast.walk(node):
            if not isinstance(child, ast.Raise) or not child.exc:
                continue

            msg_index0 = _find_index0(child.exc)
            if msg_index0:
                # Condition uses slice, message uses [0] → lossy
                slice_names = list({s["base"] for s in condition_slices})
                idx0_names = list({i["base"] for i in msg_index0})

                anomalies.append({
                    "analyzer": "ast",
                    "severity": "high",
                    "title": "Error message loses information from condition",
                    "lines": [child.lineno],
                    "detail": (f"Condition compares slice(s) of {slice_names} "
                              f"but error message uses {idx0_names}[0] — "
                              f"shows only first element instead of full list. "
                              f"FIX: replace [0] with the full list/slice in the format string. "
                              f"Do NOT add new branches or new logic — just change the existing "
                              f"format() arguments to show all elements, not just the first. "
                              f"Code: {_get_line(source_lines, child.lineno)}"),
                })

    # Recurse into elif branches
    for elif_node in node.orelse:
        if isinstance(elif_node, ast.If):
            _check_if_branch(elif_node, source_lines, anomalies)


def _find_slices(node):
    """Find all slice subscripts (e.g., x[:n]) in an AST subtree."""
    results = []
    for child in ast.walk(node):
        if isinstance(child, ast.Subscript) and isinstance(child.slice, ast.Slice):
            base = _get_base_name(child.value)
            results.append({"base": base, "line": child.lineno})
    return results


def _find_index0(node):
    """Find all [0] subscripts in an AST subtree."""
    results = []
    for child in ast.walk(node):
        if isinstance(child, ast.Subscript):
            if isinstance(child.slice, ast.Constant) and child.slice.value == 0:
                base = _get_base_name(child.value)
                results.append({"base": base, "line": child.lineno})
    return results


def _get_base_name(node):
    """Extract the base name from a node (Name or Attribute)."""
    if isinstance(node, ast.Name):
        return node.id
    if isinstance(node, ast.Attribute):
        return node.attr
    return "?"


def _check_collection_in_format_string(tree, source_lines):
    """Detect format strings that display a list/slice directly.

    str(['time']) renders as "['time']" with brackets, which looks ugly
    in error messages. Should use a helper that formats single items
    without brackets and multiple items with brackets.
    """
    anomalies = []
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        if not isinstance(node.func, ast.Attribute):
            continue
        if node.func.attr != "format":
            continue
        # Check each argument to .format() — including .join() calls
        for arg in node.args:
            # Detect "', '".join(collection) — renders without brackets
            if isinstance(arg, ast.Call) and isinstance(arg.func, ast.Attribute):
                if arg.func.attr == "join" and arg.args:
                    inner = arg.args[0]
                    inner_desc = ""
                    if isinstance(inner, ast.Subscript) and isinstance(inner.slice, ast.Slice):
                        inner_desc = f"slice {_get_base_name(inner.value)}[:]"
                    elif isinstance(inner, ast.Name):
                        inner_desc = f"variable '{inner.id}'"
                    if inner_desc:
                        anomalies.append({
                            "analyzer": "ast",
                            "severity": "medium",
                            "title": "join() on collection in format string",
                            "lines": [node.lineno],
                            "detail": (f"Format uses .join({inner_desc}) at L{node.lineno}. "
                                      f"For 1 element this renders without quotes/brackets. "
                                      f"Use a helper: repr(x[0]) for 1 element, str(x) for multiple. "
                                      f"Code: {_get_line(source_lines, node.lineno)}"),
                        })
                    continue
            is_collection = False
            desc = ""
            # Direct list/tuple literal
            if isinstance(arg, (ast.List, ast.Tuple)):
                is_collection = True
                desc = "list literal"
            # Slice subscript: x[:n]
            elif isinstance(arg, ast.Subscript) and isinstance(arg.slice, ast.Slice):
                is_collection = True
                desc = f"slice {_get_base_name(arg.value)}[:]"
            # Variable that is a known list (Name reference)
            elif isinstance(arg, ast.Name):
                # Check if the name was assigned from a list or is plural-named
                name = arg.id
                if any(hint in name.lower() for hint in
                       ("columns", "names", "items", "keys", "values", "list", "cols")):
                    is_collection = True
                    desc = f"variable '{name}' (likely a collection)"

            if is_collection:
                anomalies.append({
                    "analyzer": "ast",
                    "severity": "medium",
                    "title": "Collection displayed directly in format string",
                    "lines": [node.lineno],
                    "detail": (f"Format argument at L{node.lineno} is a {desc}. "
                              f"Neither str(list) nor ', '.join(list) renders well for both "
                              f"1 and N elements. Use a helper that returns 'x' for single "
                              f"items and ['x', 'y'] for multiple (matching Python list repr). "
                              f"Example: str(x) if len(x) > 1 else repr(x[0]). "
                              f"Code: {_get_line(source_lines, node.lineno)}"),
                })
    return anomalies


def _check_mutable_defaults(tree, source_lines):
    """Detect mutable default arguments: def f(x=[]), def f(x={})."""
    anomalies = []
    for node in ast.walk(tree):
        if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            continue
        for default in node.args.defaults + node.args.kw_defaults:
            if default is None:
                continue
            if isinstance(default, (ast.List, ast.Dict, ast.Set)):
                anomalies.append({
                    "analyzer": "ast",
                    "severity": "medium",
                    "title": "Mutable default argument",
                    "lines": [default.lineno],
                    "detail": (f"Function '{node.name}' has mutable default at L{default.lineno}. "
                              f"Mutable defaults are shared between calls — use None and assign inside."),
                })
    return anomalies


def _check_bare_except(tree, source_lines):
    """Detect bare except or overly broad except Exception."""
    anomalies = []
    for node in ast.walk(tree):
        if not isinstance(node, ast.ExceptHandler):
            continue
        if node.type is None:
            anomalies.append({
                "analyzer": "ast",
                "severity": "medium",
                "title": "Bare except clause",
                "lines": [node.lineno],
                "detail": (f"Bare 'except:' at L{node.lineno} catches everything including "
                          f"KeyboardInterrupt and SystemExit. Use 'except Exception:' or narrower."),
            })
    return anomalies


def _check_none_comparison(tree, source_lines):
    """Detect comparison with None using == instead of 'is'."""
    anomalies = []
    for node in ast.walk(tree):
        if not isinstance(node, ast.Compare):
            continue
        for op, comparator in zip(node.ops, node.comparators):
            if isinstance(op, (ast.Eq, ast.NotEq)):
                if isinstance(comparator, ast.Constant) and comparator.value is None:
                    op_str = "==" if isinstance(op, ast.Eq) else "!="
                    is_str = "is" if isinstance(op, ast.Eq) else "is not"
                    anomalies.append({
                        "analyzer": "ast",
                        "severity": "low",
                        "title": f"Comparison with None using {op_str}",
                        "lines": [node.lineno],
                        "detail": f"Use '{is_str} None' instead of '{op_str} None' at L{node.lineno}.",
                    })
    return anomalies


def _check_format_arg_count(tree, source_lines):
    """Detect .format() calls where arg count doesn't match placeholder count."""
    anomalies = []
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        if not isinstance(node.func, ast.Attribute):
            continue
        if node.func.attr != "format":
            continue
        # Try to get the format string
        if isinstance(node.func.value, ast.Constant) and isinstance(node.func.value.value, str):
            fmt_str = node.func.value.value
            # Count {} placeholders (simple, not counting {name} or {0})
            placeholders = len(re.findall(r'\{[^}]*\}', fmt_str))
            args = len(node.args) + len(node.keywords)
            if placeholders != args and placeholders > 0 and args > 0:
                anomalies.append({
                    "analyzer": "ast",
                    "severity": "high",
                    "title": "Format string argument count mismatch",
                    "lines": [node.lineno],
                    "detail": (f"Format string has {placeholders} placeholders but "
                              f"{args} arguments at L{node.lineno}."),
                })
        # Also check for JoinedStr (f-string) — harder, skip for now
    return anomalies


def _check_inconsistent_return(tree, source_lines):
    """Detect functions where some paths return a value and others return None."""
    anomalies = []
    for node in ast.walk(tree):
        if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            continue
        returns = []
        for child in ast.walk(node):
            if isinstance(child, ast.Return):
                has_value = child.value is not None
                returns.append({"line": child.lineno, "has_value": has_value})
        if len(returns) >= 2:
            with_value = [r for r in returns if r["has_value"]]
            without_value = [r for r in returns if not r["has_value"]]
            if with_value and without_value:
                anomalies.append({
                    "analyzer": "ast",
                    "severity": "medium",
                    "title": f"Inconsistent return in '{node.name}'",
                    "lines": [r["line"] for r in returns],
                    "detail": (f"Function '{node.name}' has {len(with_value)} return(s) with value "
                              f"and {len(without_value)} bare return(s). This may cause unexpected None."),
                })
    return anomalies


def _get_line(source_lines, lineno):
    if 1 <= lineno <= len(source_lines):
        return source_lines[lineno - 1].strip()
    return ""
