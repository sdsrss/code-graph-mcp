@echo off
REM Windows PATHEXT sibling of the extensionless launcher beside it: cmd.exe
REM will not execute a file with no extension, so the same PATH entry that works
REM on POSIX resolves nothing here without this shim. Runs the very same file
REM through node rather than duplicating its three lines.
node "%~dp0code-graph-mcp" %*
REM Propagate the exit code explicitly. cmd.exe leaves %ERRORLEVEL% set after the
REM last command, but a caller that runs this through `cmd /c` gets the SCRIPT's
REM status, and relying on the implicit path is what makes a non-zero `doctor` or
REM a failed search look like success to a wrapper.
exit /b %ERRORLEVEL%
