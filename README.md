# GoodWe Status

A macOS menu bar app that shows your home battery level and whether the grid is on, from your GoodWe SEMS Portal account. Unofficial and not affiliated with GoodWe: it uses the same undocumented web endpoints as the SEMS apps, which can change at any time.

<img src="docs/screenshots/menubar.jpg" alt="Menu bar item" height="30"> <img src="docs/screenshots/notification.jpg" alt="Grid-off notification" height="60">

<img src="docs/screenshots/panel-dark.jpg" alt="Panel, dark" width="300"> <img src="docs/screenshots/panel-light.jpg" alt="Panel, light" width="300">

## Install

1. Download `GoodWe Status.zip` from the Releases page, unzip it and move **GoodWe Status.app** to Applications.
2. The app is not notarized, so the first time **right-click it, choose Open**, then **Open** again. (macOS 15+: try once, then **System Settings › Privacy & Security › Open Anyway**. Or: `xattr -dr com.apple.quarantine "/Applications/GoodWe Status.app"`.)
3. Sign in with your SEMS Portal email and password. They are kept only in your Keychain. To change account, click the menu bar item, then **Sign In…**.

- **Keychain:** after each update macOS asks once to use the saved login. Click **Always Allow**.
- **Focus can mute alerts:** to hear a power cut during Focus, add GoodWe Status under **System Settings › Focus › (your Focus) › Allowed Apps**.
- **Start at login** is on after first sign-in. To turn it off: **System Settings › General › Login Items & Extensions**, switch off **GoodWe Status**.

## Build from source

Needs macOS 12+, [Rust](https://rustup.rs) (stable), and the signing identity below (make it first). No Node.js.

```sh
cargo install tauri-cli --version 2.12.3 --locked
git clone https://github.com/asifshirazi/goodwe-status.git
cd goodwe-status
cargo tauri build --bundles app
```

**Signing identity (once per Mac).** Builds are signed with a self-signed identity named `GoodWe Status Local Signing` (no Apple account needed), so Keychain "Always Allow" sticks. Its key stays in your login Keychain.

```sh
cd "$(mktemp -d)"
cat > cs.cnf <<'EOF'
[req]
distinguished_name = dn
prompt = no
[dn]
CN = GoodWe Status Local Signing
[ext]
basicConstraints = critical,CA:false
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
EOF
/usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -keyout key.pem -out cert.pem -days 3650 -config cs.cnf -extensions ext
/usr/bin/openssl pkcs12 -export -inkey key.pem -in cert.pem -out id.p12 -passout pass:temp
security import id.p12 -k ~/Library/Keychains/login.keychain-db -P temp -T /usr/bin/codesign
rm -f key.pem id.p12
security find-identity -p codesigning
```

"Not trusted" in the last line is expected. The app is at `src-tauri/target/release/bundle/macos/GoodWe Status.app`.

**Release builds** must remap your home folder, or your user name ships in the binary:

```sh
RUSTFLAGS="--remap-path-prefix=$HOME=~" cargo tauri build --bundles app
strings -a "src-tauri/target/release/bundle/macos/GoodWe Status.app/Contents/MacOS/goodwe-status" | grep -c "$HOME"   # must print 0
ditto -c -k --keepParent "src-tauri/target/release/bundle/macos/GoodWe Status.app" "GoodWe Status.zip"
```

## When GoodWe changes their API

1. When a reply can't be read, the menu bar item greys out and the reason plus the reply (personal fields removed) go to the log. Click the menu bar item, then **Open Log Folder**.
2. Every URL, header, field path and the grid-off rule lives in `src-tauri/src/sems.rs`. Fix the mapping there.
3. Save the new reply in `src-tauri/tests/fixtures/` with all personal data removed (email, tokens, station id, serial numbers, address), and add its expected reading to the tests in `sems.rs`.
4. Run `cargo test --manifest-path src-tauri/Cargo.toml` until it passes, then open an issue or pull request with the redacted sample.

## License

MIT, see [LICENSE](LICENSE).
