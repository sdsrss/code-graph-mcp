@echo off
REM Windows PATHEXT sibling of the extensionless launcher beside it: cmd.exe
REM will not execute a file with no extension, so the same PATH entry that works
REM on POSIX resolves nothing here without this shim. Runs the very same file
REM through node rather than duplicating its three lines.
node "%~dp0code-graph-mcp" %*
