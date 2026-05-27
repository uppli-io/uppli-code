"""DataFlowTracer — trace variable transformations through code paths.

Detects:
- Variable normalized in one path but used raw in another
- Transformation applied at wrong point in a pipeline (too early/too late)
- String method chaining where order matters (.replace before .rstrip vs after)
- Cross-function data flow (return value carries transformation)
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

    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            anomalies.extend(_check_inconsistent_transforms(node, source_lines))
            anomalies.extend(_check_pipeline_ordering(node, source_lines))
            anomalies.extend(_check_early_transformation(node, source_lines))

    anomalies.extend(_check_cross_function_flow(tree, source_lines))

    return anomalies


# --- Pattern 1: Inconsistent transforms across paths ---

def _check_inconsistent_transforms(func_node, source_lines):
    """Variable transformed in some paths but used raw in others."""
    anomalies = []
    assignments = _collect_assignments(func_node)
    comparisons = _collect_comparisons(func_node)

    for var_name, comp_usages in comparisons.items():
        transforms = assignments.get(var_name, [])
        if not transforms:
            continue

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


# --- Pattern 2: Pipeline ordering ---

def _check_pipeline_ordering(func_node, source_lines):
    """Detect string method chains where order might matter.

    E.g., .rstrip().replace("''", "'") vs .replace("''", "'").rstrip()
    When replace removes chars that rstrip would have stripped, order matters.
    """
    anomalies = []

    for node in ast.walk(func_node):
        if not isinstance(node, ast.Call):
            continue
        if not isinstance(node.func, ast.Attribute):
            continue

        # Detect chained calls: x.method1().method2()
        chain = _extract_method_chain(node)
        if len(chain) < 2:
            continue

        methods = [c["method"] for c in chain]

        # Check for order-sensitive combinations.
        # Only one direction per pair: the "risky" order where applying m1
        # before m2 can silently lose data (e.g., replace before rstrip).
        # The double-enumerate + i<j filter already fires regardless of
        # which order the methods appear in, so listing both directions
        # would cause every co-occurrence to match.
        order_sensitive_pairs = [
            ("replace", "rstrip"),
            ("replace", "lstrip"),
            ("replace", "strip"),
            ("replace", "split"),
            ("lower", "replace"),
            ("upper", "replace"),
        ]

        for i, m1 in enumerate(methods):
            for j, m2 in enumerate(methods):
                if i < j and (m1, m2) in order_sensitive_pairs:
                    anomalies.append({
                        "analyzer": "dataflow",
                        "severity": "low",
                        "title": f"Order-sensitive method chain: .{m1}() before .{m2}()",
                        "lines": [node.lineno],
                        "detail": (f"Method chain at L{node.lineno} calls .{m1}() before .{m2}(). "
                                  f"The order of these operations can affect the result. "
                                  f"Verify that this ordering is intentional. "
                                  f"Code: {_get_line(source_lines, node.lineno)}"),
                    })

    return anomalies


# --- Pattern 3: Early transformation in a loop/accumulator ---

def _check_early_transformation(func_node, source_lines):
    """Detect transformations inside a loop that should be done after.

    Pattern: accumulate pieces in a loop, transform each piece,
    then join — but the transform should be done on the joined result.
    E.g., replacing quotes in each CONTINUE card piece instead of
    after reconstructing the full string.
    """
    anomalies = []

    for node in ast.walk(func_node):
        if not isinstance(node, (ast.For, ast.While)):
            continue

        # Find .append() calls in the loop body
        appends = []
        transforms_in_loop = []

        for child in ast.walk(node):
            # Track appends to a list
            if (isinstance(child, ast.Call) and isinstance(child.func, ast.Attribute)
                    and child.func.attr == "append"):
                appends.append({"line": child.lineno})

            # Track string transformations in the loop
            if (isinstance(child, ast.Call) and isinstance(child.func, ast.Attribute)
                    and child.func.attr in ("replace", "translate", "encode", "decode",
                                            "strip", "lstrip", "rstrip")):
                transforms_in_loop.append({
                    "line": child.lineno,
                    "method": child.func.attr,
                    "code": _get_line(source_lines, child.lineno),
                })

        # If we have both appends and transforms, the transform might be premature
        if appends and transforms_in_loop:
            # Check if there's a join after the loop
            # (look at the next few lines after the loop)
            loop_end = node.end_lineno if hasattr(node, 'end_lineno') else node.lineno + 20
            has_join_after = False
            for sibling in ast.walk(func_node):
                if (isinstance(sibling, ast.Call) and isinstance(sibling.func, ast.Attribute)
                        and sibling.func.attr == "join"
                        and hasattr(sibling, 'lineno') and sibling.lineno > loop_end):
                    has_join_after = True
                    break

            if has_join_after:
                for t in transforms_in_loop:
                    anomalies.append({
                        "analyzer": "dataflow",
                        "severity": "medium",
                        "title": f"Transformation .{t['method']}() inside accumulation loop",
                        "lines": [t["line"]],
                        "detail": (f".{t['method']}() at L{t['line']} is inside a loop that "
                                  f"accumulates values (append). The accumulated values are "
                                  f"joined later. If .{t['method']}() depends on the complete "
                                  f"string (not individual pieces), it should be applied AFTER "
                                  f"the join, not inside the loop. "
                                  f"Code: {t['code']}"),
                    })

    return anomalies


# --- Pattern 4: Cross-function data flow ---

def _check_cross_function_flow(tree, source_lines):
    """Check if a function returns transformed data that callers might misuse.

    Limitation / known false positives: call-site matching uses bare
    ``ast.Name.id`` or ``ast.Attribute.attr`` without scope resolution, so
    identically-named functions in different modules or classes will collide.
    Severity is kept at "low" for this reason.
    """
    anomalies = []

    # Collect all functions and their return transformations
    func_transforms = {}
    for node in ast.walk(tree):
        if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            continue
        for child in ast.walk(node):
            if isinstance(child, ast.Return) and child.value:
                transform = _detect_transform(child.value)
                if transform:
                    func_transforms[node.name] = {
                        "transform": transform,
                        "line": child.lineno,
                    }

    # Check if callers re-apply the same transformation.
    # NOTE: Matches by bare name/attr without scope — may produce false
    # positives when different functions share a name across scopes.
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        func_name = None
        if isinstance(node.func, ast.Name):
            func_name = node.func.id
        elif isinstance(node.func, ast.Attribute):
            func_name = node.func.attr

        if func_name and func_name in func_transforms:
            # Check if the result of this call is transformed again with the same method
            # Look at the parent — is it x = func().transform()?
            parent_transform = _detect_transform(node)
            if parent_transform and parent_transform == func_transforms[func_name]["transform"]:
                anomalies.append({
                    "analyzer": "dataflow",
                    "severity": "low",
                    "title": f"Double transformation: {func_name}() already applies {parent_transform}",
                    "lines": [node.lineno],
                    "detail": (f"Call to '{func_name}()' at L{node.lineno} is followed by "
                              f"{parent_transform}, but '{func_name}()' already applies "
                              f"{parent_transform} at L{func_transforms[func_name]['line']}. "
                              f"This may be a double-transformation bug."),
                })

    return anomalies


# --- Helpers ---

def _extract_method_chain(node):
    """Extract a chain of method calls: x.a().b().c() → [c, b, a]."""
    chain = []
    current = node
    while isinstance(current, ast.Call) and isinstance(current.func, ast.Attribute):
        chain.append({
            "method": current.func.attr,
            "line": current.lineno,
        })
        current = current.func.value
    return chain


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
                      "casefold", "replace", "encode", "decode",
                      "translate", "split", "join"):
            return f".{method}()"
    for child in ast.walk(node):
        if isinstance(child, ast.Call) and isinstance(child.func, ast.Attribute):
            if child.func.attr in ("upper", "lower", "strip", "casefold",
                                   "replace", "encode", "decode"):
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
