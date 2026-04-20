# Security Policy

## Supported Versions

Only the latest release is actively maintained. Security fixes are not backported to older versions.

| Version | Supported |
|---------|-----------|
| latest  | ✅        |
| older   | ❌        |

## Reporting a Vulnerability

**Please do not open a public GitHub issue for security vulnerabilities.**

Report vulnerabilities privately via GitHub's built-in mechanism:
**[Report a vulnerability](https://github.com/tekanic/roe/security/advisories/new)**

Include as much of the following as possible:

- Description of the vulnerability and its potential impact
- Steps to reproduce or a proof-of-concept
- Affected version(s)
- Any suggested fix, if you have one

## Response Timeline

| Milestone | Target |
|-----------|--------|
| Acknowledgement | 48 hours |
| Initial assessment | 5 business days |
| Fix or mitigation | 30 days for critical/high, best-effort for lower severity |
| Public disclosure | Coordinated with reporter after fix is released |

## Scope

`roe` is a local terminal tool that reads and writes files on disk. It makes no network connections and handles no user authentication or credentials. The primary attack surface is **maliciously crafted input files** (JSON, YAML, TOML, XML).

**In scope:**
- Crashes, panics, or hangs caused by crafted input files
- Memory unsafety in parsing or rendering code
- Vulnerabilities in direct dependencies (file a report and we will update)
- Unexpected file writes outside the target path

**Out of scope:**
- Social engineering
- Physical access attacks
- Issues in your terminal emulator or OS

## Dependency Vulnerabilities

We track transitive dependencies via [cargo-audit](https://github.com/rustsec/rustsec) and the [RustSec Advisory Database](https://rustsec.org). If you discover a vulnerability in a dependency before we do, please report it here and we will update or replace the affected crate promptly.
