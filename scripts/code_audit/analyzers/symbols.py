"""SymbolTable analyzer — list all usages of every symbol in a file.

For each symbol (variable, string literal, attribute), reports:
- Line number
- Context (the enclosing statement as a one-liner)
- Usage type (assignment, comparison, call argument, etc.)
"""

import ast
from collections import defaultdict


def analyze(source, source_lines, focus_symbols=None):
    """Run symbol table analysis.

    Args:
        source: Python source code string
        source_lines: list of source lines
        focus_symbols: optional list of symbol names to focus on

    Returns:
        dict with 'symbols' key mapping symbol names to usage lists
    """
    tree = ast.parse(source)
    collector = _SymbolCollector(source_lines)
    collector.visit(tree)

    # Also collect string literal usages (important for case-sensitivity bugs)
    literal_collector = _LiteralCollector(source_lines)
    literal_collector.visit(tree)

    symbols = collector.symbols
    # Merge literals into symbols
    for lit, usages in literal_collector.literals.items():
        key = repr(lit)
        symbols[key].extend(usages)

    # Filter to focus_symbols if provided
    if focus_symbols:
        focus_lower = {s.lower() for s in focus_symbols}
        symbols = {
            k: v for k, v in symbols.items()
            if k.lower() in focus_lower or k.strip("'\"").lower() in focus_lower
        }
    else:
        # Only report symbols that appear 2+ times (reduce noise)
        symbols = {k: v for k, v in symbols.items() if len(v) >= 2}

    # Sort by number of usages (most used first), cap at 20 symbols
    sorted_symbols = dict(
        sorted(symbols.items(), key=lambda x: -len(x[1]))[:20]
    )

    return {"symbols": sorted_symbols}


class _SymbolCollector(ast.NodeVisitor):
    """Collect all Name and Attribute references."""

    def __init__(self, source_lines):
        self.symbols = defaultdict(list)
        self.source_lines = source_lines

    def _line(self, lineno):
        if 1 <= lineno <= len(self.source_lines):
            return self.source_lines[lineno - 1].strip()
        return ""

    def _usage_type(self, node):
        """Infer usage type from parent context."""
        # This is approximate — full parent tracking would need a custom walker
        return "reference"

    def visit_Name(self, node):
        self.symbols[node.id].append({
            "line": node.lineno,
            "context": self._line(node.lineno),
        })
        self.generic_visit(node)

    def visit_Attribute(self, node):
        self.symbols[node.attr].append({
            "line": node.lineno,
            "context": self._line(node.lineno),
        })
        self.generic_visit(node)


class _LiteralCollector(ast.NodeVisitor):
    """Collect all string literal usages with context."""

    def __init__(self, source_lines):
        self.literals = defaultdict(list)
        self.source_lines = source_lines

    def _line(self, lineno):
        if 1 <= lineno <= len(self.source_lines):
            return self.source_lines[lineno - 1].strip()
        return ""

    def visit_Constant(self, node):
        if isinstance(node.value, str) and len(node.value) <= 20:
            # Only track short string literals (likely identifiers/commands)
            if any(c.isalpha() for c in node.value):
                self.literals[node.value].append({
                    "line": node.lineno,
                    "context": self._line(node.lineno),
                })
        self.generic_visit(node)
