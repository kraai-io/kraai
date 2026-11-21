// Picks the right release download for the visitor's computer.

export const REPO = "kraai-io/kraai";
export const RELEASES_URL = `https://github.com/${REPO}/releases`;
// Only nightly pre-releases exist so far, so /releases/latest has nothing to
// return. The first entry of the list is the newest release of any kind.
export const RELEASES_API = `https://api.github.com/repos/${REPO}/releases?per_page=1`;

export type Os = "macos" | "windows" | "linux";
export type Arch = "aarch64" | "x86_64";

export interface Platform {
  os: Os | null;
  arch: Arch | null;
}

export interface ReleaseAsset {
  name: string;
  browser_download_url: string;
}

export interface Release {
  tag_name: string;
  html_url: string;
  assets: ReleaseAsset[];
}

export const OS_NAMES: Record<Os, string> = { macos: "macOS", windows: "Windows", linux: "Linux" };

export const ARCH_NAMES: Record<Os, Record<Arch, string>> = {
  macos: { aarch64: "Apple silicon", x86_64: "Intel" },
  windows: { x86_64: "x64", aarch64: "ARM64" },
  linux: { x86_64: "x86_64", aarch64: "ARM64" },
};

/** The architecture each OS gets when the browser doesn't say. */
export const DEFAULT_ARCH: Record<Os, Arch> = { macos: "aarch64", windows: "x86_64", linux: "x86_64" };

const TRIPLES: Record<Os, string> = {
  macos: "apple-darwin",
  windows: "pc-windows-msvc",
  linux: "unknown-linux-gnu",
};

/** The release file name for one platform, such as kraai-aarch64-apple-darwin.tar.gz. */
export function assetName(os: Os, arch: Arch): string {
  return `kraai-${arch}-${TRIPLES[os]}.${os === "windows" ? "zip" : "tar.gz"}`;
}

/**
 * Reads the OS and CPU from the browser. `platformHint` is the User-Agent
 * Client Hints platform and `archHint` its architecture ("arm" or "x86"),
 * when the browser offers them.
 */
export function detectPlatform(userAgent: string, platformHint = "", archHint = ""): Platform {
  const source = `${platformHint} ${userAgent}`.toLowerCase();
  let os: Os | null = null;
  // iPhones and Android phones can't run Kraai, so they get no OS.
  if (/iphone|ipad|ipod|android/.test(source)) os = null;
  else if (/mac/.test(source)) os = "macos";
  else if (/win/.test(source)) os = "windows";
  else if (/linux|x11|cros/.test(source)) os = "linux";

  let arch: Arch | null = null;
  const hint = archHint.toLowerCase();
  if (hint === "arm") arch = "aarch64";
  else if (hint === "x86") arch = "x86_64";
  else if (/aarch64|arm64|armv8/.test(source)) arch = "aarch64";
  else if (/x86_64|x64|amd64|win64|wow64/.test(source)) arch = "x86_64";
  // Browsers on Apple silicon still report "Intel Mac OS X", so a Mac without
  // a hint keeps the default instead.
  if (os === "macos" && !hint) arch = null;
  return { os, arch };
}

/** The download link for one platform, falling back to the releases page. */
export function assetUrl(release: Release | null, os: Os, arch: Arch): string {
  const asset = release?.assets.find((each) => each.name === assetName(os, arch));
  return asset?.browser_download_url ?? release?.html_url ?? RELEASES_URL;
}
