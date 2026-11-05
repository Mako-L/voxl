#!/bin/sh
# Builds android/build/voxl.apk with the Android SDK's own tools (no Gradle).
# Needs ANDROID_HOME (default ~/Library/Android/sdk) with a platform and build-tools,
# and a JDK 17+. Install: adb install -r android/build/voxl.apk
set -eu
cd "$(dirname "$0")"
SDK="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
BT="$SDK/build-tools/$(ls "$SDK/build-tools" | sort -V | tail -1)"
JAR="$SDK/platforms/$(ls "$SDK/platforms" | sort -V | tail -1)/android.jar"
OUT=build
rm -rf "$OUT/classes" "$OUT/res" "$OUT"/*.apk "$OUT/classes.dex"
mkdir -p "$OUT/classes" "$OUT/res"
"$BT/aapt2" compile --dir res -o "$OUT/res/res.zip"
"$BT/aapt2" link -I "$JAR" --manifest AndroidManifest.xml -o "$OUT/base.apk" "$OUT/res/res.zip"
javac -source 17 -target 17 -classpath "$JAR" -d "$OUT/classes" $(find src -name '*.java')
"$BT/d8" --min-api 30 --lib "$JAR" --output "$OUT" $(find "$OUT/classes" -name '*.class')
cp "$OUT/base.apk" "$OUT/unsigned.apk"
(cd "$OUT" && zip -q -j unsigned.apk classes.dex)
"$BT/zipalign" -f -p 4 "$OUT/unsigned.apk" "$OUT/aligned.apk"
# A local signing key, made on first build (kept out of git).
KS="$OUT/voxl-debug.keystore"
[ -f "$KS" ] || keytool -genkeypair -keystore "$KS" -storepass android -keypass android \
	-alias voxl -keyalg RSA -keysize 2048 -validity 10000 -dname "CN=voxl" >/dev/null 2>&1
"$BT/apksigner" sign --ks "$KS" --ks-pass pass:android --key-pass pass:android \
	--out "$OUT/voxl.apk" "$OUT/aligned.apk"
echo "built $(pwd)/$OUT/voxl.apk"
