# Autostart

Oxide Monitor starts itself at logon through a **per-user Windows Task Scheduler task**.
There is no service, no driver, and **no administrator rights / UAC prompt** at any point.

## Task definition

| Property | Value |
|---|---|
| Name | `Oxide Monitor` (root task folder `\`) |
| Trigger | At logon for the current user, delay **`PT15S`** |
| Action | `<install dir>\oxide-monitor.exe` with `--autostart`, working directory = the executable folder |
| Principal | current user, `Interactive` logon type, `Limited` run level (`TASK_RUNLEVEL_LUA`) |
| Multiple instances | `IgnoreNew` |
| Power | starts and keeps running on battery (`DisallowStartIfOnBatteries=false`, `StopIfGoingOnBatteries=false`) |
| Execution time limit | none (`PT0S`) |
| Other | `StartWhenAvailable=true`, enabled |

The task is created only while `general.autostart = true`. Registration uses
`TASK_CREATE_OR_UPDATE`, so it is idempotent and automatically follows the executable
if the installation folder moves.

## Reconciliation

The monitor reconciles the task:

- at startup (before the first frame),
- when **Settings → Apply** commits `general.autostart`,
- when `config.toml` is hot-reloaded with a changed `general.autostart`.

Live **Preview** never creates or deletes a task (the checkbox is only applied on Apply).
Disabling deletes the task. On every reconcile the legacy value
`HKCU\Software\Microsoft\Windows\CurrentVersion\Run\Oxide Monitor` is removed
unconditionally, so no duplicate autostart entry can survive an upgrade.

`--autostart` is treated as a normal widget start: it keeps the tray icon and overlay
behaviour, but if another instance is already running it exits silently instead of
bringing the existing widget to the foreground.

## Status

The monitor publishes the last reconcile result to:

```
%APPDATA%\OxideMonitor\autostart.toml      (portable mode: next to oxide-monitor.exe)
```

```toml
version = 1
updated_at_unix_ms = 1795023440248

[status]
desired_enabled = true
enabled = true
task_name = "Oxide Monitor"
# last_error = "RegisterTaskDefinition: ... (HRESULT 0x80070005)"   # only on failure
```

- `desired_enabled` — value requested by `general.autostart`.
- `enabled` — the task is actually registered and enabled.
- `last_error` — last failure (access denied, Task Scheduler policy, service disabled, …).

The file is written atomically and is already up to date when the settings GUI receives
its `CommitResult`. The settings UI reads it through
`oxide_core::load_autostart_status()`. If the file is missing, the monitor has not run
yet and the UI should show “status unavailable”.

## Disable / remove

- **In the app:** Settings → General → turn off “Start with Windows” → **Apply**.
  The scheduled task is deleted and the legacy Run value stays removed.
- **Manually (per-user task, no admin needed):**

  ```powershell
  Unregister-ScheduledTask -TaskName 'Oxide Monitor' -Confirm:$false
  # or: schtasks /delete /tn "Oxide Monitor" /f
  ```

  Inspect it with `Get-ScheduledTask -TaskName 'Oxide Monitor'`; the task lives in `\`.
- **Legacy Run value** (the app removes it automatically):

  ```powershell
  Remove-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'Oxide Monitor' -ErrorAction SilentlyContinue
  ```

## Failure behaviour

If Task Scheduler is disabled by policy or access is denied, the app logs the exact
HRESULT and reports it in `autostart.toml`; it **never** falls back to writing the Run
key and never requests elevation. The widget itself continues to work normally.
