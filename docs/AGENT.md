# The agent

`vds-agent` is the optional daemon side of VDS Admin. It runs on a machine you want to
watch, reads that machine, and serves the reading over HTTPS to the application.

You do not need it. Agentless SSH mode collects the same data. This document explains
when the agent is worth installing, how to install it safely, and how to operate it.

---

## 1. When to use it

The two modes gather identical data. They differ in what they cost and what they require.

|  | SSH | Agent |
|---|---|---|
| On the monitored host | nothing | one static binary, ~4 MB |
| Credential the app holds | SSH key or password | a bearer token |
| What that credential can do | whatever the SSH user can do | read this host's metrics, and nothing else |
| Per poll | TCP + key exchange + auth + a batch of commands | one HTTPS request |
| Firewall | outbound to port 22 | inbound on 9443, from the app only |

Two reasons to install it:

**Cost at scale.** An SSH poll is dominated by the handshake. At fifty servers every
fifteen seconds that is invisible; at a thousand it is the largest thing the application
does. The agent moves the work to the machine being watched, where it is a few `/proc`
reads behind a short cache.

**Least privilege.** SSH mode needs an account on every server that can run `df`, `ps`,
`docker` and `systemctl`. The agent's token can do exactly one thing: read this host's
metrics. There is no endpoint that changes anything, so a stolen token is worth a reading
of the machine and nothing more.

Two reasons not to:

- **Nothing to install** is a real advantage, and it is SSH mode's.
- **An open inbound port** is a real cost, and it is the agent's. If the host is not
  behind a VPN, think about it before deciding.

The choice is per server, and reversible.

---

## 2. Installing

### 2.1 The quick way

```sh
curl -fsSL https://github.com/vds-admin/vds-admin/releases/latest/download/install.sh | sudo sh
```

This detects the architecture, downloads the matching tarball, **verifies it against the
release's `SHA256SUMS` before unpacking anything**, creates the `vds-agent` system user,
installs the binary and the systemd unit, generates a token and a certificate, and starts
the service.

It ends by printing the two values you need in the app:

```
==> vds-agent is installed and running

  Version:      vds-agent 0.1.0
  Listening on: web-01:9443
  Fingerprint:  3C:F2:...:9A

Add this server in VDS Admin with the token below.

  Token: k3Jd8vQ2mN7pX1aY6bZ4cR9eT5wU0iO8sL2fH6gK4jM=
```

Re-running the installer is safe: an existing configuration, token and certificate are
never overwritten.

### 2.2 The way to use on a production fleet

Piping a script into a root shell trusts the release host completely. Checksum
verification protects you from a corrupted download and from a tampered *artefact* — but
not from a release host that serves a matching pair of tarball and checksum file.

The signature does protect you from that, because the signing key never touches a build
machine. Verify once, on a machine you trust, then distribute the verified tarball:

```sh
# 1. Fetch the artefact, the checksums and the signature.
BASE=https://github.com/vds-admin/vds-admin/releases/latest/download
curl -fsSLO $BASE/vds-agent-x86_64-unknown-linux-musl.tar.gz
curl -fsSLO $BASE/SHA256SUMS
curl -fsSLO $BASE/SHA256SUMS.asc

# 2. Check the signature FIRST. If this fails, stop; nothing below is meaningful.
gpg --verify SHA256SUMS.asc SHA256SUMS

# 3. Now the checksums can be trusted.
sha256sum --check SHA256SUMS --ignore-missing

# 4. Install from the verified file, on each host.
tar -xzf vds-agent-x86_64-unknown-linux-musl.tar.gz
sudo ./install.sh --archive vds-agent-x86_64-unknown-linux-musl.tar.gz
```

To have the installer do step 2 itself, give it the key fingerprint:

```sh
export VDS_AGENT_GPG_KEY=A1B2C3D4E5F60718293A4B5C6D7E8F9012345678
curl -fsSL $BASE/install.sh | sudo -E sh
```

It will then refuse to install unless `SHA256SUMS` carries a valid signature from that
key.

### 2.3 Architectures

| `uname -m` | Target |
|---|---|
| `x86_64` | `x86_64-unknown-linux-musl` |
| `aarch64` | `aarch64-unknown-linux-musl` |
| `armv7l` | `armv7-unknown-linux-musleabihf` |
| `armv6l` | `arm-unknown-linux-musleabihf` |
| `i686` | `i686-unknown-linux-musl` |

Every build is statically linked against musl, so there is no glibc version to satisfy.
The same binary runs on a 2019 CentOS box, a current Debian and an Alpine container.

---

## 3. Adding the server in the app

1. **Servers → Add server**, and choose **Agent** as the connection mode.
2. Enter the host, the port (9443 by default) and the token the installer printed.
3. On the first connection the app shows the certificate fingerprint and asks you to
   confirm it.

**Compare that fingerprint with the one the installer printed.** They must match. This is
the same trust-on-first-use model as an SSH host key: the app pins what it sees, and
warns loudly if it ever changes. If it does not match, something is between you and that
host — do not accept it.

To read the values again later:

```sh
sudo cat /etc/vds-agent/token
sudo vds-agent --fingerprint --config /etc/vds-agent/agent.toml
```

---

## 4. Configuration

`/etc/vds-agent/agent.toml`. Every setting has a default; a file containing nothing but a
token is valid and complete. `packaging/agent/agent.toml.example` is the annotated
reference.

| Setting | Default | Notes |
|---|---|---|
| `token_file` | `/etc/vds-agent/token` | Preferred over an inline `token`; lets the config stay readable while the secret is `0600` |
| `token` | — | Inline alternative. Ignored when `token_file` is set |
| `bind` | `0.0.0.0` | Set to the tunnel address, or `127.0.0.1`, if the app reaches this host through a VPN |
| `port` | `9443` | |
| `tls_certificate` / `tls_private_key` | — | Set both to use your own certificate. Left unset, one is generated on first start |
| `state_dir` | `/var/lib/vds-agent` | Where a generated certificate and key live |
| `cache_ttl_secs` | `5` | See below |
| `collect_timeout_secs` | `10` | Per command |
| `collect_docker` | `true` | |
| `collect_services` | `true` | |
| `collect_processes` | `true` | The biggest saving on a host with thousands of processes |
| `log_level` | `"info"` | `trace`/`debug`/`info`/`warn`/`error` |

Always check a change before restarting:

```sh
sudo vds-agent --check --config /etc/vds-agent/agent.toml
```

The systemd unit runs exactly this as `ExecStartPre`, so a bad edit fails *before* the
running agent is replaced. A typo cannot take your monitoring down.

### The cache

A collection reads a dozen `/proc` files and, where present, runs `docker` and
`systemctl`. That is cheap once and not cheap once per client per poll: several app
instances may watch one host, and a dashboard refresh and an open detail page arrive
together.

So a report is served from memory for `cache_ttl_secs` before the host is read again.
Staleness is bounded and visible — every report carries the timestamp of the collection
that produced it, and the app shows how old a reading is. Raise it on a busy host.

### Turning collectors off

A disabled collector is removed from the plan entirely; the work is not done and
discarded. On a host running thousands of processes:

```toml
collect_processes = false
```

The app then shows the Processes tab as unavailable for that server rather than empty —
the distinction between "not collected" and "nothing running" is preserved end to end.

---

## 5. Operating

```sh
systemctl status vds-agent          # is it running
journalctl -u vds-agent -f          # what it is doing
journalctl -u vds-agent -n 50       # why it stopped
systemctl restart vds-agent         # after a configuration change
```

### The endpoints

| Path | Auth | Purpose |
|---|---|---|
| `/v1/health` | none | Liveness. Reveals only that an agent is here, which the open port already gave away |
| `/v1/info` | bearer | Version, hostname, architecture, capabilities |
| `/v1/metrics` | bearer | The full reading |

Everything else is a 404. There are no write endpoints, and no endpoint that runs a
command on request.

Check by hand — `-k` because the certificate is self-signed:

```sh
curl -k https://localhost:9443/v1/health
curl -k -H "Authorization: Bearer $(sudo cat /etc/vds-agent/token)" \
     https://localhost:9443/v1/metrics | head -40
```

### Resource use

At rest the process is an idle runtime and a cached report: a few megabytes of RSS and no
measurable CPU. Collection happens on request, not on a timer, so an agent nobody is
watching costs nothing.

The systemd unit sets ceilings anyway, because a monitoring agent that destabilises the
machine it watches is worse than no agent:

```
MemoryMax=128M
CPUQuota=10%
TasksMax=64
```

### Upgrading

Re-run the installer. It replaces the binary and the unit, keeps the configuration, token
and certificate, and restarts the service. The fingerprint does not change, so the app's
pin stays valid.

```sh
curl -fsSL $BASE/install.sh | sudo sh
```

### Removing

```sh
sudo ./install.sh --uninstall   # stops and removes the service and binary
sudo ./install.sh --purge       # also removes the config, token, certificate and user
```

`--uninstall` deliberately keeps the certificate, so that reinstalling later does not
invalidate the app's pin.

---

## 6. Security

### What the token protects

Everything except `/v1/health`. It is compared in constant time — a naive comparison
leaks the token one byte at a time to anyone who can measure response latency, which is
impractical over the internet and entirely practical over a LAN or a loopback interface.

The agent refuses to start with a token shorter than 32 characters. `token = "test"`
reaching production is the failure that actually happens.

The token is never logged, at any level, and never appears in the `Debug` rendering of
the configuration.

### What TLS protects

Confidentiality and integrity in transit, plus the guarantee that the second connection
reaches the same machine as the first — that last one via fingerprint pinning in the app,
not via a certificate authority.

A self-signed certificate is the default because an agent typically runs on a host with
no public DNS name and no ACME client, and refusing to start without a CA-issued
certificate would make the daemon unusable until someone did paperwork. If you have a
real certificate, point `tls_certificate` and `tls_private_key` at it.

The generated key is written `0600` at the moment of creation. A key that is briefly
world-readable has already leaked.

### What the agent deliberately cannot do

It runs no commands on request, writes nothing outside its own state directory, and has
no endpoint that changes the host. That ceiling is the point: it is what makes a stolen
token a bounded loss.

Container and service *control* is a feature the application is structured for. It is not
in the agent yet, because adding it changes what that token is worth and it deserves
confirmation flows and an audit trail first.

### Hardening

The unit runs as an unprivileged user with `ProtectSystem=strict`, an empty capability
bounding set, `SystemCallFilter=@system-service`, `MemoryDenyWriteExecute=yes` and
`RestrictAddressFamilies` limited to inet and unix sockets. Check it after any change:

```sh
systemd-analyze security vds-agent
```

One exception is deliberate: `ProtectProc=default` rather than `invisible`. Reporting the
process table is the agent's job, and hiding other users' processes from it would return
a report showing only its own.

### Firewall

Allow 9443 **from the machine running VDS Admin, and only from it**:

```sh
# nftables
nft add rule inet filter input ip saddr 10.0.0.5 tcp dport 9443 accept

# ufw
ufw allow from 10.0.0.5 to any port 9443 proto tcp
```

Better still, if the app reaches the host over a VPN or an SSH tunnel, set
`bind = "127.0.0.1"` — an address the agent does not listen on cannot be reached by a
firewall rule someone forgets to add.

---

## 7. Troubleshooting

**The service will not start.**

```sh
sudo vds-agent --check --config /etc/vds-agent/agent.toml
```

This reports the exact problem: a missing token, a token that is too short, a malformed
file, a certificate that does not match its key. `journalctl -u vds-agent -n 50` has the
rest.

**The app says the server is offline, but the agent is running.**

Check, in this order:

```sh
curl -k https://localhost:9443/v1/health         # on the server: is it answering at all?
ss -tlnp | grep 9443                             # is it listening where you think?
curl -k https://<server>:9443/v1/health          # from the app's machine: firewall?
```

**The app reports a fingerprint mismatch.** The certificate changed. Either it was
regenerated — deleting `/var/lib/vds-agent/agent.crt` does that, and so does `--purge`
followed by a reinstall — or you are not talking to the machine you think you are. Do not
accept the new fingerprint until you know which.

**A tab is empty or unavailable.** The report says which collectors did not run and why.
"Unsupported" means the host lacks the feature — no Docker, no systemd, no thermal
sensor — and is not a fault. Confirm from the host:

```sh
curl -k -H "Authorization: Bearer $(sudo cat /etc/vds-agent/token)" \
     https://localhost:9443/v1/metrics | grep -A5 errors
```

**401 on every request.** The token in the app does not match `/etc/vds-agent/token`.
The reply says only `unauthorized` — distinguishing "no token" from "wrong token" would
hand a prober information for nothing — but the journal records which it was.

---

## 8. The protocol

`crates/agent-protocol` is the wire contract, shared by both sides so they cannot
disagree about the format. It depends on nothing else in the workspace, so a third-party
agent could implement it without pulling in the application's internals.

Every message carries a version. The app requires the same major version and tolerates a
newer minor one by ignoring fields it does not know; every additive field has a default,
so an older agent's smaller payload also parses.

Two properties the format preserves deliberately:

- a metric that was not measured is `null`, never `0`;
- `containers: null` means "no Docker on this host" and `containers: []` means "Docker
  with nothing running". The app renders those differently.
