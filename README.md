# Nexus

Control and keep track of all your devices from one app. This is **Milestone 1**: your PC and phone find each other on the same Wi-Fi, pair securely, and show whether each device is online.

## Download

Get the latest build from the **Releases** page of this repository (the link is on the right side of the repository's main page):

| Device | File |
|---|---|
| Android phone | `nexus-android.apk` |
| Windows PC | `nexus-windows.exe` |

A new build is made automatically every time something in this repository changes.

### Install on Android

1. On your phone, open the Releases page and tap **nexus-android.apk**.
2. Open the downloaded file. If Android asks, let your browser **install unknown apps**.
3. Open **Nexus**.

### Run on Windows

1. Download **nexus-windows.exe** and run it.
2. If SmartScreen warns you, click **More info**, then **Run anyway**. The file isn't signed yet.
3. When Windows Firewall asks, allow Nexus on **Private networks**.

## Pair your phone and PC

1. Open Nexus on both devices, connected to the same Wi-Fi. Each one shows the other under **Nearby** within a few seconds.
2. On the PC, click **Allow pairing**.
3. On the phone, tap **Pair** next to the PC.
4. Both screens show the same 6-digit number. Check that they match, then tap **Numbers match** on both.
5. The devices move to **My devices** and show **Online · verified**.

If a device doesn't show up, use **Pair by IP address**. Some routers stop devices from seeing each other on guest networks.

## What's in this repository

```
desktop/   Windows app (Rust, egui)
mobile/    Android app (Flutter)
PROTOCOL.md  how the two talk to each other
tools/vectors.py  shared test numbers both apps are checked against
.github/workflows/build.yml  the automatic builds
```

To build locally instead: run `cargo run --release` in `desktop/` (requires Rust). For the Android app, run `flutter create --platforms=android .` and then `flutter run` in `mobile/` (requires Flutter).
