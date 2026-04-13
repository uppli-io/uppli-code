"""Predicate logic analyzer — verify mathematical/logical properties.

Applies formal verification principles:
  ∀ (for all), ∃ (exists), → (implies), ¬ (negation)

Checks properties like:
- Associativity: a/b/c must be handled consistently
- Boundary conditions: edge values (0, -1, None, empty) must be checked
- Symmetry: if f(a,b) works, f(b,a) should too (or be explicitly handled)
- Completeness: all branches of an enum/type switch must be covered
- Idempotency: repeated operations shouldn't change the result
- Commutativity: order of operations where it matters
"""

import ast
import re


def analyze(source, source_lines, **kwargs):
    """Run predicate logic analysis.

    Returns:
        list of anomaly dicts
    """
    tree = ast.parse(source)
    anomalies = []

    anomalies.extend(_check_boundary_conditions(tree, source_lines))
    anomalies.extend(_check_operator_associativity(tree, source_lines))
    anomalies.extend(_check_incomplete_type_switch(tree, source_lines))
    anomalies.extend(_check_negation_symmetry(tree, source_lines))
    anomalies.extend(_check_division_by_zero_guard(tree, source_lines))

    return anomalies


def _check_boundary_conditions(tree, source_lines):
    """∀ comparison with numeric literal: boundary values must be considered.

    If code checks `x < 70`, there should be handling for x == 70 (off-by-one).
    If code checks `len(x) == 0`, there should be handling for len(x) == 1.
    """
    anomalies = []
    comparisons = []

    for node in ast.walk(tree):
        if not isinstance(node, ast.Compare):
            continue
        for op, comparator in zip(node.ops, node.comparators):
            if isinstance(comparator, ast.Constant) and isinstance(comparator.value, (int, float)):
                comparisons.append({
                    "line": node.lineno,
                    "op": type(op).__name__,
                    "value": comparator.value,
                    "code": _get_line(source_lines, node.lineno),
                })

    # Check for off-by-one patterns: < N and >= N without == N
    for comp in comparisons:
        val = comp["value"]
        op = comp["op"]
        # Look for boundary: x < N without x == N or x <= N-1
        if op in ("Lt", "LtE", "Gt", "GtE"):
            has_eq = any(
                c["value"] == val and c["op"] == "Eq"
                for c in comparisons
            )
            has_adjacent = any(
                c["value"] in (val - 1, val + 1) and c["op"] in ("Eq", "Lt", "LtE", "Gt", "GtE")
                for c in comparisons
            )
            if not has_eq and not has_adjacent and val not in (0, 1):
                anomalies.append({
                    "analyzer": "predicates",
                    "severity": "low",
                    "title": f"Boundary value {val} not explicitly handled",
                    "lines": [comp["line"]],
                    "detail": (f"Comparison at L{comp['line']} uses {op} {val} but the "
                              f"exact boundary value {val} is never checked with ==. "
                              f"Verify off-by-one correctness. "
                              f"Code: {comp['code']}"),
                })

    return anomalies


def _check_operator_associativity(tree, source_lines):
    """∀ chained division/subtraction: verify left vs right associativity.

    a / b / c can mean (a/b)/c or a/(b*c) depending on the parser.
    Flag chained non-associative operators in grammar rules or string parsing.
    """
    anomalies = []

    for node in ast.walk(tree):
        if not isinstance(node, ast.BinOp):
            continue
        # Chained division: a / b / c → nested BinOp(/, BinOp(/))
        if isinstance(node.op, (ast.Div, ast.Sub, ast.Mod)):
            if isinstance(node.left, ast.BinOp) and type(node.left.op) == type(node.op):
                anomalies.append({
                    "analyzer": "predicates",
                    "severity": "medium",
                    "title": f"Chained {type(node.op).__name__} — check associativity",
                    "lines": [node.lineno],
                    "detail": (f"Chained {type(node.op).__name__} at L{node.lineno}. "
                              f"a/b/c is left-associative in Python ((a/b)/c) but in "
                              f"some domains (units, math notation) it means a/(b*c). "
                              f"Verify the intended semantics. "
                              f"Code: {_get_line(source_lines, node.lineno)}"),
                })
            if isinstance(node.right, ast.BinOp) and type(node.right.op) == type(node.op):
                anomalies.append({
                    "analyzer": "predicates",
                    "severity": "medium",
                    "title": f"Nested {type(node.op).__name__} in right operand",
                    "lines": [node.lineno],
                    "detail": (f"Right-nested {type(node.op).__name__} at L{node.lineno}. "
                              f"Code: {_get_line(source_lines, node.lineno)}"),
                })

    # Also check grammar rules in PLY parsers (p_xxx functions)
    for node in ast.walk(tree):
        if not isinstance(node, ast.FunctionDef):
            continue
        if not node.name.startswith("p_"):
            continue
        # Check docstring for grammar rules with chained operators
        if node.body and isinstance(node.body[0], ast.Expr):
            if isinstance(node.body[0].value, ast.Constant):
                docstring = str(node.body[0].value.value)
                # Look for rules like: expr DIVISION expr DIVISION expr
                # or: unit_expression DIVISION combined_units
                if re.search(r'DIVISION|DIV|SLASH', docstring, re.IGNORECASE):
                    anomalies.append({
                        "analyzer": "predicates",
                        "severity": "medium",
                        "title": "Grammar rule with division — verify associativity",
                        "lines": [node.lineno],
                        "detail": (f"Parser rule '{node.name}' at L{node.lineno} contains "
                                  f"division. Verify that the rule handles left/right "
                                  f"associativity correctly for expressions like a/b/c. "
                                  f"Grammar: {docstring.strip()[:100]}"),
                    })

    return anomalies


def _check_incomplete_type_switch(tree, source_lines):
    """∀ if/elif chain comparing same variable: all cases should be covered.

    If code checks type == "A", type == "B", type == "C" but not "D",
    and "D" is a valid value, flag it.
    """
    anomalies = []

    for node in ast.walk(tree):
        if not isinstance(node, ast.If):
            continue

        # Collect all string comparisons in the if/elif chain
        cases = _collect_if_chain_cases(node)
        if len(cases) < 3:
            continue

        # Check if there's an else clause at the end
        has_else = _has_final_else(node)
        if not has_else:
            var_name = cases[0].get("var", "?")
            anomalies.append({
                "analyzer": "predicates",
                "severity": "low",
                "title": f"if/elif chain on '{var_name}' has no else clause",
                "lines": [node.lineno],
                "detail": (f"if/elif chain at L{node.lineno} checks {len(cases)} cases "
                          f"for '{var_name}' ({', '.join(repr(c['value']) for c in cases[:5])}) "
                          f"but has no final else clause to catch unexpected values."),
            })

    return anomalies


def _collect_if_chain_cases(node):
    """Collect all string comparison cases in an if/elif chain."""
    cases = []

    def _extract_case(test):
        if isinstance(test, ast.Compare):
            for op, comparator in zip(test.ops, test.comparators):
                if isinstance(op, ast.Eq) and isinstance(comparator, ast.Constant):
                    if isinstance(comparator.value, str):
                        var = ""
                        if isinstance(test.left, ast.Name):
                            var = test.left.id
                        elif isinstance(test.left, ast.Attribute):
                            var = test.left.attr
                        cases.append({"value": comparator.value, "var": var})

    _extract_case(node.test)
    for elif_node in node.orelse:
        if isinstance(elif_node, ast.If):
            _extract_case(elif_node.test)
            # Recurse
            for n in elif_node.orelse:
                if isinstance(n, ast.If):
                    _extract_case(n.test)

    return cases


def _has_final_else(node):
    """Check if an if/elif chain ends with an else clause."""
    current = node
    while current.orelse:
        if len(current.orelse) == 1 and isinstance(current.orelse[0], ast.If):
            current = current.orelse[0]
        else:
            # Has an else body (not elif)
            return not isinstance(current.orelse[0], ast.If) if current.orelse else False
    return False


def _check_negation_symmetry(tree, source_lines):
    """∀ comparison: if we check P(x), the negation ¬P(x) should be handled.

    If code does `if x is None: raise` there should be corresponding
    handling when x is not None. Flag one-sided checks.
    """
    anomalies = []

    for node in ast.walk(tree):
        if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            continue

        # Collect all None checks in the function
        none_checks = []
        for child in ast.walk(node):
            if isinstance(child, ast.Compare):
                for op, comp in zip(child.ops, child.comparators):
                    if isinstance(comp, ast.Constant) and comp.value is None:
                        if isinstance(op, ast.Is):
                            none_checks.append({"line": child.lineno, "type": "is_none"})
                        elif isinstance(op, ast.IsNot):
                            none_checks.append({"line": child.lineno, "type": "is_not_none"})

        # Check for one-sided None checks (only `is None` without `is not None` or vice versa)
        is_none = [c for c in none_checks if c["type"] == "is_none"]
        is_not_none = [c for c in none_checks if c["type"] == "is_not_none"]

        if is_none and not is_not_none and len(is_none) >= 2:
            anomalies.append({
                "analyzer": "predicates",
                "severity": "low",
                "title": f"Multiple 'is None' checks without 'is not None' in '{node.name}'",
                "lines": [c["line"] for c in is_none],
                "detail": (f"Function '{node.name}' has {len(is_none)} 'is None' checks "
                          f"but no 'is not None'. Verify that the non-None case is handled."),
            })

    return anomalies


def _check_division_by_zero_guard(tree, source_lines):
    """∀ division: denominator should be checked for zero.

    Flag divisions where the denominator is a variable that's never
    checked for zero/None/empty.
    """
    anomalies = []

    for node in ast.walk(tree):
        if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            continue

        # Collect all divisions
        divisions = []
        zero_checks = set()

        for child in ast.walk(node):
            if isinstance(child, ast.BinOp) and isinstance(child.op, (ast.Div, ast.FloorDiv)):
                if isinstance(child.right, ast.Name):
                    divisions.append({
                        "line": child.lineno,
                        "var": child.right.id,
                        "code": _get_line(source_lines, child.lineno),
                    })
            # Track zero checks
            if isinstance(child, ast.Compare):
                for op, comp in zip(child.ops, child.comparators):
                    if isinstance(comp, ast.Constant) and comp.value == 0:
                        if isinstance(child.left, ast.Name):
                            zero_checks.add(child.left.id)

        for div in divisions:
            if div["var"] not in zero_checks:
                anomalies.append({
                    "analyzer": "predicates",
                    "severity": "low",
                    "title": f"Division by '{div['var']}' without zero check",
                    "lines": [div["line"]],
                    "detail": (f"Division at L{div['line']} uses '{div['var']}' as denominator "
                              f"but no zero check found in this function. "
                              f"Code: {div['code']}"),
                })

    return anomalies


def _get_line(source_lines, lineno):
    if 1 <= lineno <= len(source_lines):
        return source_lines[lineno - 1].strip()
    return ""
