# Testing on iPhone and Android from the Mac

How to load, install and debug Iron Oxide on the iOS Simulator, the Android Emulator, a real
Android phone and a real iPhone, all served from the Mac.

The rule behind every choice here: passkeys, the service worker, Wake Lock and installation all
need a **secure context**. That means `http://localhost` or a trusted `https://` origin with a
**domain name** (never an IP: a passkey RP ID cannot be an IP). The app also rejects every
server-function `POST` whose `Origin` is not exactly `APP_BASE_URL`, so the phone must open the app
on the URL configured in `.env`.

| Device | How it reaches the Mac | URL on the device | Preset |
|---|---|---|---|
| iOS Simulator | shares the Mac's network | `http://localhost:8080` | `.env.localhost.example` |
| Android Emulator | `adb reverse tcp:8080 tcp:8080` | `http://localhost:8080` | `.env.localhost.example` |
| Real Android (USB or wireless adb) | `adb reverse tcp:8080 tcp:8080` | `http://localhost:8080` | `.env.localhost.example` |
| **Real iPhone (recommended)** | Tailscale, `tailscale serve` | `https://<machine>.<tailnet>.ts.net` | `.env.tailscale.example` |
| Real iPhone (no account) | Wi-Fi, caddy + mkcert certificate | `https://<mac>.local:8443` | `.env.lan.example` |

## What each setup can verify

| | iOS Simulator | Android Emulator | Real Android | Real iPhone, Tailscale | Real iPhone, mkcert `.local` | Any phone, `http://<LAN IP>` |
|---|---|---|---|---|---|---|
| Layout | yes | yes | yes | yes | yes | yes (read-only, see below) |
| SW / offline (release build) | yes | yes | yes | yes | yes | no (not a secure context) |
| Install to home screen | yes | yes | yes | yes | yes | bookmark only |
| Wake Lock | API only¹ | API only¹ | yes | yes⁶ | yes⁶ | no |
| Passkeys | unverified² | unverified³ | unverified³ | yes⁷ | unverified⁴ | no |
| Google sign-in | yes (local client) | yes (local client) | yes (local client) | yes (own client) | **no**⁵ | no |
| Google popup in the *installed* iOS app | not representative | n/a | n/a | **yes** | no | no |

1. The call succeeds, but a simulator's screen never sleeps, so the effect cannot be observed.
2. Enrol Face ID first (Simulator menu **Features → Face ID → Enrolled**, then **Matching Face**
   when prompted). Apple documents nothing about passkeys in the Simulator and reports are split;
   if it fails, use a real iPhone with Tailscale.
3. Needs a system image *with Google Play* (emulator), a Google account signed in and a screen lock
   set (emulator and phone). Whether Google Password Manager accepts the `localhost` RP ID is not
   verified; if it does not, the Android part of passkeys can only be tested on staging.
4. The server accepts `<mac>.local` as RP ID; whether Safari does is not verified. If it fails, use
   Tailscale.
5. Google refuses redirect URIs on a top-level domain outside the Public Suffix List (`.local`) and
   on raw IPs; only `localhost` may use http.
6. In a Safari tab from iOS 16.4; in the *installed* web app only from iOS 18.4.
7. The iPhone needs a passcode and iCloud Keychain (or another passkey provider) turned on.

Passkeys are bound to their RP ID: a passkey made on `localhost` or on the Tailscale host never
works on `iron-oxyde.com` (see [docs/auth.md](../auth.md#running-it-locally)).

## Prerequisites

MiniSim (`brew install --cask minisim`) is only a launcher: it drives `xcrun simctl` for iOS and the
Android SDK's `emulator` and `adb`. It needs:

- **Xcode** (the full app, not only the Command Line Tools) with an iOS simulator runtime:

  ```sh
  sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
  sudo xcodebuild -license accept
  xcodebuild -runFirstLaunch
  xcodebuild -downloadPlatform iOS        # the iOS simulator runtime (several GB)
  xcrun simctl list devices available     # should list iPhones
  ```

- **Android Studio** (`brew install --cask android-studio`). In **Device Manager**, create a device
  whose system image says **Google Play** (needed for passkeys and a Google account). MiniSim looks
  for the SDK in `~/Library/Android/sdk` (Android Studio's default) unless you give it another
  path during its onboarding. Put the SDK's `adb` and `emulator` on the `PATH` (in `~/.zshrc`):

  ```sh
  export ANDROID_HOME="$HOME/Library/Android/sdk"
  export PATH="$ANDROID_HOME/platform-tools:$ANDROID_HOME/emulator:$PATH"
  ```

  Use this `adb` rather than a second one from Homebrew: two different `adb` versions keep
  killing each other's server.

- For a real iPhone: **Tailscale** (recommended) or **mkcert + caddy**, see below.

## Running the app for a phone

There are two ways to run the app. The service worker is only registered in release builds, so
anything about offline, caching or installing needs the second one.

```sh
# Development: hot reload, no service worker. Also starts Postgres and applies the migrations.
make dev              # dx serve on http://127.0.0.1:8080

# Release: service worker on. Builds the bundle, then runs its server from the repository root,
# so it reads ./.env.
make run-release      # binds IP:PORT from .env, default 127.0.0.1:8080
```

**Keep the app on `127.0.0.1:8080`** in every setup in this document. The phone reaches it through
`adb reverse`, `tailscale serve` or caddy, all of which forward to the Mac's loopback. Nothing
needs `IP=0.0.0.0`. What each forwarder exposes:

- `adb reverse`: only the attached device.
- `tailscale serve`: every device in your tailnet that its ACLs allow, until `tailscale serve reset`.
- caddy (mkcert): **anyone on the same Wi-Fi** can connect on port 8443. TLS encrypts the traffic
  but does not limit who connects. On a shared or public network, stop caddy (Ctrl-C) as soon as
  you are done.

Switching setups is a copy and a restart:

```sh
make env PRESET=tailscale FORCE=1   # or PRESET=localhost / lan; the old .env is kept as .env.bak
$EDITOR .env                        # fill in the placeholders, then restart dx or the server
```

`make env` generates a fresh `SESSION_KEY` in the new `.env`; the other placeholders are yours to
fill in. Without `FORCE=1` it never replaces an existing `.env`.

`.env` and `.env.*` are git-ignored. The `*.example` templates hold placeholders only.

### macOS firewall

With the firewall on (System Settings → Network → Firewall), the first program that listens on a
non-loopback address gets an "accept incoming network connections?" prompt: **caddy** (mkcert
setup), or dx and the app server if you use `--addr 0.0.0.0` (below). Answer **Allow**; if you
missed it, allow the program under Firewall → Options. `adb reverse` and `tailscale serve` need no
prompt.

### A quick look over plain http (layout only)

`dx serve --web -p iron-oxide-app --addr 0.0.0.0` makes the dev server listen on the Wi-Fi, and dx
prints the Mac's LAN IP. A phone can then open `http://<LAN IP>:8080` for a layout check, and
nothing else: it is not a secure context (no service worker, passkeys or Wake Lock), and every
server-function `POST` fails the Origin check because `APP_BASE_URL` is not that IP. Once sign-in
lands, no valid `.env` can use an IP origin. Stop dx when done; it is exposed to the whole network.

## iOS Simulator

The simulator uses the Mac's network, so `localhost` is the Mac.

```sh
make env PRESET=localhost FORCE=1   # then fill in .env
make dev
```

Start an iPhone from MiniSim (or `xcrun simctl boot "<device name>" && open -a Simulator`), then:

```sh
make ios-open                       # xcrun simctl openurl booted http://localhost:8080
```

- Install: Safari → Share → **Add to Home Screen**.
- Debug: see [Safari Web Inspector](#safari-web-inspector-ios).
- To try the mkcert preset in the simulator, trust the CA there first:
  `xcrun simctl keychain booted add-root-cert "$(mkcert -CAROOT)/rootCA.pem"`.

## Android Emulator

In the emulator, `localhost` is the emulator itself. `10.0.2.2` reaches the Mac but is not a
secure context. Forward the port instead, so Chrome sees `http://localhost:8080`:

```sh
adb devices                          # the emulator shows as emulator-5554
make adb-reverse                     # adb reverse tcp:8080 tcp:8080, then lists the forwards;
                                     # SERIAL=<serial> when several devices are attached
make android-open                    # opens http://localhost:8080 in the device's browser
```

The forward lasts until the emulator or adb restarts: run `make adb-reverse` again after a reboot.
Install from Chrome's menu → **Add to home screen** / **Install app**. Debug with
[`chrome://inspect`](#chrome-devtools-android).

## Real Android phone

The same `adb reverse`, over USB:

1. On the phone: Settings → About phone → tap **Build number** seven times, then Settings → System
   → Developer options → **USB debugging** on.
2. Plug it in, accept the "Allow USB debugging?" prompt, and check `adb devices` shows `device`
   (not `unauthorized`).
3. `make adb-reverse`, then open `http://localhost:8080` in Chrome on the phone (or
   `make android-open`).

Wireless debugging (Android 11+, same Wi-Fi) works too: Developer options → **Wireless
debugging** → **Pair device with pairing code**, then `adb pair <ip>:<pairing port>` and
`adb connect <ip>:<port>`. `adb reverse` works the same over it.

Alternative without the command: in `chrome://inspect#devices` on the Mac, **Port forwarding**
`8080` → `localhost:8080` (active while that DevTools page is open).

Why not the Wi-Fi? Android does not reliably resolve `.local` names in Chrome, a LAN IP cannot be a
passkey RP ID, and Chrome only trusts a user-installed CA (such as mkcert's) in ways that vary by
Android version and are not verified here. `adb reverse` gives a secure `localhost` with the local
Google client and needs none of that.

## Real iPhone

A real iPhone cannot use `localhost` (there is no `adb reverse` for iOS), so it needs https on a
domain name.

**Recommendation: Tailscale.** It is the only local setup where every row of the matrix works,
including Google sign-in and the Google popup in the installed app: `*.ts.net` is on the Public
Suffix List and gets a publicly trusted Let's Encrypt certificate, so the phone needs no profile,
Google accepts the redirect URI, and the host is stable and works off the home Wi-Fi.
The costs:

- a Tailscale account, and the Tailscale app on the Mac and the iPhone (VPN on while testing);
- the machine name becomes public: every certificate is written to public Certificate
  Transparency logs, so rename the Mac in Tailscale first if its name says anything private;
- one more Google OAuth client, and passkeys made there only work on that host.

**mkcert** needs no account, but Google sign-in cannot work on `.local`, passkeys on `.local` are
unverified, and the phone must trust a root CA that can impersonate any site (see the warning
below). Use it for layout, offline and install checks when Tailscale is not an option.

### Real iPhone: Tailscale

Once per Mac and tailnet:

1. `brew install --cask tailscale-app`, sign in. Install the CLI: Tailscale menu → **Settings** →
   CLI integration → **Show me how** → **Install Now** (App Store build: use
   `/Applications/Tailscale.app/Contents/MacOS/Tailscale` instead of `tailscale`).
2. Admin console → **DNS**: MagicDNS on, then **Enable HTTPS** under HTTPS Certificates.
3. Install Tailscale on the iPhone and sign in to the same tailnet.
4. The Mac's host name:

   ```sh
   tailscale status --json | jq -r .Self.DNSName     # my-mac.tail1234.ts.net. (drop the final dot)
   ```

   It is also shown in the admin console's **Machines** page.

5. Google Cloud console, as in [docs/auth.md](../auth.md#creating-the-google-oauth-client):
   first add `<tailnet>.ts.net` under **Google Auth Platform → Branding → Authorized domains**
   (every domain used by a client must be listed there before its redirect URIs). No Search
   Console verification is needed while the app is in "Testing". Then create a new **Web
   application** client, e.g. "Iron Oxide (tailscale)", with the authorized redirect URI
   `https://<machine>.<tailnet>.ts.net/auth/google/callback`.
6. `make env PRESET=tailscale FORCE=1` and fill in `.env` with that host and client.

Each session:

```sh
make dev                # or make run-release
make tailscale-serve    # tailscale serve --bg --https=443 localhost:8080, then its status;
                        # the first run fetches the certificate
# ... test on https://<machine>.<tailnet>.ts.net ...
make tailscale-reset    # tailscale serve reset: stop publishing
```

`--bg` makes the serve config **persistent**: it survives reboots and `tailscale down`/`up`, so
the app is published again whenever something listens on 8080, until `tailscale serve reset`.
It is only reachable from your tailnet, not the internet (that would be `tailscale funnel`), but
from *every* device and user in it that the tailnet's ACLs allow, including shared nodes. Hot
reload works through it (websockets are proxied).

### Real iPhone: mkcert

Once:

```sh
brew install mkcert caddy
H="$(scutil --get LocalHostName).local"; echo "$H"
mkdir -p ~/.iron-oxide-dev && cd ~/.iron-oxide-dev
mkcert -cert-file cert.pem -key-file key.pem "$H" localhost
open "$(mkcert -CAROOT)"             # AirDrop rootCA.pem (never rootCA-key.pem) to the iPhone
```

The first `mkcert` run creates the CA without adding it to the Mac's trust stores. Skip
`mkcert -install`: nothing here needs it, and it would make the Mac trust a root whose private key
sits on disk.

On the iPhone, after AirDropping `rootCA.pem`:

1. Settings → **Profile Downloaded** (or General → VPN & Device Management) → install it.
2. Settings → General → About → **Certificate Trust Settings** → turn on full trust for the
   mkcert root.

> The mkcert root CA's private key stays on the Mac, but anyone who gets it can impersonate any
> https site to that phone. Never share `rootCA-key.pem`, and delete the profile from the iPhone
> when you stop using this setup.

Write the proxy config next to the certificates (outside the repository):

```sh
cd ~/.iron-oxide-dev
cat > Caddyfile <<EOF
{
	admin off
	auto_https disable_redirects
}
https://$(scutil --get LocalHostName).local:8443 {
	tls cert.pem key.pem
	reverse_proxy localhost:8080
}
EOF
```

Then `make env PRESET=lan FORCE=1` in the repository, replace `<mac>` in `.env` with the
`LocalHostName`, and each session:

```sh
make dev                                                 # or make run-release
cd ~/.iron-oxide-dev && caddy run --config Caddyfile     # Ctrl-C to stop
```

Open `https://<mac>.local:8443` on the iPhone (same Wi-Fi). caddy listens on all interfaces on
port 8443 only (`disable_redirects` keeps it off port 80) and proxies to the app on loopback,
websockets included. Anyone on that Wi-Fi can reach it: stop caddy with Ctrl-C when done.

`dx serve` can also serve TLS itself, through `[web.https]` in a `Dioxus.toml` (`enabled`,
`key_path`, `cert_path`); its `mkcert = true` option only covers `localhost`. That file would
change `dx serve` for everyone, so it is not committed; caddy keeps the setup outside the
repository.

## Debugging

### Safari Web Inspector (iOS)

1. On the Mac: Safari → Settings → Advanced → **Show features for web developers**.
2. On the iPhone: Settings → Apps → Safari → Advanced → **Web Inspector** on. Connect it by cable
   once and trust the Mac.
3. Safari's **Develop** menu lists the iPhone and every booted simulator, with their open pages,
   including home-screen web apps. The service worker has its own entry there.

### Chrome DevTools (Android)

1. Keep the phone or emulator connected through adb.
2. Open `chrome://inspect#devices` in Chrome on the Mac; the device's tabs and installed web apps
   are listed. **Inspect** opens DevTools; Application → Service workers shows the worker.

## Multi-viewport layout checks (optional)

For the UI work (M5), [Responsively App](https://responsively.app) shows one page in many device
sizes at once: `brew install --cask responsively`, then open `http://localhost:8080`. It is
Chromium, so it says nothing about Safari behaviour or installation.

## Staging (option, not set up)

A second Fly app, e.g. `iron-oxide-staging`, serving `https://staging.iron-oxyde.com` from `main`,
would be the only way to test on a real domain before production: the installed iOS app and its
Google popup, passkeys, the real certificate and the deployed build. It would need its own
Postgres database (a Neon branch), its own Google client with the redirect URI
`https://staging.iron-oxyde.com/auth/google/callback`, and its own `SESSION_KEY`.
`WEBAUTHN_RP_ID=staging.iron-oxyde.com` keeps its passkeys apart from production's. It costs a
little and keeps another app deployed, so it is left to the maintainer to decide.

## What has been verified

- Checked on the Mac: `dx serve` 0.7.10 options (`--addr`, `--port`, no TLS flag; TLS only through
  `Dioxus.toml`), the caddy config above serving a mkcert certificate for `<mac>.local` on the
  LAN IP without binding port 80, `.local` resolution on the Mac, and that the server's config
  validation accepts all three presets.
- From the tools' documentation and source: MiniSim's SDK lookup, Google's redirect URI rules,
  mkcert's iOS steps, `tailscale serve`, Safari and Chrome remote debugging.
- Not tried yet (no simulator runtime, Android SDK or phones were available): every step on a
  simulator, emulator or device, passkeys in the iOS Simulator, on Android with the `localhost`
  RP ID and on `.local`, and the Google console accepting the `*.ts.net` authorized domain and
  redirect URI.
