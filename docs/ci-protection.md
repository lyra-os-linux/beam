# Required CI for main

The GitHub branch protection for `main` requires the check named `contracts`,
published by GitHub Actions (app ID `15368`). The check name comes from the
published check run, rather than the workflow title `Contracts`.

The branch must be up to date before merging (`strict=true`). Protection applies
to administrators (`enforce_admins=true`). Changes use pull requests; no extra
reviewer approval is required. Force pushes and branch deletion are disabled.
This policy is configured in GitHub settings, not by this Markdown file.

## Coverage

The existing workflow runs workspace tests, Rust formatting and Clippy,
explicit loopback handshake tests, the queued-input GTK regression under Xvfb,
translation checks, desktop/AppStream metadata validation, RPM spec parsing
and the offline vendor contract. PRs have no path or branch filter, and the
required job has no job-level skip condition.

These checks do not replace native GNOME/RDP acceptance, RPM installation,
signed OBS binary qualification or the Lyra ISO gates. A green workflow alone
does not prove a successful OBS publication.

## Qualification

On 2026-09-15 the API returned HTTP 404 (`Branch not protected`) for `main`.
The branch reported `protected=false`, and its applicable-rules query returned
an empty list. Protection therefore had to be created rather than merely
adding a check to an existing policy.

[PR #5](https://github.com/lyra-os-linux/beam/pull/5) exercised actual merge
requests using the administrator account and the exact head revision
`d006d52e0bcc2b94f37eee5d794e8c00c48437a2`:

| Required check state | Merge response |
| --- | --- |
| Running | HTTP 405: `Required status check "contracts" is in progress.` |
| Intentionally failed | HTTP 405: `Required status check "contracts" is failing.` |

The [controlled run](https://github.com/lyra-os-linux/beam/actions/runs/34991817969)
used a temporary failure step limited to that PR branch. Both refusals left
`main` at `d131f19a1ba0809f96057b8e05bca65ed137e964`. The probe was then removed,
restoring the workflow byte for byte before running the full CI and integrating
this documentation. The final passing run and successful squash merge receipt
are recorded in [issue #1](https://github.com/lyra-os-linux/beam/issues/1).

## Exceptions and recovery

No user, team or app bypass is configured in this branch protection. Repository
administrators can still deliberately edit or remove the rules; administrator
enforcement constrains merges while the policy is in effect.

GitHub also accepts `neutral` and `skipped` check conclusions. Keep the required
job executing when changing workflow conditions. If the check name or producer
changes, update protection to the exact published name and app identity.

If an explicitly approved recovery requires reverting this change, restore the
recorded unprotected baseline by removing this branch protection. That also
removes its PR, CI, force-push and deletion safeguards. Diagnose a failing check
before considering that recovery; do not disable protection for routine merges.
