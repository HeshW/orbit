//! The reviewed list behind Optimize > Debloat: every verdict Orbit gives comes
//! from here, never from a guess. Kept short and conservative. An item on this
//! PC that matches no entry is "Unknown" (leave it).
//!
//! Patterns are matched without case; `*` stands for any text.
//! - Startup apps: the entry name or the program's file name ("steam.exe").
//! - Scheduled tasks: the full task path ("\Microsoft\Windows\Maps\MapsUpdateTask").
//! - Services: the service name ("DiagTrack").
//! - Built-in apps: the package name ("Microsoft.BingWeather").
//!
//! `note` is "what you lose" for Safe to turn off and "why" for Keep, one line.
//! `docs/features/debloat.md` lists every entry; a test keeps the two in step.
//! `guard.rs` still refuses anything on its never-offer list, whatever this says.

use super::{Group, Verdict};

pub struct Known {
    pub group: Group,
    pub patterns: &'static [&'static str],
    pub name: &'static str,
    pub what: &'static str,
    pub verdict: Verdict,
    pub note: &'static str,
}

const fn off(group: Group, patterns: &'static [&'static str], name: &'static str, what: &'static str, note: &'static str) -> Known {
    Known { group, patterns, name, what, verdict: Verdict::Off, note }
}

const fn keep(group: Group, patterns: &'static [&'static str], name: &'static str, what: &'static str, note: &'static str) -> Known {
    Known { group, patterns, name, what, verdict: Verdict::Keep, note }
}

use Group::{App, Service, Startup, Task};

pub static KNOWN: &[Known] = &[
    // ---------------------------------------------------------------- startup apps
    off(Startup, &["steam.exe"], "Steam", "Opens Steam in the background when you sign in.",
        "Steam opens when you start it; game updates wait until then."),
    off(Startup, &["epicgameslauncher.exe"], "Epic Games Launcher", "Opens the Epic launcher when you sign in.",
        "The launcher opens when you start it; game updates wait until then."),
    off(Startup, &["eadesktop.exe", "origin.exe"], "EA app", "Opens the EA app when you sign in.",
        "The EA app opens when you start it; game updates wait until then."),
    off(Startup, &["riotclientservices.exe"], "Riot Client", "Starts the Riot Client in the background at sign-in.",
        "Riot games update when you open the client."),
    off(Startup, &["spotify.exe"], "Spotify", "Opens Spotify, minimized, when you sign in.",
        "Spotify opens when you start it."),
    off(Startup, &["Discord"], "Discord", "Opens Discord when you sign in.",
        "You show as offline until you open Discord."),
    off(Startup, &["MicrosoftEdgeAutoLaunch_*"], "Microsoft Edge in the background", "Starts Edge hidden at sign-in so it opens faster later.",
        "Edge takes a moment longer to open the first time."),
    off(Startup, &["GoogleChromeAutoLaunch_*"], "Google Chrome in the background", "Starts Chrome hidden at sign-in so it opens faster later.",
        "Chrome takes a moment longer to open the first time."),
    off(Startup, &["ms-teams.exe", "com.squirrel.Teams.Teams"], "Microsoft Teams", "Opens Teams when you sign in.",
        "Chats and calls reach you once you open Teams."),
    off(Startup, &["adobecollabsync.exe"], "Adobe Acrobat sync", "Syncs shared PDFs and reviews with Adobe's cloud.",
        "Shared reviews sync when you open Acrobat."),
    off(Startup, &["updaterstartuputility.exe"], "Adobe updater at sign-in", "Checks for Adobe app updates when you sign in.",
        "Adobe apps check for updates when you open them."),
    off(Startup, &["browser_assistant.exe"], "Opera browser assistant", "A desktop helper that shows Opera news and offers.",
        "Nothing in the Opera browser itself."),
    off(Startup, &["wshelper.exe"], "Wondershare helper", "Wondershare's background helper for updates and offers.",
        "Wondershare apps check for updates when you open them."),
    keep(Startup, &["onedrive.exe"], "Microsoft OneDrive", "Keeps your files in sync with OneDrive.",
        "Turning it off pauses file sync and backup until you open OneDrive."),
    keep(Startup, &["GoogleUpdaterTaskUser*"], "Google updater", "Wakes Google's updater at sign-in.",
        "Chrome gets security fixes through it."),
    keep(Startup, &["SecurityHealth", "securityhealthsystray.exe"], "Windows Security icon", "Shows the Windows Security icon and its alerts.",
        "You would miss security alerts."),
    keep(Startup, &["rtkauduservice64.exe", "ravcpl64.exe"], "Realtek audio", "Runs the controls for your Realtek sound chip.",
        "Headphone and microphone jacks may stop switching by themselves."),

    // ---------------------------------------------------------------- scheduled tasks
    off(Task, &[r"\Microsoft\Windows\Application Experience\Microsoft Compatibility Appraiser"], "Windows compatibility check",
        "Scans your apps and sends compatibility data to Microsoft before big updates.",
        "Windows may not warn you early about an app that won't run after a big update."),
    off(Task, &[r"\Microsoft\Windows\Application Experience\ProgramDataUpdater"], "App usage report",
        "Collects data about the programs you use for Microsoft.", "Nothing you will notice."),
    off(Task, &[r"\Microsoft\Windows\Customer Experience Improvement Program\*"], "Windows usage reports",
        "Send Windows and USB usage data to Microsoft's improvement program.", "Nothing you will notice."),
    off(Task, &[r"\Microsoft\Windows\Autochk\Proxy"], "Disk check report",
        "Sends disk check results to Microsoft's improvement program.", "Nothing; disk checks still run."),
    off(Task, &[r"\Microsoft\Windows\DiskDiagnostic\Microsoft-Windows-DiskDiagnosticDataCollector"], "Disk diagnostics report",
        "Collects disk and system details and sends them to Microsoft.", "Nothing you will notice."),
    off(Task, &[r"\Microsoft\Windows\Feedback\Siuf\DmClient", r"\Microsoft\Windows\Feedback\Siuf\DmClientOnScenarioDownload"], "Feedback upload",
        "Sends feedback and diagnostic data to Microsoft.", "Nothing; Feedback Hub still works when you open it."),
    off(Task, &[r"\Microsoft\Windows\Device Information\Device", r"\Microsoft\Windows\Device Information\Device User"], "Device report",
        "Sends details about this PC's hardware to Microsoft.", "Nothing you will notice."),
    off(Task, &[r"\Microsoft\Windows\Windows Error Reporting\QueueReporting"], "Error report sender",
        "Sends saved crash reports to Microsoft.", "Microsoft gets no reports about apps that crashed here."),
    off(Task, &[r"\Microsoft\Windows\Maps\MapsUpdateTask", r"\Microsoft\Windows\Maps\MapsToastTask"], "Offline maps updater",
        "Updates the offline maps you downloaded and tells you about it.", "Offline maps update when you open Maps."),
    off(Task, &[r"\Microsoft\Office\OfficeTelemetryAgentLogOn*", r"\Microsoft\Office\OfficeTelemetryAgentFallBack*"], "Office usage report",
        "Sends Office usage data to Microsoft.", "Nothing you will notice; Office works the same."),
    off(Task, &[r"\OneDrive Reporting Task-*"], "OneDrive usage report",
        "Sends OneDrive usage data to Microsoft.", "Nothing; OneDrive keeps syncing."),
    off(Task, &[r"\ZoomUpdateTaskUser*"], "Zoom updater",
        "Checks for Zoom updates in the background.", "Zoom checks for updates when you open it."),
    keep(Task, &[r"\MicrosoftEdgeUpdateTask*"], "Microsoft Edge updater",
        "Checks for Edge updates.", "Edge gets security fixes through it, and Windows uses parts of Edge."),
    keep(Task, &[r"\GoogleUpdateTask*", r"\GoogleSystem\GoogleUpdater\*", r"\GoogleUser\GoogleUpdater\*"], "Google updater",
        "Checks for Chrome and other Google app updates.", "Chrome gets security fixes through it."),
    keep(Task, &[r"\Adobe Acrobat Update Task"], "Adobe Acrobat updater",
        "Checks for Acrobat updates.", "Acrobat gets security fixes through it."),
    keep(Task, &[r"\OneDrive Standalone Update Task-*"], "OneDrive updater",
        "Keeps OneDrive up to date.", "OneDrive gets fixes through it."),
    keep(Task, &[r"\Microsoft\Office\Office Automatic Updates 2.0"], "Office updater",
        "Installs Office updates.", "Office gets security fixes through it."),
    keep(Task, &[r"\NVIDIA App SelfUpdate_*"], "NVIDIA app updater",
        "Keeps the NVIDIA app up to date.", "Your graphics drivers come through the NVIDIA app."),

    // ---------------------------------------------------------------- services
    off(Service, &["DiagTrack"], "Windows telemetry",
        "Sends diagnostic and usage data to Microsoft.", "Nothing you will notice; Windows works the same."),
    off(Service, &["dmwappushservice"], "Device management messages",
        "Passes device-management and diagnostic messages.", "A PC run by work or school may need it."),
    off(Service, &["MapsBroker"], "Downloaded maps manager",
        "Keeps offline maps up to date.", "Offline maps update when you open Maps."),
    off(Service, &["TrkWks"], "Shortcut link tracker",
        "Keeps shortcuts working when files move between drives on a network.",
        "A shortcut to a file that moved on a network drive may stop working."),
    off(Service, &["AdobeUpdateService"], "Adobe update service",
        "Installs updates for Adobe Creative Cloud apps.", "Adobe apps update when you open Creative Cloud."),
    off(Service, &["AGSService", "AGMService"], "Adobe genuine check",
        "Checks that your Adobe apps are genuine.", "Nothing for licensed apps."),
    keep(Service, &["SysMain"], "App preloader (SysMain)",
        "Learns which apps you use and loads them ahead of time.", "Apps open faster with it; turning it off rarely helps."),
    keep(Service, &["WSearch"], "Windows Search indexer",
        "Indexes your files so Start and File Explorer find them fast.", "Search gets slow or misses files without it."),
    keep(Service, &["edgeupdate", "edgeupdatem"], "Microsoft Edge update service",
        "Installs Edge updates.", "Edge gets security fixes through it."),
    keep(Service, &["gupdate", "gupdatem", "GoogleUpdaterService*", "GoogleUpdaterInternalService*"], "Google update service",
        "Installs Chrome and Google app updates.", "Chrome gets security fixes through it."),
    keep(Service, &["AdobeARMservice"], "Adobe Acrobat update service",
        "Installs Acrobat updates.", "Acrobat gets security fixes through it."),
    keep(Service, &["ClickToRunSvc"], "Office Click-to-Run",
        "Runs and updates Microsoft Office.", "Office needs it to start and update."),
    keep(Service, &["GamingServices", "GamingServicesNet"], "Xbox Gaming Services",
        "Runs Game Pass and Microsoft Store games.", "Those games won't start without it."),

    // ---------------------------------------------------------------- built-in apps
    off(App, &["Clipchamp.Clipchamp"], "Clipchamp", "Microsoft's video editor.", "The Clipchamp app."),
    off(App, &["Microsoft.BingNews"], "Microsoft News", "A news headlines app.", "The News app; news sites still open in your browser."),
    off(App, &["Microsoft.BingWeather"], "Weather", "A weather forecast app.", "The Weather app."),
    off(App, &["Microsoft.BingSearch"], "Microsoft Bing", "The Bing search app.", "The Bing app; search in Start keeps working."),
    off(App, &["Microsoft.GetHelp"], "Get Help", "Microsoft's support app.", "Some Windows troubleshooters open in Get Help."),
    off(App, &["Microsoft.Getstarted"], "Tips", "Tips about Windows features.", "The Tips app."),
    off(App, &["Microsoft.WindowsFeedbackHub"], "Feedback Hub", "Sends your feedback to Microsoft.", "You can't send feedback to Microsoft."),
    off(App, &["Microsoft.MicrosoftSolitaireCollection"], "Solitaire Collection", "Card games with ads.", "The card games."),
    off(App, &["Microsoft.Microsoft3DViewer", "Microsoft.Print3D", "Microsoft.3DBuilder"], "3D apps", "Views and prints 3D models.", "Opening 3D model files."),
    off(App, &["Microsoft.MixedReality.Portal"], "Mixed Reality Portal", "Sets up Windows VR headsets.", "Windows Mixed Reality headset setup."),
    off(App, &["Microsoft.MicrosoftOfficeHub"], "Microsoft 365 hub", "A launcher for Office apps and files.", "The launcher; installed Office apps keep working."),
    off(App, &["Microsoft.SkypeApp"], "Skype", "Skype calls and chat. Microsoft retired Skype in 2025.", "The Skype app."),
    off(App, &["Microsoft.People"], "People", "A contacts app.", "The People app."),
    off(App, &["Microsoft.WindowsMaps"], "Maps", "Maps and directions.", "The Maps app and its offline maps."),
    off(App, &["microsoft.windowscommunicationsapps"], "Mail and Calendar", "Microsoft's older mail and calendar apps.", "The Mail and Calendar apps; Outlook replaces them."),
    off(App, &["Microsoft.ZuneVideo"], "Movies & TV", "A video player and store.", "The Movies & TV app; other video players still work."),
    off(App, &["Microsoft.YourPhone"], "Phone Link", "Shows texts, calls and photos from your phone.", "Phone Link."),
    off(App, &["Microsoft.549981C3F5F10"], "Cortana", "Microsoft's retired voice assistant.", "The Cortana app."),
    off(App, &["Microsoft.GamingApp"], "Xbox app", "Game Pass and Xbox games on PC.", "Installing and opening Game Pass games."),
    off(App, &["Microsoft.XboxGamingOverlay"], "Xbox Game Bar", "The Win+G overlay for screenshots, clips and chat.", "Game Bar and its screenshots and clips."),
    off(App, &["Microsoft.XboxGameOverlay", "Microsoft.XboxSpeechToTextOverlay"], "Xbox overlay parts", "Older Game Bar parts and party chat captions.", "Older Game Bar features."),
    off(App, &["Microsoft.Windows.DevHome"], "Dev Home", "Microsoft's retired developer dashboard.", "The Dev Home app."),
    off(App, &["Microsoft.PowerAutomateDesktop"], "Power Automate", "Automates tasks on the desktop.", "The Power Automate app and your flows."),
    off(App, &["MicrosoftTeams"], "Microsoft Teams (free)", "Personal Teams chat built into Windows 11.", "Personal Teams chat."),
    off(App, &["Microsoft.Copilot"], "Copilot", "Microsoft's AI assistant app.", "The Copilot app; Copilot on the web still works."),
    off(App, &["SpotifyAB.SpotifyMusic"], "Spotify", "The Spotify app, often pre-installed.", "The Spotify app."),
    off(App, &["BytedancePte.Ltd.TikTok"], "TikTok", "The TikTok app, often pre-installed.", "The TikTok app."),
    off(App, &["king.com.*"], "Candy Crush and other King games", "Games that come pre-installed.", "The game and its progress on this PC."),
    keep(App, &["Microsoft.GamingServices"], "Gaming Services", "Runs Game Pass and Microsoft Store games.", "Those games won't start without it."),
    keep(App, &["Microsoft.XboxIdentityProvider", "Microsoft.Xbox.TCUI"], "Xbox sign-in for games", "Lets PC games sign in to Xbox and show your friends.",
        "Games like Minecraft need it to sign in."),
    keep(App, &["Microsoft.HEIFImageExtension", "Microsoft.WebpImageExtension", "Microsoft.RawImageExtension", "Microsoft.VP9VideoExtensions",
        "Microsoft.AV1VideoExtension", "Microsoft.HEVCVideoExtension*", "Microsoft.WebMediaExtensions", "Microsoft.MPEG2VideoExtension"],
        "Picture and video format support", "Lets Windows open HEIF, WebP and RAW pictures and play VP9, AV1 and HEVC video.",
        "Photos, video apps and the browser use it to open these files."),
    keep(App, &["Microsoft.WindowsStore"], "Microsoft Store", "Installs and updates Store apps.", "Removed apps come back through it."),
    keep(App, &["Microsoft.DesktopAppInstaller"], "App Installer", "Installs apps and runs winget.", "Windows and many installers need it."),
    keep(App, &["NVIDIACorp.NVIDIAControlPanel", "AdvancedMicroDevicesInc-2.AMDRadeonSoftware", "AppUp.IntelGraphicsExperience",
        "RealtekSemiconductorCorp.RealtekAudioControl"], "Hardware control panel",
        "Settings for your graphics card or sound chip.", "It is how you change that hardware's settings."),
];

/// The first entry in `group` whose pattern matches one of `keys`.
pub fn lookup<'a>(list: &'a [Known], group: Group, keys: &[&str]) -> Option<&'a Known> {
    list.iter()
        .filter(|k| k.group == group)
        .find(|k| k.patterns.iter().any(|p| keys.iter().any(|key| glob(p, key))))
}

/// Case-insensitive match where `*` stands for any text (also none).
pub fn glob(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && p[pi] != '*' && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}
