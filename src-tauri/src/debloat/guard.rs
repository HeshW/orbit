//! The never-offer list for Optimize > Debloat. Checked when Orbit analyzes the
//! PC and again right before every change, whatever the reviewed list or the UI
//! says: anything here is shown as Keep and never changed.
//!
//! Covers security (Defender, SmartScreen, Windows Security, firewall), Windows
//! Update and BITS, cryptography, networking, audio, printing (the print
//! spooler is always refused, so a printer is never cut off), RPC and DCOM,
//! Plug and Play, power, user profiles, the event log, Task Scheduler itself,
//! WMI, time sync, the Store and App Installer, .NET and the VC++ runtimes,
//! drivers, graphics and sound vendors' core parts, Windows' own tasks and
//! programs that are not on the reviewed list, and Orbit itself.

use super::Group;

pub const ORBIT: &str = "This is Orbit.";
pub const SECURITY: &str = "It protects this PC.";
pub const UPDATES: &str = "Windows Update needs it.";
pub const NETWORK: &str = "Your network connection needs it.";
pub const AUDIO: &str = "Your sound needs it.";
pub const PRINTING: &str = "Printing needs it.";
pub const GRAPHICS: &str = "Your graphics card needs it.";
pub const CORE: &str = "Windows needs it to run.";
pub const STORE: &str = "Installing and updating apps needs it.";
pub const RUNTIME: &str = "Apps need it to run.";
pub const DRIVER: &str = "It is a driver. Orbit never changes drivers here.";
pub const WINDOWS_TASK: &str = "Part of Windows. Orbit only changes Windows tasks on its reviewed list.";
pub const WINDOWS_FILE: &str = "It runs from the Windows folder. Orbit only changes programs there that are on its reviewed list.";
pub const SYSTEM_APP: &str = "Part of Windows. It can't be removed.";

/// Services refused by name (exact, any case), with the reason.
const SERVICES: &[(&str, &str)] = &[
    // security
    ("WinDefend", SECURITY), ("WdNisSvc", SECURITY), ("WdNisDrv", SECURITY), ("WdFilter", SECURITY),
    ("WdBoot", SECURITY), ("Sense", SECURITY), ("MDCoreSvc", SECURITY), ("SecurityHealthService", SECURITY),
    ("wscsvc", SECURITY), ("mpssvc", SECURITY), ("BFE", SECURITY), ("SgrmBroker", SECURITY),
    ("webthreatdefsvc", SECURITY), ("webthreatdefusersvc", SECURITY), ("SamSs", SECURITY), ("VaultSvc", SECURITY),
    ("KeyIso", SECURITY), ("CryptSvc", SECURITY), ("EFS", SECURITY), ("Netlogon", SECURITY), ("wlidsvc", SECURITY),
    // updates
    ("wuauserv", UPDATES), ("UsoSvc", UPDATES), ("WaaSMedicSvc", UPDATES), ("BITS", UPDATES),
    ("TrustedInstaller", UPDATES), ("DoSvc", UPDATES), ("msiserver", UPDATES),
    // networking
    ("Dnscache", NETWORK), ("Dhcp", NETWORK), ("NlaSvc", NETWORK), ("nsi", NETWORK), ("netprofm", NETWORK),
    ("Netman", NETWORK), ("WlanSvc", NETWORK), ("Wcmsvc", NETWORK), ("LanmanWorkstation", NETWORK),
    ("LanmanServer", NETWORK), ("iphlpsvc", NETWORK), ("WinHttpAutoProxySvc", NETWORK), ("dot3svc", NETWORK),
    ("NcbService", NETWORK), ("DusmSvc", NETWORK), ("RasMan", NETWORK), ("SstpSvc", NETWORK), ("IKEEXT", NETWORK),
    ("PolicyAgent", NETWORK), ("lmhosts", NETWORK), ("mpsdrv", NETWORK), ("tcpip", NETWORK), ("Tcpip6", NETWORK),
    // audio
    ("Audiosrv", AUDIO), ("AudioEndpointBuilder", AUDIO),
    // printing
    ("Spooler", PRINTING), ("PrintNotify", PRINTING),
    // core
    ("RpcSs", CORE), ("RpcEptMapper", CORE), ("DcomLaunch", CORE), ("PlugPlay", CORE), ("Power", CORE),
    ("ProfSvc", CORE), ("EventLog", CORE), ("Schedule", CORE), ("Winmgmt", CORE), ("W32Time", CORE),
    ("gpsvc", CORE), ("LSM", CORE), ("BrokerInfrastructure", CORE), ("CoreMessagingRegistrar", CORE),
    ("SystemEventsBroker", CORE), ("TimeBrokerSvc", CORE), ("UserManager", CORE), ("Themes", CORE),
    ("DeviceInstall", CORE), ("DsmSvc", CORE), ("StateRepository", CORE), ("camsvc", CORE), ("seclogon", CORE),
    ("ShellHWDetection", CORE), ("FontCache", CORE), ("TokenBroker", CORE), ("CDPSvc", CORE), ("Appinfo", CORE),
    ("EventSystem", CORE), ("SENS", CORE), ("WpnService", CORE), ("DispBrokerDesktopSvc", CORE),
    // store and app installs
    ("AppXSvc", STORE), ("ClipSVC", STORE), ("InstallService", STORE), ("LicenseManager", STORE),
    ("AppReadiness", STORE),
    // graphics and sound vendors' core services
    ("NVDisplay.ContainerLocalSystem", GRAPHICS), ("AMD External Events Utility", GRAPHICS),
    ("AMD Crash Defender Service", GRAPHICS), ("cplspcon", GRAPHICS), ("igccservice", GRAPHICS),
    ("IntelAudioService", AUDIO), ("RtkAudioUniversalService", AUDIO), ("RtkAudioService", AUDIO),
    ("NahimicService", AUDIO), ("WavesSysSvc", AUDIO), ("DolbyDAXAPI", AUDIO),
];

/// Service name prefixes refused (any case).
const SERVICE_PREFIXES: &[(&str, &str)] = &[
    ("igfxCUIService", GRAPHICS), ("clr_optimization", RUNTIME), ("NetFramework", RUNTIME),
    ("WdiService", CORE), ("WdiSystem", CORE),
];

/// Words that mark a protected item in a name, display name or path (lowercase).
const WORDS: &[(&str, &str)] = &[
    ("defender", SECURITY), ("smartscreen", SECURITY), ("firewall", SECURITY), ("windows security", SECURITY),
    ("securityhealth", SECURITY), ("antimalware", SECURITY), ("antivirus", SECURITY),
    ("windows update", UPDATES), ("windowsupdate", UPDATES), ("updateorchestrator", UPDATES), ("waasmedic", UPDATES),
    ("cryptograph", SECURITY), ("bitlocker", SECURITY), ("\\tpm\\", SECURITY), ("exploitguard", SECURITY),
    ("print spooler", PRINTING), ("plug and play", CORE), ("time synchronization", CORE),
    ("\\taskscheduler\\", CORE), ("user profile service", CORE), ("\\.net framework\\", RUNTIME),
    ("softwareprotectionplatform", CORE), ("\\servicing\\", UPDATES), ("vcredist", RUNTIME),
    ("visual c++", RUNTIME),
];

/// Startup programs refused by file name (lowercase).
const STARTUP_FILES: &[(&str, &str)] = &[
    ("securityhealthsystray.exe", SECURITY), ("msascuil.exe", SECURITY), ("msmpeng.exe", SECURITY),
    ("rtkauduservice64.exe", AUDIO), ("ravcpl64.exe", AUDIO), ("ravbg64.exe", AUDIO), ("nahimicsvc64.exe", AUDIO),
    ("waveshell.exe", AUDIO), ("maxxaudiopro.exe", AUDIO), ("igfxem.exe", GRAPHICS), ("igfxtray.exe", GRAPHICS),
    ("radeonsoftware.exe", GRAPHICS), ("nvdisplay.container.exe", GRAPHICS), ("ctfmon.exe", CORE),
    ("orbit.exe", ORBIT),
];

/// Package names refused (glob, any case).
const APPS: &[(&str, &str)] = &[
    ("Microsoft.WindowsStore", STORE), ("Microsoft.StorePurchaseApp", STORE), ("Microsoft.DesktopAppInstaller", STORE),
    ("Microsoft.Services.Store.Engagement", STORE), ("Microsoft.Winget.*", STORE),
    ("Microsoft.SecHealthUI", SECURITY), ("Microsoft.Windows.SecHealthUI", SECURITY),
    ("Microsoft.VCLibs*", RUNTIME), ("Microsoft.NET.*", RUNTIME), ("Microsoft.UI.Xaml*", RUNTIME),
    ("Microsoft.WindowsAppRuntime*", RUNTIME), ("MicrosoftCorporationII.WinAppRuntime*", RUNTIME),
    ("Microsoft.Advertising.Xaml", RUNTIME), ("Microsoft.DirectX*", RUNTIME),
    ("Microsoft.*Extension", RUNTIME), ("Microsoft.*Extensions", RUNTIME),
    ("Microsoft.Windows.ShellExperienceHost", CORE), ("Microsoft.Windows.StartMenuExperienceHost", CORE),
    ("Microsoft.Windows.Search", CORE), ("Microsoft.Windows.CloudExperienceHost", CORE),
    ("Microsoft.Windows.ContentDeliveryManager", CORE), ("Microsoft.Windows.Apprep.ChxApp", SECURITY),
    ("Microsoft.Windows.ParentalControls", CORE), ("Microsoft.Windows.PeopleExperienceHost", CORE),
    ("MicrosoftWindows.Client.*", SYSTEM_APP), ("Windows.*", SYSTEM_APP),
    ("Microsoft.AAD.BrokerPlugin", CORE), ("Microsoft.AccountsControl", CORE), ("Microsoft.LockApp", CORE),
    ("Microsoft.MicrosoftEdge*", CORE), ("Microsoft.Win32WebViewHost", CORE), ("Microsoft.ECApp", CORE),
    ("Microsoft.CredDialogHost", CORE), ("Microsoft.AsyncTextService", CORE), ("Microsoft.BioEnrollment", CORE),
    ("NcsiUwpApp", NETWORK), ("windows.immersivecontrolpanel", CORE),
    ("NVIDIACorp.*", GRAPHICS), ("AdvancedMicroDevicesInc*", GRAPHICS), ("AppUp.Intel*", GRAPHICS),
    ("RealtekSemiconductorCorp.*", AUDIO), ("DolbyLaboratories.*", AUDIO), ("*Waves*Audio*", AUDIO),
    ("*Orbit*", ORBIT),
];

fn lower(s: &str) -> String {
    s.to_lowercase()
}

fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

fn windows_dir() -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    format!("{}\\", lower(root.trim_end_matches('\\')))
}

fn words(hay: &str) -> Option<&'static str> {
    let hay = lower(hay);
    WORDS.iter().find(|(w, _)| hay.contains(w)).map(|(_, why)| *why)
}

fn is_orbit(hay: &str) -> bool {
    let h = lower(hay);
    let leaf = file_name(&h);
    leaf == "orbit" || leaf.starts_with("orbit ") || leaf == "orbit.exe" || h.contains("\\orbit\\") || h.contains("legacytech.orbit")
}

/// What Orbit knows about an item when it checks the guard.
pub struct Subject<'a> {
    pub group: Group,
    /// Startup entry name, task path, service name or package name.
    pub key: &'a str,
    /// Display name (services), "" otherwise.
    pub display: &'a str,
    /// The program or binary path, "" if unknown.
    pub path: &'a str,
    /// On Orbit's reviewed list.
    pub reviewed: bool,
    /// Apps: a framework, resource or system package (from the package itself).
    pub system_package: bool,
}

/// Why Orbit must not change this item, or None when it may.
pub fn refuse(s: &Subject) -> Option<&'static str> {
    if is_orbit(s.key) || is_orbit(s.path) {
        return Some(ORBIT);
    }
    match s.group {
        Group::Service => {
            if let Some((_, why)) = SERVICES.iter().find(|(n, _)| n.eq_ignore_ascii_case(s.key)) {
                return Some(why);
            }
            let k = lower(s.key);
            if let Some((_, why)) = SERVICE_PREFIXES.iter().find(|(p, _)| k.starts_with(&lower(p))) {
                return Some(why);
            }
            if lower(s.path).ends_with(".sys") {
                return Some(DRIVER);
            }
            words(&format!("{} | {} | {}", s.key, s.display, s.path))
        }
        Group::Task => {
            if let Some(why) = words(&format!("{} | {}", s.key, s.path)) {
                return Some(why);
            }
            if !s.reviewed && lower(s.key).starts_with("\\microsoft\\") {
                return Some(WINDOWS_TASK);
            }
            None
        }
        Group::Startup => {
            let file = lower(file_name(s.path));
            if let Some((_, why)) = STARTUP_FILES.iter().find(|(f, _)| *f == file) {
                return Some(why);
            }
            if let Some(why) = words(&format!("{} | {}", s.key, s.path)) {
                return Some(why);
            }
            // Store apps start through explorer.exe ("shell:AppsFolder\..."): that is the app, not Windows.
            if !s.reviewed && !s.path.is_empty() && file != "explorer.exe" && lower(s.path).starts_with(&windows_dir()) {
                return Some(WINDOWS_FILE);
            }
            None
        }
        Group::App => {
            if s.system_package {
                return Some(SYSTEM_APP);
            }
            if let Some((_, why)) = APPS.iter().find(|(p, _)| super::known::glob(p, s.key)) {
                return Some(why);
            }
            None
        }
    }
}
