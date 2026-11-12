# Running concurv in production

This guide covers running a relay (`concurvd`) as a service, backing up
relays and repositories, upgrading, and checking health. The commands
themselves are in `docs/user-guide.md`; the guarantees in
`docs/compatibility.md` and `docs/security/threat-model.md`.

## The relay

One relay directory serves **one repository**. Its layout:

| Path | Contents |
|---|---|
| `objects/` | content-addressed objects; only ever added, never changed or deleted |
| `state.json` | the published head of every branch; replaced atomically, the commit point of every publish |
| `devices.json` | devices: name, scopes, hashed token |
| `tags.json`, `intents.json` | tags (create-only) and advisory intents |
| `tls/cert.der`, `tls/key.der` | the relay's identity; clients pin the certificate's fingerprint. `key.der` is owner-only on Unix |
| `publish.log` | audit trail, one JSON line per publish |
| `lock`, `tmp/`, `quarantine/` | locking and staging |

A relay can be stopped or killed at any moment: it restarts with the last
committed heads (tested with killed relay processes, `docs/ROADMAP_STATUS.md`).

### As a Linux service (systemd)

```ini
# /etc/systemd/system/concurv-myproject.service
[Unit]
Description=concurv relay (myproject)
After=network-online.target
Wants=network-online.target

[Service]
User=concurv
Group=concurv
ExecStart=/usr/local/bin/concurvd serve /srv/concurv/myproject --listen 0.0.0.0:7461
Restart=on-failure
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
ReadWritePaths=/srv/concurv/myproject

[Install]
WantedBy=multi-user.target
```

```console
# useradd --system --home /srv/concurv --shell /usr/sbin/nologin concurv
# install -d -o concurv -g concurv -m 700 /srv/concurv
# sudo -u concurv concurvd init /srv/concurv/myproject
# systemctl enable --now concurv-myproject
```

Run `concurvd device add|list|revoke` as the same user. On Windows, run
`concurvd serve` under a dedicated account from Task Scheduler ("at
startup", restart on failure); there is no native Windows service yet.

### Network exposure

- Every connection is TLS 1.3 with a certificate that clients pin, and
  every request is authenticated with a device token; no public CA or DNS
  name is needed.
- Limits: 64 concurrent connections (`--max-connections`), 120 s idle
  timeout (`--idle-timeout`), 1 MiB messages, blobs up to 4 GiB.
  **There are no per-device quotas or rate limits yet**: an authenticated
  device can fill the disk. Expose a relay to untrusted networks only
  behind a firewall or VPN, and give `write` only to devices you trust.
- Give each device the narrowest scopes: `read` for CI that only fetches,
  `publish:<branch>` to limit publishing, `attest` only to check runners
  of protected branches.

### Tokens

A token is shown once by `concurvd device add`. Store it with
`--token-file` on the client (owner-only file on Unix) or pass it in
`CONCURV_REMOTE_TOKEN`; never in shell history or logs. If one leaks:
`concurvd device revoke <dir> <name>` (effective at the next connection),
then add a device under a new name. Tokens do not expire on their own.

### Backups and restores

- **Back up:** stop the relay, copy the whole directory, start it. Objects
  are immutable, so incremental copies (rsync) of `objects/` are cheap.
- **Disk:** nothing is ever deleted (there is no garbage collection yet);
  plan for growth.
- **Restore:** every client that fetched after the backup was taken will
  report the relay's branches as `rewound` and refuse to follow or push
  them, because a relay head must never go backwards. Their history is
  intact. Bring the newer work back from a client that has it, not by
  editing `state.json`: there, `concurv remote remove origin` and `concurv
  remote add origin …` (forgetting the heads it saw; local branches stay),
  then `concurv push`, a normal fast-forward of the restored head.
- The fingerprint changes only if you re-initialize the relay; then every
  client must update its remote (`remote remove`, `remote add` with the new
  fingerprint).

### Upgrades

Stop the relay, replace the `concurvd` binary, start it. The relay format
is unchanged within 0.x unless the release notes say otherwise. Clients and
relays of different releases interoperate (`docs/compatibility.md`).

### Health

- The process is up and listening (`concurvd serve` prints `listening on
  <addr>` first).
- `publish.log` grows with every publish.
- `concurvd device list <dir>` shows devices and revocations.
- From a client: `concurv fetch` succeeds; `concurv verify --full` in a
  clone checks every object it holds.

## Repositories

- Keep repositories on a local disk, not on network filesystems or in
  cloud-synced folders.
- **Back up** by copying the repository directory while no concurv command
  runs, or by pushing to a relay. `concurv verify --full` checks every
  object; run it after restoring a copy.
- Before trying a newer concurv on an important repository, copy it:
  upgrades of the format are one-way (`docs/compatibility.md`).
- One workspace per person, agent or task. Remove finished ones with
  `concurv workspace remove` (it refuses while work would be lost).

## Agents and automation

- Use `--json` and branch on exit codes and `error.code`, never on text.
- Set `CONCURV_AUTHOR` per agent, and `CONCURV_CONFIG=` (empty) so an agent
  does not inherit a user's configuration.
- Mutating commands wait up to 30 s for a busy lock, then fail with exit
  40: retry with backoff.
- Protected branches run repository commands **without a sandbox** on the
  machine that runs `concurv check`; run checks in a disposable VM or
  container when the repository's contents are not trusted.
