# Orbit release plan: auto-update and code signing

Research only, written 2026-11-08. Nothing in the project was changed. Prices and eligibility rules change often,
so every claim cites the page it came from and the date it was read (all read 2026-11-08 unless noted).
Anything marked **(unverified)** could not be confirmed on an official page.

---

## The short version

- **Auto-update and code signing are two separate things.**
  - *Auto-update* uses Tauri's own key pair. It proves an update came from you. It is free and doesn't
    involve Microsoft at all. You can add it today.
  - *Code signing* uses a certificate from a company Windows trusts. It's what puts your name on the
    installer and slowly stops the SmartScreen "Windows protected your PC" warning.
- **No signing option makes SmartScreen go quiet on day one.** Microsoft now says plainly that EV certificates
  no longer skip the warning. Every new signed app starts out "unrecognized" until enough people install it.
  Signing still matters: your name shows on the prompt, reputation carries over from one version to the next,
  and Windows 11 Smart App Control blocks unsigned apps outright.
- **Which signing option fits depends on where you live (I don't know this):**
  - **US or Canada:** use Azure Artifact Signing, about $9.99 a month. It works cleanly with GitHub Actions and Tauri.
  - **Anywhere else:** Azure's individual plan isn't open to you. Your options are the Certum Open Source
    certificate (about €49 a year, but hard to automate in CI) or waiting for SignPath Foundation.
  - **SignPath Foundation (free):** it wants open-source projects that already have "verifiable reputation".
    A brand-new app will probably be turned down for now. Apply once Orbit has users.
- **Recommended order:**
  1. Add a LICENSE (MIT).
  2. Add the auto-updater.
  3. Set up code signing (Artifact Signing if you're eligible, otherwise Certum).
  4. Later, apply to SignPath and/or list Orbit in the Microsoft Store and winget.
- **Your one-time hassle:** everyone already on v0.2.0 has to download the next version by hand once,
  because v0.2.0 has no updater inside it. Every version after that updates itself.

---

## 1. Auto-update (Tauri 2 updater plugin)

### How it works, in plain words
1. When Orbit starts (or when you click "Check for updates"), it downloads a small file called `latest.json`
   from GitHub Releases. That file lists the newest version number, the installer's download link and a signature.
2. If the version is newer than the one installed, Orbit asks the user. If they say yes, Orbit downloads the
   new `setup.exe` and checks the signature against the public key built into the app.
3. If the signature matches, Orbit closes, the installer runs with a small progress bar, and Orbit restarts.

The signature uses a **Tauri updater key pair** (a private key plus a public key). This key pair is completely
separate from Windows code signing. The private key signs each release in CI. The public key goes into
`tauri.conf.json` and ships inside the app.

Source: [v2.tauri.app/plugin/updater](https://v2.tauri.app/plugin/updater/) (docs source read from the tauri-docs repo, 2026-11-08).

### What has to change (Claude can do all of this in code)

**`src-tauri/tauri.conf.json`**
```json
"bundle": { "createUpdaterArtifacts": true, ... },
"plugins": {
  "updater": {
    "pubkey": "<contents of the .pub file, not a path>",
    "endpoints": ["https://github.com/HeshW/orbit/releases/latest/download/latest.json"],
    "windows": { "installMode": "passive" }
  }
}
```
- `createUpdaterArtifacts: true` makes the build produce `Orbit_x.y.z_x64-setup.exe.sig` next to the installer.
- `installMode`: `"passive"` (the default and Tauri's recommendation) shows a small progress window and needs no clicks.
  `"basicUi"` needs the user to click through. `"quiet"` shows nothing and only works for per-user installs.
  Orbit installs per-user (`installMode: currentUser`), so no admin prompt appears either way.
- `releases/latest/...` only ever points at the newest **non-draft, non-prerelease** release, so drafts and
  prereleases won't be pushed to users.

**Rust**
- Run `cargo add tauri-plugin-updater tauri-plugin-process` in `src-tauri`.
- Register both plugins: `tauri_plugin_updater::Builder::new().build()` and `tauri_plugin_process::init()`.
  The process plugin is what restarts the app afterwards.

**Capabilities, in `src-tauri/capabilities/default.json`**
- Add `"updater:default"` (lets the UI check, download and install).
- Add `"process:allow-restart"`, or `"process:default"`, so the UI can relaunch the app.

**UI (plain JS, `withGlobalTauri` is on, so the API is `window.__TAURI__.updater` / `.process`)**
- **On start:** check quietly a few seconds after launch. If an update exists, show a small banner:
  "Orbit 0.3.0 is available. [Update now] [Later]". Never install without the user clicking.
- **Manual:** add a "Check for updates" button in Settings/About that shows one of three messages:
  "You're up to date", "Update available" or "Couldn't check (offline?)".
- **While updating:** show download progress (the `Started` / `Progress` / `Finished` events), then call
  `relaunch()`. On Windows the app quits on its own when the installer starts, so warn the user first.
- **Offline or GitHub down:** fail quietly at startup and show the error only on a manual check.

**`.github/workflows/release.yml`**
- Add two secrets to the build step's `env:`
  ```yaml
  TAURI_SIGNING_PRIVATE_KEY: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY }}
  TAURI_SIGNING_PRIVATE_KEY_PASSWORD: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY_PASSWORD }}
  ```
- On `tauri-action@v0`, `includeUpdaterJson` already defaults to `true` (checked in `action.yml`
  at action-v0.6.2). Once the build makes `.sig` files, the action uploads `latest.json` and the `.sig`
  to the release by itself. Only NSIS is built, so `updaterJsonPreferNsis` doesn't matter.
- Heads-up: **tauri-action v1.0.0 came out 2026-06-29** and renamed `includeUpdaterJson` to `uploadUpdaterJson`,
  along with other breaking changes ([release notes](https://github.com/tauri-apps/tauri-action/releases/tag/action-v1.0.0)).
  Staying on `@v0` is fine for now. Moving to `@v1` is a separate small job.
- The existing "Attach Orbit-Setup.exe" step can stay. `latest.json` points at the versioned installer,
  not the renamed copy.

### Updater key: safe handling (important after the trojan)
**What the key does:** whoever holds the private key (plus its password) can push an "update" that every Orbit
install will trust and run. Treat it like the keys to the house.

**What losing it means:** you can't ship auto-updates to anyone already installed. You'd make a new key, and
every user would have to download the next installer by hand once. Annoying, but not a disaster for a free app.
Tauri's warning: "if you lose this key you will NOT be able to publish new updates to the users that have the app already installed."

**Don't create it on your PC.** Even with the remote-access trojan removed, assume anything typed or saved
there may have been seen. Two safer options:

- **Option A (recommended): make the key inside GitHub and never download it.**
  1. Create a one-off manual workflow on a GitHub-hosted runner.
  2. It runs `tauri signer generate --ci -p <random password> -w key`, then saves the private key and
     password straight into the repo secrets with `gh secret set`.
  3. It prints only the **public** key, which is safe to show.
  4. Writing secrets needs a short-lived fine-grained token limited to the `orbit` repo with "Secrets: write".
     You create that token and delete it afterwards, along with the workflow.
  - Upside: the private key never touches any PC.
  - Downside: no backup, because GitHub secrets can't be read back. If it's lost, use the recovery above.
- **Option B: a clean machine.** Use a freshly installed PC, or a GitHub Codespace (a fresh cloud machine).
  Generate the key there, paste it into GitHub → Settings → Secrets, and keep one offline backup
  (for example, a password manager on your phone). Then delete the key from that machine.

**Also worth doing (separate from Orbit):** the GitHub sign-in on your PC (`gh` CLI token, browser session)
existed while the trojan was there. Signing out of GitHub everywhere and creating a fresh `gh` token would
close that gap. Otherwise someone could change the workflow and get releases signed for them.

### Steps and effort
| # | Step | Who | Effort |
|---|------|-----|--------|
| 1 | Generate the updater key (Option A or B), save it in the 2 secrets | You approve and create the token, Claude writes the workflow | 30 min |
| 2 | Plugin, config, capabilities, UI banner and button | Claude | 2-4 h |
| 3 | Workflow env vars | Claude | 10 min |
| 4 | Test: release v0.3.0, install it, release v0.3.1, confirm 0.3.0 updates itself | Claude prepares, you push the tags | 1 h |
| 5 | Note in the release notes: "0.2.0 users: download this once by hand" | Claude | 5 min |

---

## 2. Windows code signing options

### What SmartScreen actually does now
Microsoft's page ([SmartScreen reputation for Windows app developers](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/smartscreen-reputation),
updated 2026-08-17) says:
- A signed OV or EV app still shows a warning at first: "app flagged as unrecognized until reputation accumulates;
  verified publisher name is displayed".
- "EV certificates no longer bypass SmartScreen."
- Reputation builds through downloads. It "can take several weeks and hundreds of clean installs", and you can't speed it up.
- With the same certificate, reputation can carry over to new versions. Unsigned apps start from zero every release.
- The Microsoft Store is the only route with no warning at all.
- On Windows 11, Smart App Control blocks unsigned files that don't already have good reputation.

Tauri's own signing guide says the same: "Since 2024, an EV Certificate no longer gives your app an immediate reputation"
([v2.tauri.app/distribute/sign/windows](https://v2.tauri.app/distribute/sign/windows/)).

### Option A: Azure Artifact Signing (renamed from "Trusted Signing")
- **Name:** Yes, it's now called **Artifact Signing** (the docs say "formerly Trusted Signing").
  The GitHub action `azure/trusted-signing-action` now redirects to `Azure/artifact-signing-action`, latest release v2.0.0.
- **Cost:** **Basic $9.99 a month** (5,000 signatures), per the SmartScreen page above. Premium is $99.99 a month for 100,000
  signatures (seen in a search snippet of Microsoft's product page; the pricing page itself showed "$-" for me, **unverified**).
  - Billing starts the day you create the account and isn't prorated.
  - It needs a **paid** Azure subscription (pay-as-you-go). Free or trial subscriptions are refused.
  - Sources: [pricing](https://azure.microsoft.com/pricing/details/artifact-signing/), [FAQ](https://learn.microsoft.com/en-us/azure/artifact-signing/faq) (updated 2026-10-27).
- **Who can get it:**
  - **Individuals must live in the United States or Canada.**
  - **Organizations** qualify in the US, Canada, EU, UK, Australia, NZ, Japan, South Korea, Singapore,
    Switzerland, Norway and Israel ([quickstart](https://learn.microsoft.com/en-us/azure/artifact-signing/quickstart), updated 2026-10-08).
  - Microsoft Q&A answers say organizations need **3+ years of verifiable business history** (in forum answers, not the official docs, so **unverified**).
- **Identity check:**
  - Individuals: a government ID with an address, plus a selfie Face Check through AU10TIX and Microsoft Authenticator (Verified ID).
    The name and address on your Azure billing account must match the ID exactly.
  - The certificate shows **your legal name**, not "LegacyTech". A "LegacyTech" certificate needs the organization route.
  - Time: 1-20 business days for organizations (official). Individuals are often fast, since it's mostly automated (**unverified**).
- **SmartScreen:** builds reputation over time. It's not instant.
- **CI fit: excellent.** No hardware token, and the certificate lives in Microsoft's HSM. Set up a GitHub secret with an Entra
  app registration (`AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`, `AZURE_TENANT_ID`) and set Tauri's sign command:
  ```json
  "bundle": { "windows": { "signCommand": "artifact-signing-cli -e https://<region>.codesigning.azure.net -a <Account> -c <Profile> -d Orbit %1" } }
  ```
  Tauri then signs `orbit.exe` and the NSIS installer **before** it makes the updater `.sig`, so signing and auto-update work together
  with nothing extra (source: Tauri Windows signing guide, "Azure Artifact Signing" section). You'd install `artifact-signing-cli`
  ([Levminer/artifact-signing-cli](https://github.com/Levminer/artifact-signing-cli)) in the workflow.
- **Pros:** cheap, Microsoft's own recommended option, fully automatic in CI, works with tauri-action as-is.
- **Cons:** individuals only in US/Canada. It shows your personal name. Monthly fee even in months with no release.

### Option B: SignPath Foundation (free for open source)
- **Cost:** free.
- **Requirements** ([signpath.org/terms](https://signpath.org/terms), page marked "draft"):
  - **An OSI-approved license for all of it**, with no commercial dual licensing and no proprietary parts.
    **Orbit has no LICENSE file today, so adding one comes first.** MIT, Apache-2.0, GPL-3.0 and MPL-2.0 all qualify.
    MIT is the simplest. Apache-2.0 adds patent protection.
  - Actively maintained, and **already released** in the form to be signed (Orbit qualifies).
  - **"A certain verifiable reputation"** for executable programs: "cannot sign binaries based on source code that nobody knows".
    Acceptance is up to them and can't be appealed. **This is Orbit's weak spot today.**
  - Signed files must come from an automated build of the public repo. For GitHub, every job must run on GitHub-hosted runners
    (Orbit already does).
  - Assign team roles (author, reviewer, approver; you'd hold all of them). Every signing request is **approved by hand**.
    Everyone involved must use MFA.
  - A **code signing policy on the project home page** (the README or orbit.legacytech.me) that includes the text
    "Free code signing provided by SignPath.io, certificate by SignPath Foundation", plus a privacy statement.
  - No malware or PUP behaviour, and no features meant to get around security measures. Orbit can end SYSTEM processes
    and change services. That's normal for a task manager, but expect questions in review (my judgment).
- **Process and timeline:** apply at [signpath.org/apply](https://signpath.org/apply) (the form didn't load for me).
  **No official timeline is published (unverified).** Community write-ups describe weeks.
- **Publisher shown to users:** **"SignPath Foundation"**, not LegacyTech or your name.
- **CI fit: good, but with an extra step for Tauri.**
  1. Upload the unsigned files with `actions/upload-artifact`.
  2. Run `signpath/github-action-submit-signing-request@v3` (with `api-token`, `organization-id`, `project-slug`,
     `signing-policy-slug`, `github-artifact-id`, `wait-for-completion`, `output-artifact-directory`).
  3. Approve the request by hand ([docs.signpath.io/trusted-build-systems/github](https://docs.signpath.io/trusted-build-systems/github)).
  - The catch (my analysis): signing happens *after* Tauri builds, which changes the installer. The updater `.sig` and the
    `latest.json` that tauri-action wrote no longer match. The workflow would have to re-run `tauri signer sign` on the signed
    installer, rebuild `latest.json` and upload it itself. Signing `orbit.exe` *inside* the installer as well takes two
    signing rounds (sign the exe, build NSIS, sign the installer). That's doable, but it means custom workflow work.
- **SmartScreen:** builds over time. The Foundation certificate is shared by many projects, so it may already have
  good reputation (**unverified**).
- **Pros:** free forever, no ID papers, no hardware.
- **Cons:** probably refused until Orbit has users. Not your brand. A manual click on every release. More workflow work.

### Option C: Certum Open Source Code Signing (cheapest certificate you can buy)
- **Cost** ([shop.certum.eu](https://shop.certum.eu/open-source-code-signing-on-simplysign.html)):
  - **Cloud (SimplySign): from €49** (listed as out of stock when I checked).
  - **Card and reader set: from €69** ([set page](https://shop.certum.eu/open-source-code-signing.html)).
  - Roughly one year. Since 2026-02-27, a code signing certificate can last at most 459 days (per Certum, via search snippet).
- **Who can get it:** individual open-source developers in any country. You verify with an ID document
  ([required documents](https://support.certum.eu/en/code-signing-required-documents/)).
  The certificate reads **"Open Source Developer, <your name>"**.
- **Setup time:** a few days for verification (**unverified**).
- **CI fit: poor.**
  - The card version can't be used on GitHub's cloud runners.
  - SimplySign normally needs the desktop app plus a one-time code from the phone app.
  - Community tools pull out the code generator's secret to sign in CI: the `ssign` Rust CLI, and a "Windows App Signing Setup"
    action. These are unofficial, reverse-engineered and could break.
  - Without them, you'd sign each release by hand on your PC, which isn't a good idea right now.
- **SmartScreen:** an OV certificate, so reputation builds over time, the same as the others.
- **Pros:** cheapest paid option, open to anyone, any country.
- **Cons:** awkward automation. Personal name, not LegacyTech. Signing locally means using the PC that had the trojan.
- Other paid OV certificates (Sectigo resellers, SSL.com eSigner and so on) cost roughly $120-$300+ a year and have similar
  "reputation builds over time" behaviour. SSL.com eSigner is CI-friendly but adds a monthly fee (**unverified**, not researched in depth).

### Option D: Microsoft Store (and winget)
- **Microsoft Store:** registration has been **free for individuals** since September 2025
  ([Windows Developer Blog](https://blogs.windows.com/windowsdeveloper/2025/09/10/free-developer-registration-for-individual-developers-on-microsoft-store/)).
  Store installs never show SmartScreen.
  - **But** Tauri only builds EXE/MSI. Store EXE submissions **must already be signed** with a trusted certificate, need a silent
    install (`/S` for NSIS) and an offline WebView2 installer, and Microsoft does **not** re-sign them
    ([MSI/EXE requirements](https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msi/app-package-requirements),
    [Tauri Store guide](https://v2.tauri.app/distribute/microsoft-store/)).
  - Only MSIX gets free Microsoft signing, and Tauri doesn't build MSIX.
  - So the Store is a good **second channel after** you have signing, not a way around it.
- **winget:** free. Add Orbit to `microsoft/winget-pkgs` with a manifest pointing at the GitHub Release. Many Windows power users
  install that way. Installs through winget probably don't show the browser download warning, because winget downloads the file
  itself (**unverified**). It doesn't replace signing.

### Comparison
| | Artifact Signing | SignPath Foundation | Certum Open Source | Microsoft Store |
|---|---|---|---|---|
| Cost | $9.99/mo (Basic) | Free | ~€49 cloud / €69 card, per year | Free (individual) |
| Who | Individuals: US/Canada only. Orgs: 12 regions (+3 yrs history, unverified) | OSI-licensed OSS with "verifiable reputation" | Any individual OSS dev | Anyone, but EXE must already be signed |
| Name shown | Your legal name (or LegacyTech via org route) | "SignPath Foundation" | "Open Source Developer, <name>" | Your publisher name in Store |
| Setup time | Hours plus 1-20 business days for ID | Weeks, may be refused (unverified) | Days (unverified) | Days, after signing exists |
| CI fit | Excellent (signCommand, no token) | Good, but manual approval and extra updater re-sign step | Poor (unofficial tools or manual) | n/a |
| SmartScreen | Builds over time | Builds over time (shared cert may help, unverified) | Builds over time | None for Store installs |
| Works with updater as-is | Yes | Needs custom steps | Yes if signed via signCommand | Store handles updates |

### Recommendation
- **For now:**
  - **If you live in the US or Canada:** **Azure Artifact Signing** (individual, Basic). It's cheap, Microsoft-backed, fully
    automatic in GitHub Actions and works with the updater with no extra steps.
  - **If you don't:** the honest choice is between (a) staying unsigned a little longer while Orbit gains users, keeping the
    clear "More info → Run anyway" note, or (b) the **Certum Open Source cloud certificate** (€49), set up with Claude's help
    and signed via CI using the unofficial SimplySign tools. Never sign on the PC that had the trojan.
- **For later:**
  - Apply to **SignPath Foundation** once Orbit has visible traction (stars, downloads, mentions). It's free for good.
  - Once signed, add **winget** and the **Microsoft Store** as extra ways to install, which gives Store users no warning at all.
  - When LegacyTech has been a registered business for 3+ years, the Artifact Signing **organization** route can put
    "LegacyTech" on the certificate.

---

## 3. Combined plan

| Order | Step | You (Hesham) | Claude | Rough time |
|---|---|---|---|---|
| 1 | Add `LICENSE` (MIT suggested) and note it in README | Pick the license | Add the file | 10 min |
| 2 | Updater key into GitHub Secrets (Option A) | Create and later delete a short-lived fine-grained token; run the workflow | Write the one-off workflow | 30 min |
| 3 | Add updater plugin, UI, capabilities, workflow env | Review | Code it, build, test | Half a day |
| 4 | Release v0.3.0 (first updater build), then v0.3.1 to prove self-update | Push tags (Claude asks first) | Prepare release notes | 1 h |
| 5a | **US/Canada:** Azure account, pay-as-you-go subscription, Artifact Signing account, identity check, app registration secrets | All sign-ups, payment, ID and face check, pasting secrets | Write the `signCommand` and workflow steps, test | 1-2 h of your time, then 1-20 business days waiting |
| 5b | **Elsewhere:** decide to wait, or buy Certum cloud | Purchase and ID check | CI signing setup with unofficial tool | Days |
| 6 | Later: SignPath application, code signing policy page, winget manifest, Store listing | Apply, MFA, approve each release | Policy page, workflow changes, manifests | Weeks |

**Only you can do:** anything with sign-ups, payment, ID checks, accepting terms, creating tokens and secrets, and
approving signing requests. **Claude can do:** all code, config, workflow and docs changes, and testing builds. Claude
asks before any push or tag, and never sees or handles the private key or passwords.

**Realistic timeline:** the updater can ship within a week. Signing goes live 1-4 weeks after you start the identity check
(Artifact Signing), or later if you wait for SignPath.

---

### Sources (all read 2026-11-08)
- Tauri updater plugin: https://v2.tauri.app/plugin/updater/
- Tauri Windows signing: https://v2.tauri.app/distribute/sign/windows/
- Tauri Microsoft Store: https://v2.tauri.app/distribute/microsoft-store/
- tauri-action v0.6.2 `action.yml` and v1.0.0 release notes: https://github.com/tauri-apps/tauri-action/releases
- tauri-cli `signer generate` source (flags `--ci`, `-p`, `-w`): github.com/tauri-apps/tauri, `crates/tauri-cli/src/signer/`
- SmartScreen reputation (updated 2026-08-17): https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/smartscreen-reputation
- Artifact Signing quickstart (updated 2026-10-08): https://learn.microsoft.com/en-us/azure/artifact-signing/quickstart
- Artifact Signing FAQ (updated 2026-10-27): https://learn.microsoft.com/en-us/azure/artifact-signing/faq
- Artifact Signing pricing: https://azure.microsoft.com/pricing/details/artifact-signing/
- Azure/artifact-signing-action: https://github.com/Azure/artifact-signing-action
- SignPath Foundation terms: https://signpath.org/terms · GitHub integration: https://docs.signpath.io/trusted-build-systems/github
- Certum: https://shop.certum.eu/open-source-code-signing-on-simplysign.html , https://shop.certum.eu/open-source-code-signing.html
- Store MSI/EXE requirements: https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msi/app-package-requirements
- Free Store registration: https://blogs.windows.com/windowsdeveloper/2025/09/10/free-developer-registration-for-individual-developers-on-microsoft-store/
