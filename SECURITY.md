# Security

Report vulnerabilities privately: Security tab, "Report a vulnerability". Do not open a public issue.

Include the version (`acusage --version`), OS, install method, steps or a synthetic log to reproduce, and the impact. Never attach a real session log.

Only the latest release gets fixes. We aim to reply within a week.

## In scope

- Any way to make acusage write or delete a file, open a network connection or read credentials.
- A crash or hang on a crafted log file.
- Leaking log data.

Bugs in the agents themselves, GitHub, npm or your machine are out of scope.
