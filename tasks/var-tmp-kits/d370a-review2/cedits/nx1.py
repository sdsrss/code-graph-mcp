EDITS = [
 [("networkx/algorithms/threshold.py", "replace", "def triangles(", "def triangles_renamed(")],
 [("networkx/__init__.py", "append", "\nfrom networkx.classes.graph import Graph\n")],
 [("networkx/classes/function.py", "replace", "def density(", "def density_renamed("),
  ("networkx/utils/misc.py", "append", "\n\ndef density(G):\n    return 0\n")],
 [("networkx/algorithms/shortest_paths/generic.py", "replace", "def shortest_path(", "def shortest_path_x(")],
]
