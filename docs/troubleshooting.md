# Troubleshooting

Most problems are the network or the firewall, so start there.

## The phone doesn't find the computer

Check that Litz is running on the computer. On Linux:

```sh
systemctl --user status litzd
```

On Windows, look for `litzd.exe` in Task Manager. It starts at sign-in; if it isn't there, open a terminal and run `litzdw`.

Then check the network:

- Both devices have to be on the same network. A guest Wi-Fi, or a router with "client isolation" or "AP isolation" turned on, keeps devices from seeing each other.
- A VPN on the phone or the computer can hide one from the other. Turn it off, or allow local network traffic in its settings.
- The phone finds computers with mDNS (UDP port 5353). If your firewall blocks it, the computer never shows up under **Nearby**.

If the computer still doesn't appear, tap **Pair by IP address** in the app and type the computer's local address, such as `192.168.1.20`. That skips discovery.

## "Found on your network, but not answering"

Discovery works, but the firewall is dropping the audio port. Allow UDP 47165 on the computer.

**Ubuntu** uses ufw. Run `sudo ufw status` first: if it says inactive, the firewall isn't the problem.

```sh
sudo ufw allow 47165/udp
sudo ufw allow 5353/udp
```

**Fedora** comes with firewalld turned on:

```sh
sudo firewall-cmd --permanent --add-port=47165/udp --add-service=mdns
sudo firewall-cmd --reload
```

**Arch Linux** installs no firewall of its own. If you set up ufw or firewalld, use the commands above. With the stock `/etc/nftables.conf`, add this line to the `input` chain and run `sudo systemctl restart nftables`:

```
udp dport { 5353, 47165 } accept
```

**NixOS**: `services.litz-audio.enable = true` opens the port for you.

On Windows, allow `litzd.exe` on private networks in **Windows Security → Firewall & network protection → Allow an app through firewall**. Also check that your Wi-Fi is set as a private network, not a public one.

## The pairing prompt doesn't show up on the computer

The prompt is a desktop notification with **Allow** and **Deny** buttons. If the phone says the computer could not show it:

- turn off Do Not Disturb, and turn notifications on for Litz Audio;
- on a bare window manager, run a notification daemon that supports action buttons, such as dunst or mako.

## "This computer no longer trusts this phone"

The phone was removed on the computer with `litzd forget`. Pair it again from the Computers screen.

## "This computer's identity changed"

The computer has a new key, which happens when Litz's data on it was deleted or the system was reinstalled. If you did that, pair again. If you didn't, something else on your network is answering under that computer's name, so don't pair.

## "Update Litz on both your phone and computer"

The app and `litzd` are from versions that can't talk to each other. Install the latest release on both.

## Apps on the computer don't hear the phone

Pick **Litz Microphone** in the app you are calling or recording with. On Windows, if it is missing and the phone says the computer has no virtual microphone, run the Litz installer again.

Also check that **Mute** is off in the Litz app.

## Listen is on but the phone is silent

Open **Sound from** on the home screen and choose the output that is actually playing. **Default output** follows whatever the computer is using.

## The computer hears the phone's own speaker

With the microphone and Listen both on, the microphone picks up what the phone plays. Headphones fix it. Without headphones, set **Settings → Recording mode** to **Calls and meetings**, which uses the phone's echo cancellation when it has one.

## Audio cuts out or sounds choppy

That is nearly always Wi-Fi. Move closer to the router, use the 5 GHz band, or connect the phone by USB and turn on USB tethering. **Settings → About → Diagnostics** shows dropped and late packets while you test.

## Only one phone at a time

A computer streams with one phone. Turning on the microphone or Listen on a second paired phone moves the audio to it.

## Still stuck

Open an [issue](https://github.com/ArturoAtomplay/litz-audio/issues/new/choose) with the report from **Settings → About → Diagnostics**. On Linux, add the service log:

```sh
journalctl --user -u litzd -e
```
