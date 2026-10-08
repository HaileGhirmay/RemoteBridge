# Firewall checklist

Open only what is listed. Everything else stays closed.

## TURN host (coturn)

| Direction | Protocol | Port | Why |
|---|---|---|---|
| in | UDP | 3478 | STUN/TURN over UDP (the normal path) |
| in | TCP | 3478 | TURN over TCP (some networks block UDP) |
| in | TCP | 5349 | TURN over TLS |
| in | TCP | 443 | TURN over TLS on the HTTPS port, for networks that only allow 443 |
| in | UDP | 49152–65535 | relay ports handed out to sessions |
| out | any | any | the relay forwards to peers on the public internet |

With `network_mode: host` Docker publishes nothing itself: the rules above are the cloud security group / host firewall.
If TLS on 443 is served by the same VM as the signaling proxy, run coturn on another IP or let the proxy do SNI routing.

## Signaling host

| Direction | Protocol | Port | Why |
|---|---|---|---|
| in | TCP | 443 | `wss://signal.example.com/ws` via your TLS proxy |
| in | TCP | 80 | only for certificate issuance / redirect |
| local | TCP | 8080 | proxy to the signaling container; **do not expose publicly** |

## Before go-live

- [ ] Clocks are synced (NTP) on the TURN host and the website. REST credentials expire by timestamp; a skewed clock rejects good credentials.
- [ ] `TURN_SHARED_SECRET` and `SIGNALING_JWT_SECRET` are random, at least 32 bytes, stored only as secrets (never in git), and the **same values** are set on the website.
- [ ] `denied-peer-ip` ranges are in place (private, loopback, link-local, multicast), so the relay cannot reach internal services.
- [ ] `total-quota`, `user-quota`, `bps-capacity`, `max-bps` match the VM size and bandwidth bill.
- [ ] TLS certificates renew automatically.
- [ ] `turnutils_uclient` test passes with a minted credential and fails with a tampered one (see `test-credentials.sh`).
- [ ] Logs hold addresses and timings only. Media is end-to-end encrypted; nothing here can see the screen.
