<h1 align="center">Huragok</h1>

<p align="center">In-game gameplay toolbox and control panel</p>

<p align="center">
  <a href="https://github.com/dend/huragok/actions/workflows/build.yml"><img alt="Build" src="https://img.shields.io/github/actions/workflow/status/dend/huragok/build.yml?branch=main"></a>
  <a href="https://github.com/dend/huragok/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/dend/huragok?sort=semver"></a>
  <a href="LICENSE"><img alt="License" src="https://img.shields.io/github/license/dend/huragok"></a>
  <img alt="Platform" src="https://img.shields.io/badge/platform-Windows-blue">
</p>

> [!CAUTION]
> **Use this at your own risk.** Huragok modifies the game while it runs. Halo Studios
> allows modding for this title within the limits it sets out in
> [Get Ready for Early Access](https://www.halowaypoint.com/news/early-access-primer), but
> there is no guarantee that this particular mod will always be considered acceptable, and
> that stance could change in the future. Experiment for fun, and understand that you alone
> are responsible for how you use it.

Huragok is a single-player toolbox for Halo: Campaign Evolved. It adds a free-flying
camera, an in-game control panel, skull toggles, time controls, a keyframe system
for camera shots, and a way to take over an enemy and play as it, all from inside the
running game. Point it at a mission and you can fly around a firefight, slow it down,
flip skulls on and off, drop into a Grunt or an Elite and fight as it, or set up a
moving camera path for a clip.

It runs as a mod that loads while the game is running. Nothing on disk is patched and
the game's own files are left alone.

> [!NOTE]
> Named after the [Huragok](https://www.halopedia.org/Huragok) (the Engineers) from
> Halo, who take technology apart and put it back together.

<img width="3838" height="2159" alt="ImGui" src="https://github.com/user-attachments/assets/bb489b69-0de9-4c1c-bef7-dd07f768f594" />

## What you can do

- Fly a free camera anywhere in the level, with adjustable speed and field of view
- Slow down or speed up time, or pause on a frame
- Turn skulls on and off from a checklist, including third-person and night vision
- Record camera keyframes and play back a smooth moving shot
- Take over a nearby enemy and play as it in third person, aiming and firing its weapon
- Read live mission info: current mission, difficulty, checkpoint, and framerate

## Play as an enemy

Aim at an enemy and press `Ctrl+G`. The view drops into third person and you take over
that body. Move with `W` `A` `S` `D`, aim with the mouse, and click to fire its weapon.
Its own side leaves you alone while you wear it, so you can walk through a Covenant
patrol as one of them. Press `Ctrl+G` again to step out and go back to the Chief.

This is for messing around, not for finishing a mission. It works best up close.
Enemies far across the level may stand still until you walk toward them, the same way
the game brings them to life as you get near.

## Install

1. Download `huragok-<version>.zip` from the
   [latest release](https://github.com/dend/huragok/releases/latest) and extract it
   anywhere.
2. Right-click `install.ps1` and run it with PowerShell, or from a PowerShell window:
   ```powershell
   .\install.ps1
   ```
   It finds your Steam copy of the game, sets up the proxy it loads through, and copies
   the mod in. If your game is not on Steam, point it at the install folder:
   ```powershell
   .\install.ps1 -GamePath "D:\Games\Halo Campaign Evolved"
   ```
   > [!NOTE]
   > The Microsoft Store and Xbox app (Game Pass) version is not supported out of the box.
   > Its install layout and file protections differ, so the installer may not find or write
   > to it. You can likely get the same result by placing the `huragok.dll` binary and the
   > proxy into the game folders by hand, following the manual steps below.
3. Start the game and load a mission. Press `INSERT` to detach the camera, or `Ctrl+B`
   to open the panel.

> [!NOTE]
> If you would rather not run a script you have not read, the installer is short and
> plain to follow: read [`install.ps1`](https://github.com/dend/huragok/blob/main/install.ps1),
> then run it as is or adjust it to fit your setup.

### Manual install

The mod loads through a `dwmapi.dll` proxy that side-loads anything in a `mods` folder
next to the game. To set it up by hand:

1. Copy Windows' own `dwmapi.dll` (from `C:\Windows\System32\`) into the game's binaries
   folder:
   ```
   ...\Halo Campaign Evolved\Meteorite\Binaries\Win64\
   ```
2. In that same folder, create a `mods` folder if there isn't one, and copy `huragok.dll`
   into it.
3. Launch the game.

## Controls

The camera and hotkeys are always live. Every command shortcut is `Ctrl` plus a key, so
nothing fires by accident while you type in the console.

Free camera:

- `INSERT` toggle the free camera
- `W` `A` `S` `D` move, `Space` or `E` up, `Q` down
- `Shift` move faster
- Arrow keys aim, or `Ctrl+M` to steer with the mouse
- `[` and `]` field of view
- `Z` `C` roll, `X` reset roll

Panel and commands:

- `Ctrl+B` open or close the panel
- `Ctrl+I` hand the mouse to the panel so you can click its controls
- `Ctrl+P` pause
- `Ctrl+,` and `Ctrl+.` slow down or speed up time, `Ctrl+/` back to normal
- `Ctrl+Home` / `Ctrl+End` cinematic bars on or off
- `Ctrl+F5` / `Ctrl+F6` fade out or in
- `Ctrl+K` add a camera keyframe, `Ctrl+J` play or stop the path, `Ctrl+L` clear it

Play as an enemy:

- `Ctrl+G` take over the enemy you are aiming at, or hand the body back
- Then move, aim, and fire as you normally would

Skulls, time, the console, and the mission readout all live inside the panel.

## Build it yourself

You need the Rust toolchain (MSVC) and the Visual Studio C++ build tools. Then:

```powershell
cargo build --release
```

The DLL lands at `target\release\huragok.dll`. Every push builds the same file on
GitHub Actions, so you can download it from a workflow run instead of building locally.

## Disclaimer

This is an independent, fan-made project. It is not affiliated with, endorsed by, or
sponsored by Microsoft or Halo Studios. All game names, trademarks, and assets belong to
their respective owners.

## License

MIT. See [LICENSE](LICENSE).
