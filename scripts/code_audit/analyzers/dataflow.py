"""DataFlowTracer — trace variable transformations through code paths.

Detects:
- Variable normalized in one path but used raw in another
- Transformation applied at wrong point in a pipeline (too early/too late)
- String method chaining where order matters (.replace before .rstrip vs after)
- Cross-function data flow (return value carries transformation)
"""

import ast
from collections import defaultdict

# Order-sensitive method pairs — frozenset at module level so the set is
# built once, not per-function.  Each tuple is (before, after) where
# calling `before` then `after` can silently lose data.
_ORDER_SENSITIVE_PAIRS = frozenset([
    ("replace", "rstrip"),
    ("replace", "lstrip"),
    ("replace", "strip"),
    ("replace", "split"),
    ("lower", "replace"),
    ("upper", "replace"),
])


def analyze(source, source_lines, **kwargs):
    """Run data flow analysis.

    Returns:
        list of anomaly dicts
    """
    tree = ast.parse(source)
    anomalies = []

    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            visitor = _DataFlowVisitor(source_lines)
            visitor.visit(node)
            anomalies.extend(visitor.check_inconsistent_transforms())
            anomalies.extend(visitor.check_pipeline_ordering())
            anomalies.extend(visitor.check_early_transformation(node))

    anomalies.extend(_check_cross_function_flow(tree, source_lines))

    return anomalies


class _DataFlowVisitor(ast.NodeVisitor):
    """Single-pass visitor that collects all data needed by the four checks.

    Instead of walking the AST four times (once per check), this visitor
    collects assignments, comparisons, method chains, and loop/append
    information in a single traversal.
    """

    def __init__(self, source_lines):
        self.source_lines = source_lines
        self.assignments = defaultdict(list)
        self.comparisons = defaultdict(list)
        self.method_chains = []        # list of (node, chain)
        self.loop_contexts = []        # list of (loop_node, appends, transforms)
        # Stack tracking the innermost loop we are inside (for early-transform detection).
        self._loop_stack = []

    # ── Generic visiting ──────────────────────────────────────────────

    def visit_Assign(self, node):
        for target in node.targets:
            if isinstance(target, ast.Name):
                transform = _detect_transform(node.value)
                self.assignments[target.id].append({
                    "line": node.lineno,
                    "transform": transform,
                })
        self.generic_visit(node)

    def visit_AugAssign(self, node):
        if isinstance(node.target, ast.Name):
            transform = _detect_transform(node.value)
            self.assignments[node.target.id].append({
                "line": node.lineno,
                "transform": transform,
            })
        self.generic_visit(node)

    def visit_Compare(self, node):
        for op, comparator in zip(node.ops, node.comparators):
            if not isinstance(op, (ast.Eq, ast.NotEq)):
                continue
            if isinstance(node.left, ast.Name):
                self.comparisons[node.left.id].append({
                    "line": node.lineno,
                    "normalized": _has_normalization(node.left),
                })
            elif isinstance(node.left, ast.Call) and isinstance(node.left.func, ast.Attribute):
                if isinstance(node.left.func.value, ast.Name):
                    self.comparisons[node.left.func.value.id].append({
                        "line": node.lineno,
                        "normalized": _has_normalization(node.left),
                    })
            if isinstance(comparator, ast.Name):
                self.comparisons[comparator.id].append({
                    "line": node.lineno,
                    "normalized": _has_normalization(comparator),
                })
        self.generic_visit(node)

    def visit_Call(self, node):
        # Collect method chains for pipeline-ordering check.
        if isinstance(node.func, ast.Attribute):
            chain = _extract_method_chain(node)
            if len(chain) >= 2:
                self.method_chains.append((node, chain))

        # Track append / transform inside loops for early-transform check.
        if self._loop_stack:
            ctx = self._loop_stack[-1]
            if isinstance(node.func, ast.Attribute):
                if node.func.attr == "append":
                    ctx["appends"].append({"line": node.lineno})
                elif node.func.attr in (
                    "replace", "translate", "encode", "decode",
                    "strip", "lstrip", "rstrip",
                ):
                    ctx["transforms"].append({
                        "line": node.lineno,
                        "method": node.func.attr,
                        "code": _get_line(self.source_lines, node.lineno),
                    })

        self.generic_visit(node)

    def _visit_loop(self, node):
        ctx = {"node": node, "appends": [], "transforms": []}
        self._loop_stack.append(ctx)
        self.generic_visit(node)
        self._loop_stack.pop()
        self.loop_contexts.append((node, ctx["appends"], ctx["transforms"]))

    def visit_For(self, node):
        self._visit_loop(node)

    def visit_While(self, node):
        self._visit_loop(node)

    # ── Checks (read collected state, no further walks) ───────────────

    def check_inconsistent_transforms(self):
        """Variable transformed in some paths but used raw in others."""
        anomalies = []
        for var_name, comp_usages in self.comparisons.items():
            transforms = self.assignments.get(var_name, [])
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
                    "detail": (
                        f"'{var_name}' is transformed ({transforms[0]['transform']}) at "
                        f"L{transform_lines[0] if transform_lines else '?'} "
                        f"but compared without transformation at L{raw_lines[0]}. "
                        f"Code: {_get_line(self.source_lines, raw_lines[0])}"
                    ),
                })
        return anomalies

    def check_pipeline_ordering(self):
        """Detect string method chains where order might matter."""
        anomalies = []
        for node, chain in self.method_chains:
            methods = [c["method"] for c in chain]
            for i, m1 in enumerate(methods):
                for j, m2 in enumerate(methods):
                    if i < j and (m1, m2) in _ORDER_SENSITIVE_PAIRS:
                        anomalies.append({
                            "analyzer": "dataflow",
                            "severity": "low",
                            "title": f"Order-sensitive method chain: .{m1}() before .{m2}()",
                            "lines": [node.lineno],
                            "detail": (
                                f"Method chain at L{node.lineno} calls .{m1}() before .{m2}(). "
                                f"The order of these operations can affect the result. "
                                f"Verify that this ordering is intentional. "
                                f"Code: {_get_line(self.source_lines, node.lineno)}"
                            ),
                        })
        return anomalies

    def check_early_transformation(self, func_node):
        """Detect transformations inside a loop that should be done after."""
        anomalies = []
        for loop_node, appends, transforms_in_loop in self.loop_contexts:
            if not appends or not transforms_in_loop:
                continue

            loop_end = loop_node.end_lineno if hasattr(loop_node, 'end_lineno') else loop_node.lineno + 20
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
                        "detail": (
                            f".{t['method']}() at L{t['line']} is inside a loop that "
                            f"accumulates values (append). The accumulated values are "
                            f"joined later. If .{t['method']}() depends on the complete "
                            f"string (not individual pieces), it should be applied AFTER "
                            f"the join, not inside the loop. "
                            f"Code: {t['code']}"
                        ),
                    })
        return anomalies


# --- Pattern 4: Cross-function data flow ---

def _check_cross_function_flow(tree, source_lines):
    """Check if a function returns transformed data that callers might misuse.

    Uses a single walk to collect both function return transforms and call
    sites, then cross-references them.

    Limitation / known false positives: call-site matching uses bare
    ``ast.Name.id`` or ``ast.Attribute.attr`` without scope resolution, so
    identically-named functions in different modules or classes will collide.
    Severity is kept at "low" for this reason.
    """
    anomalies = []
    func_transforms = {}
    call_sites = []

    for node in ast.walk(tree):
        # Collect function return transformations.
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            for child in ast.walk(node):
                if isinstance(child, ast.Return) and child.value:
                    transform = _detect_transform(child.value)
                    if transform:
                        func_transforms[node.name] = {
                            "transform": transform,
                            "line": child.lineno,
                        }

        # Collect call sites.
        if isinstance(node, ast.Call):
            func_name = None
            if isinstance(node.func, ast.Name):
                func_name = node.func.id
            elif isinstance(node.func, ast.Attribute):
                func_name = node.func.attr
            if func_name is not None:
                call_sites.append((func_name, node))

    # Cross-reference: does a caller re-apply the same transform?
    for func_name, node in call_sites:
        if func_name not in func_transforms:
            continue
        parent_transform = _detect_transform(node)
        if parent_transform and parent_transform == func_transforms[func_name]["transform"]:
            anomalies.append({
                "analyzer": "dataflow",
                "severity": "low",
                "title": f"Double transformation: {func_name}() already applies {parent_transform}",
                "lines": [node.lineno],
                "detail": (
                    f"Call to '{func_name}()' at L{node.lineno} is followed by "
                    f"{parent_transform}, but '{func_name}()' already applies "
                    f"{parent_transform} at L{func_transforms[func_name]['line']}. "
                    f"This may be a double-transformation bug."
                ),
            })

    return anomalies


# --- Helpers ---

def _extract_method_chain(node):
    """Extract a chain of method calls: x.a().b().c() -> [c, b, a]."""
    chain = []
    current = node
    while isinstance(current, ast.Call) and isinstance(current.func, ast.Attribute):
        chain.append({
            "method": current.func.attr,
            "line": current.lineno,
        })
        current = current.func.value
    return chain


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
