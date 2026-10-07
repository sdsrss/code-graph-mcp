//! Shape corpus for [`super::scan_source`] — written before the scanner.
//!
//! Every row is one small source file, the name it is scanned for, and the
//! exact `(line, shape)` list the scan must return. An empty list is a
//! look-alike: a comment, an unrelated string, an identifier that merely
//! contains the name, the definition itself, a direct call, an import, a
//! parameter. Rows are grouped per language; each group has both kinds.

use super::{scan_source, scan_source_with_defs, Shape};

type Row = (
    &'static str,
    &'static str,
    &'static str,
    &'static [(usize, Shape)],
);

use Shape::{EventName as E, FunctionReference as F, Reflection as R, StringKey as K, Symbol as S};

const CORPUS: &[Row] = &[
    // ---- JavaScript / TypeScript -------------------------------------------
    (
        "javascript",
        "handlers[\"save\"](doc);\n",
        "save",
        &[(1, K)],
    ),
    (
        "javascript",
        "const table = { \"save\": onSave };\n",
        "save",
        &[(1, K)],
    ),
    ("javascript", "const fn = obj[`save`];\n", "save", &[(1, K)]),
    (
        "javascript",
        "emitter.on(\"save\", handler);\n",
        "save",
        &[(1, E)],
    ),
    ("javascript", "bus.emit('save', doc)\n", "save", &[(1, E)]),
    (
        "javascript",
        "el.addEventListener(\"save\", cb);\n",
        "save",
        &[(1, E)],
    ),
    (
        "javascript",
        "await ipcRenderer.invoke(\"save\");\n",
        "save",
        &[(1, E)],
    ),
    ("javascript", "app.post(\"/x\", save);\n", "save", &[(1, F)]),
    (
        "javascript",
        "const actions = { save, load };\n",
        "save",
        &[(1, F)],
    ),
    (
        "javascript",
        "setTimeout(this.save, 10);\n",
        "save",
        &[(1, F)],
    ),
    ("javascript", "module.exports = save;\n", "save", &[(1, F)]),
    (
        "typescript",
        "export const handlers = [save, load];\n",
        "save",
        &[(1, F)],
    ),
    (
        "typescript",
        "const h = {\n  a: 1,\n  onSave: save,\n};\n",
        "save",
        &[(3, F)],
    ),
    ("javascript", "// handlers[\"save\"](doc)\n", "save", &[]),
    (
        "javascript",
        "/* emit(\"save\")\n   app.post('/', save) */\n",
        "save",
        &[],
    ),
    ("javascript", "console.log(\"save failed\");\n", "save", &[]),
    ("javascript", "log(\"save\");\n", "save", &[]),
    (
        "javascript",
        "saveAll(x); autosave(y); save_as(z); $save(q);\n",
        "save",
        &[],
    ),
    (
        "javascript",
        "function save(doc) { return 1; }\n",
        "save",
        &[],
    ),
    ("javascript", "save(doc);\n", "save", &[]),
    (
        "javascript",
        "import { save, load } from \"./store\";\n",
        "save",
        &[],
    ),
    (
        "javascript",
        "import {\n  load,\n  save,\n} from \"./store\";\n",
        "save",
        &[],
    ),
    ("javascript", "export { save };\n", "save", &[]),
    ("javascript", "export default save;\n", "save", &[]),
    (
        "javascript",
        "const { save } = require(\"./store\");\n",
        "save",
        &[],
    ),
    ("javascript", "if (this.save) { x(); }\n", "save", &[]),
    ("javascript", "const f = (a, save) => a;\n", "save", &[]),
    (
        "javascript",
        "x = cond ? \"save\" : \"load\";\n",
        "save",
        &[],
    ),
    (
        "javascript",
        "switch (a) { case \"save\": go(); }\n",
        "save",
        &[],
    ),
    ("javascript", "obj.save = function () {};\n", "save", &[]),
    ("javascript", "const x = { save: 1 };\n", "save", &[]),
    ("javascript", "const s = `${save}`;\n", "save", &[]),
    ("javascript", "if (x === save) {}\n", "save", &[]),
    // Found on express: `onerror: logerror.bind(this)` hands the function
    // over through its own `.bind` / `.call` / `.apply`.
    (
        "javascript",
        "const h = { onerror: logerror.bind(this) };\n",
        "logerror",
        &[(1, F)],
    ),
    (
        "javascript",
        "handler.call(ctx, a);\n",
        "handler",
        &[(1, F)],
    ),
    ("typescript", "fn.apply(null, args)\n", "fn", &[(1, F)]),
    ("javascript", "const n = save.length;\n", "save", &[]),
    ("python", "x = save.bind(y)\n", "save", &[]),
    (
        "typescript",
        "class A { private save(): void {} }\n",
        "save",
        &[],
    ),
    (
        "typescript",
        "type H = { save?: () => void };\n",
        "save",
        &[],
    ),
    // ---- Python ------------------------------------------------------------
    ("python", "getattr(obj, \"save\")()\n", "save", &[(1, R)]),
    (
        "python",
        "fn = getattr(self, 'save', None)\n",
        "save",
        &[(1, R)],
    ),
    (
        "python",
        "if hasattr(obj, \"save\"):\n    pass\n",
        "save",
        &[(1, R)],
    ),
    (
        "python",
        "call = operator.methodcaller(\"save\")\n",
        "save",
        &[(1, R)],
    ),
    (
        "python",
        "HANDLERS = {\"save\": save, \"load\": load}\n",
        "save",
        &[(1, K)],
    ),
    ("python", "handlers[\"save\"](doc)\n", "save", &[(1, K)]),
    (
        "python",
        "threading.Thread(target=save).start()\n",
        "save",
        &[(1, F)],
    ),
    ("python", "callbacks.append(self.save)\n", "save", &[(1, F)]),
    ("python", "handler = save\nrun()\n", "save", &[(1, F)]),
    ("python", "# getattr(obj, \"save\")\n", "save", &[]),
    (
        "python",
        "def f():\n    \"\"\"Call save to persist; getattr(x, \"save\").\"\"\"\n",
        "save",
        &[],
    ),
    ("python", "print(\"save\")\n", "save", &[]),
    ("python", "def save(self, doc):\n    pass\n", "save", &[]),
    ("python", "def run(self, save):\n    pass\n", "save", &[]),
    (
        "python",
        "def run(self, save=True):\n    pass\n",
        "save",
        &[],
    ),
    ("python", "run(save=True)\n", "save", &[]),
    ("python", "self.save(doc)\n", "save", &[]),
    ("python", "from store import save\n", "save", &[]),
    (
        "python",
        "from store import (\n    load,\n    save,\n)\n",
        "save",
        &[],
    ),
    ("python", "if save:\n    pass\n", "save", &[]),
    ("python", "autosave = 1\nsave_all()\n", "save", &[]),
    ("python", "x = \"save\"\n", "save", &[]),
    ("python", "@save\ndef g():\n    pass\n", "save", &[]),
    ("python", "s = f\"{save}\"\n", "save", &[]),
    // ---- Ruby --------------------------------------------------------------
    ("ruby", "obj.send(:save)\n", "save", &[(1, R)]),
    ("ruby", "obj.public_send(:save, x)\n", "save", &[(1, R)]),
    ("ruby", "m = method(:save)\n", "save", &[(1, R)]),
    (
        "ruby",
        "return unless respond_to?(:save)\n",
        "save",
        &[(1, R)],
    ),
    ("ruby", "obj.send \"save\"\n", "save", &[(1, R)]),
    ("ruby", "before_action :save\n", "save", &[(1, S)]),
    (
        "ruby",
        "HANDLERS = { \"save\" => :handle }\n",
        "save",
        &[(1, K)],
    ),
    ("ruby", "# obj.send(:save)\n", "save", &[]),
    ("ruby", "def save(record)\nend\n", "save", &[]),
    ("ruby", "record.save\nfoo(save)\n", "save", &[]),
    (
        "ruby",
        "before_action :save!\nobj.send(:saved?)\n",
        "save",
        &[],
    ),
    ("ruby", "Foo::save\n", "save", &[]),
    ("ruby", "opts = { save: true }\n", "save", &[]),
    ("ruby", "puts \"save\"\n", "save", &[]),
    // ---- Go ----------------------------------------------------------------
    (
        "go",
        "v.MethodByName(\"Save\").Call(nil)\n",
        "Save",
        &[(1, R)],
    ),
    (
        "go",
        "http.HandleFunc(\"/save\", Save)\n",
        "Save",
        &[(1, F)],
    ),
    (
        "go",
        "var handlers = map[string]func(){\"Save\": Save}\n",
        "Save",
        &[(1, K)],
    ),
    ("go", "sort.Slice(xs, less)\n", "less", &[(1, F)]),
    ("go", "// v.MethodByName(\"Save\")\n", "Save", &[]),
    ("go", "func Save(w http.ResponseWriter) {}\n", "Save", &[]),
    (
        "go",
        "func (s *Store) Save() error {\n\treturn nil\n}\n",
        "Save",
        &[],
    ),
    ("go", "Save(w)\n", "Save", &[]),
    ("go", "fmt.Println(\"Save\")\n", "Save", &[]),
    ("go", "s := `Save`\n", "Save", &[]),
    ("go", "SaveAll(); AutoSave()\n", "Save", &[]),
    ("go", "func run(Save func()) {}\n", "Save", &[]),
    ("go", "r := 'x'\ng(less)\n", "less", &[(2, F)]),
    // ---- Rust --------------------------------------------------------------
    (
        "rust",
        "let v: Vec<_> = xs.iter().map(Self::save).collect();\n",
        "save",
        &[(1, F)],
    ),
    ("rust", "let f: fn() = save;\n", "save", &[(1, F)]),
    (
        "rust",
        "registry.insert(\"save\", save);\n",
        "save",
        &[(1, F)],
    ),
    (
        "rust",
        "let h = Handler { on_save: save };\n",
        "save",
        &[(1, F)],
    ),
    (
        "rust",
        "fn f<'a>(x: &'a str) { g(save) }\n",
        "save",
        &[(1, F)],
    ),
    ("rust", "let c = '\"'; g(save);\n", "save", &[(1, F)]),
    ("rust", "// callbacks.push(save);\n", "save", &[]),
    ("rust", "/* outer /* inner */ g(save); */\n", "save", &[]),
    (
        "rust",
        "pub fn save(&self) -> Result<()> {\n    Ok(())\n}\n",
        "save",
        &[],
    ),
    ("rust", "self.save()?;\n", "save", &[]),
    ("rust", "use crate::store::save;\n", "save", &[]),
    (
        "rust",
        "use crate::store::{\n    load,\n    save,\n};\n",
        "save",
        &[],
    ),
    ("rust", "println!(\"save\");\n", "save", &[]),
    ("rust", "let save_all = 1; autosave();\n", "save", &[]),
    ("rust", "fn run(save: bool) {}\n", "save", &[]),
    (
        "rust",
        "let s = r#\"handlers[\"save\"] g(save)\"#;\n",
        "save",
        &[],
    ),
    ("rust", "let s = \"multi\nline g(save)\";\n", "save", &[]),
    // Found by the dogfood survey on this repo: a match arm is not a hash
    // rocket, a `json!` key is data, and a pattern binding is not a value.
    (
        "rust",
        "match d { \"both\" => Some(\"both\"), _ => None }\n",
        "both",
        &[],
    ),
    ("rust", "let v = json!({\"save\": 1});\n", "save", &[]),
    ("rust", "out[\"save\"] = json!(1);\n", "save", &[]),
    ("rust", "(handlers[\"save\"])(doc);\n", "save", &[(1, K)]),
    (
        "javascript",
        "handlers[\"save\"] = save;\n",
        "save",
        &[(1, K)],
    ),
    ("rust", "let Some(save) = x else { return };\n", "save", &[]),
    ("rust", "match x { Some(save) => 1, _ => 2 }\n", "save", &[]),
    ("javascript", "const [a, save] = useState();\n", "save", &[]),
    ("python", "(a, save) = pair\n", "save", &[]),
    // ---- Java --------------------------------------------------------------
    (
        "java",
        "Method m = cls.getMethod(\"save\");\n",
        "save",
        &[(1, R)],
    ),
    (
        "java",
        "cls.getDeclaredMethod(\"save\", String.class);\n",
        "save",
        &[(1, R)],
    ),
    ("java", "list.forEach(this::save);\n", "save", &[(1, F)]),
    ("java", "executor.submit(Store::save);\n", "save", &[(1, F)]),
    ("java", "public void save(Doc d) {\n}\n", "save", &[]),
    ("java", "// cls.getMethod(\"save\")\n", "save", &[]),
    ("java", "store.save(d);\n", "save", &[]),
    ("java", "import static com.x.Store.save;\n", "save", &[]),
    ("java", "log.info(\"save\");\n", "save", &[]),
    ("java", "void run(Doc save) {}\n", "save", &[]),
    // ---- C / C++ -----------------------------------------------------------
    ("c", "void *p = dlsym(h, \"save\");\n", "save", &[(1, R)]),
    ("c", "signal(SIGINT, save);\n", "save", &[(1, F)]),
    (
        "c",
        "static const struct ops o = { .save = save, };\n",
        "save",
        &[(1, F)],
    ),
    ("c", "cb = &save;\n", "save", &[(1, F)]),
    (
        "c",
        "static const struct e t[] = {\n  {\"save\", save},\n};\n",
        "save",
        &[(2, F)],
    ),
    ("c", "char c = '\"'; g(save);\n", "save", &[(1, F)]),
    ("c", "void save(struct doc *d);\n", "save", &[]),
    ("c", "/* signal(SIGINT, save); */\n", "save", &[]),
    ("c", "#include \"save.h\"\n", "save", &[]),
    ("c", "printf(\"save\\n\");\n", "save", &[]),
    ("c", "int save_count = 0;\n", "save", &[]),
    ("c", "void (*save)(void);\n", "save", &[]),
    ("c", "x = a ? \"save\" : b;\n", "save", &[]),
    ("c", "if (a && save) {}\n", "save", &[]),
    (
        "cpp",
        "std::function<void()> f = &Store::save;\n",
        "save",
        &[(1, F)],
    ),
    (
        "cpp",
        "QMetaObject::invokeMethod(obj, \"save\");\n",
        "save",
        &[(1, R)],
    ),
    ("cpp", "store.save(d);\n", "save", &[]),
    // ---- C# / Kotlin / Swift / Dart / PHP ----------------------------------
    (
        "csharp",
        "var m = typeof(T).GetMethod(\"Save\");\n",
        "Save",
        &[(1, R)],
    ),
    ("csharp", "button.Click += Save;\n", "Save", &[(1, F)]),
    ("csharp", "public void Save() {}\n", "Save", &[]),
    ("kotlin", "list.forEach(::save)\n", "save", &[(1, F)]),
    ("kotlin", "fun save(d: Doc) {}\n", "save", &[]),
    (
        "swift",
        "b.addTarget(self, action: #selector(save), for: .tap)\n",
        "save",
        &[(1, F)],
    ),
    ("swift", "func save() {}\n", "save", &[]),
    (
        "dart",
        "ElevatedButton(onPressed: save, child: x)\n",
        "save",
        &[(1, F)],
    ),
    ("dart", "void save() {}\n", "save", &[]),
    (
        "php",
        "call_user_func([$this, 'save']);\n",
        "save",
        &[(1, R)],
    ),
    (
        "php",
        "if (method_exists($obj, 'save')) {}\n",
        "save",
        &[(1, R)],
    ),
    ("php", "$map = ['save' => 'onSave'];\n", "save", &[(1, K)]),
    ("php", "$save = 1;\n", "save", &[]),
    ("php", "# call_user_func('save');\n", "save", &[]),
    ("php", "$this->save($d);\n", "save", &[]),
    // ---- a local binding of the name shadows bare references in its file ----
    // Found by the hono/express survey: `url`, `method`, `type` are functions
    // there AND locals/parameters, and every bare use read as a reference.
    // Qualified references (`obj.method`) are still reported.
    (
        "javascript",
        "function f(url) { return fetch(url); }\n",
        "url",
        &[],
    ),
    (
        "javascript",
        "const type = x;\nreturn { type: type };\n",
        "type",
        &[],
    ),
    (
        "javascript",
        "const method = pick();\nlog(method);\nreg(obj.method);\n",
        "method",
        &[(3, F)],
    ),
    (
        "typescript",
        "app.get('/', (c, url) => fetch(url));\n",
        "url",
        &[],
    ),
    (
        "javascript",
        "class A { m(url) {\n  fetch(url);\n} }\n",
        "url",
        &[],
    ),
    ("python", "def f(url):\n    fetch(url)\n", "url", &[]),
    ("python", "url = get()\nfetch(url)\n", "url", &[]),
    ("python", "for url in urls:\n    fetch(url)\n", "url", &[]),
    ("go", "url := get()\nfetch(url)\n", "url", &[]),
    ("rust", "let url = get();\nfetch(url);\n", "url", &[]),
    ("rust", "xs.map(|url| fetch(url));\n", "url", &[]),
    (
        "java",
        "void f(String url) {\n  fetch(url);\n}\n",
        "url",
        &[],
    ),
    (
        "javascript",
        "function run() { register(save); }\n",
        "save",
        &[(1, F)],
    ),
    (
        "javascript",
        "if (save) { x(); }\nregister(save);\n",
        "save",
        &[(2, F)],
    ),
    // A default parameter VALUE is a reference, not the parameter (found on
    // this repo: `function uninstall({ install = npmInstallGlobal } = {})`).
    (
        "javascript",
        "function f({ install = save } = {}) {}\n",
        "save",
        &[(1, F)],
    ),
    (
        "javascript",
        "function g(run = save) {}\n",
        "save",
        &[(1, F)],
    ),
    ("python", "def g(run=save):\n    pass\n", "save", &[(1, F)]),
    (
        "javascript",
        "const { run = save } = opts;\n",
        "save",
        &[(1, F)],
    ),
    // Not bindings: a bitwise `|`, and a brace-less condition's call.
    (
        "javascript",
        "x = a | save;\nregister(save);\n",
        "save",
        &[(2, F)],
    ),
    (
        "rust",
        "if check(save) {\n}\nregister(save);\n",
        "save",
        &[(1, F), (3, F)],
    ),
    (
        "go",
        "if check(Save) {\n}\nregister(Save)\n",
        "Save",
        &[(1, F), (3, F)],
    ),
    // ---- languages with no shape table: never report -------------------------
    ("markdown", "handlers[\"save\"]\n", "save", &[]),
    ("bash", "trap save EXIT\n", "save", &[]),
    // ---- review of afffd6b: tokio's false positives ---------------------------
    // Rust `a.b` not followed by `(` is a field read: a method cannot be named
    // through `.` (`Self::b` / `Type::b` is how one is passed).
    ("rust", "data: me.data,\n", "data", &[]),
    ("rust", "let delay = me.delay;\n", "delay", &[]),
    ("rust", "Some(self.status)\n", "status", &[]),
    (
        "rust",
        "g(&self.shared.worker_metrics);\n",
        "worker_metrics",
        &[],
    ),
    // A parameter of a generic fn binds its name (`(` follows `>`, not the name).
    (
        "rust",
        "fn map<T, F>(self, f: F) -> Map<Self, F> {\n    Map::new(self, f)\n}\n",
        "f",
        &[],
    ),
    (
        "rust",
        "pub fn arg<S: AsRef<OsStr>>(&mut self, arg: S) {\n    self.std.arg(arg);\n}\n",
        "arg",
        &[],
    ),
    (
        "rust",
        "fn run<F: Fn(u8) -> u8>(f: F) {\n    g(f)\n}\n",
        "f",
        &[],
    ),
    // An attribute argument is not a value.
    ("rust", "#[inline(never)]\nfn g() {}\n", "never", &[]),
    ("rust", "#![allow(dead)]\n", "dead", &[]),
    // A macro metavariable is not the function.
    (
        "rust",
        "macro_rules! m { ($save:expr) => { g($save) } }\n",
        "save",
        &[],
    ),
    // What tokio really dispatches by value still reports.
    (
        "rust",
        "RawWakerVTable::new(clone_waker, wake_by_val);\n",
        "clone_waker",
        &[(1, F)],
    ),
    (
        "rust",
        "x.local_addr().and_then(convert_address)\n",
        "convert_address",
        &[(1, F)],
    ),
    (
        "rust",
        "let rc = f(Some(callback), 1);\n",
        "callback",
        &[(1, F)],
    ),
    // An import list separated by commas, on one line.
    ("python", "from store import load, save\n", "save", &[]),
    // A decorator is not a value, even one that binds.
    ("typescript", "@save.bind(this)\nclass A {}\n", "save", &[]),
];

#[test]
fn every_corpus_row_reports_exactly_its_expected_sites() {
    let mut failures = Vec::new();
    for (i, (lang, src, name, want)) in CORPUS.iter().enumerate() {
        let got: Vec<(usize, Shape)> = scan_source(lang, src, name)
            .into_iter()
            .map(|h| (h.line, h.shape))
            .collect();
        if got.as_slice() != *want {
            failures.push(format!(
                "row {i} [{lang}] {src:?} name={name}: want {want:?}, got {got:?}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} corpus rows wrong:\n{}",
        failures.len(),
        CORPUS.len(),
        failures.join("\n")
    );
}

/// Non-vacuity: the corpus must exercise every shape and both verdicts, so a
/// scanner that returns nothing (or one shape for everything) cannot pass.
#[test]
fn corpus_covers_every_shape_and_every_language_group() {
    for shape in [R, E, K, S, F] {
        assert!(
            CORPUS.iter().any(|r| r.3.iter().any(|(_, s)| *s == shape)),
            "no accepted row for {shape:?}"
        );
    }
    for lang in [
        "javascript",
        "typescript",
        "python",
        "ruby",
        "go",
        "rust",
        "java",
        "c",
        "cpp",
        "csharp",
        "kotlin",
        "swift",
        "dart",
        "php",
    ] {
        let rows: Vec<_> = CORPUS.iter().filter(|r| r.0 == lang).collect();
        assert!(!rows.is_empty(), "no rows for {lang}");
        assert!(
            rows.iter().any(|r| !r.3.is_empty()),
            "{lang}: no accepted row"
        );
        assert!(
            rows.iter().any(|r| r.3.is_empty()),
            "{lang}: no look-alike row"
        );
    }
}

/// The `via` detail names the dispatching callee for the two call-shaped kinds.
#[test]
fn reflection_and_event_hits_name_their_callee() {
    let h = scan_source("python", "getattr(obj, \"save\")()\n", "save");
    assert_eq!(h[0].via.as_deref(), Some("getattr"));
    let h = scan_source("javascript", "bus.on(\"save\", f);\n", "save");
    assert_eq!(h[0].via.as_deref(), Some("on"));
    let h = scan_source("javascript", "app.post(\"/x\", save);\n", "save");
    assert_eq!(h[0].via, None);
}

/// A definition written as a binding (`const save = () => …`) is the
/// function, not a shadowing local: with its line passed as a definition, the
/// file's bare references still report.
#[test]
fn a_binding_on_a_definition_line_does_not_shadow() {
    let src = "const save = () => 1;\napp.post(\"/\", save);\n";
    assert!(scan_source("javascript", src, "save").is_empty());
    let hits = scan_source_with_defs("javascript", src, "save", &[1]);
    assert_eq!(
        hits.iter().map(|h| (h.line, h.shape)).collect::<Vec<_>>(),
        vec![(2, F)]
    );
}

/// Only the definition's own name on its line is the definition: a parameter
/// of the same name there (`fn append(&mut self, append: bool)`, tokio's
/// `open_options.rs`) still binds the name, so the body's `append` is the
/// parameter, not the function.
#[test]
fn a_parameter_on_the_definition_line_still_shadows() {
    let src = "impl O {\n    pub fn append(&mut self, append: bool) -> &mut Self {\n        self.0.append(append);\n        self\n    }\n}\n";
    assert_eq!(scan_source_with_defs("rust", src, "append", &[2]), vec![]);
    let src = "def save(self, save):\n    register(save)\n";
    assert_eq!(scan_source_with_defs("python", src, "save", &[1]), vec![]);
}

/// A deeply nested file (the review's `var a = [get,[get,…0]]…;`) scans in
/// linear time. Every occurrence's innermost opener is one byte back and its
/// matching close at the end of the file, so a forward scan per occurrence
/// was quadratic: 240 KB took 21 s in release. The bound is generous (debug
/// build, loaded machine); the quadratic scan misses it by minutes.
#[test]
fn a_deeply_nested_file_scans_in_linear_time() {
    let n = 40_000;
    let one_line = format!("var a = {}0{};\n", "[get,".repeat(n), "]".repeat(n));
    let many_lines = format!("var a = {}0{};\n", "[get,\n".repeat(n), "]".repeat(n));
    assert!(one_line.len() >= 240_000, "{}", one_line.len());
    let t = std::time::Instant::now();
    let a = scan_source("javascript", &one_line, "get");
    let b = scan_source("javascript", &many_lines, "get");
    let took = t.elapsed();
    assert_eq!(a.len(), 1, "one line, one site");
    assert_eq!(b.len(), n, "one site per line");
    assert!(
        took < std::time::Duration::from_secs(5),
        "two 240 KB nested files took {took:?}"
    );
}

/// One site per line: a line carrying several shapes reports the most specific.
#[test]
fn one_line_reports_one_site() {
    let h = scan_source(
        "python",
        "D = {\"save\": save}; getattr(x, \"save\")\n",
        "save",
    );
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].shape, R);
}

// ---- the project scan: what it reads, what it skips, and saying so ----------

/// A project of `(path, language, bytes)` files on disk, with the one table
/// [`super::scan_project`] reads.
fn project(files: &[(&str, &str, Vec<u8>)]) -> (tempfile::TempDir, rusqlite::Connection) {
    let dir = tempfile::TempDir::new().unwrap();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE files (path TEXT, language TEXT);")
        .unwrap();
    for (path, lang, body) in files {
        let p = dir.path().join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
        conn.execute("INSERT INTO files VALUES (?1, ?2)", [path, lang])
            .unwrap();
    }
    (dir, conn)
}

/// `register(save);` padded with a comment to exactly `len` bytes.
fn padded_site(len: u64) -> Vec<u8> {
    let mut s = b"register(save);\n//".to_vec();
    s.resize(len as usize - 1, b'x');
    s.push(b'\n');
    s
}

fn site_list(b: &super::Boundaries) -> Vec<(String, usize)> {
    b.sites
        .iter()
        .map(|s| (s.file_path.clone(), s.line))
        .collect()
}

/// A file over [`super::MAX_SCAN_BYTES`] is not read, and the answer counts
/// it instead of reading as a complete "none"; one at the cap is read.
#[test]
fn a_file_over_the_size_cap_is_skipped_and_counted() {
    let cap = super::MAX_SCAN_BYTES;
    let (dir, conn) = project(&[
        ("at_cap.js", "javascript", padded_site(cap)),
        ("over_cap.js", "javascript", padded_site(cap + 1)),
    ]);
    let b = super::scan_project(&conn, dir.path(), "save", &[]).unwrap();
    assert_eq!(site_list(&b), vec![("at_cap.js".to_string(), 1)]);
    assert_eq!(b.skipped_files, 1);
    let mut text = Vec::new();
    b.render_text(&mut text, "").unwrap();
    let text = String::from_utf8(text).unwrap();
    assert!(
        text.contains("not scanned: 1 file over 2 MB or not UTF-8"),
        "{text}"
    );
    assert_eq!(b.to_json()["not_scanned"]["skipped_files"], 1);

    // Not UTF-8: counted when the bytes hold the name, not otherwise.
    let (dir, conn) = project(&[
        (
            "latin1.js",
            "javascript",
            b"register(save); // caf\xe9\n".to_vec(),
        ),
        ("other.js", "javascript", b"x(); // caf\xe9\n".to_vec()),
    ]);
    let b = super::scan_project(&conn, dir.path(), "save", &[]).unwrap();
    assert!(b.sites.is_empty());
    assert_eq!(b.skipped_files, 1);
}

/// A site on a line that defines the name is not reported: that line is where
/// the function is written, not where it is dispatched from.
#[test]
fn a_definition_line_is_not_its_own_site() {
    let (dir, conn) = project(&[(
        "a.js",
        "javascript",
        b"bus.on(\"save\", function save(doc) { return doc; });\nbus.on(\"save\", other);\n"
            .to_vec(),
    )]);
    let b = super::scan_project(&conn, dir.path(), "save", &[("a.js".to_string(), 1)]).unwrap();
    assert_eq!(site_list(&b), vec![("a.js".to_string(), 2)]);
}

/// The review's F-M2: bash has functions and dispatches by name (`trap cleanup
/// EXIT`), but no shape table, so its files are never scanned. A definition in
/// such a language must not get the complete "none" line.
#[test]
fn a_definition_in_an_unscanned_language_is_named_not_scanned() {
    let (dir, conn) = project(&[(
        "run.sh",
        "bash",
        b"cleanup() {\n  rm -f x\n}\ntrap cleanup EXIT\n".to_vec(),
    )]);
    let b = super::scan_project_with(
        &conn,
        dir.path(),
        "cleanup",
        &[("run.sh".to_string(), 1)],
        &["run.sh".to_string()],
        None,
        None,
        super::SCAN_TIME_LIMIT,
    )
    .unwrap();
    assert!(b.sites.is_empty());
    assert_eq!(b.unscanned_languages, vec!["bash".to_string()]);
    let mut text = Vec::new();
    b.render_text(&mut text, "  ").unwrap();
    assert_eq!(
        String::from_utf8(text).unwrap(),
        "  (no dynamic-dispatch site names 'cleanup' in the files scanned; not scanned: bash files)\n    next: code-graph-mcp grep -w -F cleanup\n"
    );
    assert_eq!(
        b.to_json(),
        serde_json::json!({
            "total": 0,
            "sites": [],
            "not_scanned": {"languages": ["bash"]},
            "next": "code-graph-mcp grep -w -F cleanup",
        })
    );
    // A complete scan keeps the one short line and the two-key JSON: a bash
    // file that defines nothing of the name, and a definition in a test file
    // (a scanned language, skipped by rule) are not named.
    let (dir, conn) = project(&[
        ("a.py", "python", b"def cleanup():\n    pass\n".to_vec()),
        ("other.sh", "bash", b"trap cleanup EXIT\n".to_vec()),
        (
            "tests/test_a.py",
            "python",
            b"def cleanup():\n    pass\n".to_vec(),
        ),
    ]);
    let b = super::scan_project_with(
        &conn,
        dir.path(),
        "cleanup",
        &[("a.py".to_string(), 1), ("tests/test_a.py".to_string(), 1)],
        &["a.py".to_string(), "tests/test_a.py".to_string()],
        None,
        None,
        super::SCAN_TIME_LIMIT,
    )
    .unwrap();
    assert!(b.unscanned_languages.is_empty(), "{b:?}");
    let mut text = Vec::new();
    b.render_text(&mut text, "  ").unwrap();
    assert_eq!(
        String::from_utf8(text).unwrap(),
        "  (no dynamic-dispatch site names 'cleanup')\n"
    );
    assert_eq!(b.to_json(), serde_json::json!({"total": 0, "sites": []}));
}

/// The per-query time limit stops the scan, and the answer says how many
/// files it did not reach instead of reading as complete — every one past the
/// limit, since a file not read may hold the name.
#[test]
fn the_scan_stops_at_its_time_limit_and_says_so() {
    let files = [
        ("a.js", "javascript", b"register(save);\n".to_vec()),
        ("b.js", "javascript", b"register(save);\n".to_vec()),
        ("c.js", "javascript", b"other();\n".to_vec()),
    ];
    let (dir, conn) = project(&files);
    let b = super::scan_project_with(
        &conn,
        dir.path(),
        "save",
        &[],
        &[],
        None,
        None,
        std::time::Duration::ZERO,
    )
    .unwrap();
    assert!(b.sites.is_empty());
    assert_eq!(b.files_past_limit, 3);
    let mut text = Vec::new();
    b.render_text(&mut text, "").unwrap();
    let text = String::from_utf8(text).unwrap();
    assert!(
        text.contains("not scanned: 3 files past the scan time limit"),
        "{text}"
    );
    assert_eq!(b.to_json()["not_scanned"]["files_past_time_limit"], 3);
    // A file already being read stops at the deadline too.
    assert_eq!(
        super::scan_source_until(
            "javascript",
            "register(save);\n",
            "save",
            &[],
            None,
            Some(std::time::Instant::now()),
        ),
        None
    );
    // The same project with the real limit reads both.
    let b = super::scan_project(&conn, dir.path(), "save", &[]).unwrap();
    assert_eq!(b.sites.len(), 2);
    assert_eq!(b.files_past_limit, 0);
}

/// A Rust path names this project's function only through a qualifier where
/// one of its definitions lives: `thread::JoinHandle::join` and `Result::ok`
/// are std's (tokio's `join` is a free function in `io/join.rs`).
#[test]
fn a_rust_path_counts_only_through_a_qualifier_that_owns_the_name() {
    let q = super::path_qualifiers(
        [
            ("tokio/src/io/join.rs", Some("join")),
            ("src/store/mod.rs", Some("Store::join")),
        ]
        .into_iter(),
    );
    for want in ["Self", "self", "super", "crate", "join", "Store", "store"] {
        assert!(q.iter().any(|x| x == want), "{want} missing from {q:?}");
    }
    let scan = |src: &str| -> Vec<(usize, Shape)> {
        super::scan_source_until("rust", src, "join", &[], Some(&q), None)
            .unwrap()
            .hits
            .into_iter()
            .map(|h| (h.line, h.shape))
            .collect()
    };
    assert_eq!(scan("x.map(thread::JoinHandle::join);\n"), vec![]);
    assert_eq!(scan("x.map(Result::join);\n"), vec![]);
    for ok in [
        "x.map(join::join);\n",
        "x.map(Self::join);\n",
        "x.map(crate::io::join::join);\n",
        "x.map(Store::join);\n",
        "x.map(store::join);\n",
        "x.map(join);\n",
    ] {
        assert_eq!(scan(ok), vec![(1, F)], "{ok}");
    }
    // Without definition facts (the corpus) any qualifier counts.
    assert_eq!(
        scan_source("rust", "x.map(thread::JoinHandle::join);\n", "join").len(),
        1
    );
    // Pre-tag review H1: a qualifier not known to be the definition's is not a
    // site, but it is recorded as undecided — it can be the crate's own name,
    // an inline `mod`, a `use … as` alias or a trait — and so are generic and
    // qualified-self paths. A call, a known qualifier and an import are not.
    let unresolved = |lang: &str, src: &str| -> Vec<usize> {
        super::scan_source_until(lang, src, "join", &[], Some(&q), None)
            .unwrap()
            .unresolved_lines
    };
    for undecided in [
        "x.map(thread::JoinHandle::join);\n",
        "x.map(myapp::join);\n",
        "x.map(inner::join);\n",
        "x.map(h::join);\n",
        "x.map(Joiner::join);\n",
        "x.map(Wrapper::<u8>::join);\n",
        "x.map(<Db as Joiner>::join);\n",
        "let f = h::join;\n",
    ] {
        assert_eq!(unresolved("rust", undecided), vec![1], "{undecided}");
    }
    for decided in [
        "h::join(x);\n",
        "x.map(Store::join);\n",
        "use h::join;\n",
        "#[doc(alias = h::join)]\n",
        "x.map(join);\n",
        "// x.map(h::join);\n",
        "x.map(<Db as Joiner>::join(a));\n",
    ] {
        assert_eq!(
            unresolved("rust", decided),
            Vec::<usize>::new(),
            "{decided}"
        );
    }
    // Only Rust checks the qualifier: other languages report the site as
    // before and record nothing undecided.
    for (lang, src) in [
        ("cpp", "run(Other::join);\n"),
        ("php", "run(Other::join);\n"),
        ("javascript", "run(other.join);\n"),
    ] {
        let s = super::scan_source_until(lang, src, "join", &[], Some(&q), None).unwrap();
        assert_eq!(s.hits.len(), 1, "{lang}: {s:?}");
        assert!(s.unresolved_lines.is_empty(), "{lang}: {s:?}");
    }
}

/// The bracket index answers what the scans it replaced answered: the
/// innermost unmatched opener at most 4 KB back, and an opener's matching
/// close (any closer closes any opener). Checked against those scans, kept
/// here as the reference, over generated text with unbalanced brackets.
#[test]
fn the_bracket_index_matches_the_scans_it_replaced() {
    fn enclosing_scan(m: &[u8], pos: usize) -> Option<usize> {
        let mut depth = 0usize;
        let floor = pos.saturating_sub(4096);
        let mut j = pos;
        while j > floor {
            j -= 1;
            match m[j] {
                b')' | b']' | b'}' => depth += 1,
                b'(' | b'[' | b'{' => {
                    if depth == 0 {
                        return Some(j);
                    }
                    depth -= 1;
                }
                _ => {}
            }
        }
        None
    }
    fn close_scan(m: &[u8], open: usize) -> Option<usize> {
        let mut depth = 0usize;
        for (j, &b) in m.iter().enumerate().skip(open) {
            match b {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(j);
                    }
                }
                _ => {}
            }
        }
        None
    }
    // xorshift: deterministic, no dependency.
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let alphabet = b"([{)]}a\n";
    let mut checked = 0usize;
    for round in 0..40 {
        // Long enough in some rounds to cross the 4 KB floor.
        let len = if round % 4 == 0 { 9000 } else { 300 };
        let m: Vec<u8> = (0..len)
            .map(|_| alphabet[(next() % alphabet.len() as u64) as usize])
            .collect();
        let src = super::Src::new(&m);
        for pos in 0..=m.len() {
            assert_eq!(
                src.enclosing_opener(pos),
                enclosing_scan(&m, pos),
                "round {round} pos {pos}"
            );
            if pos < m.len() && matches!(m[pos], b'(' | b'[' | b'{') {
                assert_eq!(
                    src.matching_close(pos),
                    close_scan(&m, pos),
                    "round {round} open {pos}"
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 10_000, "{checked}");
    // Non-vacuity: the 4 KB floor is exercised — a deep opener far back.
    let mut m = b"(".to_vec();
    m.resize(5000, b'a');
    let src = super::Src::new(&m);
    assert_eq!(src.enclosing_opener(100), Some(0));
    assert_eq!(src.enclosing_opener(4999), None);
}

// ---- Call sites (D#229) ------------------------------------------------------
//
// Written before the scanner. Each row: a source, the name, and the exact
// 1-based lines holding a CALL of the name. A call is the name followed by
// its argument list: `name(`, `.name(`, `::name(`, through a turbofish or
// type arguments. Look-alikes: a definition or declaration, a comment, a
// string, a field read or the function as a value, a macro, an attribute, a
// local binding of the name, an identifier containing it.

type CallRow = (&'static str, &'static str, &'static str, &'static [usize]);

const CALL_CORPUS: &[CallRow] = &[
    // ---- Rust ----
    ("rust", "list.remove(node);\n", "remove", &[1]),
    ("rust", "let x = Self::remove(a);\n", "remove", &[1]),
    ("rust", "remove(a);\n", "remove", &[1]),
    ("rust", "x.remove::<u8>(a);\n", "remove", &[1]),
    ("rust", "Foo::remove::<u8>(a);\n", "remove", &[1]),
    ("rust", "let n = list\n    .remove(node);\n", "remove", &[2]),
    ("rust", "if remove(x) {\n}\n", "remove", &[1]),
    (
        "rust",
        "Some(a.remove(1)).map(|v| v.remove(2));\n",
        "remove",
        &[1],
    ),
    ("rust", "fn remove(&mut self) {}\n", "remove", &[]),
    ("rust", "pub(crate) fn remove<T>(x: T) {}\n", "remove", &[]),
    (
        "rust",
        "trait T {\n    fn remove(&self);\n}\n",
        "remove",
        &[],
    ),
    ("rust", "// list.remove(x)\n", "remove", &[]),
    ("rust", "/* remove(x) */\n", "remove", &[]),
    ("rust", "let s = \"remove(x)\";\n", "remove", &[]),
    ("rust", "let r = list.remove;\n", "remove", &[]),
    ("rust", "remove!(x);\n", "remove", &[]),
    ("rust", "#[remove(x)]\nfn g() {}\n", "remove", &[]),
    (
        "rust",
        "#[cfg_attr(test, remove(x))]\nfn g() {}\n",
        "remove",
        &[],
    ),
    ("rust", "struct remove(u8);\n", "remove", &[]),
    ("rust", "use crate::x::remove;\n", "remove", &[]),
    ("rust", "x.removed(a);\n", "remove", &[]),
    ("rust", "let f = Self::remove;\n", "remove", &[]),
    (
        "rust",
        "fn g(remove: impl Fn()) {\n    remove();\n}\n",
        "remove",
        &[],
    ),
    // ---- Python ----
    ("python", "obj.remove(x)\n", "remove", &[1]),
    ("python", "remove(x)\n", "remove", &[1]),
    ("python", "if remove(x):\n    pass\n", "remove", &[1]),
    ("python", "@remove(1)\ndef f():\n    pass\n", "remove", &[1]),
    ("python", "def remove(self, x):\n    pass\n", "remove", &[]),
    ("python", "async def remove(x):\n    pass\n", "remove", &[]),
    ("python", "class remove(Base):\n    pass\n", "remove", &[]),
    ("python", "@remove\ndef f():\n    pass\n", "remove", &[]),
    ("python", "# remove(x)\n", "remove", &[]),
    ("python", "s = 'remove(x)'\n", "remove", &[]),
    ("python", "x = obj.remove\n", "remove", &[]),
    // ---- JavaScript / TypeScript ----
    ("javascript", "list.remove(x);\n", "remove", &[1]),
    ("javascript", "remove(x);\n", "remove", &[1]),
    ("javascript", "a?.remove(x);\n", "remove", &[1]),
    ("javascript", "if (remove(x)) {\n}\n", "remove", &[1]),
    (
        "javascript",
        "const y = c ? remove(x) : 0;\n",
        "remove",
        &[1],
    ),
    ("javascript", "const s = `${remove(x)}`;\n", "remove", &[1]),
    ("typescript", "remove<T>(x);\n", "remove", &[1]),
    ("typescript", "obj.remove<string>(x);\n", "remove", &[1]),
    ("typescript", "@remove(x)\nclass A {}\n", "remove", &[1]),
    ("javascript", "function remove(x) {}\n", "remove", &[]),
    ("javascript", "async function remove(x) {}\n", "remove", &[]),
    (
        "javascript",
        "export function remove(x) {}\n",
        "remove",
        &[],
    ),
    ("javascript", "function* remove() {}\n", "remove", &[]),
    (
        "javascript",
        "class A {\n  remove(x) {\n    return 1;\n  }\n}\n",
        "remove",
        &[],
    ),
    (
        "javascript",
        "class A {\n  async remove(x) {}\n}\n",
        "remove",
        &[],
    ),
    (
        "javascript",
        "const o = { remove(x) { return x; } };\n",
        "remove",
        &[],
    ),
    (
        "typescript",
        "interface I {\n  remove(x: string): void;\n}\n",
        "remove",
        &[],
    ),
    (
        "typescript",
        "class A {\n  remove(x: string): void {\n  }\n}\n",
        "remove",
        &[],
    ),
    (
        "typescript",
        "abstract class A {\n  abstract remove(x: string): void;\n}\n",
        "remove",
        &[],
    ),
    ("javascript", "// remove(x)\n", "remove", &[]),
    ("javascript", "const s = 'remove(x)';\n", "remove", &[]),
    ("javascript", "const f = obj.remove;\n", "remove", &[]),
    (
        "javascript",
        "const remove = (a) => a;\nremove(1);\n",
        "remove",
        &[],
    ),
];

#[test]
fn every_call_corpus_row_reports_exactly_its_call_lines() {
    let mut failures = Vec::new();
    for (i, (lang, src, name, want)) in CALL_CORPUS.iter().enumerate() {
        let got = super::scan_calls(lang, src, name, &[]);
        if got.as_deref() != Some(*want) {
            failures.push(format!(
                "row {i} [{lang}] {src:?} name={name}: want {want:?}, got {got:?}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} call corpus rows wrong:\n{}",
        failures.len(),
        CALL_CORPUS.len(),
        failures.join("\n")
    );
}

/// Non-vacuity, and the language boundary: every counted language has both
/// verdicts, and a language without a call table answers `None`, not "no
/// calls".
#[test]
fn call_corpus_covers_each_counted_language_and_refuses_the_rest() {
    for lang in ["rust", "python", "javascript", "typescript"] {
        let rows: Vec<_> = CALL_CORPUS.iter().filter(|r| r.0 == lang).collect();
        assert!(rows.iter().any(|r| !r.3.is_empty()), "{lang}: no call row");
        assert!(
            rows.iter().any(|r| r.3.is_empty()),
            "{lang}: no look-alike row"
        );
    }
    for lang in ["go", "java", "ruby", "markdown"] {
        assert_eq!(
            super::scan_calls(lang, "remove(x)\n", "remove", &[]),
            None,
            "{lang}"
        );
    }
}

/// A definition line is not a call of itself, even when it also calls.
#[test]
fn a_definition_line_holds_no_call_of_its_own_name() {
    let src = "fn remove(&self) -> u8 { self.inner.remove(0) }\nfn g() { h.remove(1); }\n";
    assert_eq!(
        super::scan_calls("rust", src, "remove", &[1]),
        Some(vec![2])
    );
}

/// A real index of `files` (path, source) under a temp project root.
fn indexed_project(
    files: &[(&str, &str)],
) -> (
    tempfile::TempDir,
    tempfile::TempDir,
    crate::storage::db::Database,
) {
    let dir = tempfile::TempDir::new().unwrap();
    for (path, body) in files {
        let p = dir.path().join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
    }
    let db_dir = tempfile::TempDir::new().unwrap();
    let db = crate::storage::db::Database::open(&db_dir.path().join("index.db")).unwrap();
    crate::indexer::pipeline::run_full_index(&db, dir.path(), None, None).unwrap();
    (dir, db_dir, db)
}

fn text_of(b: &super::Boundaries) -> String {
    let mut out = Vec::new();
    b.render_text(&mut out, "").unwrap();
    String::from_utf8(out).unwrap()
}

/// D#229: two `remove` methods, called on receivers whose type the graph
/// does not resolve, so neither has a caller. Those calls are listed with
/// the empty answer; a call the graph resolved (to the free `remove`) is
/// not, and neither is one in a test function.
const D229_FIXTURE: &[(&str, &str)] = &[
    ("src/lib.rs", "pub mod a;\npub mod b;\npub mod c;\npub mod d;\npub mod e;\npub mod f;\n"),
    ("src/a.rs", "pub struct L;\nimpl L {\n    pub fn remove(&self, i: u8) -> u8 {\n        i\n    }\n}\n"),
    ("src/d.rs", "pub struct M;\nimpl M {\n    pub fn remove(&self, i: u8) -> u8 {\n        i + 1\n    }\n}\n"),
    ("src/b.rs", "pub fn uses(x: u8) -> u8 {\n    let l = crate::c::make();\n    l.remove(x)\n}\n"),
    ("src/c.rs", "pub fn make() -> crate::a::L {\n    crate::a::L\n}\npub fn typed(m: &crate::d::M) -> u8 {\n    m.remove(2)\n}\n"),
    ("src/e.rs", "pub fn remove(i: u8) -> u8 {\n    i\n}\n"),
    ("src/f.rs", "pub fn k() -> u8 {\n    crate::e::remove(1)\n}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {\n        let l = crate::c::make();\n        l.remove(3);\n    }\n}\n"),
    ("Cargo.toml", "[package]\nname = \"fx\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
];

#[test]
fn an_empty_answer_lists_the_calls_with_no_resolved_target() {
    let (dir, _db_dir, db) = indexed_project(D229_FIXTURE);
    let b = super::for_empty_result(db.conn(), dir.path(), "remove", &[])
        .unwrap()
        .unwrap();
    let calls: Vec<(String, usize, bool)> = b
        .calls
        .as_ref()
        .expect("rust calls are counted")
        .iter()
        .map(|c| (c.file_path.clone(), c.line, c.resolved))
        .collect();
    assert_eq!(
        calls,
        vec![
            ("src/b.rs".to_string(), 3, false),
            ("src/c.rs".to_string(), 5, false),
            ("src/f.rs".to_string(), 2, true),
        ],
        "the test function's call is dropped"
    );
    let text = text_of(&b);
    assert!(
        text.contains(
            "2 calls of 'remove' in 2 files have no resolved target; a caller of this definition may be among them:"
        ) && text.contains("src/b.rs:3")
            && text.contains("src/c.rs:5")
            && !text.contains("src/f.rs:2")
            && text.contains("next: code-graph-mcp grep -w -F remove"),
        "{text}"
    );
    let json = b.to_json();
    assert_eq!(json["unresolved_calls"]["total"], 2, "{json}");
    assert_eq!(json["unresolved_calls"]["files"], 2, "{json}");
    assert_eq!(
        json["unresolved_calls"]["sites"],
        serde_json::json!([
            {"file_path": "src/b.rs", "line": 3},
            {"file_path": "src/c.rs", "line": 5},
        ]),
        "{json}"
    );
    assert!(json["next"].is_string(), "{json}");
}

/// No call at all, or only resolved ones: the zero is backed by the count,
/// in the one line the answer already had.
#[test]
fn an_empty_answer_with_no_unresolved_call_says_so_in_one_line() {
    let (dir, _db_dir, db) = indexed_project(&[
        (
            "src/lib.rs",
            "pub fn lonely() -> u8 {\n    7\n}\npub fn other() -> u8 {\n    8\n}\n",
        ),
        (
            "Cargo.toml",
            "[package]\nname = \"fx\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
    ]);
    let b = super::for_empty_result(db.conn(), dir.path(), "lonely", &[])
        .unwrap()
        .unwrap();
    assert_eq!(
        text_of(&b),
        "(no dynamic-dispatch site or unresolved call names 'lonely')\n"
    );
    assert_eq!(
        b.to_json()["unresolved_calls"],
        serde_json::json!({"total": 0})
    );
}

/// A definition in a language whose calls are not counted keeps the answer
/// it had: no claim about calls either way.
#[test]
fn an_uncounted_language_makes_no_claim_about_calls() {
    let (dir, _db_dir, db) = indexed_project(&[(
        "main.go",
        "package main\n\nfunc lonely() int {\n\treturn 7\n}\n",
    )]);
    let b = super::for_empty_result(db.conn(), dir.path(), "lonely", &[])
        .unwrap()
        .unwrap();
    assert!(b.calls.is_none());
    assert_eq!(text_of(&b), "(no dynamic-dispatch site names 'lonely')\n");
    assert!(b.to_json().get("unresolved_calls").is_none());
}

/// The listed calls start nearest the definition asked about: more shared
/// directories first. Path order put tokio's `examples/` and `tokio-util/`
/// ahead of the `tokio/src/` calls of `LinkedList::remove`, and nearest to
/// ANY same-named definition still ranked `tokio-util` beside them.
#[test]
fn unresolved_calls_are_listed_nearest_the_definition_first() {
    let (dir, _db_dir, db) = indexed_project(&[
        (
            "pkg/core/a.rs",
            "pub struct L;\nimpl L {\n    pub fn remove(&self) -> u8 {\n        1\n    }\n}\n",
        ),
        (
            "util/n.rs",
            "pub struct N;\nimpl N {\n    pub fn remove(&self) -> u8 {\n        2\n    }\n}\n",
        ),
        (
            "examples/x.rs",
            "pub fn ex(v: &V) -> u8 {\n    v.remove()\n}\n",
        ),
        (
            "pkg/other/c.rs",
            "pub fn oc(v: &V) -> u8 {\n    v.remove()\n}\n",
        ),
        (
            "pkg/core/b.rs",
            "pub fn cb(v: &V) -> u8 {\n    v.remove()\n}\n",
        ),
        ("util/u.rs", "pub fn uu(v: &V) -> u8 {\n    v.remove()\n}\n"),
    ]);
    let order = |near: &[i64]| -> Vec<String> {
        super::for_empty_result(db.conn(), dir.path(), "remove", near)
            .unwrap()
            .unwrap()
            .unresolved_calls()
            .unwrap()
            .iter()
            .map(|c| c.file_path.clone())
            .collect()
    };
    let l = crate::storage::queries::get_nodes_with_files_by_name(db.conn(), "remove")
        .unwrap()
        .into_iter()
        .find(|d| d.file_path == "pkg/core/a.rs")
        .unwrap()
        .node
        .id;
    assert_eq!(
        order(&[l]),
        [
            "pkg/core/b.rs",
            "pkg/other/c.rs",
            "examples/x.rs",
            "util/u.rs"
        ]
    );
    // Not told which: nearest to any of them.
    assert_eq!(
        order(&[]),
        [
            "pkg/core/b.rs",
            "pkg/other/c.rs",
            "util/u.rs",
            "examples/x.rs"
        ]
    );
}
