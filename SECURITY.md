# Security policy

The Tick packages handle broker credentials: `longbridge` reads and renews OAuth tokens for a Longbridge account, and packages may receive API keys from Tickloop. Please report vulnerabilities privately.

## Reporting a vulnerability

Report it through [GitHub Security Advisories](https://github.com/tickloopai/ticks/security/advisories/new) on this repository. Do not open a public issue, and do not include real tokens, keys or account data in the report.

Include the affected package and version, the steps to reproduce, and the impact you observed. We will acknowledge the report, keep you informed while we investigate, and credit you in the advisory if you wish.

## Scope

- The package programs and their handling of tokens, keys and the token file.
- The release pipeline: archives, their SHA-256 in `marketplace.json`, and the workflows that publish them.

Vulnerabilities in Tickloop itself, including how it stores credentials and installs packages, belong to [tickloopai/tickloop](https://github.com/tickloopai/tickloop). Issues in a broker's own service or SDK should go to that broker.

Only the latest version of each package is supported; fixes ship as a new package version.
