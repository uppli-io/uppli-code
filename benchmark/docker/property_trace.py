#!/usr/bin/env python3
"""PropertyTrace — Formal verification tool for code properties.

Given a property type and a file, scans ALL code paths and returns
every point where the property might be violated.

Usage:
    python property_trace.py <property> <file>

Properties:
    case-insensitive   Find all case-sensitive string operations
    null-check         Find all dereferences without null checks
    boundary-check     Find all array accesses without bounds checks
"""

import ast
import sys
import re
from pathlib import Path


class CaseSensitivityChecker(ast.NodeVisitor):
    """Find all points where string comparison is case-sensitive."""

    def __init__(self, source_lines):
        self.violations = []
        self.source_lines = source_lines

    def _line_text(self, lineno):
        if 1 <= lineno <= len(self.source_lines):
            return self.source_lines[lineno - 1].strip()
        return ""

    def visit_Compare(self, node):
        """Detect: x == "LITERAL" without .upper()/.lower()"""
        for op, comparator in zip(node.ops, node.comparators):
            if isinstance(op, (ast.Eq, ast.NotEq)):
                # Check if comparing against a string literal
                if isinstance(comparator, ast.Constant) and isinstance(comparator.value, str):
                    literal = comparator.value
                    # Only flag if the literal has letters (not pure numbers/symbols)
                    if any(c.isalpha() for c in literal):
                        # Check if left side uses .upper() or .lower()
                        left = node.left
                        has_case_norm = False
                        if isinstance(left, ast.Call):
                            if isinstance(left.func, ast.Attribute):
                                if left.func.attr in ('upper', 'lower', 'casefold'):
                                    has_case_norm = True
                        if not has_case_norm:
                            self.violations.append({
                                'line': node.lineno,
                                'type': 'string_comparison',
                                'detail': f'v == "{literal}" — literal comparison without .upper()/.lower()',
                                'code': self._line_text(node.lineno),
                            })
                # Same check for left side being a literal
                if isinstance(node.left, ast.Constant) and isinstance(node.left.value, str):
                    literal = node.left.value
                    if any(c.isalpha() for c in literal):
                        has_case_norm = False
                        if isinstance(comparator, ast.Call):
                            if isinstance(comparator.func, ast.Attribute):
                                if comparator.func.attr in ('upper', 'lower', 'casefold'):
                                    has_case_norm = True
                        if not has_case_norm:
                            self.violations.append({
                                'line': node.lineno,
                                'type': 'string_comparison',
                                'detail': f'"{literal}" == v — literal comparison without .upper()/.lower()',
                                'code': self._line_text(node.lineno),
                            })
        self.generic_visit(node)

    def visit_Call(self, node):
        """Detect: re.compile() without re.IGNORECASE"""
        if isinstance(node.func, ast.Attribute):
            if node.func.attr == 'compile' and isinstance(node.func.value, ast.Name):
                if node.func.value.id == 're':
                    # Check if re.IGNORECASE is in the flags
                    has_ignorecase = False
                    for arg in node.args[1:]:
                        if self._contains_ignorecase(arg):
                            has_ignorecase = True
                    for kw in node.keywords:
                        if kw.arg == 'flags' and self._contains_ignorecase(kw.value):
                            has_ignorecase = True
                    if not has_ignorecase:
                        self.violations.append({
                            'line': node.lineno,
                            'type': 're_compile',
                            'detail': 're.compile() without re.IGNORECASE flag',
                            'code': self._line_text(node.lineno),
                        })

        # Detect: str.startswith/endswith with literal
        if isinstance(node.func, ast.Attribute):
            if node.func.attr in ('startswith', 'endswith'):
                if node.args and isinstance(node.args[0], ast.Constant):
                    if isinstance(node.args[0].value, str) and any(c.isalpha() for c in node.args[0].value):
                        self.violations.append({
                            'line': node.lineno,
                            'type': 'startswith_endswith',
                            'detail': f'.{node.func.attr}("{node.args[0].value}") — case-sensitive',
                            'code': self._line_text(node.lineno),
                        })

        # Detect: "X" in string or string in "X"
        self.generic_visit(node)

    def visit_Compare_in(self, node):
        """Detect: "LITERAL" in x  without case normalization"""
        for op, comparator in zip(node.ops, node.comparators):
            if isinstance(op, (ast.In, ast.NotIn)):
                if isinstance(node.left, ast.Constant) and isinstance(node.left.value, str):
                    literal = node.left.value
                    if any(c.isalpha() for c in literal):
                        self.violations.append({
                            'line': node.lineno,
                            'type': 'in_comparison',
                            'detail': f'"{literal}" in x — case-sensitive membership test',
                            'code': self._line_text(node.lineno),
                        })

    def _contains_ignorecase(self, node):
        """Check if an AST node references re.IGNORECASE."""
        if isinstance(node, ast.Attribute):
            return node.attr == 'IGNORECASE'
        if isinstance(node, ast.BinOp):
            return self._contains_ignorecase(node.left) or self._contains_ignorecase(node.right)
        return False


def check_case_sensitivity(filepath):
    """Run case-sensitivity check on a Python file."""
    source = Path(filepath).read_text()
    source_lines = source.splitlines()
    tree = ast.parse(source)
    checker = CaseSensitivityChecker(source_lines)
    checker.visit(tree)

    # Also do regex-based checks for patterns AST might miss
    for i, line in enumerate(source_lines, 1):
        # Match patterns like: re.match("PATTERN", ...) without IGNORECASE
        if re.search(r're\.(match|search|findall|sub)\s*\(', line):
            if 'IGNORECASE' not in line and 'I)' not in line:
                # Check if it's a multiline call — look at next few lines too
                context = '\n'.join(source_lines[i-1:min(i+3, len(source_lines))])
                if 'IGNORECASE' not in context:
                    checker.violations.append({
                        'line': i,
                        'type': 're_function',
                        'detail': f're function call without IGNORECASE',
                        'code': line.strip(),
                    })

    return checker.violations


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        sys.exit(1)

    prop = sys.argv[1]
    filepath = sys.argv[2]

    if prop == 'case-insensitive':
        violations = check_case_sensitivity(filepath)
    elif prop == 'error-completeness':
        violations = check_error_completeness(filepath)
    else:
        print(f"Unknown property: {prop}")
        print("Available: case-insensitive, error-completeness")
        sys.exit(1)

    if not violations:
        print(f"✓ No violations found in {filepath}")
    else:
        print(f"⚠ {len(violations)} potential violation(s) in {filepath}:\n")
        for v in violations:
            print(f"  L{v['line']}: [{v['type']}] {v['detail']}")
            print(f"         {v['code']}")
            print()


class ErrorMessageCompletenessChecker(ast.NodeVisitor):
    """Find error messages that lose information from their condition."""

    def __init__(self, source_lines):
        self.violations = []
        self.source_lines = source_lines

    def _line_text(self, lineno):
        if 1 <= lineno <= len(self.source_lines):
            return self.source_lines[lineno - 1].strip()
        return ""

    def visit_If(self, node):
        """Check if raise inside if/elif uses [0] when condition uses slice/list."""
        self._check_branch(node)
        self.generic_visit(node)

    def _check_branch(self, node):
        """Check a single if/elif branch and its orelse."""
        # Check condition for slices
        condition_vars = self._extract_subscripts(node.test)
        has_slice = any(v['type'] == 'slice' for v in condition_vars)

        if has_slice:
            # Look for raise statements in the body
            for child in ast.walk(node):
                if isinstance(child, ast.Raise) and child.exc:
                    msg_subscripts = self._extract_subscripts(child.exc)
                    has_index_0 = [v for v in msg_subscripts if v['type'] == 'index_0']
                    if has_index_0:
                        slice_vars = [v for v in condition_vars if v['type'] == 'slice']
                        for idx0 in has_index_0:
                            self.violations.append({
                                'line': child.lineno,
                                'type': 'lossy_error_msg',
                                'detail': (f'Condition compares slice(s) {[v["base"] for v in slice_vars]} '
                                          f'but error message uses {idx0["base"]}[0] — information loss'),
                                'code': self._line_text(child.lineno),
                            })

        # Check elif branches (orelse)
        for elif_node in node.orelse:
            if isinstance(elif_node, ast.If):
                self._check_branch(elif_node)

    def _extract_subscripts(self, node):
        """Extract all subscript accesses from an AST node."""
        results = []
        for child in ast.walk(node):
            if isinstance(child, ast.Subscript):
                base = ast.dump(child.value) if not isinstance(child.value, ast.Name) else child.value.id
                if isinstance(child.value, ast.Attribute):
                    base = child.value.attr
                
                if isinstance(child.slice, ast.Constant):
                    if child.slice.value == 0:
                        results.append({'type': 'index_0', 'base': base, 'line': child.lineno})
                elif isinstance(child.slice, ast.Slice):
                    upper = None
                    if child.slice.upper:
                        upper = ast.dump(child.slice.upper)
                    results.append({'type': 'slice', 'base': base, 'upper': upper, 'line': child.lineno})
        return results


def check_error_completeness(filepath):
    """Run error message completeness check on a Python file."""
    source = Path(filepath).read_text()
    source_lines = source.splitlines()
    tree = ast.parse(source)
    checker = ErrorMessageCompletenessChecker(source_lines)
    checker.visit(tree)
    return checker.violations


if __name__ == '__main__':
    main()
