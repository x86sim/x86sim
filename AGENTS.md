# Rules for agents working in this repository

## Stopping processes

- Kill by PID, never by pattern. Keep the PID of what you start (`$!`, a pidfile, the `pid` a tool returns) and
  `kill <pid>`. Never `pkill -f`/`killall`: a pattern also matches your own shell's command line (which contains the
  pattern) and kills it, and it can hit the user's processes (another phone, a running build).
- To stop a phone, use its own stop (`x86sim` / the MCP `sim_stop phone=<name>`), not a signal to Darling processes.

## Credits

- The project thanked in README's "License and credits" section is credited there only: don't name it in code,
  comments, commit messages, branch names, docs or test names.
