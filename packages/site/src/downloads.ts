// The download buttons, the full build list, and the copy buttons.
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

function fillLinks(platform: Platform, release: Release | null): void {
  // Each build in the list: data-asset="<os> <arch>".
  for (const link of document.querySelectorAll<HTMLAnchorElement>("[data-asset]")) {
    const [os, arch] = (link.dataset.asset ?? "").split(" ") as [Os, Arch];
    link.href = assetUrl(release, os, arch);
  }

  // The main buttons point at the visitor's own build when we can tell what it is.
  for (const button of document.querySelectorAll<HTMLAnchorElement>("[data-download]")) {
    if (!platform.os) continue;
    const arch = platform.arch ?? DEFAULT_ARCH[platform.os];
    button.href = assetUrl(release, platform.os, arch);
    button.textContent =
      button.dataset.download === "full"
        ? `Download for ${OS_NAMES[platform.os]} (${ARCH_NAMES[platform.os][arch]})`
        : `Download for ${OS_NAMES[platform.os]}`;
  }

  const tag = document.querySelector<HTMLElement>("[data-release-tag]");
  if (tag && release) tag.textContent = `Newest build: ${release.tag_name}.`;
}

function setUpCopy(): void {
  for (const button of document.querySelectorAll<HTMLButtonElement>("[data-copy]")) {
    const command = button.parentElement?.querySelector<HTMLElement>("[data-command]");
    if (!command) continue;
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
      button.setAttribute("aria-label", "Copied");
      clearTimeout(timer);
      timer = window.setTimeout(() => {
        button.classList.remove("is-copied");
        button.setAttribute("aria-label", "Copy command");
      }, 1600);
    });
  }
}

export async function setUpDownloads(): Promise<void> {
  setUpCopy();
  const platform = await readPlatform();
  fillLinks(platform, null);
  fillLinks(platform, await fetchRelease());
}
