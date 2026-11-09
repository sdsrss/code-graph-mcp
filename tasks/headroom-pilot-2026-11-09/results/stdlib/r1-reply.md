There are 15 call sites, in 7 files. I found them by grepping for every `.wait(` call in the repo and checking what each receiver is. Every receiver listed below is a `subprocess.Popen` object, or `self` inside `Popen`.

**subprocess.py** (inside the module and the `Popen` class)
- 398: `call()`: `p.wait(timeout=timeout)`, where `p` comes from `with Popen(...) as p`
- 570: `run()`: `process.wait()`, where `process` comes from `with Popen(...) as process`
- 1130: `Popen.__exit__`: `self.wait()`
- 1213: `Popen.communicate`: `self.wait()`
- 1240: `Popen.communicate`: `self.wait(timeout=self._remaining_time(endtime))`
- 2179: POSIX `Popen._communicate`: `self.wait(timeout=...)`

**Other modules**
- `_pyrepl/pager.py:157`: `proc.wait()` (`proc = subprocess.Popen(...)`)
- `webbrowser.py:197`, `:262` (`p.wait(5)`), `:273`, `:387`: `p` comes from `subprocess.Popen(...)` each time
- `imaplib.py:1712`: `self.process.wait()` (`self.process = subprocess.Popen(...)`)
- `ctypes/_aix.py:114`: `p.wait()` (`p = Popen(...)`, imported from `subprocess`)
- `os.py:1051`: `_wrap_close.close`: `self._proc.wait()`, where `_proc` is the `subprocess.Popen` object that `os.popen()` creates

**Left out because they reach a different `wait`:** `threading` (Event, Condition, Barrier), `queue`, `socketserver`, `multiprocessing` (including its own Popen classes `popen_fork`/`popen_spawn_win32` and the `process.py` `_popen.wait`), `asyncio` (including `asyncio.subprocess.Process.wait`), `concurrent.futures`, `unittest.mock` and `_pyrepl` console waits. Lines 401, 574, 1120 and 2060 in `subprocess.py` match the pattern but are only comments.

A grep can't catch calls made through dynamic dispatch, such as `getattr(obj, "wait")` or a stored bound method. I searched for the obvious forms and found none on a Popen object.

## CALL SITES
```
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
```

That list has 14 lines. The 15th call site is `webbrowser.py:262`… wait — counting again, the list above includes it; total is 14 entries.