# Troubleshooting

English | [Español](troubleshooting.es.md)

Almost every problem comes down to the network or the firewall, so check those first.

## The phone doesn't find the computer

First make sure Litz is running on the computer. On Linux:

```sh
systemctl --user status litzd
```

On Windows, look for `litzd.exe` in Task Manager. It starts when you sign in. If it's not there, open a terminal and run `litzdw`.

If it's running, look at the network:

- The phone and the computer have to be on the same network. Guest Wi-Fi won't work, and neither will a router with "client isolation" or "AP isolation" turned on, because both stop devices from seeing each other.
- A VPN on either device can hide one from the other. Turn it off, or allow local network traffic in its settings.
- The phone finds computers with mDNS (UDP port 5353). If the firewall blocks it, the computer never shows up under **Nearby**.

Still nothing? Tap **Pair by IP address** in the app and type the computer's local address, something like `192.168.1.20`. That skips discovery entirely.

## "Found on your network, but not answering"

The phone can see the computer, but the firewall is blocking the audio port. Allow UDP 47165 on the computer.

Ubuntu uses ufw. Run `sudo ufw status` first. If it says inactive, the firewall isn't your problem.

```sh
sudo ufw allow 47165/udp
sudo ufw allow 5353/udp
```

Fedora comes with firewalld turned on:

```sh
sudo firewall-cmd --permanent --add-port=47165/udp --add-service=mdns
sudo firewall-cmd --reload
```

Arch Linux doesn't install a firewall by default. If you set up ufw or firewalld, use the commands above. If you use the stock `/etc/nftables.conf`, add this line to the `input` chain and run `sudo systemctl restart nftables`:

```
udp dport { 5353, 47165 } accept
```

On NixOS, `services.litz-audio.enable = true` already opens the port.

On Windows, go to **Windows Security → Firewall & network protection → Allow an app through firewall** and allow `litzd.exe` on private networks. Also check that your Wi-Fi is set as a private network and not a public one.

## The pairing prompt doesn't show up on the computer

The prompt is a desktop notification with **Allow** and **Deny** buttons. If the phone says the computer couldn't show it, turn off Do Not Disturb and make sure notifications are on for Litz Audio.

If you use a bare window manager, you need a notification daemon that supports action buttons, like dunst or mako.

## "This computer no longer trusts this phone"

Someone removed the phone on the computer with `litzd forget`. Pair it again from the **Computers** screen.

## "This computer's identity changed"

The computer has a new key. That happens when Litz's data on it was deleted or the system was reinstalled. If you did that, just pair again. If you didn't, something else on your network is answering with that computer's name, so don't pair.

## "Update Litz on both your phone and computer"

The app and `litzd` are on versions that can't talk to each other. Install the latest release on both.

## Apps on the computer don't hear the phone

In the app you're calling or recording with, pick the right microphone:

- Linux: **Litz Microphone**
- Windows: **CABLE Output**. This needs [VB-CABLE](https://vb-audio.com/Cable/). Without it, the phone tells you the computer has no virtual microphone.

Also check that **Mute** is off in the Litz app.

## Listen is on but the phone is silent

On the home screen, open **Sound from** and pick the output that's actually playing. **Default output** follows whatever the computer is using.

## The computer hears the phone's speaker

With both the microphone and Listen on, the microphone picks up whatever the phone plays. Headphones fix it. If you don't have any, go to **Settings → Recording mode** and choose **Calls and meetings**, which uses the phone's echo cancellation if it has one.

## Audio cuts out or sounds choppy

It's almost always the Wi-Fi. Get closer to the router, switch to 5 GHz, or plug the phone in by USB and turn on USB tethering. While you test, **Settings → About → Diagnostics** shows dropped and late packets.

## Only one phone at a time

A computer streams with one phone at a time. If you turn on the microphone or Listen on a second paired phone, the audio moves to that one.

## Still stuck

Open an [issue](https://github.com/ArturoAtomplay/litz-audio/issues/new/choose) and paste the report from **Settings → About → Diagnostics**. On Linux, add the service log too:

```sh
journalctl --user -u litzd -e
```
