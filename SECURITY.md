# Security policy

## Reporting a vulnerability

Please report security problems privately, not in a public issue or pull request.

Use GitHub's private vulnerability reporting: open the repository's **Security** tab and choose
**Report a vulnerability**. Only the maintainers see the report.

Include what you can:

- what the problem is and what an attacker could do with it;
- how to reproduce it: a design file, command, MCP message or steps;
- the CircuitCraft version or commit, and your platform.

We aim to acknowledge a report within a week, keep you updated while we work on a fix, and credit
you in the release notes unless you prefer otherwise. CircuitCraft is maintained by volunteers, so
these are aims, not guarantees.

## What counts

CircuitCraft opens files from other people and accepts commands from agents, so these are security
problems:

- a design file, library or other input that crashes CircuitCraft, hangs it, or makes it use
  unbounded memory or disk;
- anything that lets a file, a command or an MCP or control-channel message read or write files it
  should not, run code, or reach the network;
- a leaked secret or credential in the repository or a release.

A crash on hostile input is a bug we always fix, whether or not you think it is exploitable.

## Supported versions

CircuitCraft has no releases yet. Fixes go to `main`. Once releases begin, the latest release will
receive security fixes.
