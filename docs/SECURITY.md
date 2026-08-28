# Security

This document states what VDS Admin protects, what it does not, and how it handles the
credentials you give it. It is written to be checked against the code, not to reassure.

---

## 1. What the application holds

To do its job it must hold, at minimum:

- **SSH credentials** for every agentless server — a password, a private key, or an
  encrypted private key and its passphrase;
- **agent tokens**, one per agent-mode server;
- **OAuth tokens** for analytics providers;
- **a database** of servers, websites, metric history, incidents and events.

That is a concentrated target. A machine running VDS Admin with a fleet configured can
reach every server in that fleet. Treat it accordingly: it deserves the same care as a
jump host.

---

## 2. Where secrets live

**Never in the database, never in the configuration file, never in plaintext on disk.**

The database stores a *reference* — a UUID — and the secret itself goes to the platform's
credential store:

| Platform | Store |
|---|---|
| Windows | Credential Manager |
| macOS | Keychain |
| Linux | Secret Service (GNOME Keyring, KWallet) |
| Android | Keystore |

Where no such store exists — a headless Linux box with no D-Bus session, which is a
common way to run this — the fallback is an encrypted file:

- **Argon2id** derives the key: 64 MiB of memory, 3 passes, a 16-byte random salt per
  vault. The parameters are asserted against the OWASP minimum in a `const` block, so
  lowering them to speed up a test run fails the build.
- **XChaCha20-Poly1305** encrypts the contents, with a fresh 24-byte nonce per write.
- Writes go to a temporary file and are renamed into place, so an interrupted write
  cannot leave a half-encrypted vault.

Which backend is in use is shown in **Settings → Security**. It is not hidden, because
"my secrets are in the OS keystore" and "my secrets are in a file protected by a
passphrase derived from this installation" are different security postures and you should
know which one you have.

### The passphrase for the fallback

Derived from a per-installation value, so it is stable across restarts without prompting.
This protects against a stolen *file* — a backup, a snapshot, a discarded disk — not
against an attacker who already runs code as your user. Nothing on a single-user desktop
can protect against that, and claiming otherwise would be dishonest.

---

## 3. What never reaches a log

- passwords
- private keys and passphrases
- bearer tokens and OAuth tokens
- certificate private keys

This is enforced in three places rather than by discipline:

1. **The type.** `Secret` has a hand-written `Debug` that prints `Secret(<redacted>)`, does
   not implement `Serialize`, and zeroes its contents on drop. It cannot be printed by
   accident.
2. **The writer.** Every log line passes through a redacting layer that recognises
   `password=`, `token=`, `authorization:`, `Bearer `, `Basic `, and private-key PEM
   headers, and replaces the value. Redaction is per line, so a secret at the end of one
   line cannot swallow the next.
3. **The tests.** Each of the above has tests that assert a known secret does not appear
   in the rendered output. They are the reason the `Basic <credential>` case was found.

`AgentConfig` and `TlsMaterial` have hand-written `Debug` implementations for the same
reason: a derived one would print a new field the day someone adds it.

If you find a secret in a log file, that is a bug worth reporting — see §8.

---

## 4. Connections

### SSH

- Host keys are pinned on first use and verified on every subsequent connection. A changed
  key stops the connection and reports it; it is not accepted silently.
- Password, private key, and encrypted private key are supported. Prefer keys.
- Every operation has a timeout. Sessions are pooled and reused.
- The application does not write to `~/.ssh/known_hosts`; its pins are its own.

Details and troubleshooting: [SSH.md](SSH.md).

### The agent

- HTTPS only. Bearer token on every endpoint except liveness.
- Tokens are compared in constant time.
- The certificate is pinned on first connection, SSH-style, and a change is reported.
- The agent exposes no endpoint that changes the host.

Details: [AGENT.md](AGENT.md).

### Website monitoring

Certificates are inspected, not trusted. Checking that a certificate expires in nine days
requires reading an expired or otherwise invalid certificate, so the checker uses a
verifier that accepts anything and *reports* what it found. That verifier is used for
monitoring and nothing else — it is named `InspectionOnlyVerifier`, documented as
read-only, and lives in a module that makes no other network requests. Analytics API
calls and every other outbound request use full certificate validation against
`webpki-roots`.

### Analytics providers

- OAuth tokens go through the same credential store as SSH credentials.
- Every request has a timeout; rate limits are respected with exponential backoff.
- A provider is asked only for what its declared capabilities say it supports.

---

## 5. Threat model

### Defended

| Threat | Defence |
|---|---|
| Stolen laptop, disk read offline | Secrets in the OS keystore, or Argon2id + XChaCha20-Poly1305 |
| Passive network attacker | SSH transport; TLS for agents and providers |
| Server impersonation | SSH host-key pinning; agent certificate pinning |
| Credentials leaking into logs or a bug report | Redaction at the type, the writer and the test |
| Timing attack on an agent token | Constant-time comparison |
| Compromised agent host reaching further | The token can read one host and do nothing else |
| Tampered agent download | SHA-256 checked before unpacking; GPG signature available |
| Malicious server sending huge output | Bounded reads and per-command timeouts |

### Not defended

Stated plainly, because a threat model that claims everything is worthless.

| Threat | Why not |
|---|---|
| Malware running as your user on the machine running the app | It can read the keystore as you can. No local application defends against this |
| A compromised release host serving a matching tarball and checksum | Use the signature path in [AGENT.md](AGENT.md) §2.2; the signing key never touches a build machine |
| A hostile SSH server exploiting the SSH client | Mitigated by `russh` being memory-safe Rust and by `#![forbid(unsafe_code)]`, not eliminated |
| Someone with physical access to an unlocked machine | Out of scope |
| Traffic analysis of monitoring polls | Out of scope |

### Deliberately absent

**Remote execution.** Neither mode can start, stop or restart anything. The interface is
built for it and the agent's endpoint list is deliberately short. Adding it changes what
a stolen credential is worth, so it waits for confirmation flows and an audit trail.

**Telemetry.** The application makes no network request you did not configure. There is no
usage reporting, no crash reporting and no update check. It talks to your servers, your
websites, and the analytics provider you connected. Nothing else.

---

## 6. Hardening a deployment

**The machine running the application**

- Full-disk encryption. It holds credentials for the whole fleet.
- Do not run it as an administrator. It has no need for those rights.
- If it is shared, remember that the OS keystore unlocks with the *session*: anyone at
  that unlocked session has the fleet.

**SSH-mode servers**

- Use keys, not passwords.
- A dedicated monitoring user is better than root. It needs `df`, `ps`, `cat /proc/*`,
  and — only for those panels — `docker ps` and `systemctl list-units`.
- Restrict the key in `authorized_keys` with `from=` to the app's address.

**Agent-mode servers**

- Firewall 9443 to the app's address only.
- Better: `bind = "127.0.0.1"` and reach it over a VPN or SSH tunnel. An address the agent
  does not listen on cannot be exposed by a forgotten rule.
- Keep the default systemd hardening. Check it with `systemd-analyze security vds-agent`.

**Analytics**

- Give the OAuth token read-only scope. The application never writes.

---

## 7. Supply chain

- Dependencies are pinned in `Cargo.lock`, which is committed.
- `#![forbid(unsafe_code)]` in every crate. `apps/ui` uses `deny` instead, because Slint's
  generated code carries its own `allow`; every hand-written line is still safe Rust.
- Clippy runs with `-D warnings` in CI, including `unwrap_used`, `expect_used` and `panic`
  as denials in production code.
- Release artefacts are built in CI from the tagged commit. Nothing is uploaded from a
  developer's machine.
- Every release carries `SHA256SUMS`; releases with signing configured also carry
  `SHA256SUMS.asc`, signed by a key that never touches a build machine.

Cryptography comes from `ring` (via `rustls`), `argon2` and `chacha20poly1305`. None of it
is hand-rolled.

---

## 8. Reporting a vulnerability

Please do not open a public issue.

Use **GitHub → Security → Report a vulnerability** on this repository, or e-mail the
maintainers. Include what you found, how to reproduce it, and what you think the impact
is.

You can expect an acknowledgement within a few days and an assessment within two weeks.
If it is a real issue, we will agree a disclosure timeline with you and credit you in the
release notes unless you would rather we did not.

---

## 9. Auditing this yourself

The claims above map to specific code, which is the only way to check them:

| Claim | Where |
|---|---|
| Secrets are not in the database | `crates/infra-db/src/` — no table has a secret column; only `CredentialRef` |
| Argon2id parameters | `crates/infra-secrets/src/encrypted_file.rs`, the `const` block in the tests |
| Redaction | `crates/composition/src/logging.rs`, and its tests |
| `Secret` cannot be printed or serialised | `crates/domain/src/ports/secrets.rs` |
| Host-key pinning | `crates/infra-ssh/src/known_hosts.rs` |
| Inspection-only TLS is monitoring-only | `crates/infra-web/src/tls.rs` |
| Constant-time token comparison | `agent/src/auth.rs` |
| The agent has no write endpoints | `agent/src/server.rs` — the whole router is one function |
| No fabricated data in production | Demo providers are behind the `demo-providers` feature, off by default |
