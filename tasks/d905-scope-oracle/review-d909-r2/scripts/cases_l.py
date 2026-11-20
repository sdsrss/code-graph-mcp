CASES = {
 "L01 js default call + nested fn": {"m.js":
  "function make() {}\n\nfunction f(x = make()) {\n  function make() {}\n  return make();\n}\n\nconst g = (y = make()) => { const make = () => 1; return make(); };\nclass C { m(z = make()) { function make() {} return make(); } }\n"},
 "L02 ts default + nested class + global-like names": {"m.ts":
  "class Row {}\nfunction make(): Row { return new Row(); }\n\nfunction f(x: Row = make()): Row {\n  class Row {}\n  function make(): Row { return new Row(); }\n  return make();\n}\nfunction g() { return make(); }\n"},
 "L03 rust closure + nested fn": {"src/lib.rs":
  "fn make() -> i32 { 1 }\n\nfn f() -> i32 {\n    fn make() -> i32 { 2 }\n    let c = |x: i32| make() + x;\n    c(make())\n}\n\nfn g() -> i32 { make() }\n"},
 "L04 dart nested fn + class": {"lib/m.dart":
  "int make() => 1;\n\nclass Row {}\n\nint f([int x = 0]) {\n  int make() => 2;\n  return make();\n}\n\nint g() => make();\n\nvoid main() {\n  f();\n  g();\n  Row();\n}\n"},
 "L05 tsx component nested": {"m.tsx":
  "function Item() { return <div/>; }\nexport function List({ render = Item }) {\n  function Item() { return <span/>; }\n  return <Item />;\n}\n"},
 "L06 js 'global' identifier names": {"m.js":
  "function h() {}\nfunction f() {\n  var global = 1;\n  function h() {}\n  return h();\n}\nfunction g() { return h(); }\n"},
}
