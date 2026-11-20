// The hero download button, the install tabs, and the download cards.
import {
  ARCH_NAMES,
  DEFAULT_ARCH,
  OS_NAMES,
  RELEASES_API,
  assetUrl,
  detectPlatform,
  type Arch,
  type Os,
  type Platform,
  type Release,
} from "./release.ts";

interface NavigatorUAData {
  platform: string;
  getHighEntropyValues(hints: string[]): Promise<{ architecture?: string }>;
}

async function readPlatform(): Promise<Platform> {
  const uaData = (navigator as Navigator & { userAgentData?: NavigatorUAData }).userAgentData;
  let archHint = "";
  if (uaData) {
    try {
      archHint = (await uaData.getHighEntropyValues(["architecture"])).architecture ?? "";
    } catch {
      // The hint is optional; the default architecture stands in.
    }
  }
  return detectPlatform(navigator.userAgent, uaData?.platform ?? "", archHint);
}

async function fetchRelease(): Promise<Release | null> {
  try {
    const response = await fetch(RELEASES_API, { headers: { Accept: "application/vnd.github+json" } });
    if (!response.ok) return null;
    const [release] = (await response.json()) as Release[];
    return release ?? null;
  } catch {
    return null;
  }
}

function otherArch(arch: Arch): Arch {
  return arch === "aarch64" ? "x86_64" : "aarch64";
}

/** The build a card offers first: the visitor's own CPU on their own OS, else the usual one. */
function preferredArch(os: Os, platform: Platform): Arch {
  return platform.os === os && platform.arch ? platform.arch : DEFAULT_ARCH[os];
}

function fillLinks(platform: Platform, release: Release | null): void {
  for (const link of document.querySelectorAll<HTMLAnchorElement>("[data-asset]")) {
    const os = link.dataset.asset as Os;
    const primary = preferredArch(os, platform);
    const arch = link.hasAttribute("data-alternate") ? otherArch(primary) : primary;
    link.href = assetUrl(release, os, arch);
    if (link.classList.contains("button")) link.textContent = `Download for ${ARCH_NAMES[os][arch]}`;
    else if (link.hasAttribute("data-alternate")) link.textContent = `${ARCH_NAMES[os][arch]} build`;
  }

  const hero = document.querySelector<HTMLAnchorElement>("[data-hero-download]");
  if (hero && platform.os) {
    hero.href = assetUrl(release, platform.os, preferredArch(platform.os, platform));
    hero.textContent = `Download for ${OS_NAMES[platform.os]}`;
  }

  const tag = document.querySelector<HTMLElement>("[data-release-tag]");
  if (tag && release) tag.textContent = `Newest build: ${release.tag_name}`;
}

function setUpInstallTabs(platform: Platform): void {
  const tabs = [...document.querySelectorAll<HTMLButtonElement>("[data-install-tab]")];
  const panel = document.querySelector<HTMLElement>("#install-panel");
  const nix = document.querySelector<HTMLElement>("[data-install-nix]");
  const windows = document.querySelector<HTMLElement>("[data-install-windows]");
  if (!panel || !nix || !windows) return;

  const select = (tab: HTMLButtonElement, focus: boolean) => {
    for (const each of tabs) {
      const selected = each === tab;
      each.setAttribute("aria-selected", String(selected));
      each.tabIndex = selected ? 0 : -1;
    }
    panel.setAttribute("aria-labelledby", tab.id);
    const isWindows = tab.dataset.installTab === "windows";
    nix.hidden = isWindows;
    windows.hidden = !isWindows;
    if (focus) tab.focus();
  };

  tabs.forEach((tab, index) => {
    tab.addEventListener("click", () => select(tab, false));
    tab.addEventListener("keydown", (event) => {
      const step = { ArrowRight: 1, ArrowLeft: -1 }[event.key];
      if (!step) return;
      event.preventDefault();
      select(tabs[(index + step + tabs.length) % tabs.length], true);
    });
  });

  const own = tabs.find((tab) => tab.dataset.installTab === platform.os);
  if (own) select(own, false);
}

function setUpCopy(): void {
  const button = document.querySelector<HTMLButtonElement>("[data-copy]");
  const command = document.querySelector<HTMLElement>("[data-command]");
  const status = document.querySelector<HTMLElement>("[data-copy-status]");
  if (!button || !command || !status) return;
  const note = status.textContent ?? "";
  let timer = 0;
  button.addEventListener("click", async () => {
    try {
      await navigator.clipboard.writeText(command.textContent ?? "");
    } catch {
      // Without clipboard access, select the command so it can be copied by hand.
      getSelection()?.selectAllChildren(command);
      return;
    }
    button.classList.add("is-copied");
    status.textContent = "Copied to the clipboard.";
    clearTimeout(timer);
    timer = window.setTimeout(() => {
      button.classList.remove("is-copied");
      status.textContent = note;
    }, 1600);
  });
}

export async function setUpDownloads(): Promise<void> {
  setUpCopy();
  const platform = await readPlatform();
  setUpInstallTabs(platform);
  fillLinks(platform, null);
  fillLinks(platform, await fetchRelease());
}
