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
    "REQUEST_IGNORE_BATTERY_OPTIMIZATIONS",
    "FOREGROUND_SERVICE_CONNECTED_DEVICE",
    "CHANGE_NETWORK_STATE",
    "RECEIVE_BOOT_COMPLETED",
    "VIBRATE",
    "MANAGE_EXTERNAL_STORAGE",
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

# "Share → Nexus": sends photos and files from any app to the PCs.
share = """
        <activity
            android:name=".ShareActivity"
            android:exported="true"
            android:label="Nexus"
            android:excludeFromRecents="true"
            android:noHistory="true"
            android:theme="@android:style/Theme.Translucent.NoTitleBar">
            <intent-filter>
                <action android:name="android.intent.action.SEND" />
                <category android:name="android.intent.category.DEFAULT" />
                <data android:mimeType="*/*" />
            </intent-filter>
            <intent-filter>
                <action android:name="android.intent.action.SEND_MULTIPLE" />
                <category android:name="android.intent.category.DEFAULT" />
                <data android:mimeType="*/*" />
            </intent-filter>
        </activity>
"""
if "ShareActivity" not in s:
    s = s.replace("</application>", share + "    </application>", 1)

# "Stay connected" (reachable with Nexus closed), starting again after a restart,
# and the Stop button of Find my phone.
background = """
        <service
            android:name=".KeepAliveService"
            android:exported="false"
            android:foregroundServiceType="connectedDevice" />
        <receiver
            android:name=".BootReceiver"
            android:exported="true">
            <intent-filter>
                <action android:name="android.intent.action.BOOT_COMPLETED" />
                <action android:name="android.intent.action.QUICKBOOT_POWERON" />
                <action android:name="android.intent.action.MY_PACKAGE_REPLACED" />
            </intent-filter>
        </receiver>
        <receiver
            android:name=".RingStopReceiver"
            android:exported="false" />
"""
if "KeepAliveService" not in s:
    s = s.replace("</application>", background + "    </application>", 1)

open(path, "w", encoding="utf-8").write(s)
print("Manifest patched:", ", ".join(perms), "+ screen sharing, remote control, Share → Nexus and Stay connected")
