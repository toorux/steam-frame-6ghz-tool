<h1 align="center">Steam Frame 6 GHz Tool</h1>

<p align="center">View the country code and 6 GHz status of a Steam Frame USB adapter, and set its runtime region to US.</p>

<p align="center"><a href="README.md">简体中文</a> · <a href="README.en.md">English</a></p>

<p align="center">
  <a href="https://github.com/toorux/steam-frame-6ghz-tool/actions/workflows/release.yml"><img src="https://github.com/toorux/steam-frame-6ghz-tool/actions/workflows/release.yml/badge.svg" alt="Build status"></a>
  <img src="https://img.shields.io/badge/Windows-10%20%2F%2011%20x64-0078D4" alt="Windows x64">
  <a href="https://github.com/toorux/steam-frame-6ghz-tool/releases"><img src="https://img.shields.io/github/v/release/toorux/steam-frame-6ghz-tool?include_prereleases=true" alt="Latest release"></a>
</p>

<p align="center">The tool does not modify driver files. It supports Windows 10/11 x64 only.</p>

## Contents

- [Getting started](#getting-started)
- [Auto-apply](#auto-apply)
- [Headset setup](#headset-setup)
  - [Set with the tool](#set-with-the-tool)
    - [Finding the Frame IP](#finding-the-frame-ip)
    - [Enabling SSH on Frame](#enabling-ssh-on-frame)
  - [Manual setup](#manual-setup)
- [Common problems](#common-problems)
- [Notes](#notes)
- [Build](#build)
- [Release](#release)

## Getting started

Download the program from [Releases](https://github.com/toorux/steam-frame-6ghz-tool/releases). If it comes in an archive, extract it to a fixed folder first.

1. Plug in the adapter and run `steam-frame-6ghz-tool.exe` as administrator.
2. If one matching adapter is found, it is selected and queried automatically. If several are found, select one to query it.
3. To change the adapter region, click **Set US** and confirm. The tool sends one request and verifies the result.

![Main window: adapter, Set US, and auto-apply](docs/images/main-usage-annotated.png)

The screenshot shows an adapter already set to US with auto-apply enabled. The yellow notice means the installed service copy needs updating; it may not appear on first use. Screenshots show the Chinese UI; use the globe button in the top-right corner to switch to English.

> [!TIP]
> If the Windows adapter shows `US` but Frame still cannot connect, see [Headset setup](#headset-setup).

## Auto-apply

The adapter's runtime setting may be lost after a reboot or unplugging it. Auto-apply restores it when needed.

Run the tool as administrator, click **Enable auto-apply**, and confirm. It checks immediately and later on startup or adapter reconnect. The service exits after processing; it does not stay running in the background. The screenshot above shows it enabled at ③.

- Logs are stored in the `logs` folder beside the program. Re-enable the service after moving the program.
- At most 20 log files are kept. They do not contain Wi-Fi passwords.
- An uncertain or interrupted operation pauses auto-apply. A definite failure is logged and is not immediately retried.
- Removing the service keeps the logs and does not undo the adapter's current US setting.
- After updating the program, **Update service** appears if the installed service copy differs. Confirming reinstalls it and keeps the logs. It will not reinstall while running or paused.
- Unverified driver versions can still be tried, but may not work.

## Headset setup

If the Windows adapter shows `US` and `6 GHz Available` but Frame still cannot connect, you can also check the headset's wireless regulatory region.

You can set it with the tool or manually in a headset terminal. If you are not comfortable with command-line steps, use the tool.

### Set with the tool

First [enable SSH on Frame](#enabling-ssh-on-frame) and set a password. Keep the PC and headset on the same LAN. Then click **Headset settings** in the main window (① below).

![Main window: headset settings button](docs/images/main-headset-entry-annotated.png)

1. Select a discovered `frame` device. If none appears, enter its IP address manually. See [Finding the Frame IP](#finding-the-frame-ip).
2. The default username is `steamos`. Enter the SSH password. If the sudo password differs, expand that option and enter it separately. Then click **Connect and preview commands**. The screenshot's IP is obscured; enter your own headset's address.

![Headset settings: device and connection details](docs/images/headset-settings-annotated.png)

3. Verify the target, SSH host key fingerprint, and commands. On the first connection, confirm that you trust the fingerprint. The tool sets runtime US, enables the persistent setting if needed, and verifies the result. It does not rewrite an already enabled US setting or reboot the headset. Afterward, reboot the headset yourself and run `iw reg get` to confirm both the global region and `phy#0` show `US`.

The program does not collect, save, or upload your password. It uses the password in local memory for this SSH/sudo session only and does not write it to logs.

#### Finding the Frame IP

On the headset, open **Settings → Internet** and select a connected network that the PC can reach. The screenshot uses Wi-Fi; a reachable wired connection can also work.

![Open the connected Wi-Fi network](docs/images/frame-ip-network-annotated.png)

In the network details, find **IP address** under **IPv4 address** and enter it in the tool. Do not use the MAC address or subnet mask. The network name, MAC address, and original IP are obscured in the screenshot.

![Find the IPv4 address](docs/images/frame-ip-details-annotated.png)

#### Enabling SSH on Frame

1. On Frame, open **Settings → System** and enable **Developer Mode** (①② below).

![Enable developer mode](docs/images/frame-enable-developer-mode-annotated.png)

2. Open **Developer** in the settings sidebar. Find **User password**, then set or change the password (③④ below). The screenshot says “Change user password” because that headset already has one.

![Set the user password](docs/images/frame-set-user-password-annotated.png)

3. With the PC and headset on the same LAN, try connecting from the PC:

```sh
ssh steamos@frame
```

If `frame` does not resolve, [find its IP](#finding-the-frame-ip) and run `ssh steamos@<headset-IP>`. Use the password set above.

### Manual setup

> [!IMPORTANT]
> If you are not familiar with the command line, use [the tool](#set-with-the-tool). Skip this section if the tool already completed the setup.

Connect to Frame over SSH or open a terminal on the headset. Check the current region and configuration:

```sh
iw reg get
grep -nE '^(#)?WIRELESS_REGDOM=' /etc/conf.d/wireless-regdom
```

If the region is not `US`, set it for the current session:

```sh
sudo iw reg set US
```

If the configuration contains exactly one `#WIRELESS_REGDOM="US"` line, uncomment it so US remains enabled after reboot:

```sh
sudo sed -i 's/^#WIRELESS_REGDOM="US"$/WIRELESS_REGDOM="US"/' /etc/conf.d/wireless-regdom
grep '^WIRELESS_REGDOM=' /etc/conf.d/wireless-regdom
```

If `WIRELESS_REGDOM="US"` is already enabled, do not edit it again. Confirm the `grep` command prints exactly one active line:

```text
WIRELESS_REGDOM="US"
```

If the file is in another state, do not run `sed`. The tool also stops when its preflight check finds unexpected configuration. Once configured, reboot the headset yourself:

```sh
sudo reboot
```

After reboot, run `iw reg get` again. Confirm both the global region and `phy#0` show `US`, then try pairing again.

If the original line was commented out and you enabled it with the command above, you can restore it with:

```sh
sudo sed -i 's/^WIRELESS_REGDOM="US"$/#WIRELESS_REGDOM="US"/' /etc/conf.d/wireless-regdom
sudo reboot
```

## Common problems

- **Adapter not found:** Check the connection and driver, then click **Refresh**.
- **Access denied:** Restart the tool as administrator.
- **Headset not found:** Confirm that the PC and headset are on the same LAN and SSH is enabled, or enter the headset IP manually.

## Notes

- The tool has only been tested in a limited environment and may not work with every device, system, or driver version.
- Windows 10/11 x64 is supported. The verified original driver version is `5.32.908.2026`; other versions show a warning but are not blocked.
- Adapter changes affect runtime state only. Without auto-apply, you may need to set US again after rebooting or unplugging the adapter.
- Close other adapter diagnostic tools before use. If an operation fails or its result is uncertain, check the log and refresh the status later.
- Switching languages affects the interface only; driver and service logs keep their original text.
- Use only in an authorized shielded lab. Setting US changes wireless operating policy; follow local radio regulations.

## Build

Install the Rust MSVC toolchain, Visual Studio C++ Build Tools, and the Windows SDK, then run:

```powershell
cargo test --locked
cargo build --release --locked
```

The output is `target/release/steam-frame-6ghz-tool.exe`.

## Release

Pushing a tag that matches the version in `Cargo.toml` makes GitHub Actions test, build, and publish a Release with a Windows x64 EXE and SHA256 checksum file.

Replace `X.Y.Z` with the current `Cargo.toml` version, including any preview suffix:

```powershell
git tag vX.Y.Z
git push origin vX.Y.Z
```

You can also choose `main` in **Actions → Build and release → Run workflow** and supply an existing tag. The workflow builds the code at that tag.
