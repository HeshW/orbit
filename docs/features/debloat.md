# Feature: Guided debloat (v0.6, 8f), Optimize > Debloat

Most people can't tell what "MicrosoftEdgeUpdateTaskMachineCore" or "SysMain" is. Debloat
reads the startup apps, scheduled tasks, services and built-in apps on this PC, explains each
one in plain words and gives a verdict from a reviewed list that ships with Orbit. It turns
off only the items the list calls safe, only the ones you tick, after a preview, and every
change can be undone. Shapes and commands are in `docs/CONTRACT.md` ("v0.6").

Files: `src-tauri/src/debloat/` (`mod.rs` engine, change log and commands; `known.rs` the
reviewed list; `guard.rs` the never-offer list; `system.rs` the real Windows calls;
`tests.rs`), `ui/js/views/optimize/debloat.js`, `ui/css/views/optimize/debloat.css`,
`ui/js/mock/debloat.js`. Shared files touched: `lib.rs` (module + six commands),
`Cargo.toml` (WinRT features `ApplicationModel`, `Foundation`, `Foundation_Collections`,
`Management_Deployment`), `autoruns.rs` (`all_tasks`, `auto_services`; the task walk is now a
visitor shared with the autoruns list), `services.rs` (`collect_services` and `forget_config`
are `pub(crate)`, new `set_start_type`), `actions.rs` (`shell_open` is `pub(crate)`),
`health.rs` (startup warning links here), `views/optimize.js` (fifth tab), `index.html`,
`mock/index.js`, `mock/health.js`, `tests/ui-smoke.mjs`.

## What it shows

- Before the first run: one line about what Debloat does and **Analyze this PC**. Nothing is
  read until you press it. The Changes list (below) shows even before analyzing.
- Summary: "12 things are safe to turn off" (counts up once), "Orbit checked N items against
  its reviewed list", chips for safe / keep / not on the list, **Tick all safe** (or **Clear
  ticks**) and **Review changes (n)**. When a ticked item needs administrator rights and Orbit
  is not elevated: "Tasks and services need administrator rights. Restart as administrator".
  The Debloat tab shows the safe count as its badge.
- Four groups, in this order: **Startup apps**, **Scheduled tasks**, **Services**, **Built-in
  apps**. Each has a one-line note on what turning off means for that group. Safe items are
  listed first; **Keep (n)** and **Not on Orbit's list (n)** fold away under them.
- Each item: the plain name with the raw Windows name in small mono text under it, one line
  on what it does, the verdict chip and one line under it:
  - **Safe to turn off**: "You lose ..." and a tick box. Already off: an "Already off" chip and
    no tick box. Startup apps measured as high or medium impact add "Windows may start a
    little faster." (the only speed wording, and only where Orbit measured a cost).
  - **Keep**: "Why keep ...". Items the guard refuses show the guard's reason ("It protects
    this PC.", "Your graphics card needs it.").
  - **Unknown**: "Not on Orbit's list. Leave it on unless you know what it is." and **Search
    online** (Bing, as elsewhere). An unknown startup app that Orbit's startup check picked
    (measured high or medium cost) says so and links to Startup instead.
  - Startup apps show the measured impact chip; the program or command, the task's program
    or a service's start type is the last line.
- **Review changes** opens a dialog, "Apply 3 changes?": one row per change ("Turn off at
  sign-in: On at sign-in -> Off", "Set to Manual: Automatic (delayed) -> Manual", "Remove for
  this user: Installed -> Removed for you"), then "Skipped" with the reason for each ticked
  item that won't change, and for removals "Removed apps come back from the Microsoft Store".
  If every ticked item needs administrator rights, it asks to restart as administrator
  instead. **Apply** then toasts "Changed 3 items. Undo any of them from Changes."
- **Changes**: every change Orbit made, newest first, with what was done, the old service
  start type and the time. **Undo** per item; a removed app has **Open Store page** instead.
  **Undo all** (asks first) turns startup apps and tasks back on and restores services; removed
  apps stay listed so you can open their Store pages one at a time. Undone rows say "Undone".
- Health's "Startup load" warning (high-impact apps Orbit would suggest) now offers **Review
  in Debloat** instead of Go to Startup.

## Rules

- Verdicts come only from `known.rs` (82 entries, listed below). Patterns match without case,
  `*` = any text: startup apps by entry name or program file name, tasks by full path,
  services by name, apps by package name. The first match in the group wins.
- What is listed: every startup entry; tasks outside `\Microsoft\` plus Windows tasks on the
  list (unreviewed Windows tasks are not shown at all); services on the list plus services that
  start by themselves and run a program outside the Windows folder (drivers excluded); built-in
  apps only when on the list. Built-in apps come from
  `PackageManager.FindPackagesByUserSecurityIdWithPackageTypes("", Main)`: this user's main
  packages only; framework, resource and system-signed packages are marked system and refused.
- The guard (`guard.rs`) runs at analysis and again on the fresh item right before every
  change, whatever the list or the UI says. A refused item is Keep and never changes. It
  refuses: Defender / SmartScreen / Windows Security / firewall / BFE / cryptography / LSA
  services; Windows Update, BITS, Delivery Optimization, Windows Installer, TrustedInstaller;
  networking (DNS, DHCP, NLA, NSI, WLAN, Wcm, Workstation/Server, IP Helper, RAS, IKE...);
  audio; the print spooler (always, so a printer is never cut off); RPC, DCOM, Plug and Play,
  Power, User Profile, Event Log, Task Scheduler, WMI, time sync and other core brokers; the
  Store, App Installer, ClipSVC, AppX deployment; .NET and VC++ runtimes; drivers (`.sys`);
  graphics and sound vendors' core services and control panels (NVIDIA display container,
  AMD, Intel, Realtek, Nahimic, Waves, Dolby); Windows tasks and programs in the Windows folder
  that are not on the reviewed list; framework and system packages, codec extensions, the
  shell, Start, Search, Edge; and Orbit itself. A unit test checks that no Safe entry on the
  list is refused by the guard.
- Changes (only Safe items, still on, not refused, ticked by you, previewed):
  - Startup apps: the StartupApproved value, through the same code as the Startup view
    (`startup::set_startup_enabled`). Never deleted. HKLM / all-users entries need admin.
  - Scheduled tasks: `ITaskService` -> `GetTask(path)` -> `IRegisteredTask::put_Enabled(false)`.
    Never deleted. Tasks under `\Microsoft\` or running as another account need admin.
  - Services: `ChangeServiceConfigW` to Manual (`SERVICE_DEMAND_START`), never Disabled; the
    service is not stopped or started. Always needs admin.
  - Built-in apps: `PackageManager.RemovePackageAsync(fullName)` without removal options, so
    for the current user only, never provisioned / all users. The family name is kept so Undo
    can open `ms-windows-store://pdp/?PFN=<family>`.
- Change log: `debloat-changes.json` in Orbit's data folder (`%LOCALAPPDATA%\<app id>\debloat`;
  debug builds honour `ORBIT_DEBLOAT_DIR`). Each change is written as "pending" with the
  previous state BEFORE the Windows call, then marked "applied"; if the call fails the entry
  is dropped (nothing changed). A pending entry left by a crash can be undone like an applied
  one. Writes are atomic (temp file + rename) and serialized by a lock.
- Undo: startup apps and tasks are turned back on; a service gets its recorded start type
  back (Automatic or Automatic (delayed)); a removed app opens its Store page and counts as
  undone once the package is installed again (checked when the Changes list loads). Undoing
  a task or service change needs admin like making it. Undo all goes newest first and skips
  removed apps.
- Copy: plain, one line each, no scare words and no speed promises; a test rejects words like
  "danger", "risk", "boost" and "speed up" in the list.

## Testing

- `cargo test debloat` (fake backend; nothing touches the PC): glob matching; the list's size
  and wording; the list never contradicts the guard; the docs list every entry and pattern;
  the guard refuses the never-offer list (services, tasks, startup, apps, system packages); the
  guard wins over a wrong list that calls Defender, the spooler, the Store and Orbit safe, and
  no backend call happens; the UI cannot get anything but Safe items changed (Keep, Unknown,
  guarded, already off, not on the PC, unreviewed Windows tasks); analysis groups, verdicts,
  names and counts; the preview changes and writes nothing; every change is on disk as
  pending before its call; a failed change leaves nothing to undo; Undo restores each kind
  (startup, task, service with its delayed start, app via the Store and its reinstall);
  Undo all; admin rules for apply and undo; a pending change from a crash can be undone.
- `cargo test debloat::tests::real_analysis -- --ignored --nocapture` reads this PC (read only)
  and prints the verdicts.
- `tests/ui-smoke.mjs` "debloat" (mock): Health's link, every verdict in every group, no tick
  box on Keep or Unknown, Search online and the impact hint, the preview text with an
  admin-only item skipped, apply, Undo, Restart as administrator then a service change, Undo
  all leaving the removed app, Open Store page and its reinstall.
- Mock (`mock/debloat.js`): a fixed PC with every verdict in every group; apply and undo only
  change the mock's state; a removed app's Undo counts as reinstalled on the next list.

## The reviewed list

Generated from `src-tauri/src/debloat/known.rs`; `docs_list_every_reviewed_entry` fails when
an entry or pattern is missing here.

### Startup apps (entry name or program file)

| Verdict | Plain name | Matches | What it does | What you lose / why keep |
|---|---|---|---|---|
| Safe to turn off | Steam | `steam.exe` | Opens Steam in the background when you sign in. | Steam opens when you start it; game updates wait until then. |
| Safe to turn off | Epic Games Launcher | `epicgameslauncher.exe` | Opens the Epic launcher when you sign in. | The launcher opens when you start it; game updates wait until then. |
| Safe to turn off | EA app | `eadesktop.exe`<br>`origin.exe` | Opens the EA app when you sign in. | The EA app opens when you start it; game updates wait until then. |
| Safe to turn off | Riot Client | `riotclientservices.exe` | Starts the Riot Client in the background at sign-in. | Riot games update when you open the client. |
| Safe to turn off | Spotify | `spotify.exe` | Opens Spotify, minimized, when you sign in. | Spotify opens when you start it. |
| Safe to turn off | Discord | `Discord` | Opens Discord when you sign in. | You show as offline until you open Discord. |
| Safe to turn off | Microsoft Edge in the background | `MicrosoftEdgeAutoLaunch_*` | Starts Edge hidden at sign-in so it opens faster later. | Edge takes a moment longer to open the first time. |
| Safe to turn off | Google Chrome in the background | `GoogleChromeAutoLaunch_*` | Starts Chrome hidden at sign-in so it opens faster later. | Chrome takes a moment longer to open the first time. |
| Safe to turn off | Microsoft Teams | `ms-teams.exe`<br>`com.squirrel.Teams.Teams` | Opens Teams when you sign in. | Chats and calls reach you once you open Teams. |
| Safe to turn off | Adobe Acrobat sync | `adobecollabsync.exe` | Syncs shared PDFs and reviews with Adobe's cloud. | Shared reviews sync when you open Acrobat. |
| Safe to turn off | Adobe updater at sign-in | `updaterstartuputility.exe` | Checks for Adobe app updates when you sign in. | Adobe apps check for updates when you open them. |
| Safe to turn off | Opera browser assistant | `browser_assistant.exe` | A desktop helper that shows Opera news and offers. | Nothing in the Opera browser itself. |
| Safe to turn off | Wondershare helper | `wshelper.exe` | Wondershare's background helper for updates and offers. | Wondershare apps check for updates when you open them. |
| Keep | Microsoft OneDrive | `onedrive.exe` | Keeps your files in sync with OneDrive. | Turning it off pauses file sync and backup until you open OneDrive. |
| Keep | Google updater | `GoogleUpdaterTaskUser*` | Wakes Google's updater at sign-in. | Chrome gets security fixes through it. |
| Keep | Windows Security icon | `SecurityHealth`<br>`securityhealthsystray.exe` | Shows the Windows Security icon and its alerts. | You would miss security alerts. |
| Keep | Realtek audio | `rtkauduservice64.exe`<br>`ravcpl64.exe` | Runs the controls for your Realtek sound chip. | Headphone and microphone jacks may stop switching by themselves. |

### Scheduled tasks (task path)

| Verdict | Plain name | Matches | What it does | What you lose / why keep |
|---|---|---|---|---|
| Safe to turn off | Windows compatibility check | `\Microsoft\Windows\Application Experience\Microsoft Compatibility Appraiser` | Scans your apps and sends compatibility data to Microsoft before big updates. | Windows may not warn you early about an app that won't run after a big update. |
| Safe to turn off | App usage report | `\Microsoft\Windows\Application Experience\ProgramDataUpdater` | Collects data about the programs you use for Microsoft. | Nothing you will notice. |
| Safe to turn off | Windows usage reports | `\Microsoft\Windows\Customer Experience Improvement Program\*` | Send Windows and USB usage data to Microsoft's improvement program. | Nothing you will notice. |
| Safe to turn off | Disk check report | `\Microsoft\Windows\Autochk\Proxy` | Sends disk check results to Microsoft's improvement program. | Nothing; disk checks still run. |
| Safe to turn off | Disk diagnostics report | `\Microsoft\Windows\DiskDiagnostic\Microsoft-Windows-DiskDiagnosticDataCollector` | Collects disk and system details and sends them to Microsoft. | Nothing you will notice. |
| Safe to turn off | Feedback upload | `\Microsoft\Windows\Feedback\Siuf\DmClient`<br>`\Microsoft\Windows\Feedback\Siuf\DmClientOnScenarioDownload` | Sends feedback and diagnostic data to Microsoft. | Nothing; Feedback Hub still works when you open it. |
| Safe to turn off | Device report | `\Microsoft\Windows\Device Information\Device`<br>`\Microsoft\Windows\Device Information\Device User` | Sends details about this PC's hardware to Microsoft. | Nothing you will notice. |
| Safe to turn off | Error report sender | `\Microsoft\Windows\Windows Error Reporting\QueueReporting` | Sends saved crash reports to Microsoft. | Microsoft gets no reports about apps that crashed here. |
| Safe to turn off | Offline maps updater | `\Microsoft\Windows\Maps\MapsUpdateTask`<br>`\Microsoft\Windows\Maps\MapsToastTask` | Updates the offline maps you downloaded and tells you about it. | Offline maps update when you open Maps. |
| Safe to turn off | Office usage report | `\Microsoft\Office\OfficeTelemetryAgentLogOn*`<br>`\Microsoft\Office\OfficeTelemetryAgentFallBack*` | Sends Office usage data to Microsoft. | Nothing you will notice; Office works the same. |
| Safe to turn off | OneDrive usage report | `\OneDrive Reporting Task-*` | Sends OneDrive usage data to Microsoft. | Nothing; OneDrive keeps syncing. |
| Safe to turn off | Zoom updater | `\ZoomUpdateTaskUser*` | Checks for Zoom updates in the background. | Zoom checks for updates when you open it. |
| Keep | Microsoft Edge updater | `\MicrosoftEdgeUpdateTask*` | Checks for Edge updates. | Edge gets security fixes through it, and Windows uses parts of Edge. |
| Keep | Google updater | `\GoogleUpdateTask*`<br>`\GoogleSystem\GoogleUpdater\*`<br>`\GoogleUser\GoogleUpdater\*` | Checks for Chrome and other Google app updates. | Chrome gets security fixes through it. |
| Keep | Adobe Acrobat updater | `\Adobe Acrobat Update Task` | Checks for Acrobat updates. | Acrobat gets security fixes through it. |
| Keep | OneDrive updater | `\OneDrive Standalone Update Task-*` | Keeps OneDrive up to date. | OneDrive gets fixes through it. |
| Keep | Office updater | `\Microsoft\Office\Office Automatic Updates 2.0` | Installs Office updates. | Office gets security fixes through it. |
| Keep | NVIDIA app updater | `\NVIDIA App SelfUpdate_*` | Keeps the NVIDIA app up to date. | Your graphics drivers come through the NVIDIA app. |

### Services (service name)

| Verdict | Plain name | Matches | What it does | What you lose / why keep |
|---|---|---|---|---|
| Safe to turn off | Windows telemetry | `DiagTrack` | Sends diagnostic and usage data to Microsoft. | Nothing you will notice; Windows works the same. |
| Safe to turn off | Device management messages | `dmwappushservice` | Passes device-management and diagnostic messages. | A PC run by work or school may need it. |
| Safe to turn off | Downloaded maps manager | `MapsBroker` | Keeps offline maps up to date. | Offline maps update when you open Maps. |
| Safe to turn off | Shortcut link tracker | `TrkWks` | Keeps shortcuts working when files move between drives on a network. | A shortcut to a file that moved on a network drive may stop working. |
| Safe to turn off | Adobe update service | `AdobeUpdateService` | Installs updates for Adobe Creative Cloud apps. | Adobe apps update when you open Creative Cloud. |
| Safe to turn off | Adobe genuine check | `AGSService`<br>`AGMService` | Checks that your Adobe apps are genuine. | Nothing for licensed apps. |
| Keep | App preloader (SysMain) | `SysMain` | Learns which apps you use and loads them ahead of time. | Apps open faster with it; turning it off rarely helps. |
| Keep | Windows Search indexer | `WSearch` | Indexes your files so Start and File Explorer find them fast. | Search gets slow or misses files without it. |
| Keep | Microsoft Edge update service | `edgeupdate`<br>`edgeupdatem` | Installs Edge updates. | Edge gets security fixes through it. |
| Keep | Google update service | `gupdate`<br>`gupdatem`<br>`GoogleUpdaterService*`<br>`GoogleUpdaterInternalService*` | Installs Chrome and Google app updates. | Chrome gets security fixes through it. |
| Keep | Adobe Acrobat update service | `AdobeARMservice` | Installs Acrobat updates. | Acrobat gets security fixes through it. |
| Keep | Office Click-to-Run | `ClickToRunSvc` | Runs and updates Microsoft Office. | Office needs it to start and update. |
| Keep | Xbox Gaming Services | `GamingServices`<br>`GamingServicesNet` | Runs Game Pass and Microsoft Store games. | Those games won't start without it. |

### Built-in apps (package name)

| Verdict | Plain name | Matches | What it does | What you lose / why keep |
|---|---|---|---|---|
| Safe to turn off | Clipchamp | `Clipchamp.Clipchamp` | Microsoft's video editor. | The Clipchamp app. |
| Safe to turn off | Microsoft News | `Microsoft.BingNews` | A news headlines app. | The News app; news sites still open in your browser. |
| Safe to turn off | Weather | `Microsoft.BingWeather` | A weather forecast app. | The Weather app. |
| Safe to turn off | Microsoft Bing | `Microsoft.BingSearch` | The Bing search app. | The Bing app; search in Start keeps working. |
| Safe to turn off | Get Help | `Microsoft.GetHelp` | Microsoft's support app. | Some Windows troubleshooters open in Get Help. |
| Safe to turn off | Tips | `Microsoft.Getstarted` | Tips about Windows features. | The Tips app. |
| Safe to turn off | Feedback Hub | `Microsoft.WindowsFeedbackHub` | Sends your feedback to Microsoft. | You can't send feedback to Microsoft. |
| Safe to turn off | Solitaire Collection | `Microsoft.MicrosoftSolitaireCollection` | Card games with ads. | The card games. |
| Safe to turn off | 3D apps | `Microsoft.Microsoft3DViewer`<br>`Microsoft.Print3D`<br>`Microsoft.3DBuilder` | Views and prints 3D models. | Opening 3D model files. |
| Safe to turn off | Mixed Reality Portal | `Microsoft.MixedReality.Portal` | Sets up Windows VR headsets. | Windows Mixed Reality headset setup. |
| Safe to turn off | Microsoft 365 hub | `Microsoft.MicrosoftOfficeHub` | A launcher for Office apps and files. | The launcher; installed Office apps keep working. |
| Safe to turn off | Skype | `Microsoft.SkypeApp` | Skype calls and chat. Microsoft retired Skype in 2025. | The Skype app. |
| Safe to turn off | People | `Microsoft.People` | A contacts app. | The People app. |
| Safe to turn off | Maps | `Microsoft.WindowsMaps` | Maps and directions. | The Maps app and its offline maps. |
| Safe to turn off | Mail and Calendar | `microsoft.windowscommunicationsapps` | Microsoft's older mail and calendar apps. | The Mail and Calendar apps; Outlook replaces them. |
| Safe to turn off | Movies & TV | `Microsoft.ZuneVideo` | A video player and store. | The Movies & TV app; other video players still work. |
| Safe to turn off | Phone Link | `Microsoft.YourPhone` | Shows texts, calls and photos from your phone. | Phone Link. |
| Safe to turn off | Cortana | `Microsoft.549981C3F5F10` | Microsoft's retired voice assistant. | The Cortana app. |
| Safe to turn off | Xbox app | `Microsoft.GamingApp` | Game Pass and Xbox games on PC. | Installing and opening Game Pass games. |
| Safe to turn off | Xbox Game Bar | `Microsoft.XboxGamingOverlay` | The Win+G overlay for screenshots, clips and chat. | Game Bar and its screenshots and clips. |
| Safe to turn off | Xbox overlay parts | `Microsoft.XboxGameOverlay`<br>`Microsoft.XboxSpeechToTextOverlay` | Older Game Bar parts and party chat captions. | Older Game Bar features. |
| Safe to turn off | Dev Home | `Microsoft.Windows.DevHome` | Microsoft's retired developer dashboard. | The Dev Home app. |
| Safe to turn off | Power Automate | `Microsoft.PowerAutomateDesktop` | Automates tasks on the desktop. | The Power Automate app and your flows. |
| Safe to turn off | Microsoft Teams (free) | `MicrosoftTeams` | Personal Teams chat built into Windows 11. | Personal Teams chat. |
| Safe to turn off | Copilot | `Microsoft.Copilot` | Microsoft's AI assistant app. | The Copilot app; Copilot on the web still works. |
| Safe to turn off | Spotify | `SpotifyAB.SpotifyMusic` | The Spotify app, often pre-installed. | The Spotify app. |
| Safe to turn off | TikTok | `BytedancePte.Ltd.TikTok` | The TikTok app, often pre-installed. | The TikTok app. |
| Safe to turn off | Candy Crush and other King games | `king.com.*` | Games that come pre-installed. | The game and its progress on this PC. |
| Keep | Gaming Services | `Microsoft.GamingServices` | Runs Game Pass and Microsoft Store games. | Those games won't start without it. |
| Keep | Xbox sign-in for games | `Microsoft.XboxIdentityProvider`<br>`Microsoft.Xbox.TCUI` | Lets PC games sign in to Xbox and show your friends. | Games like Minecraft need it to sign in. |
| Keep | Picture and video format support | `Microsoft.HEIFImageExtension`<br>`Microsoft.WebpImageExtension`<br>`Microsoft.RawImageExtension`<br>`Microsoft.VP9VideoExtensions`<br>`Microsoft.AV1VideoExtension`<br>`Microsoft.HEVCVideoExtension*`<br>`Microsoft.WebMediaExtensions`<br>`Microsoft.MPEG2VideoExtension` | Lets Windows open HEIF, WebP and RAW pictures and play VP9, AV1 and HEVC video. | Photos, video apps and the browser use it to open these files. |
| Keep | Microsoft Store | `Microsoft.WindowsStore` | Installs and updates Store apps. | Removed apps come back through it. |
| Keep | App Installer | `Microsoft.DesktopAppInstaller` | Installs apps and runs winget. | Windows and many installers need it. |
| Keep | Hardware control panel | `NVIDIACorp.NVIDIAControlPanel`<br>`AdvancedMicroDevicesInc-2.AMDRadeonSoftware`<br>`AppUp.IntelGraphicsExperience`<br>`RealtekSemiconductorCorp.RealtekAudioControl` | Settings for your graphics card or sound chip. | It is how you change that hardware's settings. |
