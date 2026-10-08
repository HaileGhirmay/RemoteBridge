# Deploying signaling and TURN

Two secrets tie the website to these servers. Generate each once, store them as secrets, and give the **same values** to the website:

```
openssl rand -base64 48    # SIGNALING_JWT_SECRET
openssl rand -base64 48    # TURN_SHARED_SECRET
```

## Option A: signaling on Fly.io, TURN on a small VM

Signaling (Fly terminates TLS). Run these from the **repository root**, because the Docker build needs the whole workspace:

```
fly launch --no-deploy --copy-config --config signaling/fly.toml --dockerfile signaling/Dockerfile
fly secrets set --config signaling/fly.toml SIGNALING_JWT_SECRET="…"
fly deploy --config signaling/fly.toml --dockerfile signaling/Dockerfile
```

Clients then use `wss://<app>.fly.dev/ws`.

TURN needs a real public IP and a UDP port range, which Fly's HTTP proxy does not give, so run coturn on a VM (any provider, 1 vCPU / 1–2 GB is plenty to start):

1. Point `turn.example.com` at the VM. Get a TLS certificate for it (certbot) into `signaling/turn/tls/` as `fullchain.pem` and `privkey.pem`.
2. Open the ports in [turn/FIREWALL.md](turn/FIREWALL.md).
3. `cd signaling/turn && TURN_SHARED_SECRET="…" docker compose up -d coturn`.
4. If the VM sits behind NAT (most clouds), set `external-ip=<public>/<private>` in `turnserver.conf`.

## Option B: both on one VM

`signaling/turn/docker-compose.yml` starts coturn and the signaling server together. Put a TLS proxy (Caddy is the least work) in front of `127.0.0.1:8080`:

```
signal.example.com {
    reverse_proxy 127.0.0.1:8080
}
```

## Website settings

Set these on the website (Prompt 06 note in `PROMPTS.md`):

| Name | Value |
|---|---|
| `SIGNALING_URL` | `wss://signal.example.com/ws` |
| `SIGNALING_JWT_SECRET` | the same secret the signaling server has |
| `TURN_URLS` | `turn:turn.example.com:3478?transport=udp,turns:turn.example.com:443?transport=tcp` |
| `TURN_SHARED_SECRET` | the same secret coturn has |
| `STUN_URLS` | optional, for example `stun:turn.example.com:3478` |

Until `SIGNALING_URL` and `SIGNALING_JWT_SECRET` are set, `media_credentials` answers `503 MEDIA_UNCONFIGURED` and the viewer shows that screen streaming is not switched on.

## Checks after deploying

1. `curl https://signal.example.com/healthz` prints `ok`.
2. `TURN_SHARED_SECRET=… signaling/turn/test-credentials.sh turn.example.com` passes all three cases (valid accepted, tampered refused, expired refused).
3. From a network that blocks UDP, a session still connects and the usage ledger shows relay seconds.
