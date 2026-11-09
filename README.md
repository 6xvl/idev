# idev

A small iPhone toolbox for Windows, written in Rust ([egui](https://github.com/emilk/egui) + [idevice](https://github.com/jkcoxson/idevice)).

It talks to the phone the same way iTunes and 3uTools do: Apple's usbmuxd service, then lockdownd over the
pairing record your PC already has once you tapped **Trust**. No jailbreak, no app on the phone.

## What it shows

| Page | What you get |
|---|---|
| **Overview** | Your real home screen, iOS version, serial, IMEIs, model, sales region, activation, iCloud backup, crash count, battery health, storage, and a checks card |
| **Battery** | Live watts / voltage / temperature graphs, charger verdict, time to full, design vs full-charge capacity, cycle count; one health reading per day is saved so idev can forecast when health reaches 75% and 70% |
| **Storage** | Where the space goes (top apps, other apps, iOS & media, free), app vs data size per app, what offloading would free |
| **Home screen** | Every page drawn from SpringBoard's layout with real icons; optional **tidy (A to Z)** with a preview, an automatic backup and restore |
| **Crashes** | Every crash report classified (crash, killed for memory, CPU overuse, heavy disk writes, freeze) and summarised in plain words, 14-day chart, export |
| **Security** | Wi-Fi sync, Wi-Fi debugging, Developer Mode, provisioning profiles, apps from outside the App Store, iCloud backup age, afc2 |
| **Live log** | The phone's system log with search, process filter, errors-only, pause, save |
| **Network** | Capture all phone traffic to a `.pcap` for Wireshark, with live traffic per app |
| **All values** | Every lockdown, battery, disk and power-source key, filterable |

Dark and light themes, frameless window with Windows 11 rounded corners. Phones on Wi-Fi are picked up when
Wi-Fi sync is on and no cable is plugged in.

## Requirements

- Windows 10/11
- **Apple Mobile Device Support** (comes with iTunes or the Apple Devices app). idev uses its usbmuxd service.
- The phone unlocked and **Trust This Computer** accepted once.

## Run

Download `idev.exe` from [Releases](https://github.com/6xvl/idev/releases) and run it.

Options: `--page <name>` opens on a page (`battery`, `storage`, `crashes`, …), `--dump` prints every value as text.

## Where files go

- `%APPDATA%\idev\battery-<UDID>.csv`: daily battery health history
- `%APPDATA%\idev\homescreen-<time>.plist`: home screen backups taken before a tidy
- `Documents\idev\`: network captures, exported crash reports, saved logs

Nothing is sent anywhere; idev only talks to the phone over the local usbmuxd socket.

## Not included

Screen mirroring and simulated location need Developer Mode and Apple's developer disk image on the phone.
Find My status, MDM profiles and parts history are not readable over this connection on current iOS.

## License

All rights reserved. You can read the code and run the official release for personal use. Copying, modifying,
re-uploading or distributing it is not allowed without written permission. See [LICENSE](LICENSE).
