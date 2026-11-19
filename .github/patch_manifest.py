"""Adds the permissions and services Nexus needs to the Android manifest that
`flutter create` generates, and names the app "Nexus"."""
import re
import sys

path = sys.argv[1]
s = open(path, encoding="utf-8").read()
perms = [
    "INTERNET",
    "ACCESS_NETWORK_STATE",
    "ACCESS_WIFI_STATE",
    "CHANGE_WIFI_MULTICAST_STATE",
    "WAKE_LOCK",
    "FOREGROUND_SERVICE",
    "FOREGROUND_SERVICE_MEDIA_PROJECTION",
    "POST_NOTIFICATIONS",
    "REQUEST_INSTALL_PACKAGES",
    "RECORD_AUDIO",
    "FOREGROUND_SERVICE_MICROPHONE",
]
add = "".join(
    f'\n    <uses-permission android:name="android.permission.{p}"/>'
    for p in perms
    if f'android.permission.{p}"' not in s
)
s = re.sub(r"(<manifest[^>]*>)", lambda m: m.group(1) + add, s, count=1)
s = re.sub(r'android:label="[^"]*"', 'android:label="Nexus"', s, count=1)

services = """
        <!-- Shares this phone's screen with paired devices (shows a notification while on). -->
        <service
            android:name=".ScreenCaptureService"
            android:exported="false"
            android:foregroundServiceType="mediaProjection|microphone" />
        <!-- Lets paired devices tap and type on this phone once the user turns it on in Accessibility settings. -->
        <service
            android:name=".NexusAccessibilityService"
            android:exported="true"
            android:label="Nexus remote control"
            android:permission="android.permission.BIND_ACCESSIBILITY_SERVICE">
            <intent-filter>
                <action android:name="android.accessibilityservice.AccessibilityService" />
            </intent-filter>
            <meta-data
                android:name="android.accessibilityservice"
                android:resource="@xml/nexus_accessibility" />
        </service>
"""
# Lets a viewed screen shrink into a small floating window (picture-in-picture).
if "supportsPictureInPicture" not in s:
    s = s.replace("<activity", '<activity\n            android:supportsPictureInPicture="true"', 1)

if "ScreenCaptureService" not in s:
    s = s.replace("</application>", services + "    </application>", 1)

open(path, "w", encoding="utf-8").write(s)
print("Manifest patched:", ", ".join(perms), "+ screen sharing and remote-control services")
