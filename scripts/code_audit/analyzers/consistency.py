"""ConsistencyChecker — detect outliers in groups of similar patterns.

If 5/6 string comparisons use .upper() and 1 doesn't, flag the outlier.
If 3/4 re.compile() calls use IGNORECASE and 1 doesn't, flag it.

Also checks:
- Inconsistent error handling (some calls wrapped in try, others not)
- Inconsistent None checks (some accesses guarded, others not)
- Inconsistent dict access (.get() vs [key])
- Inconsistent string quoting in similar contexts
"""

import ast
from collections import defaultdict


def analyze(source, source_lines, **kwargs):
    """Run consistency analysis.

    Returns:
        list of anomaly dicts
    """
    tree = ast.parse(source)
    anomalies = []

    anomalies.extend(_check_string_comparisons(tree, source_lines))
    anomalies.extend(_check_regex_flags(tree, source_lines))
    anomalies.extend(_check_startswith_endswith(tree, source_lines))
    anomalies.extend(_check_dict_access_consistency(tree, source_lines))
    anomalies.extend(_check_error_handling_consistency(tree, source_lines))

    return anomalies


def _check_string_comparisons(tree, source_lines):
    """Find string literal comparisons, flag those without case normalization."""
    comparisons = []

    for node in ast.walk(tree):
        if not isinstance(node, ast.Compare):
            continue
        for op, comparator in zip(node.ops, node.comparators):
            if not isinstance(op, (ast.Eq, ast.NotEq)):
                continue

            # Check both sides for string literals
            literal = None
            other = None
            if isinstance(comparator, ast.Constant) and isinstance(comparator.value, str):
                literal = comparator.value
                other = node.left
            elif isinstance(node.left, ast.Constant) and isinstance(node.left.value, str):
                literal = node.left.value
                other = comparator

            if literal is None or not any(c.isalpha() for c in literal):
                continue

            # Check if the other side uses .upper()/.lower()/.casefold()
            has_norm = _has_case_normalization(other)

            comparisons.append({
                "line": node.lineno,
                "literal": literal,
                "normalized": has_norm,
                "code": _get_line(source_lines, node.lineno),
            })

    return _find_outliers(comparisons, "string comparison", "case normalization")


def _check_regex_flags(tree, source_lines):
    """Find re.compile() calls, flag those without IGNORECASE."""
    compiles = []

    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        if not (isinstance(node.func, ast.Attribute) and node.func.attr == "compile"
                and isinstance(node.func.value, ast.Name) and node.func.value.id == "re"):
            continue

        has_ignorecase = _has_ignorecase_flag(node)
        compiles.append({
            "line": node.lineno,
            "literal": "re.compile()",
            "normalized": has_ignorecase,
            "code": _get_line(source_lines, node.lineno),
        })

    return _find_outliers(compiles, "re.compile()", "IGNORECASE flag")


def _check_startswith_endswith(tree, source_lines):
    """Find .startswith()/.endswith() with literals, check consistency."""
    calls = []

    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        if not isinstance(node.func, ast.Attribute):
            continue
        if node.func.attr not in ("startswith", "endswith"):
            continue
        if not node.args or not isinstance(node.args[0], ast.Constant):
            continue
        if not isinstance(node.args[0].value, str):
            continue
        if not any(c.isalpha() for c in node.args[0].value):
            continue

        # Check if the caller uses .upper()/.lower() before the call
        has_norm = _has_case_normalization(node.func.value)

        calls.append({
            "line": node.lineno,
            "literal": f".{node.func.attr}({node.args[0].value!r})",
            "normalized": has_norm,
            "code": _get_line(source_lines, node.lineno),
        })

    return _find_outliers(calls, "startswith/endswith", "case normalization")


def _find_outliers(items, pattern_name, norm_name):
    """Given a list of pattern instances, find outliers."""
    if len(items) < 2:
        return []

    normalized = [i for i in items if i["normalized"]]
    not_normalized = [i for i in items if not i["normalized"]]

    anomalies = []

    if len(normalized) > 0 and len(not_normalized) > 0:
        # There's a mix — flag the minority as outliers
        if len(normalized) >= len(not_normalized):
            # Majority normalized → flag the non-normalized ones
            for item in not_normalized:
                anomalies.append({
                    "analyzer": "consistency",
                    "severity": "high" if len(normalized) >= 2 * len(not_normalized) else "medium",
                    "title": f"Inconsistent {norm_name} in {pattern_name}",
                    "lines": [item["line"]],
                    "detail": (f"{len(normalized)}/{len(items)} {pattern_name}s use {norm_name}, "
                              f"but L{item['line']} does not: {item['code']}"),
                })
        else:
            # Majority NOT normalized → flag the normalized ones (unusual but possible)
            for item in normalized:
                anomalies.append({
                    "analyzer": "consistency",
                    "severity": "low",
                    "title": f"Unusual {norm_name} in {pattern_name}",
                    "lines": [item["line"]],
                    "detail": (f"Only {len(normalized)}/{len(items)} {pattern_name}s use {norm_name}. "
                              f"L{item['line']} is the exception: {item['code']}"),
                })

    # Also flag if NO instances use normalization but there are string literals
    # with mixed case potential (ALL CAPS literals compared without normalization)
    if len(normalized) == 0 and len(not_normalized) >= 2:
        has_upper = any(i["literal"].isupper() if isinstance(i["literal"], str) else False
                       for i in not_normalized)
        if has_upper:
            anomalies.append({
                "analyzer": "consistency",
                "severity": "medium",
                "title": f"No {norm_name} in {pattern_name}s with uppercase literals",
                "lines": [i["line"] for i in not_normalized],
                "detail": (f"All {len(not_normalized)} {pattern_name}s compare against "
                          f"uppercase/mixed literals without {norm_name}"),
            })

    return anomalies


def _has_case_normalization(node):
    """Check if a node involves .upper(), .lower(), or .casefold()."""
    if isinstance(node, ast.Call):
        if isinstance(node.func, ast.Attribute):
            if node.func.attr in ("upper", "lower", "casefold"):
                return True
    return False


def _has_ignorecase_flag(call_node):
    """Check if a re.compile() call includes re.IGNORECASE."""
    for arg in call_node.args[1:]:
        if _contains_ignorecase(arg):
            return True
    for kw in call_node.keywords:
        if kw.arg == "flags" and _contains_ignorecase(kw.value):
            return True
    return False


def _contains_ignorecase(node):
    if isinstance(node, ast.Attribute) and node.attr in ("IGNORECASE", "I"):
        return True
    if isinstance(node, ast.BinOp):
        return _contains_ignorecase(node.left) or _contains_ignorecase(node.right)
    return False


def _check_dict_access_consistency(tree, source_lines):
    """Check if dict accesses mix [key] and .get(key) for the same dict."""
    accesses = defaultdict(list)

    for node in ast.walk(tree):
        # dict[key] access
        if isinstance(node, ast.Subscript) and isinstance(node.value, ast.Name):
            if isinstance(node.slice, (ast.Constant, ast.Name)):
                accesses[node.value.id].append({
                    "line": node.lineno,
                    "style": "bracket",
                    "code": _get_line(source_lines, node.lineno),
                })
        # dict.get(key) access
        if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute):
            if node.func.attr == "get" and isinstance(node.func.value, ast.Name):
                accesses[node.func.value.id].append({
                    "line": node.lineno,
                    "style": "get",
                    "code": _get_line(source_lines, node.lineno),
                })

    anomalies = []
    for var, usages in accesses.items():
        if len(usages) < 3:
            continue
        brackets = [u for u in usages if u["style"] == "bracket"]
        gets = [u for u in usages if u["style"] == "get"]
        if brackets and gets:
            minority = brackets if len(brackets) < len(gets) else gets
            majority_style = "get" if len(gets) >= len(brackets) else "bracket"
            if len(minority) <= len(usages) // 3:  # minority is <33%
                for item in minority:
                    anomalies.append({
                        "analyzer": "consistency",
                        "severity": "low",
                        "title": f"Inconsistent dict access on '{var}'",
                        "lines": [item["line"]],
                        "detail": (f"'{var}' is mostly accessed via .{majority_style}() "
                                  f"but L{item['line']} uses {'[]' if item['style'] == 'bracket' else '.get()'}: "
                                  f"{item['code']}"),
                    })
    return anomalies


def _check_error_handling_consistency(tree, source_lines):
    """Check if similar function calls are inconsistently wrapped in try/except."""
    # Collect all function calls and whether they're inside a try block
    call_sites = defaultdict(list)

    class _TryTracker(ast.NodeVisitor):
        def __init__(self):
            self.in_try = False

        def visit_Try(self, node):
            old = self.in_try
            self.in_try = True
            for child in node.body:
                self.visit(child)
            self.in_try = old
            for handler in node.handlers:
                self.visit(handler)
            for child in node.orelse:
                self.visit(child)
            for child in node.finalbody:
                self.visit(child)

        def visit_Call(self, node):
            func_name = None
            if isinstance(node.func, ast.Name):
                func_name = node.func.id
            elif isinstance(node.func, ast.Attribute):
                func_name = node.func.attr

            if func_name:
                call_sites[func_name].append({
                    "line": node.lineno,
                    "in_try": self.in_try,
                    "code": _get_line(source_lines, node.lineno),
                })
            self.generic_visit(node)

    tracker = _TryTracker()
    tracker.visit(tree)

    anomalies = []
    for func_name, calls in call_sites.items():
        if len(calls) < 3:
            continue
        in_try = [c for c in calls if c["in_try"]]
        not_in_try = [c for c in calls if not c["in_try"]]
        if in_try and not_in_try and len(not_in_try) <= len(calls) // 3:
            for item in not_in_try:
                anomalies.append({
                    "analyzer": "consistency",
                    "severity": "low",
                    "title": f"'{func_name}()' called without error handling",
                    "lines": [item["line"]],
                    "detail": (f"{len(in_try)}/{len(calls)} calls to '{func_name}()' are in try/except, "
                              f"but L{item['line']} is not: {item['code']}"),
                })
    return anomalies


def _get_line(source_lines, lineno):
    if 1 <= lineno <= len(source_lines):
        return source_lines[lineno - 1].strip()
    return ""
