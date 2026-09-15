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

Issue #1 tracks the live qualification: an administrator attempts to merge a
controlled PR while the check is pending, again while it fails intentionally,
and finally after the temporary probe is removed and the full check succeeds.
The exact revisions and API responses are recorded before closing the issue.

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
