I found 14 call sites that reach `subprocess.Popen.wait`. I searched every `.wait(` call in the repository and checked what object each one is called on.

**Inside `subprocess.py`**
- Line 398, `call()`: `p` comes from `with Popen(...) as p`, and the call passes `timeout=` as a keyword.
- Line 570, `run()`: `process.wait()`, where `process` is a `Popen`.
- Line 1130, `Popen.__exit__`: `self.wait()`.
- Line 1213, `Popen.__del__` / communicate path: `self.wait()`.
- Line 1240, `Popen.communicate`: `self.wait(timeout=...)`.
- Line 2179, POSIX `_communicate`: `self.wait(timeout=...)`.

**In other modules** (each receiver is created with `subprocess.Popen(...)`)
- `os.py:1051`: `self._proc.wait()` in `_wrap_close.close`. `_proc` is the `Popen` that `os.popen()` creates.
- `_pyrepl/pager.py:157`: `proc` is created at line 142.
- `imaplib.py:1712`: `self.process` is created at line 1685.
- `ctypes/_aix.py:114`: `p` is created at line 109.
- `webbrowser.py:197, 262, 273, 387`: `p` is created at lines 194/196, 255, 213/215 and 380.

**Excluded because they call a different `wait`:**
- **asyncio:** `asyncio.subprocess.Process.wait` (async, and calls the transport's `_wait`).
- **multiprocessing:** `multiprocessing` `Popen*.wait`, `Pool`/`ApplyResult.wait` and `connection.wait`.
- **Synchronisation objects:** threading `Condition`/`Event`/`Barrier` waits, `queue` condition waits and `socketserver`'s event.
- **Others:** `concurrent.futures` waits and `_pyrepl` console `self.wait`.

The internal `self._wait(...)` calls in `subprocess.py` go to the private `_wait` method, not `wait`, so I left them out too. I didn't find `wait` being passed around as a reference (e.g. `f = proc.wait`).

Lines 398, 1240 and 2179 pass `timeout=` by name, so they will need changing if you rename that parameter.

## CALL SITES
subprocess.py:398
subprocess.py:570
subprocess.py:1130
subprocess.py:1213
subprocess.py:1240
subprocess.py:2179
os.py:1051
_pyrepl/pager.py:157
imaplib.py:1712
ctypes/_aix.py:114
webbrowser.py:197
webbrowser.py:262
webbrowser.py:273
webbrowser.py:387