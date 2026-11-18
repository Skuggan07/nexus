"""Makes the release APK use the Nexus signing key (android/app/nexus-release.jks,
password from the NEXUS_KEY_PASSWORD environment variable) instead of the
temporary debug key that `flutter create` sets up."""
import sys

path = sys.argv[1]
s = open(path, encoding="utf-8").read()
block = """
    signingConfigs {
        create("nexus") {
            storeFile = file("nexus-release.jks")
            storePassword = System.getenv("NEXUS_KEY_PASSWORD")
            keyAlias = "nexus"
            keyPassword = System.getenv("NEXUS_KEY_PASSWORD")
        }
    }

    buildTypes {"""
if 'create("nexus")' not in s:
    if s.count("    buildTypes {") != 1 or 'signingConfigs.getByName("debug")' not in s:
        sys.exit("Unexpected build.gradle.kts layout; cannot add the signing key")
    s = s.replace("\n    buildTypes {", block, 1)
    s = s.replace('signingConfigs.getByName("debug")', 'signingConfigs.getByName("nexus")')
open(path, "w", encoding="utf-8").write(s)
print("Release builds are signed with the Nexus key")
