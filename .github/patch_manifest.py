"""Adds the network permissions Nexus needs to the Android manifest that
`flutter create` generates, and names the app "Nexus"."""
import re
import sys

path = sys.argv[1]
s = open(path, encoding="utf-8").read()
perms = ["INTERNET", "ACCESS_NETWORK_STATE", "ACCESS_WIFI_STATE", "CHANGE_WIFI_MULTICAST_STATE", "WAKE_LOCK"]
add = "".join(
    f'\n    <uses-permission android:name="android.permission.{p}"/>'
    for p in perms
    if f'android.permission.{p}"' not in s
)
s = re.sub(r"(<manifest[^>]*>)", lambda m: m.group(1) + add, s, count=1)
s = re.sub(r'android:label="[^"]*"', 'android:label="Nexus"', s, count=1)
open(path, "w", encoding="utf-8").write(s)
print("Manifest patched:", ", ".join(perms))
