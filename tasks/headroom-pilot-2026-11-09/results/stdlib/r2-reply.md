I found 16 call sites in this repository that reach `subprocess.Popen.wait`. To find them I searched every `.wait(` call (and every bare `.wait` reference, to catch aliasing) across the repo. For each one I checked that the object it's called on is a `subprocess.Popen` instance.

**Inside `subprocess.py` (on `self` within `Popen`, or on a local `Popen`):**
- `call()` at line 398: `with Popen(...) as p:` → `p.wait(timeout=timeout)`
- `run()` at line 570: `process.wait()` in the timeout handler. `process` comes from `with Popen(...) as process`.
- `Popen.__exit__` at line 1130: `self.wait()`
- `Popen.communicate` at lines 1213 and 1240: `self.wait()` and `self.wait(timeout=...)`
- POSIX `Popen._communicate` at line 2179: `self.wait(timeout=...)`

**Other modules:**
- `_pyrepl/pager.py:157`: `proc` is `subprocess.Popen(...)` (line 142)
- `webbrowser.py`: lines 197, 262, 273 and 387. Each `p` is `subprocess.Popen(...)`, and line 262 passes a positional timeout (`p.wait(5)`).
- `imaplib.py:1712`: `self.process` is `subprocess.Popen(...)` (line 1685)
- `ctypes/_aix.py:114`: `p = Popen(...)`
- `os.py:1051`: `_wrap_close.close` calls `self._proc.wait()`, where `_proc` is the `subprocess.Popen` created by `os.popen`.

**Excluded because they call a different `wait`:**
- `asyncio/subprocess.py:202` calls `asyncio.subprocess.Process.wait`.
- The `threading`, `multiprocessing`, `queue`, `socketserver`, `asyncio` locks and queues, `unittest.mock` and `_pyrepl` console `wait` calls belong to their own classes.

The repo has no test directory, so there are no test call sites to update. Three lines pass a timeout to `wait` positionally or by keyword, so check those first if you're changing the parameter: `subprocess.py:398` and `:1240` use `timeout=`, `:2179` uses `timeout=`, and `webbrowser.py:262` passes it positionally.

## CALL SITES
subprocess.py:398
subprocess.py:570
subprocess.py:1130
subprocess.py:1213
subprocess.py:1240
subprocess.py:2179
_pyrepl/pager.py:157
webbrowser.py:197
webbrowser.py:262
webbrowser.py:273
webbrowser.py:387
imaplib.py:1712
ctypes/_aix.py:114
os.py:1051