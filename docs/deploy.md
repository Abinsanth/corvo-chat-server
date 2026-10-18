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

Then on the server, install them and make the **server key** once:
```bash
sudo install -m 755 /tmp/corvo-chat-server /usr/local/bin/
sudo install -m 644 /tmp/corvo-chat-server.service /etc/systemd/system/
sudo install -d -o corvo-chat -g corvo-chat -m 700 /var/lib/corvo-chat
sudo -u corvo-chat /usr/local/bin/corvo-chat-server keygen
sudo systemctl daemon-reload
sudo systemctl enable --now corvo-chat-server
```
`keygen` prints `opaque.key written. Back it up offline: losing it breaks
every account.` It never overwrites an existing key, and `serve` won't start
without one.

**Back up `/var/lib/corvo-chat/opaque.key` offline right away.** Every
account's sign-in depends on it: without it, nobody can sign in again, and
there's no way to make it again. For example, print it as text and keep it
in a password manager:
```bash
sudo cat /var/lib/corvo-chat/opaque.key | base64
```
Never paste it anywhere public. To put it back on a new server, before the
first start:
```bash
echo '<the base64 text>' | base64 -d | sudo -u corvo-chat tee /var/lib/corvo-chat/opaque.key >/dev/null
sudo chmod 600 /var/lib/corvo-chat/opaque.key
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

## 5. Accounts and invites

New accounts wait as **pending** for a day until someone active invites
them. The admin commands run on the server as the service's user, while the
service runs.

- **The first account** has nobody to invite it. Sign up from a corvo-chat
  client, which shows the new account's number, then activate it:
  ```bash
  sudo -u corvo-chat /usr/local/bin/corvo-chat-server activate <number>
  ```
  It prints `activated <username> (<number>)`.
- **Inviting as the admin:** active users invite from their client, and the
  admin can too:
  ```bash
  sudo -u corvo-chat /usr/local/bin/corvo-chat-server invite <number>
  ```
  It prints the 6-digit code, then who it's for and until when it works. Pass
  the code on to that person.
- **Device limit:** each account may have 3 devices signed in. To change it,
  add `--max-devices` to the `ExecStart` line with a drop-in, which keeps the
  unit file as it ships:
  ```bash
  sudo systemctl edit corvo-chat-server
  ```
  In the editor, add:
  ```
  [Service]
  ExecStart=
  ExecStart=/usr/local/bin/corvo-chat-server serve --domain chat.example.com --max-devices 5
  ```
  Then `sudo systemctl restart corvo-chat-server`.

### Reports and bans

People report others from their client. On the server:
```bash
sudo -u corvo-chat /usr/local/bin/corvo-chat-server stats
sudo -u corvo-chat /usr/local/bin/corvo-chat-server tree <number>
sudo -u corvo-chat /usr/local/bin/corvo-chat-server ban <number> [<number>...]
sudo -u corvo-chat /usr/local/bin/corvo-chat-server unban <number>
```
- `stats` lists reported accounts, most reports first, with the time of
  the last one.
- `tree` shows who invited an account (up to an account you activated) and
  everyone it invited, two spaces deeper per level; the account itself is
  marked `*`.
- `ban` signs out every device of each account (connected ones within a
  minute) and refuses them from then on: they're told `This account has been
  banned.` once they enter their right password. An unknown number is
  reported and makes the exit code 1; the others are still banned.
- `unban` lets the account sign in again.

Clients talk to the server as [docs/protocol.md](protocol.md) describes.

## 6. Updating

Download and copy the new build as in step 3 (the unit file only when it
changed), then on the server:
```bash
sudo install -m 755 /tmp/corvo-chat-server /usr/local/bin/
sudo systemctl restart corvo-chat-server
curl -sS https://chat.example.com/
```

## 7. Hardening

Do these once, after the server works. Keep a second SSH session open while
you change anything about SSH or the firewall, so a mistake can't lock you
out.

1. **Firewall: only 22 and 443 in.**
   - In your provider's list, remove every inbound rule except TCP 22 and
     TCP 443.
   - On the server, `sudo iptables -S INPUT` should accept only `22`, `443`,
     established traffic and loopback, then end in a `REJECT` or `DROP`.
     Remove anything else with `sudo iptables -D INPUT <rule>` and save with
     `sudo netfilter-persistent save`.
2. **SSH keys only.**
   ```bash
   sudo sshd -T | grep -E '^(passwordauthentication|permitrootlogin|kbdinteractiveauthentication)'
   ```
   Expected: `passwordauthentication no`, `kbdinteractiveauthentication no`
   and `permitrootlogin no` (or `prohibit-password`). If not, make
   `/etc/ssh/sshd_config.d/10-hardening.conf` with
   ```
   PasswordAuthentication no
   KbdInteractiveAuthentication no
   PermitRootLogin no
   ```
   then `sudo sshd -t && sudo systemctl reload ssh`, and check you can still
   log in from a new terminal before closing the old one.
3. **Unattended security updates.**
   ```bash
   sudo apt install unattended-upgrades
   sudo dpkg-reconfigure -plow unattended-upgrades
   systemctl status unattended-upgrades --no-pager
   ```
   Expected: `active (running)`.
4. **The sandboxed service.** The unit in `deploy/` locks the server down
   (no devices, no kernel knobs, a syscall filter, only network and Unix
   sockets). After installing a new copy of it:
   ```bash
   sudo systemctl daemon-reload
   sudo systemctl restart corvo-chat-server
   systemd-analyze security corvo-chat-server --no-pager | tail -1
   ```
   The last line gives an exposure score; lower is better, and with this
   unit it should say `OK` or `MEDIUM`. If the service won't start after an
   update and the journal mentions `SIGSYS`, a syscall filter line is too
   strict: tell the maintainers which one.
5. **Daily backups.** Copy `deploy/corvo-chat-backup.service` and
   `deploy/corvo-chat-backup.timer` to the server like the main unit, then:
   ```bash
   sudo install -m 644 /tmp/corvo-chat-backup.service /tmp/corvo-chat-backup.timer /etc/systemd/system/
   sudo systemctl daemon-reload
   sudo systemctl enable --now corvo-chat-backup.timer
   systemctl list-timers corvo-chat-backup.timer --no-pager
   sudo systemctl start corvo-chat-backup
   sudo ls -l /var/lib/corvo-chat/backups/
   ```
   Each day makes `server-YYYY-MM-DD.db` (readable only by `corvo-chat`), and
   the newest 7 are kept. Backups don't include `opaque.key`; keep that one
   offline as in section 3. To restore one:
   ```bash
   sudo systemctl stop corvo-chat-server
   sudo -u corvo-chat cp /var/lib/corvo-chat/backups/server-YYYY-MM-DD.db /var/lib/corvo-chat/server.db
   sudo rm -f /var/lib/corvo-chat/server.db-wal /var/lib/corvo-chat/server.db-shm
   sudo systemctl start corvo-chat-server
   ```
   Everything after that backup (new accounts, messages) is lost.

## 8. Troubleshooting

- **`no opaque.key in /var/lib/corvo-chat: run "corvo-chat-server keygen"
  first`** in the journal: the server has no key yet. On a server that has
  never had accounts, run `keygen` as in step 3. If it had accounts, put the
  backed-up key back instead: a new key breaks every account.
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
