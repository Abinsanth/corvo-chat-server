# Deploying corvo-chat-server

The server is one program that serves HTTPS on port 443 and gets its own
Let's Encrypt certificate (TLS-ALPN-01, so nothing else may use port 443).
systemd runs it as the locked-down user `corvo-chat`, with its data in
`/var/lib/corvo-chat`.

## 1. What you need

- An x86_64 Linux server (these steps use Ubuntu 24.04) with a fixed public
  IPv4 address, and SSH access as a user with `sudo`.
- A domain where you can add DNS records.
- On your own machine: [`gh`](https://cli.github.com/), logged in, to download
  builds.

The examples use `chat.example.com` and `<server-ip>`. Replace them with yours.

## 2. DNS and the firewalls

1. **DNS:** add an `A` record: host `chat`, answer `<server-ip>`. Check it
   from your machine:
   ```bash
   dig +short chat.example.com
   ```
   It should print `<server-ip>`. A different address may be your
   registrar's parking page, or an older answer still cached.
2. **Your provider's firewall** (it may be called a security group, security
   list or cloud firewall): allow inbound **TCP 443** from `0.0.0.0/0`.
3. **The server's own firewall.** On the server:
   - `sudo ufw status` says `active`: run `sudo ufw allow 443/tcp`.
   - `sudo iptables -S INPUT` ends in a `REJECT` rule (some cloud Ubuntu
     images ship this way): run
     ```bash
     sudo iptables -I INPUT 1 -p tcp --dport 443 -m conntrack --ctstate NEW -j ACCEPT
     sudo netfilter-persistent save
     ```
     `sudo grep 443 /etc/iptables/rules.v4` should then show the rule, so it
     survives a reboot.
   - Neither: nothing to do.

To check both firewalls before installing anything, run this from your
machine:
```bash
nc -vz <server-ip> 443
```
`Connection refused` is the answer you want at this point: the port is open
and nothing listens yet. A timeout means a firewall still drops it.

## 3. Installing

Once, on the server, create the user the service runs as:
```bash
sudo useradd --system --no-create-home --shell /usr/sbin/nologin corvo-chat
```

On your machine, download the build from the latest green CI run on `main`
(a static `x86_64-unknown-linux-musl` binary) and copy it to the server with
the systemd unit:
```bash
run=$(gh run list -R Abinsanth/corvo-chat-server --workflow CI --branch main --status success --limit 1 --json databaseId -q '.[0].databaseId')
gh run download "$run" -R Abinsanth/corvo-chat-server -n corvo-chat-server-x86_64-linux -D corvo-chat-build
curl -fsSLo corvo-chat-build/corvo-chat-server.service https://raw.githubusercontent.com/Abinsanth/corvo-chat-server/main/deploy/corvo-chat-server.service
scp corvo-chat-build/* <user>@<server-ip>:/tmp/
```

The unit starts `corvo-chat-server serve`, which defaults to
`--domain chat.corvo.live`. For your own domain, edit the `ExecStart` line
in `/tmp/corvo-chat-server.service` on the server before installing:
```
ExecStart=/usr/local/bin/corvo-chat-server serve --domain chat.example.com
```

Then on the server:
```bash
sudo install -m 755 /tmp/corvo-chat-server /usr/local/bin/
sudo install -m 644 /tmp/corvo-chat-server.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now corvo-chat-server
```

## 4. Checking it

On the server, about a minute after starting:
```bash
journalctl -u corvo-chat-server -n 30 --no-pager
```
Expected: `listening on :443 for chat.example.com`, then
`certificate: DeployedNewCert`.

From your machine:
```bash
curl -sS https://chat.example.com/
openssl s_client -connect chat.example.com:443 -servername chat.example.com </dev/null 2>/dev/null | openssl x509 -noout -issuer
```
Expected: `corvo-chat-server <version>`, and an issuer with
`O=Let's Encrypt`.

On the server, check that it runs as intended:
```bash
ps -o user= -C corvo-chat-server; sudo stat -c "%U %a" /var/lib/corvo-chat
```
Expected: `corvo-chat`, then `corvo-chat 700`.

A restart must reuse the saved certificate:
```bash
sudo systemctl restart corvo-chat-server; sleep 5; journalctl -u corvo-chat-server -n 5 --no-pager
```
Expected: `certificate: DeployedCachedCert`, with no new `DeployedNewCert`.

Finally, `sudo reboot`. A minute later, `curl -sS https://chat.example.com/`
from your machine should answer again, with nobody logging in.

## 5. Updating

Download and copy the new build as in step 3 (the unit file only when it
changed), then on the server:
```bash
sudo install -m 755 /tmp/corvo-chat-server /usr/local/bin/
sudo systemctl restart corvo-chat-server
curl -sS https://chat.example.com/
```

## 6. Troubleshooting

- **`warn` lines with `certificate:`** usually mean Let's Encrypt can't reach
  the server: DNS doesn't point here yet, or port 443 is shut in one of the
  firewalls (check with `nc -vz` from step 2). The server keeps retrying on
  its own; fix the cause and wait.
- **Logs:** `journalctl -u corvo-chat-server -f` follows them live.
  `systemctl status corvo-chat-server` shows whether it runs.
- **Let's Encrypt allows 5 identical certificates a week.** The certificate
  and account live in `/var/lib/corvo-chat/acme`. Never delete that folder:
  each fresh start without it orders a new certificate.
- **Nothing else may listen on port 443** (nginx, Apache, Caddy). Check with
  `sudo ss -ltnp 'sport = :443'`.
