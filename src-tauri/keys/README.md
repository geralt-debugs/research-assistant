# Android Signing

The release keystore and its credentials are generated locally in this directory:

- `vane-release.jks`
- `keystore.properties`

Both files are ignored by Git. Keep an encrypted backup outside the repository. Losing the keystore prevents future updates to an app distributed under that signing identity.

The keystore uses the `vane-release` alias and contains a 2048-bit RSA signing key valid until January 2054. The properties file contains `storeFile`, `storePassword`, `keyAlias`, and `keyPassword`; do not expose it in logs or commit it.

`src-tauri/gen/android/app/build.gradle.kts` loads these files automatically for release builds. If the credential file is absent, Gradle can still produce debug builds, but release output will not use this signing identity.

Build the signed universal APK from the repository root:

```sh
npm run tauri android build -- --apk
```

Verify its certificate:

```sh
"$ANDROID_HOME/build-tools/37.0.0/apksigner" verify --verbose --print-certs \
  src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk
```

Expected certificate SHA-256 digest:

```text
b980e5995f58435e9b483574c8a72845912051ba230cea4e0cf969ffaf0216ce
```
