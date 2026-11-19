CASES = {
    "J01 js nested classes in sibling functions": {"m.js":
        "function a() {\n  class Row {}\n  return new Row();\n}\n\nfunction b() {\n  class Row {}\n  return new Row();\n}\n\nfunction c() {\n  return Row();\n}\n"},
    "J02 js module class vs nested class": {"m.js":
        "class Row {}\n\nfunction a() {\n  class Row {}\n  return Row();\n}\n\nfunction b() {\n  return Row();\n}\n"},
    "J03 ts nested classes": {"m.ts":
        "export class Row {}\n\nexport function a(): Row {\n  class Row {}\n  const r: Row = new Row();\n  return Row();\n}\n\nfunction b() {\n  return Row();\n}\n"},
    "J04 dart classes": {"lib/m.dart":
        "class Row {}\n\nRow a() {\n  return Row();\n}\n\nclass Other {\n  Row make() => Row();\n}\n"},
}
