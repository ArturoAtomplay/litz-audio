# Privacy Policy

Last updated: 2026-11-07

Litz Audio streams audio between your Android phone and your own computer over your local network. This policy covers the Android app and the `litzd` desktop service.

## What Litz does with your data

- **Audio.** When you turn on microphone mode, the app captures audio from your phone's microphone and sends it to the computer you paired, and only to that computer. In listening mode it receives audio from that computer. Audio travels over your local network or USB tethering, encrypted with TLS, and is never sent to the developer or anyone else. Litz does not record or store audio.
- **Pairing.** The phone and the computer each create a private key and certificate on the device. The app stores the paired computer's name, address and certificate fingerprint on your phone. The computer stores the phone's name and certificate. None of this leaves the two devices.
- **Settings.** Audio settings are stored on your phone and are excluded from cloud backups.
- **No accounts, analytics or crash reporting.** Litz has no user accounts, and its own code does not collect analytics or send crash reports.

## Ads in the Google Play version

The Google Play version of the app shows a banner ad, served by Google AdMob. The version on GitHub and F-Droid has no ads and contains no proprietary Google libraries.

In the Google Play version, the Google Mobile Ads SDK may collect and share with Google:

- your device's advertising ID,
- your IP address and approximate location derived from it,
- device and app information (model, operating system version, app version, language),
- ad interactions and diagnostics (ads viewed or tapped, performance and crash data of the ads SDK).

Google uses this data to serve, measure and personalize ads, and to prevent fraud, under [Google's Privacy Policy](https://policies.google.com/privacy) and its rules on [how Google uses information from apps that use its services](https://policies.google.com/technologies/partner-sites). The ads SDK never has access to your audio.

If you are in the European Economic Area, the United Kingdom, Switzerland or another region where consent is required, the app asks for your consent before personalized ads are shown. You can change your choice at any time in **Settings → About → Ad privacy choices**. You can also reset or delete your advertising ID in Android's settings, under **Privacy → Ads**.

## Permissions

- **Microphone**: used only in microphone mode, to send your voice to your paired computer.
- **Nearby devices / local network**: finds your computers on the local network.
- **Notifications**: shows that a session is running and lets you stop it.
- **Internet**: connects to your computer over the network. In the Google Play version, the ads SDK also uses it to load ads.

## Children

Litz is not directed at children under 13.

## Changes

Changes to this policy are published in this file, and the date at the top is updated. The history is available in the [repository](https://github.com/ArturoAtomplay/litz-audio/commits/main/PRIVACY.md).

## Contact

Open an issue at <https://github.com/ArturoAtomplay/litz-audio/issues>, or write to the email address shown on the app's Google Play page.
