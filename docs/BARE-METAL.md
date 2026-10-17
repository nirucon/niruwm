# Bare-metal test protocol — NIRUWM 0.2.0

Run this on a machine that **already runs NIRUPI DWM** and that you can log
out of and back into. The point of the test is twofold:

1. NIRUWM starts from SDDM and is usable for a normal session.
2. NIRUPI DWM is exactly as it was afterwards.

Copy this file to the machine if you want, or just follow it.

## 0. Before touching anything

```sh
# Record what NIRUPI looks like now, so you can prove it is unchanged.
ls -l /usr/share/xsessions/ > /tmp/xsessions.before
sha256sum ~/.local/bin/dwm ~/.local/bin/nirupi-session \
          ~/.local/bin/dwm-status.sh > /tmp/nirupi.before
cat ~/.local/state/nirupi/managed-helpers.tsv > /tmp/helpers.before 2>/dev/null || true
```

Keep these files. You compare them in step 6.

## 1. Install

```sh
git clone https://github.com/nirucon/niruwm
cd niruwm
./install.sh                       # downloads static binaries
./install.sh --check               # should print OK for all four binaries
./install.sh --register-session    # adds the SDDM entry (sudo)
```

Expected: `NIRUWM is now selectable in SDDM. NIRUPI DWM remains unchanged.`

If the download fails because no release exists yet, build locally instead:

```sh
./install.sh --build --register-session
```

## 2. Log out of NIRUPI DWM

Log out normally (do not reboot). In SDDM, the session picker should now list
**NIRUWM** next to **NIRUPI DWM**.

### If the list is hidden

Some SDDM themes hide it until you click the session button (top-right or
bottom-left depending on the theme). The NIRU Noir SDDM theme in NIRUPI shows
it as a small row of names.

## 3. Log into NIRUWM

You should see:

- A bar at the top: workspaces `1`–`9` on the left, Swedish date and `hh:mm`
  on the right.
- Tag `1` highlighted.
- Niru Noir background (`#0f0f10`).

Walk through this list and note anything that fails:

| Check | Expected |
| --- | --- |
| `Super+Enter` | a terminal opens, tiled *below* the bar |
| `Super+P` | the launcher opens |
| Type `se` | the keyboard types `se`, not `us` (`z` is on the left) |
| `Super+Shift+Enter` is unbound | nothing happens (by design in 0.2.0) |
| Click workspace `3` in the bar | tag 3 becomes active |
| `Super+2` | tag 2 becomes active |
| `Super+Shift+1` on tag 2 | the focused window moves to tag 1 |
| `Super+J` / `Super+K` | focus cycles between windows |
| `Super+M`, `Super+T` | monocle, then tile layout |
| `Super+Shift+Q` | (not bound) nothing |
| `Super+Q` | focused window closes |
| `Super+Shift+E` | focused window goes fullscreen and back |
| Open a third app (e.g. Firefox) | it tiles next to the terminal |
| Open a file dialog from an app | it appears floating |

Also check that the things NIRUPI normally starts still work inside NIRUWM:

| Check | Expected |
| --- | --- |
| `Super+X` | screen locks (uses `nirupi-lock.sh`) |
| Volume/brightness keys | work (they are hardware keys, not WM keys) |
| Notifications (`notify-send test`) | appear (dunst) |
| `Super+Shift+P` | the NIRUPI launcher opens |
| Wallpaper | unchanged |

## 4. Log out of NIRUWM

Use SDDM's own logout, or from a terminal:

```sh
niructl quit
```

Expected: you return to the SDDM login screen. Check that the bar is gone and
that no orphan process is left:

```sh
pgrep -a nirubar    # no output
pgrep -a niruwm     # no output
```

If either of those prints a PID, that is a bug — report it.

## 5. Log back into NIRUPI DWM

Pick **NIRUPI DWM** in SDDM. Everything should be as before: your bar, your
wallpaper, your keybindings, your scratchpad.

## 6. Prove NIRUPI was not modified

```sh
ls -l /usr/share/xsessions/ > /tmp/xsessions.after
sha256sum ~/.local/bin/dwm ~/.local/bin/nirupi-session \
          ~/.local/bin/dwm-status.sh > /tmp/nirupi.after
diff /tmp/xsessions.before /tmp/xsessions.after   # only the new niruwm.desktop line
diff /tmp/nirupi.before /tmp/nirupi.after         # must be empty
cat ~/.local/state/nirupi/managed-helpers.tsv > /tmp/helpers.after 2>/dev/null || true
diff /tmp/helpers.before /tmp/helpers.after       # must be empty
```

The only acceptable difference in `xsessions` is the added
`niruwm.desktop`. The DWM binaries and the NIRUPI manifest must be byte
identical.

## 7. Remove NIRUWM if you want

```sh
cd ~/niruwm    # or wherever you cloned it
./install.sh --uninstall
```

Then log out and confirm that **NIRUWM has disappeared from SDDM** while
**NIRUPI DWM still works**.

## What to report back

For every failure, send:

- the step number and the command,
- the expected and actual result,
- the output of `journalctl --user -b -o short 2>/dev/null | tail -50`, or
  `dmesg | tail -20` if journalctl is unavailable,
- any output in `~/.xsession-errors` or the SDDM log
  (`journalctl -u sddm -b` or `/var/log/sddm*`).

The most useful single thing: a photo or screenshot of the NIRUWM desktop once
you are logged in.
