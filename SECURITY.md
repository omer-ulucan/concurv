# Security policy

## Supported versions

Only the **latest release** and `main` receive security fixes; there are no
backports to older releases yet. Pre-releases (`-alpha`) are not supported.

## Reporting a vulnerability

Please **do not open a public issue** for a vulnerability. Report it
privately through GitHub: the repository's **Security** tab →
**Report a vulnerability** (private vulnerability reporting). Include the
version, the platform, and the smallest reproduction you have.

This is a one-maintainer project; reports are answered as soon as possible,
and fixes are released and credited (unless you prefer otherwise) once a
fix is available.

## What counts

The threat model (`docs/security/threat-model.md`) lists what concurv
defends against. In particular, these are vulnerabilities:

- **loss or silent corruption of acknowledged work**: a successful `save`,
  `publish` or `push` whose data is later missing, or a corrupt object that
  is accepted instead of refused;
- **writes outside the working copy**, or overwriting files concurv must
  never touch (dirty files without `--force`, ignored or untracked files),
  through restore, update, merge, branch switch, clone or Git import;
- **relay authentication or authorization bypass**: acting without a valid
  token, beyond a device's scopes, or after revocation; a client sending its
  token to a server that does not hold the pinned certificate;
- **a hostile relay or peer** making a client store objects that fail
  verification, follow a rewound head, or create aliasing references;
- **policy checks running without approval**: commands from a
  `concurv-policy.json` executed on a device that has not trusted that exact
  policy version, or an attestation accepted for another candidate, branch,
  policy or platform;
- leaks of device tokens or of the relay's private key.

## Known limits (not vulnerabilities by themselves)

These are documented design limits; reports that show they are worse than
documented are still welcome.

- Approved policy checks run with **no sandbox** (`"sandbox": "none"`);
  approving a policy means trusting its commands.
- Content addressing provides integrity, not authorship: there are no
  signatures yet (planned for R7). A device with a valid token is trusted
  for what its scopes allow.
- A relay can withhold or delay publications; per-device quotas and rate
  limits do not exist yet.
- An attacker with write access to `.concurv` can destroy data (it will be
  detected, not prevented); concurv trusts the `git` found on `PATH`.
- Ignore rules are not a secrets boundary: already-saved secrets stay in
  history.
