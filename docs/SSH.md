# Agentless mode (SSH)

The default way to monitor a server: no software installed on the target, no inbound
port, no agent to upgrade. The application connects, runs a batch of read-only commands,
and parses the output.

This document covers what it runs, what it needs, and what to do when it does not work.

---

## 1. What the server needs

A POSIX shell and the standard userland — which is to say, nothing you have to install.
Specifically:

| Needed for | Command or file |
|---|---|
| Hostname, OS, kernel, CPU model | `/proc/sys/kernel/hostname`, `/etc/os-release`, `uname -s -r -m`, `/proc/cpuinfo` |
| CPU usage | `/proc/stat`, sampled twice |
| Memory and swap | `/proc/meminfo` |
| Filesystems | `df -PkT` (falls back to `df -Pk`) |
| Network counters | `/proc/net/dev` |
| Load average and uptime | `/proc/loadavg`, `/proc/uptime` |
| Processes | `ps -eo pid,user,pcpu,pmem,rss,args` |
| Temperature | `/sys/class/thermal/thermal_zone*/temp` |
| Docker | `docker ps -a`, `docker stats --no-stream` |
| systemd | `systemctl list-units --type=service` |

Everything is read-only. Nothing is written, installed or changed.

A host that lacks something degrades rather than fails: no thermal sensor, no Docker and
no systemd are all reported as *unsupported*, which is shown as an absence and does not
count against the server's health. A collector that fails for a real reason — a
permission error, a truncated read — is reported as a failure and does count.

---

## 2. The account

Root works and is not necessary. A dedicated monitoring user is better:

```sh
sudo useradd --system --create-home --shell /bin/sh vds-monitor
sudo mkdir -p /home/vds-monitor/.ssh
sudo tee /home/vds-monitor/.ssh/authorized_keys > /dev/null <<'KEY'
from="10.0.0.5",no-agent-forwarding,no-port-forwarding,no-pty ssh-ed25519 AAAA... vds-admin
KEY
sudo chown -R vds-monitor:vds-monitor /home/vds-monitor/.ssh
sudo chmod 700 /home/vds-monitor/.ssh
sudo chmod 600 /home/vds-monitor/.ssh/authorized_keys
```

The `from=` restriction is worth the two minutes: it means a stolen key is useless from
anywhere but the machine running the application.

Two panels need more than the default:

**Docker.** Reading containers needs access to the Docker socket:

```sh
sudo usermod -aG docker vds-monitor
```

Be aware of what that grants: membership of the `docker` group is equivalent to root on
that host. If you are not comfortable with it, leave it out — the Docker panel then shows
as unavailable and everything else works normally.

**systemd.** `systemctl list-units` works for an unprivileged user on a default systemd
installation. No extra rights are needed to *read* unit state.

---

## 3. Authentication

Three methods, in order of preference:

**Private key.** Paste or load the key when adding the server. It goes to the OS keystore,
never to the database or the configuration file.

**Encrypted private key.** The key and its passphrase are stored together, both in the
keystore.

**Password.** Supported, stored the same way. Prefer a key: a password is replayable, and
`from=`-restricted keys are not.

The application does not read `~/.ssh/id_*` or use `ssh-agent`. Credentials are explicit,
so that what the application can reach is exactly what you told it about — and so that
uninstalling it does not leave your personal keys entangled with a monitoring tool.

Supported key types: Ed25519, ECDSA, and RSA (SHA-256 or SHA-512 signatures; SHA-1 is not
offered).

---

## 4. Host key verification

On the first connection to a server the application records its host key. On every
connection after that it checks it.

If the key changes, the connection **stops**. The server is marked offline with a clear
reason, and no commands are sent. This is trust on first use, the same model as an SSH
client — and the same caveat applies: the first connection is the one you have to be sure
about.

A key legitimately changes when a host is rebuilt or its `sshd` keys are regenerated. Then
**Server → Settings → Forget host key** and reconnect, checking the new fingerprint
against what the server actually has:

```sh
ssh-keyscan -t ed25519 web-01 | ssh-keygen -lf -
```

Pins live in the application's own data directory. `~/.ssh/known_hosts` is neither read
nor written: a monitoring tool should not be able to change what your interactive SSH
client trusts.

---

## 5. How a collection cycle works

The interesting part is that a cycle is **one channel and one round trip**, not one per
metric.

Each collector declares the commands it needs without running them. The registry
concatenates every command into a single script, with each command's output bracketed by
a unique marker and its exit status recorded:

```sh
printf '\n__VDS_ADMIN_MARKER__:BEGIN:0\n'
cat /proc/stat; sleep 0.2; printf '__VDS_SAMPLE__\n'; cat /proc/stat
printf '\n__VDS_ADMIN_MARKER__:END:0:%d\n' "$?"
printf '\n__VDS_ADMIN_MARKER__:BEGIN:1\n'
cat /proc/meminfo
printf '\n__VDS_ADMIN_MARKER__:END:1:%d\n' "$?"
...
```

The reply is split back apart on the markers and handed to each collector to parse. This
matters at scale: ten collectors over ten servers is ten connections rather than a
hundred.

Two consequences worth knowing:

- **A command whose markers are missing is reported as an error, not as empty output.** A
  connection that dropped part way through must never look like a host with nothing to
  say.
- **CPU usage needs two samples.** They are taken 200 ms apart *inside* the same script,
  so the delay is on the server and costs no extra round trip.

Sessions are pooled and reused between cycles, and closed when they go stale. A server
whose SSH daemon restarts gets a new session on the next cycle without any special
handling.

---

## 6. Timeouts, retries and offline detection

Every operation is bounded: connection, authentication, and the command batch. The
per-server timeout defaults to 20 seconds and is configurable.

Failures back off exponentially with jitter, so a server that goes down does not turn into
a tight reconnect loop, and a network partition does not produce a thundering herd when it
heals.

A server is marked **offline** after a configurable number of consecutive failures —
three by default. A single failed poll shows as a warning, not an outage: transient
failures are common and an alert for every one of them trains you to ignore alerts.

One exception. An authentication failure is **not** retried with backoff. A rejected
password will be rejected again in thirty seconds; retrying hard risks tripping account
lockouts, and the condition needs a human either way.

---

## 7. Troubleshooting

Start by reproducing what the application does, by hand:

```sh
ssh -i /path/to/key vds-monitor@web-01 'cat /proc/stat; df -PkT; ps -eo pid,user,pcpu,pmem,rss,args | head'
```

If that works and the application does not, the difference is in the connection, not the
commands.

**"Authentication failed."** The username, the key or the passphrase is wrong, or the key
is not in `authorized_keys`. Check the server:

```sh
sudo journalctl -u ssh -n 50        # or -u sshd
```

A common cause is permissions: `sshd` refuses a key when `~/.ssh` is not `700` or
`authorized_keys` is not `600`.

**"Connection refused" or a timeout.** Wrong port, firewall, or `sshd` not running.

```sh
ss -tlnp | grep :22
```

**"Host key mismatch."** Read §4. Do not clear the pin until you know why it changed.

**A tab is empty.** The server detail page lists which collectors did not run and why.
"Unsupported" means the host lacks the feature and is not a fault. For Docker
specifically, check the account is in the `docker` group:

```sh
ssh vds-monitor@web-01 'docker ps'
```

**Everything is slow.** A cycle is one round trip, so per-server latency is roughly the
SSH handshake plus the batch. If a server is far away or the fleet is large, raise its
poll interval — or move it to [agent mode](AGENT.md), which is what that mode is for.

**Nothing works and the reason is not obvious.** Turn on **Settings → Debug mode**, which
raises the log level and records every command and its exit status. Secrets are redacted
at three separate layers, so the log is safe to attach to an issue —
[SECURITY.md](SECURITY.md) §3 explains how that is enforced, and a leak there is itself a
bug worth reporting.

---

## 8. Where this lives in the code

| | |
|---|---|
| `crates/infra-ssh/src/session.rs` | Connection, authentication, running a script |
| `crates/infra-ssh/src/known_hosts.rs` | Host-key pinning and fingerprints |
| `crates/infra-ssh/src/batch.rs` | Building the batched script and splitting the reply |
| `crates/infra-ssh/src/probe.rs` | The `ServerProbe` port, and session pooling |
| `crates/infra-collectors/` | The parsers — pure functions, tested against captured real output |

The split is deliberate: a collector declares commands and parses text, and performs no
I/O at all. That is what lets the same parsers serve both this mode and the agent, and be
tested without a server. See [ADR-002](adr/002-monitoring-architecture.md).
